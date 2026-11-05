#!/usr/bin/env bash
# =============================================================================
# 预演库回归：证明「待跑迁移 = 磁盘全部 up.sql - 库内 success 记录（集合差）」修好了，
# 并用 2026-11-05 那次发版中断的现场做回归（V0219 基线 → V0241）。
#
# 背景（卡 t_9467ce9f）：
#   旧版 deploy.sh 用 `version > max(applied)` 选待跑迁移。当时库里最大版本是 V0235，
#   而 V0220/V0221/V0226..V0234（11 条）压根没记账 —— 这 11 条**永远选不出来**。
#   deploy.sh 只挑了 V0236..V0241，跑到 V0240 时它依赖的 biz_budget_subject_map
#   （V0227 建）不存在 → ERROR: relation "biz_budget_subject_map" does not exist
#   → [5/8] 中断、V0241 也没跑。
#
# 本脚本在**预演库**上分四幕复现并验证（全程不碰生产）：
#   ① 建基线：把 ≤BASELINE（默认 219）的全部 up.sql 按版本升序跑掉并记账
#   ② 复现事故现场：只补跑 V0235..V0239（= 旧版当时挑中的那几条）⇒ 库里 max=239、
#      仍缺 11 条 gap —— 与生产出事时一模一样的库状态
#   ③ 对比两种选法：旧法（max 比大小）只挑出 2 条且跑 V0240 必失败；新法（集合差）挑出 13 条
#   ④ 用新法 + 迁移工具的 --assert-pending 把 13 条跑完：对账 PASS、集合复核 PASS、
#      applied_via 合法；并把 V0240 真跑通（说明 V0227 确实在它之前补上了）
#   ⑤ 负样本：非法 applied_via 必须被拦住；清单少给一条必须被拒跑
#
# 用法（在仓库根或任意目录执行）：
#   DEPLOY_ENV_FILE=/path/to/rehearsal.env bash scripts/deploy/selftest_migrate_plan.sh
# 环境变量：
#   DEPLOY_ENV_FILE  预演库连接（需含 NIK_DATABASE_{NAME,USER,PASSWORD,HOST}），
#                    默认 /etc/cloudpivot/cloudpivot.env —— 生产机上请务必显式传一个预演库！
#   REHEARSAL_DB     预演库名，默认 cloudpivot_rehearsal_deploy（每次重建）
#   BASELINE         基线版本，默认 219
#   INCIDENT_RAN     事故现场「旧版跑过的那几条」，默认 "235 236 237 238 239"
#   KEEP=1           跑完保留预演库（默认删库）
# 退出码：0 全部断言通过；1 有断言失败（逐条打印 PASS/FAIL）
# =============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ENV_FILE="${DEPLOY_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}"
DB="${REHEARSAL_DB:-cloudpivot_rehearsal_deploy}"
BASELINE="${BASELINE:-219}"
INCIDENT_RAN="${INCIDENT_RAN:-235 236 237 238 239}"
KEEP="${KEEP:-0}"
TOOL="$ROOT/scripts/verify/deploya_migrate.sh"

[ -f "$ENV_FILE" ] || { echo "FAIL: 找不到预演库环境变量文件 $ENV_FILE" >&2; exit 2; }
set -a; . "$ENV_FILE"; set +a
export PGPASSWORD="${NIK_DATABASE_PASSWORD:?环境变量里没有 NIK_DATABASE_PASSWORD}"
DBHOST="${NIK_DATABASE_HOST:-127.0.0.1}"
DBUSER="${NIK_DATABASE_USER:-cloudpivot}"
export PATH="/root/.cargo/bin:$PATH"

Q()     { psql -h "$DBHOST" -U "$DBUSER" -d "$DB" -Atq -c "$1"; }
QADMIN() { psql -h "$DBHOST" -U "$DBUSER" -d postgres -Atq -c "$1"; }
# 建/删预演库：连库账号没 CREATEDB 权限时退到本机 postgres（peer 认证）
SU_PSQL() { printf '%s\n' "$1" | su - postgres -c "psql -d postgres -Atq -f -"; }
admin_sql() {
  if [ "$(QADMIN 'select rolcreatedb from pg_roles where rolname=current_user' 2>/dev/null)" = "t" ]; then
    QADMIN "$1"
  else
    SU_PSQL "$1"
  fi
}

# 复用 deploy.sh 的同一份判定实现（改口径只改一处）
mp_q() { Q "$1"; }
MP_REPO="$ROOT"
export MP_REPO
# shellcheck source=scripts/deploy/migrate_plan.sh
. "$ROOT/scripts/deploy/migrate_plan.sh"

PASS=0; FAILN=0
pass() { PASS=$((PASS+1)); echo "  PASS $*"; }
fail() { FAILN=$((FAILN+1)); echo "  FAIL $*"; }
step() { echo; echo "=============================================================="; echo "== $*"; echo "=============================================================="; }
# 断言：完全相同
assert_eq() { [ "$1" = "$2" ] && pass "$3（$1）" || fail "$3：期望 [$2]，实际 [$1]"; }

# 旧版 deploy.sh（commit 00c3a32 L184-192 的选法，仅用于复现事故）：
#   取 max(version::int) 当游标，只挑 (version > max) 的 —— gap 永远挑不出来
old_algo_pending() {
  local applied
  applied=$(Q "select coalesce(max(version::int),0) from schema_migrations
                where kind='versioned' and success and version ~ '^[0-9]+\$'")
  mp_disk_files | while IFS= read -r f; do
    v=$(mp_ver_of "$f")
    [ "$v" -gt "$applied" ] && printf '%d:%s\n' "$v" "$(mp_name_of "$f")"
  done
}

pending_pairs() { mp_pending | while IFS=$'\t' read -r v f; do printf '%d:%s\n' "$v" "$(mp_name_of "$f")"; done; }
pairs_le_baseline() {
  mp_disk_files | while IFS= read -r f; do
    v=$(mp_ver_of "$f")
    [ "$v" -le "$BASELINE" ] && printf '%d:%s\n' "$v" "$(mp_name_of "$f")"
  done
}

cleanup() {
  if [ "$KEEP" = "1" ]; then
    echo "（KEEP=1：保留预演库 $DB）"
  else
    admin_sql "drop database if exists \"$DB\"" >/dev/null 2>&1 || true
    echo "（已删除预演库 $DB）"
  fi
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
step "① 建预演库 $DB 并铺基线（≤V$(printf '%04d' "$BASELINE") 全部 up.sql）"
admin_sql "drop database if exists \"$DB\"" >/dev/null || { echo "FAIL: 删不掉旧预演库（权限？）"; exit 2; }
admin_sql "create database \"$DB\" owner \"$DBUSER\"" >/dev/null || { echo "FAIL: 建不了预演库"; exit 2; }
echo "  预演库已建：$DBHOST/$DB（owner $DBUSER）"
# 空库里还没有记录表：按引擎 DDL 建好（vendor/nik-db/src/migrate/history.rs:48-64）
Q "create table if not exists schema_migrations (
     kind         varchar(16)  not null,
     name         varchar(100) not null,
     version      varchar(64),
     checksum     varchar(64),
     applied_via  varchar(16)  not null,
     applied_at   bigint       not null,
     applied_seq  bigint       not null,
     execution_ms bigint,
     success      boolean      not null,
     source       varchar(255),
     primary key (kind, name))" >/dev/null || { echo "FAIL: 建 schema_migrations 失败"; exit 2; }
Q "create index if not exists idx_schema_migrations_version on schema_migrations(version)" >/dev/null
echo "  schema_migrations 已按引擎 DDL 建好（kind/name/version/checksum/applied_via/applied_at/applied_seq/execution_ms/success/source）"

mapfile -t BASE_PAIRS < <(pairs_le_baseline)
echo "  基线迁移 ${#BASE_PAIRS[@]} 条（最早..V$(printf '%04d' "$BASELINE")）"
DEPLOY_ENV_FILE="$ENV_FILE" bash "$TOOL" "$DB" --allow-partial "${BASE_PAIRS[@]}" > /tmp/selftest-baseline.log 2>&1
RC=$?
grep -E 'RECON_OK|VIA_OK|PARTIAL_OK|FAIL|APPLY_FAILED' /tmp/selftest-baseline.log | sed 's/^/    /'
[ "$RC" = "0" ] || { echo "FAIL: 基线铺不起来（见 /tmp/selftest-baseline.log 尾部）"; tail -20 /tmp/selftest-baseline.log; exit 1; }
mp_db_applied_load
assert_eq "$MP_DB_APPLIED_COUNT" "${#BASE_PAIRS[@]}" "基线记账条数 == 基线文件数"
BASE_PENDING=$(mp_pending | wc -l)
assert_eq "$BASE_PENDING" "$(( $(mp_disk_files | wc -l) - ${#BASE_PAIRS[@]} ))" "基线后待跑条数 == 磁盘总数 - 基线数"

# ---------------------------------------------------------------------------
step "② 复现事故现场：只补跑 V0235..V0239（旧版当时挑中的那几条）"
RAN_PAIRS=()
for v in $INCIDENT_RAN; do
  f=$(mp_disk_files | while IFS= read -r x; do [ "$(mp_ver_of "$x")" = "$((10#$v))" ] && echo "$x"; done)
  RAN_PAIRS+=("$((10#$v)):$(mp_name_of "$f")")
done
echo "  模拟旧版实际跑过：${RAN_PAIRS[*]}"
DEPLOY_ENV_FILE="$ENV_FILE" bash "$TOOL" "$DB" --allow-partial "${RAN_PAIRS[@]}" > /tmp/selftest-incident-ran.log 2>&1
RC=$?
grep -E 'RECON_OK|VIA_OK|PARTIAL_OK|FAIL' /tmp/selftest-incident-ran.log | sed 's/^/    /'
[ "$RC" = "0" ] || { echo "FAIL: 复现现场铺设失败（/tmp/selftest-incident-ran.log）"; exit 1; }
mp_db_applied_load
CUR_MAX=$(mp_db_max_version)
assert_eq "$CUR_MAX" "$((10#${INCIDENT_RAN##* }))" "现场库内最大已应用版本 == 旧版跑到的那条"

# ---------------------------------------------------------------------------
step "③ 对比两种选法（同一时刻、同一个库）"
mapfile -t OLD_LIST < <(old_algo_pending)
mp_build_pending                        # ← deploy.sh 走的就是这个函数（同一段代码）
NEW_LIST=("${MP_PENDING_PAIRS[@]}")
echo "  旧法（max 比大小）挑出 ${#OLD_LIST[@]} 条：${OLD_LIST[*]:-（无）}"
echo "  新法（mp_build_pending，= deploy.sh 的清单）挑出 ${#NEW_LIST[@]} 条：${NEW_LIST[*]:-（无）}"
echo "  新法标记的乱序补迁（版本号 < max）：${MP_PENDING_LOW_N} 条 → ${MP_PENDING_LOW}"
assert_eq "$MP_PENDING_LOW_N" "11" "新法把 11 条 gap 标成「乱序补迁」（旧法连标都没标，直接丢）"
assert_eq "${#NEW_LIST[@]}" "$(( $(mp_disk_files | wc -l) - MP_DB_APPLIED_COUNT ))" "新法：磁盘全部未记账的都在清单里"
OLD_MAX=$(Q "select coalesce(max(version::int),0) from schema_migrations where kind='versioned' and success and version ~ '^[0-9]+\$'")
GAP_COUNT=$(mp_pending | awk -v m="$OLD_MAX" '{ if ($1+0 < m) c++ } END { print c+0 }')
echo "  其中版本号低于库内最大已应用 V$(printf '%04d' "$OLD_MAX") 的 gap：$GAP_COUNT 条（旧法一条都挑不出来）"
GAP_IN_NEW=0
for p in "${NEW_LIST[@]}"; do [ "${p%%:*}" -lt "$OLD_MAX" ] && GAP_IN_NEW=$((GAP_IN_NEW+1)); done
assert_eq "$GAP_IN_NEW" "$GAP_COUNT" "新法把 gap 也纳入了清单"
[ "${#OLD_LIST[@]}" -lt "${#NEW_LIST[@]}" ] && pass "旧法确实漏跑（${#OLD_LIST[@]} < ${#NEW_LIST[@]}）" || fail "旧法没有漏跑？口径可能没换成功"

# 卡面点名的 13 条（事故当时漏掉的）必须一条不少地出现在新法清单里
INCIDENT_MISSING="220 221 226 227 228 229 230 231 232 233 234 240 241"
MISS_IN_NEW=0; MISS_MSG=""
for v in $INCIDENT_MISSING; do
  found=0
  for p in "${NEW_LIST[@]}"; do [ "${p%%:*}" = "$v" ] && found=1 && break; done
  if [ "$found" = "1" ]; then MISS_IN_NEW=$((MISS_IN_NEW+1)); else MISS_MSG="$MISS_MSG V$(printf '%04d' "$v")"; fi
done
assert_eq "$MISS_IN_NEW" "13" "卡面点名的 13 条漏跑迁移全部进入新法清单${MISS_MSG:+（缺：$MISS_MSG）}"
# 反向：旧法一条 gap 都挑不到（220/221/226..234）
OLD_HAS_GAP=0
for p in "${OLD_LIST[@]}"; do
  case " $INCIDENT_MISSING " in *" ${p%%:*} "*) OLD_HAS_GAP=$((OLD_HAS_GAP+1));; esac
done
assert_eq "$OLD_HAS_GAP" "2" "旧法只挑到 240/241 两条（13 条里的其余 11 条 gap 全被丢掉）"

# ---------------------------------------------------------------------------
step "④ 旧法真跑一遍：V0240 必须失败（依赖 V0227 建的 biz_budget_subject_map）"
HS_EXIST_BEFORE=$(Q "select count(*) from information_schema.tables where table_name='biz_budget_subject_map'")
assert_eq "$HS_EXIST_BEFORE" "0" "跑 V0240 前 biz_budget_subject_map 不存在"
DEPLOY_ENV_FILE="$ENV_FILE" bash "$TOOL" "$DB" "${OLD_LIST[@]}" > /tmp/selftest-old-algo.log 2>&1
RC=$?
grep -E 'APPLY_FAILED|does not exist' /tmp/selftest-old-algo.log | head -3 | sed 's/^/    /'
if [ "$RC" != "0" ] && grep -q 'biz_budget_subject_map' /tmp/selftest-old-algo.log; then
  pass "旧法在 V0240 失败，报错点名 biz_budget_subject_map（与 2026-11-05 生产日志一致）"
else
  fail "旧法居然没在 V0240 失败（rc=$RC）—— 事故没复现，结论不可信"
fi
STILL=$(pending_pairs | wc -l)
assert_eq "$STILL" "${#NEW_LIST[@]}" "旧法失败后待跑条数不变（V0240 是单事务，失败不落账）"

# ---------------------------------------------------------------------------
step "⑤ 新法 + --assert-pending：先验「少给一条会被拒跑」，再把清单跑完"
mp_db_applied_load
BEFORE=$(mp_db_applied_count)
PARTIAL_LIST=( "${NEW_LIST[@]:0:${#NEW_LIST[@]}-1}" )
echo "  故意少给一条（${#PARTIAL_LIST[@]}/${#NEW_LIST[@]}）…"
DEPLOY_ENV_FILE="$ENV_FILE" bash "$TOOL" "$DB" --assert-pending "${PARTIAL_LIST[@]}" > /tmp/selftest-partial.log 2>&1
RC=$?
grep -E 'FAIL|PENDING_ASSERT_OK' /tmp/selftest-partial.log | head -3 | sed 's/^/    /'
[ "$RC" = "2" ] && pass "少给一条 → 迁移工具拒跑（exit 2）" || fail "少给一条居然没被拦住（rc=$RC）"
assert_eq "$(mp_db_applied_count)" "$BEFORE" "被拒跑后库内记账条数不变（没跑半截）"

echo "  跑完整清单 ${#NEW_LIST[@]} 条 …"
DEPLOY_ENV_FILE="$ENV_FILE" bash "$TOOL" "$DB" --assert-pending "${NEW_LIST[@]}" > /tmp/selftest-new-algo.log 2>&1
RC=$?
grep -E 'PENDING_ASSERT_OK|RECON_OK|VIA_OK|SET_OK|FAIL|APPLY_FAILED' /tmp/selftest-new-algo.log | sed 's/^/    /'
[ "$RC" = "0" ] || { fail "新法跑完失败（见 /tmp/selftest-new-algo.log 尾部）"; tail -15 /tmp/selftest-new-algo.log; }
grep -q 'RECON_OK' /tmp/selftest-new-algo.log && pass "自检①：对账 applied 增量 == 本次条数" || fail "自检①：没有 RECON_OK"
grep -q 'VIA_OK'   /tmp/selftest-new-algo.log && pass "自检②：applied_via 全表合法" || fail "自检②：没有 VIA_OK"
grep -q 'SET_OK'   /tmp/selftest-new-algo.log && pass "自检③：磁盘 - 库 = 空" || fail "自检③：没有 SET_OK"

mp_db_applied_load
AFTER=$(mp_db_applied_count)
assert_eq "$((AFTER - BEFORE))" "${#NEW_LIST[@]}" "对账（外部复算）：新增记账 == 待跑条数"
assert_eq "$(mp_missing_names)" "" "集合复核（外部复算）：磁盘上没有未记账的迁移"
HS_EXIST_AFTER=$(Q "select count(*) from information_schema.tables where table_name='biz_budget_subject_map'")
assert_eq "$HS_EXIST_AFTER" "1" "V0240 跑通的前提表 biz_budget_subject_map 已被 V0227 建好"
V0240=$(Q "select count(*) from schema_migrations where kind='versioned' and name='erp_budget_mapping_material_type' and success")
V0241=$(Q "select count(*) from schema_migrations where kind='versioned' and name='erp_contract_permission_prefix_unify' and success")
assert_eq "$V0240" "1" "V0240 已记账"
assert_eq "$V0241" "1" "V0241 已记账（旧版根本没跑到）"
DISK_MAX=$(mp_disk_files | tail -1 | while IFS= read -r f; do mp_ver_of "$f"; done)
assert_eq "$(mp_db_max_version)" "$DISK_MAX" "库内最大已应用版本 == 磁盘最大版本（V$(printf '%04d' "$DISK_MAX")）"

# ---------------------------------------------------------------------------
step "⑥ 负样本：非法 applied_via 必须被拦住（服务启动 FATAL 的那一类）"
Q "insert into schema_migrations (kind,name,version,checksum,applied_via,applied_at,applied_seq,execution_ms,success,source)
   values ('versioned','selftest_bogus_via','900','deadbeef','deploy-backfill',0,99999,1,true,'selftest')" >/dev/null
BAD=$(mp_bad_applied_via)
[ -n "$BAD" ] && pass "mp_bad_applied_via 报出非法值：$BAD" || fail "非法 applied_via 没被识别"
DEPLOY_ENV_FILE="$ENV_FILE" bash "$TOOL" "$DB" --assert-pending "901:whatever" > /tmp/selftest-badvia.log 2>&1
RC=$?
grep -E 'FAIL|kind.rs' /tmp/selftest-badvia.log | head -2 | sed 's/^/    /'
[ "$RC" = "2" ] && pass "迁移工具在预检阶段拒跑（exit 2）" || fail "非法 applied_via 没被迁移工具拦住（rc=$RC）"
Q "delete from schema_migrations where name='selftest_bogus_via'" >/dev/null
assert_eq "$(mp_bad_applied_via)" "" "清理后 applied_via 恢复全合法"

# ---------------------------------------------------------------------------
step "汇总"
echo "  预演库：$DB（$DBHOST/$DBUSER）"
echo "  基线：V$(printf '%04d' "$BASELINE")　回归目标：V0241　本次清单：${#NEW_LIST[@]} 条"
echo "  PASS=$PASS  FAIL=$FAILN"
[ "$FAILN" = "0" ] && echo "  ✅ 全部断言通过" || echo "  ❌ 有 $FAILN 条断言失败"
[ "$FAILN" = "0" ] || exit 1
