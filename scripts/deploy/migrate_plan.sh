#!/usr/bin/env bash
# =============================================================================
# 待应用迁移判定库（source 使用，不单独执行）
#
# 口径 = 迁移引擎 nik-db 的口径，**按名字做集合差**，不是「> max(applied) 比大小」：
#
#   disk  = $MIG_ROOT_REL/*/V<n>__<name>.up.sql（全部子目录；引擎 path="migrations" 递归装载）
#   done  = schema_migrations 里 kind='versioned' 且 success=true 的 name 集合
#   todo  = disk - done          ← 按版本号数值升序（版本号只用于排序/展示）
#
# 依据（都在 backend/vendor/nik-db/src/migrate/ 里，改口径前先读）：
#   · kind.rs:84-111       AppliedVia 只认 executed / baseline / imported 三个值；
#   · history.rs:219-221   记录表里出现别的值 → DbError::Migration
#                          → 服务**启动期 FATAL**（2026-11-05 手工用 'deploy-backfill' 踩过）；
#   · mod.rs:1263-1275     needs_apply：键是 (kind,name)，有 success 记录就跳过，
#                          **没有记录就执行 —— 完全不看版本号大小**；
#   · mod.rs:852-888       pending 收集：低版本（gap）也进 pending，只打 out_of_order WARN。
#
# 为什么不能用 max(version) 比大小（2026-11-05 发版中断的真因）：
#   库里最大已应用版本是 V0235，但 V0220/V0221/V0226..V0234 因为历史原因没记账；
#   「> max」只会挑 0236..0241，gap 里的 11 条永远挑不出来 —— 于是 V0240 依赖的
#   biz_budget_subject_map（V0227 建）不存在，迁移在 [5/8] 直接失败、部署中断。
#
# 调用方必须先定义：
#   mp_q "<SQL>"     执行一条查询，输出 -Atq 风格（纯文本、字段间无分隔）
# 可选：
#   MP_REF=<git ref> 从该 ref 的树里读迁移清单（部署时传 $NEW_HEAD）；
#                    留空则扫工作区（自测/演练用）
#   MP_REPO=<path>   仓库根，默认当前目录
# =============================================================================

MIG_ROOT_REL="${MIG_ROOT_REL:-backend/migrations}"
# 引擎认可的版本化迁移文件名；认不出的落盘文件要么改文件名，要么显式失败（不静默跳过）
MIG_PATH_RE='^backend/migrations/[A-Za-z0-9_.-]+/V[0-9]+__[A-Za-z0-9_]+\.up\.sql$'
# 引擎认可的 applied_via（kind.rs:95-111）；写记录时只用 imported
MP_ALLOWED_APPLIED_VIA="executed imported baseline"
MP_WRITE_APPLIED_VIA="imported"

declare -gA MP_DB_APPLIED=()
declare -g  MP_DB_APPLIED_COUNT=0

mp_name_of() { local b="${1##*/}"; b="${b#*__}"; printf '%s' "${b%.up.sql}"; }
mp_ver_of()  { local b="${1##*/}"; b="${b%%__*}"; printf '%d' "$((10#${b#V}))"; }

# 磁盘上的版本化 up 脚本（仓库相对路径，按版本数值升序）
mp_disk_files() {
  local raw
  if [ -n "${MP_REF:-}" ]; then
    raw=$(git -C "${MP_REPO:-.}" ls-tree -r --name-only "$MP_REF" "$MIG_ROOT_REL/")
  else
    raw=$(cd "${MP_REPO:-.}" && find "$MIG_ROOT_REL" -type f -name 'V*.up.sql' | sed 's#^\./##')
  fi
  printf '%s\n' "$raw" \
    | grep -E "$MIG_PATH_RE" \
    | awk -F/ '{ v=$NF; sub(/^V/,"",v); sub(/__.*/,"",v); printf "%010d\t%s\n", v+0, $0 }' \
    | sort -k1,1 -k2,2 \
    | cut -f2-
}

# 磁盘上「文件名不符合引擎命名规则」的 up 脚本（必须为空；非空说明有迁移永远跑不到）
mp_bad_disk_names() {
  local raw
  if [ -n "${MP_REF:-}" ]; then
    raw=$(git -C "${MP_REPO:-.}" ls-tree -r --name-only "$MP_REF" "$MIG_ROOT_REL/")
  else
    raw=$(cd "${MP_REPO:-.}" && find "$MIG_ROOT_REL" -type f -name '*.up.sql' | sed 's#^\./##')
  fi
  printf '%s\n' "$raw" | grep -E '\.up\.sql$' | grep -vE "$MIG_PATH_RE" || true
}

# 同名不同目录（引擎按 (kind,name) 记账，会互相覆盖 → 必须先改名）
mp_dup_names() {
  mp_disk_files | while IFS= read -r f; do mp_name_of "$f"; done | sort | uniq -d
}

# 读库里的已应用集合（只认 success 的 versioned 记录，与 engine needs_apply 一致）
mp_db_applied_load() {
  MP_DB_APPLIED=(); MP_DB_APPLIED_COUNT=0
  local n
  while IFS= read -r n; do
    [ -n "$n" ] || continue
    MP_DB_APPLIED["$n"]=1
    MP_DB_APPLIED_COUNT=$((MP_DB_APPLIED_COUNT + 1))
  done < <(mp_q "select name from schema_migrations where kind='versioned' and success")
}

# 待应用清单：输出 "版本<TAB>仓库相对路径"，版本数值升序
mp_pending() {
  local f n
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    n=$(mp_name_of "$f")
    [ -n "${MP_DB_APPLIED[$n]:-}" ] && continue
    printf '%d\t%s\n' "$(mp_ver_of "$f")" "$f"
  done < <(mp_disk_files)
}

# 待应用清单：输出 "<版本>:<名字>"（deploya_migrate.sh 的参数格式）
mp_pending_pairs() {
  local v f
  while IFS=$'\t' read -r v f; do
    [ -n "${f:-}" ] || continue
    printf '%s:%s\n' "$v" "$(mp_name_of "$f")"
  done < <(mp_pending)
}

# 集合复核：磁盘有、库里仍没有 success 记录的迁移（逗号分隔带 V 前缀；空 = 全部已记账）
mp_missing_names() {
  local out="" v f
  while IFS=$'\t' read -r v f; do
    [ -n "${f:-}" ] || continue
    out="${out:+$out,}V$(printf '%04d' "$v"):$(mp_name_of "$f")"
  done < <(mp_pending)
  printf '%s' "$out"
}

# applied_via 合法性：输出 "非法值|条数"（空 = 全部合法）
# 非法值会让服务启动期 FATAL（history.rs:219-221），必须在切换二进制**之前**就挡住
mp_bad_applied_via() {
  mp_q "select applied_via || '|' || count(*)
          from schema_migrations
         where applied_via not in ('executed', 'imported', 'baseline')
         group by applied_via
         order by applied_via"
}

# 库内已应用版本的最大值（仅用于展示与「乱序补迁」提示，不再参与判定）
mp_db_max_version() {
  mp_q "select coalesce(max(version::int), 0)
          from schema_migrations
         where kind='versioned' and success and version ~ '^[0-9]+\$'"
}

# 库内已应用条数（对账用）
mp_db_applied_count() {
  mp_q "select count(*) from schema_migrations where kind='versioned' and success"
}

# 组装待跑清单 —— **deploy.sh 走的就是这个函数**（自测脚本调同一个，覆盖同一段代码）。
# 填充：MP_PENDING_PAIRS=(ver:name ...)  MP_PENDING_FILES=(仓库相对路径 ...)
#       MP_PENDING_LOW="V0220 V0221 ..."（版本号低于库内最大已应用的，即乱序补迁）
#       MP_PENDING_LOW_N=n  MP_APPLIED_MAX=n  MP_DB_APPLIED_COUNT=n
declare -ga MP_PENDING_PAIRS=() MP_PENDING_FILES=()
declare -g  MP_PENDING_LOW="" MP_PENDING_LOW_N=0 MP_APPLIED_MAX=0

mp_build_pending() {
  mp_db_applied_load
  MP_APPLIED_MAX=$(mp_db_max_version)
  MP_PENDING_PAIRS=(); MP_PENDING_FILES=(); MP_PENDING_LOW=""; MP_PENDING_LOW_N=0
  local v f
  while IFS=$'\t' read -r v f; do
    [ -n "${f:-}" ] || continue
    MP_PENDING_PAIRS+=("$v:$(mp_name_of "$f")")
    MP_PENDING_FILES+=("$f")
    if [ "$MP_APPLIED_MAX" -gt 0 ] && [ "$v" -lt "$MP_APPLIED_MAX" ]; then
      MP_PENDING_LOW="$MP_PENDING_LOW V$(printf '%04d' "$v")"
      MP_PENDING_LOW_N=$((MP_PENDING_LOW_N + 1))
    fi
  done < <(mp_pending)
}
