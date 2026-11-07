#!/usr/bin/env bash
# =============================================================================
# 云枢（cloudpivot）一键部署脚本 —— 生产 144 /opt/cloudpivot
#
# 固化 DEPLOY-A/B/C 三次手工部署做对的流程，避免每次临场重写：
#   备份 → fetch+ff → 按迁移引擎口径逐条跑新迁移 → 只在有后端改动时编译重启
#   → 只在有前端改动时 build 并比对入口指纹 → 收口核对（进程==磁盘、health、HEAD）
#   失败即停，并按「已经做到哪一步」打印精确的回滚命令。
#
# 用法（在**生产机**上跑，即 /opt/cloudpivot 与 systemctl cloudpivot 所在主机）：
#   bash scripts/deploy/deploy.sh                 # 全流程
#   bash scripts/deploy/deploy.sh --dry-run       # 只体检 + 打印计划，不写任何东西
#   bash scripts/deploy/deploy.sh --skip-migrate --skip-frontend
#   bash scripts/deploy/deploy.sh --skip-backend --skip-frontend   # 只跑迁移
#   bash scripts/deploy/deploy.sh --skip-migrate --skip-backend --force-frontend  # 只重建前端
#   bash scripts/deploy/deploy.sh --tag mylabel   # 指定备份/构建标签（默认时间戳）
#
# 选项：
#   --skip-migrate    不跑新迁移（默认：自动检测并逐条跑）
#   --skip-backend    不编译/不重启后端
#   --skip-frontend   不构建/不替换前端 dist
#   --force-frontend  无视「前端无改动」判据，强制 typecheck + build + 换 dist
#                     （补装前端 / 复现 build 问题时用；仍受 --skip-frontend 压制）
#   --dry-run         只打印计划
#   --tag <label>     备份与构建目录标签
#   -h|--help
#
# 环境变量：
#   DEPLOY_REPO        仓库路径，默认 /opt/cloudpivot
#   DEPLOY_ENV_FILE    生产环境变量文件，默认 /etc/cloudpivot/cloudpivot.env
#   DEPLOY_SERVICE     systemd 服务名，默认 cloudpivot
#   CARGO_BUILD_JOBS   默认 1（7GB 机器上高并发会 OOM）
#   NODE_MAX_OLD_SPACE 前端 build 的 node 老生代上限（MB），默认 4096
#                      （本项目 378 assets / tdesign chunk 8MB，默认堆 ~2GB 必 OOM；
#                        144 实测 4608 可用、4096 留余量。日志里出现
#                        "Reached heap limit / JavaScript heap out of memory" 就是这个值不够）
#
# 退出码：0 成功；1 失败（已按需回滚后端/前端，详细提示见日志尾）
# 日志：/var/log/cloudpivot-deploy-<TAG>.log
# =============================================================================
set -uo pipefail

REPO="${DEPLOY_REPO:-/opt/cloudpivot}"
ENV_FILE="${DEPLOY_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}"
SERVICE="${DEPLOY_SERVICE:-cloudpivot}"
BACKEND_DIR="$REPO/backend"
FRONTEND_DIR="$REPO/frontend"
MIG_ROOT="$REPO/backend/migrations"   # 引擎口径：递归全部子目录（business/system/support/…）
MIGRATE_TOOL="$REPO/scripts/verify/deploya_migrate.sh"
MIG_PLAN_LIB="$REPO/scripts/deploy/migrate_plan.sh"   # 待迁移判定唯一实现（deploy.sh 与自测共用）
HEALTH_URL="http://127.0.0.1:8023/health"
WEB_URL="http://127.0.0.1/"
TAG="$(date +%Y%m%d-%H%M%S)"

SKIP_MIGRATE=0
SKIP_BACKEND=0
SKIP_FRONTEND=0
FORCE_FRONTEND=0
DRY_RUN=0
# 前端 build 的 node 堆上限：不设就吃默认（这台 7GB/aarch64 上 ~2GB，本轮实测 OOM）
NODE_MAX_OLD_SPACE="${NODE_MAX_OLD_SPACE:-4096}"

while [ $# -gt 0 ]; do
  case "$1" in
    --skip-migrate)  SKIP_MIGRATE=1 ;;
    --skip-backend)  SKIP_BACKEND=1 ;;
    --skip-frontend) SKIP_FRONTEND=1 ;;
    --force-frontend) FORCE_FRONTEND=1 ;;
    --dry-run)       DRY_RUN=1 ;;
    --tag)           TAG="${2:?--tag 需要值}"; shift ;;
    -h|--help)       sed -n '2,/^set -uo pipefail/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "未知参数：$1（--help 看用法）" >&2; exit 2 ;;
  esac
  shift
done

BACKUP_DIR="/root/deploy-backup-$TAG"
if [ "$DRY_RUN" = "1" ]; then
  # --dry-run 承诺「不写任何文件」：连部署日志都不落盘。
  # （旧版无条件 `tee -a /var/log/...`，dry-run 也会建出日志文件。）
  LOG="（dry-run：不落盘，仅终端输出）"
else
  LOG="/var/log/cloudpivot-deploy-$TAG.log"
  exec > >(tee -a "$LOG") 2>&1
fi

say()  { echo "[$(date +%H:%M:%S)] $*"; }
step() { echo; echo "=============================================================="; echo "== $*"; echo "=============================================================="; }
ok()   { echo "  ✅ $*"; }
bad()  { echo "  ❌ $*"; }

# 已经做过的动作，决定失败时该提示哪些回滚命令
DID_BACKUP=0; DID_FF=0; DID_MIGRATE=0; DID_BACKEND=0; DID_FRONTEND=0
NEW_HEAD=""; OLD_HEAD=""; APPLIED_MIGS=""; ROLLED_BACK=""

die() {
  echo
  echo "############ 部署失败：$* ############"
  echo "日志：$LOG"
  if [ "$ROLLED_BACK" = "1" ]; then
    echo "（已自动回滚到部署前状态，见上）"
  fi
  echo
  echo "—— 按当前进度给出的回滚/排查命令 ——"
  if [ "$DID_FRONTEND" = "1" ]; then
    echo "  前端：mv $FRONTEND_DIR/dist ${FRONTEND_DIR}/dist-failed-$TAG && mv ${FRONTEND_DIR}/dist-prev-$TAG $FRONTEND_DIR/dist"
  fi
  if [ "$DID_BACKEND" = "1" ]; then
    echo "  后端：systemctl stop $SERVICE && cp $BACKUP_DIR/cloudpivot $BACKEND_DIR/target/release/cloudpivot.rollback && mv -f $BACKEND_DIR/target/release/cloudpivot.rollback $BACKEND_DIR/target/release/cloudpivot && systemctl start $SERVICE"
  fi
  if [ "$DID_MIGRATE" = "1" ]; then
    echo "  迁移：$APPLIED_MIGS 已应用（迁移不可逆，回滚要跑对应 .down.sql，见 $MIG_ROOT）"
  fi
  if [ "$DID_BACKUP" = "1" ]; then
    echo "  数据库整库回滚（慎用，会丢部署后写入）："
    echo "    systemctl stop $SERVICE"
    echo "    su - postgres -c 'dropdb cloudpivot && createdb -O cloudpivot cloudpivot'"
    echo "    su - postgres -c 'pg_restore -d cloudpivot $BACKUP_DIR/db.dump'"
    echo "    systemctl start $SERVICE"
  fi
  echo "  代码回退：git -C $REPO reset --hard ${OLD_HEAD:-<部署前HEAD>}"
  exit 1
}

# ----------------------------------------------------------------------------
step "[0/8] 前置检查"
# 用 rev-parse 而不是 [ -d "$REPO/.git" ]：worktree 里 .git 是文件不是目录
git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1 || die "找不到 git 仓库 $REPO（本脚本要在生产机上跑）"
[ -f "$ENV_FILE" ] || die "找不到生产环境变量 $ENV_FILE"
[ -f "$MIGRATE_TOOL" ] || die "找不到迁移工具 $MIGRATE_TOOL"
command -v git >/dev/null || die "缺 git"
systemctl cat "$SERVICE" >/dev/null 2>&1 || die "systemd 里没有 $SERVICE（本脚本要在生产机上跑）"
[ -x "$BACKEND_DIR/target/release/cloudpivot" ] || die "找不到线上二进制 $BACKEND_DIR/target/release/cloudpivot"

set -a; . "$ENV_FILE"; set +a
DB_NAME="${NIK_DATABASE_NAME:?环境变量里没有 NIK_DATABASE_NAME}"
DB_USER="${NIK_DATABASE_USER:-cloudpivot}"
DB_HOST="${NIK_DATABASE_HOST:-127.0.0.1}"
export PGPASSWORD="${NIK_DATABASE_PASSWORD:?环境变量里没有 NIK_DATABASE_PASSWORD}"
export PATH="/root/.cargo/bin:$PATH"
PSQL="psql -h $DB_HOST -U $DB_USER -d $DB_NAME -Atq"

# 迁移清单口径的唯一实现在 scripts/deploy/migrate_plan.sh（source 复用，避免 deploy.sh /
# 自测脚本 / 迁移工具三份实现各说各话）。这里只提供它需要的 mp_q（查库）。
# 关键：pending 是**集合差**（磁盘文件名 - 库内 kind='versioned' 且 success 的 name），
#       键是 (kind,name)，**与版本号大小无关**（vendor/nik-db/src/migrate/mod.rs:1263-1275）。
# 旧版用 `version > max(applied)` 比大小 ⇒ 库内最大版本以下的 gap 永远挑不出来：
# 2026-11-05 发版漏掉 V0220/V0221/V0226..V0234（11 条），V0240 依赖的
# biz_budget_subject_map（V0227 建）不存在 → [5/8] 失败、部署中断。详见库文件头注释。
[ -f "$MIG_PLAN_LIB" ] || die "找不到待迁移判定库 $MIG_PLAN_LIB"
mp_q() { $PSQL -c "$1"; }
MP_REPO="$REPO"
export MP_REPO
# shellcheck source=scripts/deploy/migrate_plan.sh
. "$MIG_PLAN_LIB"

say "仓库 $REPO，库 $DB_NAME，服务 $SERVICE，标签 $TAG"
[ "$DRY_RUN" = "1" ] && say ">>> dry-run：只体检与打印计划，不写文件、不动服务 <<<"

# ----------------------------------------------------------------------------
step "[1/8] 查清待上线内容（git fetch + 差异清单）"
cd "$REPO" || die "cd $REPO 失败"
git fetch --quiet origin || die "git fetch 失败"
OLD_HEAD="$(git rev-parse HEAD)"
NEW_HEAD="$(git rev-parse origin/main)"
say "当前 HEAD      $OLD_HEAD  $(git log -1 --format=%s HEAD)"
say "origin/main    $NEW_HEAD  $(git log -1 --format=%s origin/main)"

if ! git merge-base --is-ancestor "$OLD_HEAD" "$NEW_HEAD"; then
  die "本地 HEAD 不是 origin/main 的祖先（历史分叉），需要人工处理：git log --oneline $OLD_HEAD..$NEW_HEAD"
fi
NCOMMITS=$(git rev-list --count "$OLD_HEAD..$NEW_HEAD")
say "落后提交数：$NCOMMITS"
if [ "$NCOMMITS" != "0" ]; then
  git log --oneline "$OLD_HEAD..$NEW_HEAD" | sed 's/^/    /'
fi

# --- 待应用迁移 -----------------------------------------------------------------
# 口径（= 迁移引擎的 pending 语义，唯一实现在 scripts/deploy/migrate_plan.sh）：
#   disk = $NEW_HEAD 的树里 backend/migrations/**/V<n>__<name>.up.sql（全部子目录；
#          引擎 `[database.migration] path = "migrations"` 是递归装载）
#   done = 库内 kind='versioned' 且 success 的 name 集合
#   todo = disk - done，按版本号数值升序（版本号只用于排序/展示）
#
# 为什么必须是「集合差」而不是「> 库内最大版本」（2026-11-05 发版中断的真因）：
#   旧版 L184 取 max(version::int) 当游标，L189 用 `(v+0) > applied` 过滤，L228 再按
#   `-le APPLIED_MAX` 二次跳过 —— 三处都是「比大小」。库内最大版本是 V0235 时，
#   gap 里的 V0220/V0221/V0226..V0234（11 条）永远进不了清单；而 V0240 要的
#   biz_budget_subject_map 是 V0227 建的 ⇒ [5/8] 报 relation does not exist，部署中断。
#   引擎不这么判：needs_apply（vendor/nik-db/src/migrate/mod.rs:1263-1275）只看
#   (kind,name) 有没有 success 记录，低版本照样执行（只打 out_of_order WARN，mod.rs:876-888）。
#
# 也不再信 `git diff --diff-filter=A $OLD_HEAD..$NEW_HEAD`：那只认「本次同步新加的迁移」，
# 主仓同步了代码但迁移没对库跑过时，待跑迁移根本不在 diff 里 ⇒ 会报「本次无新迁移」。
# 按「库 vs 磁盘」比才是生产是否真生效。
MP_REF="$NEW_HEAD"
export MP_REF
# 清单构造只有一份实现（migrate_plan.sh: mp_build_pending），自测脚本调的是同一个函数
mp_build_pending
APPLIED_MAX="$MP_APPLIED_MAX"
APPLIED_BEFORE="$MP_DB_APPLIED_COUNT"
PENDING_PAIRS=("${MP_PENDING_PAIRS[@]}")
PENDING_FILES=("${MP_PENDING_FILES[@]}")
PENDING_LOW="$MP_PENDING_LOW"
PENDING_LOW_N="$MP_PENDING_LOW_N"
say "库内已应用 versioned 迁移：$APPLIED_BEFORE 条，最大版本 V$(printf '%04d' "$APPLIED_MAX")"

# 记录表合法性预检（动任何东西之前）：非法 applied_via 会让服务**启动期 FATAL**
BAD_VIA=$(mp_bad_applied_via)
if [ -n "$BAD_VIA" ]; then
  die "schema_migrations.applied_via 出现引擎不认识的值：$(echo "$BAD_VIA" | paste -sd' ' -)（引擎只认 $MP_ALLOWED_APPLIED_VIA，见 vendor/nik-db/src/migrate/kind.rs:95-111；非法值会让服务启动 FATAL，history.rs:219-221）。修法：update schema_migrations set applied_via='imported' where applied_via='<非法值>';"
fi
BAD_FILES=$(mp_bad_disk_names)
[ -z "$BAD_FILES" ] || die "迁移目录里有不符合引擎命名规则的 up 脚本（引擎永远装不到，等于没写）：$(echo "$BAD_FILES" | paste -sd' ' -)"
DUP_NAMES=$(mp_dup_names)
[ -z "$DUP_NAMES" ] || die "同名迁移出现在多个目录（引擎按 (kind,name) 记账会互相覆盖，必须先改名）：$(echo "$DUP_NAMES" | paste -sd' ' -)"
say "预检通过：applied_via 取值合法、迁移文件名可被引擎识别"

# --- 后端/前端是否要重做 ---------------------------------------------------------
# 判据不只是「本次 diff 有没有碰这个目录」，还要看**产物是否比最近一次提交旧**：
# 主仓同步了但没编译/没 build 时，diff 是空的、产物却过期（v2 检测卡抓到的正是这个）。
BE_LAST_TS=$(git log -1 --format=%ct "$NEW_HEAD" -- backend/ ':!backend/migrations' ':!backend/tests' 2>/dev/null || echo 0)
BIN_TS=$(stat -c %Y "$BACKEND_DIR/target/release/cloudpivot" 2>/dev/null || echo 0)
BE_CHANGED=$(git diff --name-only "$OLD_HEAD" "$NEW_HEAD" -- backend/ | grep -vE '^backend/(migrations|tests)/' | head -1 || true)
if [ -z "$BE_CHANGED" ] && [ "${BE_LAST_TS:-0}" -gt "${BIN_TS:-0}" ]; then
  BE_CHANGED="stale-binary"
  say "二进制过期：线上二进制 $(date -d @"$BIN_TS" '+%F %T') 早于最近后端提交 $(date -d @"$BE_LAST_TS" '+%F %T') ⇒ 重编译"
fi

FE_LAST_TS=$(git log -1 --format=%ct "$NEW_HEAD" -- frontend/ ':!frontend/tests' ':!frontend/dist' 2>/dev/null || echo 0)
DIST_TS=$(stat -c %Y "$FRONTEND_DIR/dist" 2>/dev/null || echo 0)
FE_CHANGED=$(git diff --name-only "$OLD_HEAD" "$NEW_HEAD" -- frontend/ | head -1 || true)
if [ -z "$FE_CHANGED" ] && [ "${FE_LAST_TS:-0}" -gt "${DIST_TS:-0}" ]; then
  FE_CHANGED="stale-dist"
  say "dist 过期：线上 dist $(date -d @"$DIST_TS" '+%F %T') 早于最近前端提交 $(date -d @"$FE_LAST_TS" '+%F %T') ⇒ 重构建"
fi

[ -n "$(git diff --name-only "$OLD_HEAD" "$NEW_HEAD" -- Cargo.toml Cargo.lock rust-toolchain.toml backend/Cargo.toml backend/Cargo.lock backend/rust-toolchain.toml 2>/dev/null)" ] && BE_CHANGED=1

[ "$FORCE_FRONTEND" = "1" ] && [ -z "$FE_CHANGED" ] && { FE_CHANGED="forced"; say "（--force-frontend：无视「前端无改动」，强制重建 dist）"; }
[ "$SKIP_BACKEND" = "1" ] && BE_CHANGED="" && say "（--skip-backend：忽略后端改动）"
[ "$SKIP_FRONTEND" = "1" ] && FE_CHANGED="" && say "（--skip-frontend：忽略前端改动）"
say "后端改动：$([ -n "$BE_CHANGED" ] && echo 有 || echo 无)　　前端改动：$([ -n "$FE_CHANGED" ] && echo 有 || echo 无)　　待应用迁移：${#PENDING_PAIRS[@]} 个（口径：磁盘全部 up.sql − 库内 success 记录）"
for i in "${!PENDING_PAIRS[@]}"; do
  say "    + ${PENDING_FILES[$i]}  [V$(printf '%04d' "${PENDING_PAIRS[$i]%%:*}")]"
done
if [ "$PENDING_LOW_N" -gt 0 ]; then
  say "  ⚠ 其中 $PENDING_LOW_N 条版本号低于库内最大已应用 V$(printf '%04d' "$APPLIED_MAX")：$PENDING_LOW"
  say "     这是引擎允许的乱序补迁（启动期只打 out_of_order WARN）；旧版 deploy.sh 正是把这些 gap 整批丢掉才炸的"
fi

# ----------------------------------------------------------------------------
step "[2/8] 备份（库 dump + 线上二进制 + 现有 dist）"
if [ "$DRY_RUN" = "1" ]; then
  say "dry-run：将备份到 $BACKUP_DIR/{db.dump,cloudpivot,dist/}"
else
  mkdir -p "$BACKUP_DIR" || die "建备份目录失败"
  say "pg_dump …"
  pg_dump -h "$DB_HOST" -U "$DB_USER" -d "$DB_NAME" -Fc -f "$BACKUP_DIR/db.dump" || die "pg_dump 失败"
  cp -p "$BACKEND_DIR/target/release/cloudpivot" "$BACKUP_DIR/cloudpivot" || die "备份二进制失败"
  if [ -d "$FRONTEND_DIR/dist" ]; then
    cp -a "$FRONTEND_DIR/dist" "$BACKUP_DIR/dist" || die "备份 dist 失败"
  else
    say "（$FRONTEND_DIR/dist 不存在，跳过 dist 备份）"
  fi
  for p in "$BACKUP_DIR/db.dump" "$BACKUP_DIR/cloudpivot"; do
    sz=$(stat -c%s "$p" 2>/dev/null || echo 0)
    [ "$sz" -gt 0 ] || die "备份文件为空：$p"
    ok "$(printf '%-28s %12s 字节' "$(basename "$p")" "$sz")"
  done
  if [ -d "$BACKUP_DIR/dist" ]; then
    ok "$(printf '%-28s %12s' dist/ "$(du -sh "$BACKUP_DIR/dist" | cut -f1)")"
  fi
  say "备份目录：$BACKUP_DIR"
  DID_BACKUP=1
fi

# ----------------------------------------------------------------------------
step "[3/8] 同步代码（git merge --ff-only）"
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
  git status --short --untracked-files=no | sed 's/^/    /'
  die "生产工作区有未提交的已跟踪改动，脚本不替你 stash（可能是别人在跑的任务）。请先处理：git -C $REPO status"
fi
if [ "$NCOMMITS" = "0" ]; then
  say "已是最新，无需同步"
else
  # 未跟踪副本会挡住 ff（历史上 scp 过同一路径的脚本就会这样）：
  # 与将要合入的版本**逐字节相同**就挪到 /root/deploy-untracked-<TAG>/，不同则停下让人比对。
  mapfile -t INCOMING < <(git diff --name-only --diff-filter=A "$OLD_HEAD" "$NEW_HEAD")
  for f in "${INCOMING[@]:-}"; do
    [ -n "${f:-}" ] || continue
    [ -e "$f" ] || continue
    if git ls-files --error-unmatch "$f" >/dev/null 2>&1; then continue; fi
    if git show "$NEW_HEAD:$f" | cmp -s - "$f"; then
      SIDE="/root/deploy-untracked-$TAG"; mkdir -p "$SIDE/$(dirname "$f")"
      mv "$f" "$SIDE/$f" && say "挪开未跟踪副本（与提交逐字节相同）：$f → $SIDE/$f"
    else
      die "未跟踪文件 $f 与将要合入的版本内容不同，脚本不覆盖（先人工 diff / 决定保留哪份）"
    fi
  done
  if [ "$DRY_RUN" = "1" ]; then
    say "dry-run：将执行 git merge --ff-only $NEW_HEAD"
  else
    git merge --ff-only "$NEW_HEAD" || die "ff-only 合并失败"
    ok "HEAD → $(git rev-parse --short HEAD) $(git log -1 --format=%s)"
  fi
fi
DID_FF=1

# ----------------------------------------------------------------------------
# 顺序：先编译（不重启）→ 再跑迁移 → 最后换二进制重启。
# 为什么不是「先迁移再编译」：编译要 5–10 分钟，而 V0217 之类会删列；
# 迁移一落地，**仍在服务的旧二进制**就可能因为引用已删列而 500，
# 旧二进制会带着错误撑到编译完。先编好再迁移，把「旧二进制 × 新 schema」
# 的窗口从「编译耗时」压到「迁移耗时」（秒级）。
step "[4/8] 后端编译（只编译不重启）"
BIN_BUILT=0
if [ -z "$BE_CHANGED" ]; then
  say "无后端改动，跳过编译"
elif [ "$DRY_RUN" = "1" ]; then
  say "dry-run：将 cargo build --release（CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-1}），迁移完再换二进制重启"
else
  cd "$BACKEND_DIR" || die "cd backend 失败"
  export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-1}"
  BUILD_T0=$(date +%s)
  say "cargo build --release（jobs=$CARGO_BUILD_JOBS，日志 $LOG）…"
  cargo build --release || die "cargo build --release 失败"
  BIN_BUILT=1
  ok "编译完成（$(( $(date +%s) - BUILD_T0 ))s）新二进制 md5=$(md5sum target/release/cloudpivot | cut -d' ' -f1)  $(stat -c%s target/release/cloudpivot) 字节"
fi

# ----------------------------------------------------------------------------
step "[5/8] 数据库迁移（逐条跑 + 按引擎口径记账 + 跑完对账）"
if [ "$SKIP_MIGRATE" = "1" ]; then
  say "--skip-migrate：跳过"
elif [ "${#PENDING_PAIRS[@]}" -eq 0 ]; then
  ok "无待应用迁移（磁盘上全部 up.sql 都已记账；库内最大版本 V$(printf '%04d' "$APPLIED_MAX")）"
elif [ "$DRY_RUN" = "1" ]; then
  say "dry-run：将依次应用 ${#PENDING_PAIRS[@]} 条：${PENDING_PAIRS[*]}"
else
  TODO_COUNT=${#PENDING_PAIRS[@]}
  # 对账基线压到迁移前一刻（[1/8] 那次读数是给人看的计划，这里才是判据）
  APPLIED_BEFORE=$(mp_db_applied_count)
  say "迁移前已记账 $APPLIED_BEFORE 条，本次待跑 $TODO_COUNT 条"
  # --assert-pending：让迁移工具用它自己独立算出的待跑集合对本清单做双向校验，
  # 不一致就拒跑（防止 deploy.sh 少给 / 多给 / 顺序错）
  bash "$MIGRATE_TOOL" "$DB_NAME" --assert-pending "${PENDING_PAIRS[@]}" \
    || die "迁移应用失败（上面的 APPLY_FAILED 行）"
  DID_MIGRATE=1
  APPLIED_MIGS="${PENDING_PAIRS[*]}"

  # ---- 自检 1：对账（增量记账 == 待跑条数），不相等不许静默通过 ----
  APPLIED_AFTER=$(mp_db_applied_count)
  DELTA=$((APPLIED_AFTER - APPLIED_BEFORE))
  if [ "$DELTA" = "$TODO_COUNT" ]; then
    ok "对账通过：新增记账 $DELTA 条 == 待跑 $TODO_COUNT 条（$APPLIED_BEFORE → $APPLIED_AFTER）"
  else
    die "对账失败：待跑 $TODO_COUNT 条，实际新增记账 $DELTA 条（$APPLIED_BEFORE → $APPLIED_AFTER）—— 有迁移没跑到，或有别的写入"
  fi

  # ---- 自检 2：applied_via 必须全部是引擎认可的值（非法值 = 服务启动 FATAL）----
  BAD_VIA_AFTER=$(mp_bad_applied_via)
  if [ -n "$BAD_VIA_AFTER" ]; then
    die "记账后出现非法 applied_via：$(echo "$BAD_VIA_AFTER" | paste -sd' ' -)（引擎只认 $MP_ALLOWED_APPLIED_VIA；不修就会在下一步启动时 FATAL）"
  fi
  ok "applied_via 合法（全部 ∈ $MP_ALLOWED_APPLIED_VIA）"

  # ---- 自检 3：集合复核（磁盘 - 库 = 空），gap 与 system/support 一起覆盖 ----
  mp_db_applied_load
  MISSING_MIG=$(mp_missing_names)
  if [ -z "$MISSING_MIG" ]; then
    ok "集合复核：磁盘上的版本化迁移全部已记账（含 business/system/support 全部分目录）"
  else
    die "迁移后仍有未记账的迁移：$MISSING_MIG（磁盘有、库里没有 success 记录）"
  fi
fi

# ----------------------------------------------------------------------------
step "[6/8] 启用新二进制（换文件 → 重启 → /health → 进程==磁盘 → 记账自检）"
if [ -z "$BE_CHANGED" ]; then
  say "无后端改动，不动服务"
elif [ "$DRY_RUN" = "1" ]; then
  say "dry-run：将 cp → .new + mv -f 换二进制 → systemctl restart $SERVICE → 轮询 $HEALTH_URL"
elif [ "$BIN_BUILT" != "1" ]; then
  say "本次没有新编译的二进制，不动服务（无谓重启只会抖一下）"
else
  cd "$BACKEND_DIR" || die "cd backend 失败"
  say "换二进制（cp → .new + mv -f，绝不 cp 到正在运行的路径）…"
  cp target/release/cloudpivot target/release/cloudpivot.new \
    && mv -f target/release/cloudpivot.new target/release/cloudpivot \
    || die "换二进制失败"

  say "重启服务 …"
  systemctl restart "$SERVICE" || die "systemctl restart 失败"
  DID_BACKEND=1
  UP=0
  for _ in $(seq 1 30); do
    if curl -s -m 3 "$HEALTH_URL" | grep -q '"status":"ok"'; then UP=1; break; fi
    sleep 2
  done
  if [ "$UP" != "1" ]; then
    say "health 未在 60s 内就绪，自动回滚二进制 …"
    systemctl stop "$SERVICE"
    cp "$BACKUP_DIR/cloudpivot" target/release/cloudpivot.rollback && mv -f target/release/cloudpivot.rollback target/release/cloudpivot
    systemctl start "$SERVICE"; sleep 3
    ROLLED_BACK=1
    journalctl -u "$SERVICE" -n 30 --no-pager | sed 's/^/    /'
    die "重启后 /health 不正常（二进制已回滚）"
  fi
  ok "/health 正常：$(curl -s -m 3 "$HEALTH_URL")"

  PID=$(systemctl show -p MainPID --value "$SERVICE")
  PROC_MD5=$(md5sum "/proc/$PID/exe" 2>/dev/null | cut -d' ' -f1)
  DISK_MD5=$(md5sum target/release/cloudpivot | cut -d' ' -f1)
  [ "$PROC_MD5" = "$DISK_MD5" ] && ok "进程 == 磁盘（md5 $PROC_MD5，PID $PID）" || die "进程 md5($PROC_MD5) ≠ 磁盘 md5($DISK_MD5)：换了文件没重启？"

  # 铁律（硬判据，不能用日志文本代替）：重启后「磁盘上有、库里没记账」的版本化迁移必须为空。
  # 引擎判定是集合语义、gap 补迁只打 out_of_order WARN 不报错 ⇒「启动期补跑了漏记的迁移」
  # 不会以任何失败形式暴露，只能在这里用集合差揪出来（2026-11-05 的 V0235 正是这么漏的）。
  mp_db_applied_load
  MISSING_AFTER=$(mp_missing_names)
  if [ -n "$MISSING_AFTER" ]; then
    die "重启后仍有未记账的迁移：$MISSING_AFTER —— 启动期被引擎补跑了，记账与引擎不一致（铁律被破坏）"
  fi
  ok "集合复核：重启后无未记账的版本化迁移（含 business/system/support 全部分目录）"

  # applied_via 再查一次：非法值会让服务起来就 FATAL，这里挡住比上线后查日志快
  BAD_VIA_START=$(mp_bad_applied_via)
  [ -z "$BAD_VIA_START" ] && ok "applied_via 合法（全部 ∈ $MP_ALLOWED_APPLIED_VIA）" \
    || die "启动后 applied_via 出现非法值：$(echo "$BAD_VIA_START" | paste -sd' ' -)"

  NMIG=$(journalctl -u "$SERVICE" --since "-3min" --no-pager | grep -c "数据库迁移完成: 应用 0 个迁移" || true)
  if [ "${NMIG:-0}" -ge 1 ]; then
    ok "启动日志确认「应用 0 个迁移」（记账与引擎一致）"
  else
    say "ℹ️  启动日志没出现「应用 0 个迁移」：版本化迁移已全部记账（上面集合差已校验为空），"
    say "     通常是本次改动了 seed/可重复脚本被引擎重跑；人工核对：journalctl -u $SERVICE -n 60 | grep 迁移"
  fi
fi

# ----------------------------------------------------------------------------
step "[7/8] 前端（只在有前端改动时 typecheck + build，并比对入口指纹）"
if [ -z "$FE_CHANGED" ]; then
  say "无前端改动，跳过 dist 构建（要强制重建加 --force-frontend）"
elif [ "$DRY_RUN" = "1" ]; then
  say "dry-run：将 export NODE_OPTIONS=--max-old-space-size=$NODE_MAX_OLD_SPACE → pnpm typecheck → vite build --outDir dist-$TAG → 双 rename → curl $WEB_URL 比对入口 chunk"
else
  cd "$FRONTEND_DIR" || die "cd frontend 失败"
  # 必须显式抬堆：这台 7GB / aarch64 机器上 node 默认老生代上限约 2GB，
  # 本项目 dist（378 assets、tdesign chunk 8MB）在 rendering chunks 阶段必 OOM
  # （2026-11-08 生产部署实测：默认堆 → FATAL ERROR: Reached heap limit @ ~2002MB；
  #   4608 后同一条 build 37s 通过）。不继承外部环境，脚本自己钉住口径。
  export NODE_OPTIONS="--max-old-space-size=$NODE_MAX_OLD_SPACE"
  say "node $(command -v node) $(node -v 2>/dev/null)　NODE_OPTIONS=$NODE_OPTIONS"
  say "pnpm typecheck …"
  pnpm typecheck || die "pnpm typecheck 有错（按规范前端 0 错才允许上线）"
  ok "typecheck 0 错"

  say "vite build → dist-$TAG …"
  VITE_T0=$(date +%s)
  if ! pnpm exec vite build --outDir "dist-$TAG" --emptyOutDir; then
    # 把「怎么修」写进失败提示：本卡就是被这一条咬出来的
    say "（build 失败；若日志含 'Reached heap limit' / 'JavaScript heap out of memory' / Mark-Compact，就是堆上限不够）"
    die "vite build 失败（NODE_OPTIONS=$NODE_OPTIONS）。堆不够时这样重跑前端：NODE_MAX_OLD_SPACE=5120 bash scripts/deploy/deploy.sh --skip-migrate --skip-backend --force-frontend"
  fi
  ok "vite build 完成（$(( $(date +%s) - VITE_T0 ))s，NODE_OPTIONS=$NODE_OPTIONS）"
  NEW_ENTRY=$(grep -o 'assets/index-[^"]*\.js' "dist-$TAG/index.html" | head -1)
  [ -n "$NEW_ENTRY" ] || die "新 dist 里找不到入口 chunk"
  ok "新入口：$NEW_ENTRY"

  # 双 rename：旧 dist 留档 → 新 dist 就位（绝不先 mv 再 build）
  if [ -d dist ]; then
    mv dist "dist-prev-$TAG" || die "旧 dist 改名失败"
  fi
  mv "dist-$TAG" dist || die "新 dist 就位失败"
  DID_FRONTEND=1

  sleep 1
  LIVE_ENTRY=$(curl -s -m 5 "$WEB_URL" | grep -o 'assets/index-[^"]*\.js' | head -1)
  say "线上入口：$LIVE_ENTRY"
  if [ "$NEW_ENTRY" != "$LIVE_ENTRY" ]; then
    say "入口不一致，回滚前端 …"
    mv dist "dist-failed-$TAG" && mv "dist-prev-$TAG" dist
    ROLLED_BACK=1
    die "线上入口($LIVE_ENTRY) ≠ 新 dist 入口($NEW_ENTRY)（nginx root 是否指向 $FRONTEND_DIR/dist？已回滚）"
  fi
  ok "前端入口指纹一致：$NEW_ENTRY"
fi

# ----------------------------------------------------------------------------
step "[8/8] 收口核对"
if [ "$DRY_RUN" = "1" ]; then
  say "dry-run 结束：未做任何变更"
  exit 0
fi
cd "$REPO" || die "cd $REPO 失败"
FINAL_HEAD=$(git rev-parse HEAD)
BARE=$(git --git-dir="$REPO/../git/cloudpivot.git" rev-parse main 2>/dev/null || git ls-remote origin refs/heads/main | cut -f1)
say "主仓 HEAD          $(git rev-parse --short HEAD)  $(git log -1 --format=%s)"
say "origin/main        $(git rev-parse --short origin/main)"
[ "$FINAL_HEAD" = "$(git rev-parse origin/main)" ] && ok "主仓 HEAD == origin/main" || bad "主仓 HEAD ≠ origin/main"
say "迁移记账           $(mp_db_applied_count) 条，最大版本 V$(printf '%04d' "$(mp_db_max_version)")"
mp_db_applied_load
FINAL_MISSING=$(mp_missing_names)
if [ -z "$FINAL_MISSING" ]; then
  ok "磁盘迁移全部已记账（磁盘 - 库 = 空）"
else
  bad "仍有未记账迁移：$FINAL_MISSING —— 库与磁盘不一致，下一次启动引擎会静默补跑（先跑 --skip-backend 只补迁移）"
fi
say "服务状态           $(systemctl is-active "$SERVICE")"
say "/health            $(curl -s -m 5 "$HEALTH_URL")"
PID=$(systemctl show -p MainPID --value "$SERVICE")
say "进程==磁盘         $([ "$(md5sum /proc/$PID/exe 2>/dev/null | cut -d' ' -f1)" = "$(md5sum "$BACKEND_DIR/target/release/cloudpivot" | cut -d' ' -f1)" ] && echo yes || echo NO)"
say "线上前端入口       $(curl -s -m 5 "$WEB_URL" | grep -o 'assets/index-[^"]*\.js' | head -1)（dist/index.html: $(grep -o 'assets/index-[^"]*\.js' "$FRONTEND_DIR/dist/index.html" | head -1)）"
say "备份目录           $BACKUP_DIR"
echo
echo "================= 部署完成 ================="
echo "  发布提交   $(git rev-parse --short HEAD)"
echo "  备份        $BACKUP_DIR（db.dump / cloudpivot / dist）"
echo "  日志        $LOG"
echo "  回滚：见 $REPO/scripts/deploy/README.md「回滚」一节（三件套：二进制 / dist / 库 dump）"
echo "        二进制  systemctl stop $SERVICE && cp $BACKUP_DIR/cloudpivot $BACKEND_DIR/target/release/cloudpivot.rollback && mv -f $BACKEND_DIR/target/release/cloudpivot.rollback $BACKEND_DIR/target/release/cloudpivot && systemctl start $SERVICE"
echo "        前端    mv $FRONTEND_DIR/dist $FRONTEND_DIR/dist-failed-$TAG && mv $FRONTEND_DIR/dist-prev-$TAG $FRONTEND_DIR/dist"
echo "        代码    git -C $REPO reset --hard $OLD_HEAD"
echo "============================================"
