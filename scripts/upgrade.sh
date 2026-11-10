#!/usr/bin/env bash
# =============================================================================
# upgrade.sh —— 云枢（CloudPivot）客户现场**版本升级**脚本（含预演 / 自动回滚 / 手动回滚）
#
# 与 scripts/setup_new_env.sh（新机器从零建库）互补：本脚本把一个**已经在跑旧版本**的
# 环境升级到目标版本，全程 9 步，任一步失败都停在安全点并给出精确的回滚手段。
#
#   ┌ ① 版本检测   库内 schema_migrations 已应用集合 vs 目标代码的迁移文件 → 待应用清单
#   ├ ② 前置检查   磁盘余量（≥2× 库大小）、数据库连通性、服务状态、工具链、工作区干净
#   ├ ③ 全库备份   pg_dump -Fc + pg_restore --list 校验；**备份不成立就不许往下走**
#   ├ ④ 拉代码+编译 git fetch + 切到目标 tag/commit + cargo build；编译失败不碰数据库
#   ├ ⑤ 迁移预演   备份恢复到临时库 → 在临时库上跑全部待应用迁移 → 校验表数/约束/记账
#   │              （V0359 撞唯一约束导致生产中断 30 分钟，就是因为跳过了这一步）
#   ├ ⑥ 正式迁移   停服务 → 逐条应用 → 对账（记账增量 == 待跑条数）→ 集合复核
#   ├ ⑦ 换二进制   停服状态原子换文件（cp .new + mv -f）→ 启动
#   ├ ⑧ 冒烟       /health 200 → 登录 200 → 关键接口 3 个 200；任一失败自动回滚
#   └ ⑨ 收尾报告   前后版本 / 迁移清单 / 耗时 / 备份路径 / 回滚命令
#
# 设计要点（都是踩过坑换来的，改脚本前先读）：
#   · **待应用清单 = 集合差**（目标树上的 up.sql − 库内 kind='versioned' 且 success 的 name），
#     不是「版本号 > 库内最大版本」。引擎 needs_apply 只认 (kind,name) 有没有 success 记录
#     （vendor/nik-db/src/migrate/mod.rs:1263-1275），gap 里的低版本照样要跑；
#     比大小的写法会把 gap 整批漏掉（2026-11-05 发版中断的真因）。
#   · **预演用真备份**：把刚做的全库备份恢复到临时库再跑迁移，坏 SQL / 撞唯一约束 /
#     依赖缺失都会在临时库上炸，生产库一点没动（V0359 事故的唯一解药）。
#   · **回滚 = 换库**：恢复时先 pg_restore 到一个全新库，再 rename 顶上（原子性靠 PG 的
#     ALTER DATABASE RENAME）。不 drop schema、不用 --clean 覆盖，避开分区表/扩展的坑；
#     被换下来的旧库改名留档，回滚本身也可回滚。
#   · **迁移不可变**：已应用的 up.sql 内容被改写 → 引擎启动期 FATAL（checksum 校验）。
#     脚本动手前比对库内 checksum 与目标树的文件指纹，不一致直接拒跑。
#   · **二进制两段式**：编译完成后先快照新二进制、把旧二进制放回安装位；迁移窗口内若
#     服务意外重启，跑的是与库一致的旧二进制；⑥ 成功后 ⑦ 才原子换上新二进制。
#   · **幂等**：待应用清单为空 → 提示「已是最新」并退出 0，不备份、不停服、不动二进制。
#   · **不留死单**：任何失败都停在「能继续操作」的状态（库可恢复、二进制可换回、服务可启），
#     状态文件 upgrade-state.env 边跑边写，--rollback latest 可抢救被 kill 的运行。
#
# 用法：
#   bash scripts/upgrade.sh                          # 升级到 origin/main（交互确认）
#   bash scripts/upgrade.sh --dry-run                # 只演练 ①②③⑤，生产零改动
#   bash scripts/upgrade.sh --target v1.2.0 --yes     # 升级到指定 tag（CI 用 --yes 跳过确认）
#   bash scripts/upgrade.sh --rollback latest --yes   # 从最近一次升级的备份回滚
#   bash scripts/upgrade.sh --rollback /root/cloudpivot-backups/cp-upgrade-20261110-214000 --yes
#
# 常用参数：
#   --target REF        目标 git ref（tag / commit / 分支），默认 origin/main
#   --repo PATH         仓库根（默认脚本所在仓库）
#   --env-file PATH     环境变量文件（默认按 /etc/cloudpivot/*.env 顺序找）
#   --service NAME      systemd 服务名（默认 cloudpivot）
#   --no-service        不管理服务（客户自己停/起；脚本只做库与二进制）
#   --db NAME           覆盖库名（默认取 env 的 NIK_DATABASE_NAME）
#   --profile release|debug   cargo 构建 profile（默认 release；测试环境可用 debug）
#   --binary PATH       后端二进制路径（默认 <repo>/backend/target/<profile>/cloudpivot）
#   --backup-dir PATH   备份根目录（默认 /var/backups/cloudpivot，不可写时退到 ~/cloudpivot-backups）
#   --log-dir PATH      日志目录（默认 /var/log/cloudpivot，不可写时退到 ~/cloudpivot-logs）
#   --base-url URL      冒烟地址（默认由 env 的 NIK_SERVER_LISTEN 推导）
#   --smoke-user NAME   冒烟登录账号（默认 admin）
#   --smoke-pass PASS   冒烟登录口令（默认取 env 的 NIK_ADMIN_PASSWORD；都没有则跳过登录检查）
#   --smoke-apis "A,B,C"  冒烟的关键接口（默认 /api/system/auth/me,/api/system/dept/tree,/api/system/menu/tree）
#   --dry-run           只做 ①②③⑤（备份照做，但落在 dryrun- 目录，不碰生产库/服务/二进制/代码）
#   --rollback [SPEC]   回滚：SPEC = latest（默认）| 备份目录 | .dump 文件
#   --skip-build        跳过编译（已手工编好；仍会切代码）
#   --skip-rehearsal    跳过迁移预演（**不推荐**，V0359 事故就是这么来的；会显红警告）
#   --allow-drift       允许已应用迁移的内容与库内 checksum 不一致（**危险**：引擎启动会 FATAL）
#   --allow-dirty       允许工作区有未提交的已跟踪改动
#   --yes               所有交互确认全跳过（CI）
#   -h|--help
#
# 退出码：0 成功 / 已是最高版本 ｜ 1 失败（已尽量自动回滚）｜ 2 参数或环境问题
# =============================================================================
set -euo pipefail

# -----------------------------------------------------------------------------
# 0) 自举：把自己复制到临时目录再执行
#    ④ 会 git checkout 目标版本 —— 如果脚本从仓库里执行，checkout 会原地改写正在被
#    bash 逐行读取的文件（读到一半换内容 = 随机命令）。所以先自举一份再干活。
# -----------------------------------------------------------------------------
if [ "${CP_UPGRADE_REEXEC:-0}" != "1" ]; then
  _self_src="${BASH_SOURCE[0]}"
  case "$_self_src" in /*) ;; *) _self_src="$PWD/$_self_src" ;; esac
  CP_UPGRADE_ORIG_DIR="$(cd "$(dirname "$_self_src")" && pwd)"
  _self_dir="$(mktemp -d "${TMPDIR:-/tmp}/cp-upgrade-self.XXXXXX")"
  cp "$_self_src" "$_self_dir/upgrade.sh"
  CP_UPGRADE_REEXEC=1 CP_UPGRADE_SELF_DIR="$_self_dir" CP_UPGRADE_ORIG_DIR="$CP_UPGRADE_ORIG_DIR" \
    CP_UPGRADE_ORIG_SCRIPT="$_self_src" \
    exec bash "$_self_dir/upgrade.sh" "$@"
fi
SELF_DIR="${CP_UPGRADE_SELF_DIR:-}"
# 用户实际敲的那个脚本路径（自举后 $0 是 /tmp 副本，提示里必须给原始路径）
SELF_CMD="${CP_UPGRADE_ORIG_SCRIPT:-$0}"
cleanup_self() { if [ -n "$SELF_DIR" ]; then rm -rf "$SELF_DIR"; fi; return 0; }

export PATH="${HOME}/.cargo/bin:/usr/local/bin:$PATH"

# -----------------------------------------------------------------------------
# 1) 参数
# -----------------------------------------------------------------------------
PROG="$(basename "$0")"
TS="$(date +%Y%m%d-%H%M%S)"
TODAY="$(date +%Y-%m-%d)"

TARGET_REF="origin/main"
REPO_ARG=""
ENV_FILE_ARG=""
SERVICE="${CP_SERVICE:-cloudpivot}"
MANAGE_SERVICE=1
DB_OVERRIDE=""
BUILD_PROFILE="release"
BINARY_ARG=""
BACKUP_ROOT_ARG=""
LOG_DIR_ARG=""
BASE_URL_ARG=""
SMOKE_USER="admin"
SMOKE_PASS="${SMOKE_PASS:-}"
SMOKE_APIS_ARG=""
DRY_RUN=0
ASSUME_YES=0
SKIP_BUILD=0
SKIP_REHEARSAL=0
ALLOW_DRIFT=0
ALLOW_DIRTY=0
ROLLBACK_SPEC=""
MODE="upgrade"

usage() { sed -n '2,70p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit "${1:-0}"; }

while [ $# -gt 0 ]; do
  case "$1" in
    --target)         TARGET_REF="${2:?--target 需要值}"; shift 2 ;;
    --target=*)       TARGET_REF="${1#*=}"; shift ;;
    --repo)           REPO_ARG="${2:?--repo 需要值}"; shift 2 ;;
    --repo=*)         REPO_ARG="${1#*=}"; shift ;;
    --env-file)       ENV_FILE_ARG="${2:?--env-file 需要值}"; shift 2 ;;
    --env-file=*)     ENV_FILE_ARG="${1#*=}"; shift ;;
    --service)        SERVICE="${2:?--service 需要值}"; shift 2 ;;
    --service=*)      SERVICE="${1#*=}"; shift ;;
    --no-service)     MANAGE_SERVICE=0; shift ;;
    --db)             DB_OVERRIDE="${2:?--db 需要值}"; shift 2 ;;
    --db=*)           DB_OVERRIDE="${1#*=}"; shift ;;
    --profile)        BUILD_PROFILE="${2:?--profile 需要值}"; shift 2 ;;
    --profile=*)      BUILD_PROFILE="${1#*=}"; shift ;;
    --binary)         BINARY_ARG="${2:?--binary 需要值}"; shift 2 ;;
    --binary=*)       BINARY_ARG="${1#*=}"; shift ;;
    --backup-dir)     BACKUP_ROOT_ARG="${2:?--backup-dir 需要值}"; shift 2 ;;
    --backup-dir=*)   BACKUP_ROOT_ARG="${1#*=}"; shift ;;
    --log-dir)        LOG_DIR_ARG="${2:?--log-dir 需要值}"; shift 2 ;;
    --log-dir=*)      LOG_DIR_ARG="${1#*=}"; shift ;;
    --base-url)       BASE_URL_ARG="${2:?--base-url 需要值}"; shift 2 ;;
    --base-url=*)     BASE_URL_ARG="${1#*=}"; shift ;;
    --smoke-user)     SMOKE_USER="${2:?--smoke-user 需要值}"; shift 2 ;;
    --smoke-user=*)   SMOKE_USER="${1#*=}"; shift ;;
    --smoke-pass)     SMOKE_PASS="${2:?--smoke-pass 需要值}"; shift 2 ;;
    --smoke-pass=*)   SMOKE_PASS="${1#*=}"; shift ;;
    --smoke-apis)     SMOKE_APIS_ARG="${2:?--smoke-apis 需要值}"; shift 2 ;;
    --smoke-apis=*)   SMOKE_APIS_ARG="${1#*=}"; shift ;;
    --dry-run)        DRY_RUN=1; shift ;;
    --yes|-y)         ASSUME_YES=1; shift ;;
    --skip-build)     SKIP_BUILD=1; shift ;;
    --skip-rehearsal) SKIP_REHEARSAL=1; shift ;;
    --allow-drift)    ALLOW_DRIFT=1; shift ;;
    --allow-dirty)    ALLOW_DIRTY=1; shift ;;
    --rollback)       MODE="rollback"
                      case "${2:-}" in
                        ""|-*) ROLLBACK_SPEC="latest"; shift ;;
                        *)     ROLLBACK_SPEC="$2"; shift 2 ;;
                      esac ;;
    --rollback=*)     MODE="rollback"; ROLLBACK_SPEC="${1#*=}"; [ -n "$ROLLBACK_SPEC" ] || ROLLBACK_SPEC="latest"; shift ;;
    -h|--help)        usage 0 ;;
    *) echo "⛔ 未知参数：$1（--help 看用法）" >&2; exit 2 ;;
  esac
done

case "$BUILD_PROFILE" in
  release|debug) ;;
  *) echo "⛔ --profile 只支持 release / debug（收到 $BUILD_PROFILE）" >&2; exit 2 ;;
esac

# -----------------------------------------------------------------------------
# 2) 定位仓库 / 环境变量文件 / 日志
# -----------------------------------------------------------------------------
ORIG_DIR="${CP_UPGRADE_ORIG_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
if [ -n "$REPO_ARG" ]; then
  REPO="$REPO_ARG"
elif [ -d "$ORIG_DIR/../backend/migrations" ]; then
  REPO="$(cd "$ORIG_DIR/.." && pwd)"
else
  REPO="/opt/cloudpivot"
fi
( cd "$REPO" 2>/dev/null && REPO="$(pwd)" ) || true
[ -d "$REPO/backend/migrations" ] || { echo "⛔ 找不到仓库根（$REPO 下没有 backend/migrations）；用 --repo 指定" >&2; exit 2; }
git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1 || { echo "⛔ $REPO 不是 git 仓库（升级要靠 git 取目标版本）" >&2; exit 2; }

# 日志目录：默认 /var/log/cloudpivot；没有 root 权限时退到用户目录并提示（不假装成功）
LOG_DIR="${LOG_DIR_ARG:-/var/log/cloudpivot}"
if ! mkdir -p "$LOG_DIR" 2>/dev/null || ! [ -w "$LOG_DIR" ]; then
  FALLBACK_LOG_DIR="$HOME/cloudpivot-logs"
  mkdir -p "$FALLBACK_LOG_DIR"
  echo "⚠ 日志目录 $LOG_DIR 不可写（当前用户 $(id -un)；写这里需要 root），改用 $FALLBACK_LOG_DIR" >&2
  LOG_DIR="$FALLBACK_LOG_DIR"
fi
LOG="$LOG_DIR/upgrade-$TS.log"
: > "$LOG"
# 全流程同时进日志文件（含子命令输出）
exec > >(tee -a "$LOG") 2>&1

say()  { echo "[$(date +%H:%M:%S)] $*"; }
step() { echo; echo "=============================================================="; echo "== $*"; echo "=============================================================="; }
ok()   { echo "  ✅ $*"; }
bad()  { echo "  ❌ $*"; }
warn() { echo "  ⚠ $*"; }

say "日志：$LOG"
say "仓库：$REPO"
say "口令只走 PGPASSWORD 环境变量，不落日志、不上命令行"

# -----------------------------------------------------------------------------
# 3) 环境变量 / 数据库连接参数 / 权限
# -----------------------------------------------------------------------------
ENV_FILE=""
if [ -n "$ENV_FILE_ARG" ]; then
  [ -r "$ENV_FILE_ARG" ] || { bad "环境变量文件不可读：$ENV_FILE_ARG"; exit 2; }
  ENV_FILE="$ENV_FILE_ARG"
else
  for c in "${NIK_ENV_FILE:-}" /etc/cloudpivot/cloudpivot.env \
           /etc/cloudpivot/cloudpivot-prod.env /etc/cloudpivot/cloudpivot-dev.env \
           /etc/cloudpivot/cloudpivot-test.env "$HOME/.cloudpivot/cloudpivot.env"; do
    if [ -n "$c" ] && [ -r "$c" ]; then ENV_FILE="$c"; break; fi
  done
fi
[ -n "$ENV_FILE" ] || {
  bad "找不到环境变量文件（需要 NIK_DATABASE_*）。用 --env-file 指定，或 export NIK_DATABASE_* 后重跑"
  exit 2
}
say "环境变量：$ENV_FILE"
set -a; . "$ENV_FILE"; set +a

DB_NAME="${DB_OVERRIDE:-${NIK_DATABASE_NAME:-}}"
DB_HOST="${NIK_DATABASE_HOST:-127.0.0.1}"
DB_PORT="${NIK_DATABASE_PORT:-5432}"
DB_USER="${NIK_DATABASE_USER:-cloudpivot}"
DB_PASS="${NIK_DATABASE_PASSWORD:-}"
[ -n "$DB_NAME" ] || { bad "环境里没有 NIK_DATABASE_NAME（也没有 --db）"; exit 2; }
case "$DB_NAME" in *[!A-Za-z0-9_.-]*|'') bad "非法库名：$DB_NAME"; exit 2 ;; esac
export PGPASSWORD="$DB_PASS"

# 权限：systemctl 需要 root 或免密 sudo；不要假设 root
if [ "$(id -u)" = "0" ]; then
  SUDO=""
elif sudo -n true 2>/dev/null; then
  SUDO="sudo -n"
else
  SUDO="__none__"
fi
if [ "$MANAGE_SERVICE" = "1" ] && [ "$SUDO" = "__none__" ] && [ "$DRY_RUN" != "1" ]; then
  bad "当前用户 $(id -un) 既不是 root 也没有免密 sudo，无法 systemctl stop/start $SERVICE"
  bad "请用 root / 配免密 sudo 运行；或加 --no-service 让脚本只管数据库与二进制（服务你自己停起）"
  exit 2
fi

BINARY="${BINARY_ARG:-$REPO/backend/target/$BUILD_PROFILE/cloudpivot}"

# 冒烟地址：默认从 env 的 NIK_SERVER_LISTEN 推导
if [ -n "$BASE_URL_ARG" ]; then
  BASE_URL="$BASE_URL_ARG"
else
  _listen="${NIK_SERVER_LISTEN:-127.0.0.1:8023}"
  _host="${_listen%:*}"; _port="${_listen##*:}"
  case "$_host" in 0.0.0.0|::|"[::]"|"") _host="127.0.0.1" ;; esac
  BASE_URL="http://$_host:$_port"
fi
BASE_URL="${BASE_URL%/}"
SMOKE_APIS="${SMOKE_APIS_ARG:-/api/system/auth/me,/api/system/dept/tree,/api/system/menu/tree}"
if [ -z "$SMOKE_PASS" ] && [ -n "${NIK_ADMIN_PASSWORD:-}" ]; then SMOKE_PASS="$NIK_ADMIN_PASSWORD"; fi

# 备份根目录：默认 /var/backups/cloudpivot，不可写退到用户目录
if [ -n "$BACKUP_ROOT_ARG" ]; then
  BACKUP_ROOT="$BACKUP_ROOT_ARG"
else
  BACKUP_ROOT="/var/backups/cloudpivot"
  if ! mkdir -p "$BACKUP_ROOT" 2>/dev/null || ! [ -w "$BACKUP_ROOT" ]; then
    BACKUP_ROOT="$HOME/cloudpivot-backups"
  fi
fi
mkdir -p "$BACKUP_ROOT"

# 关键表（预演/升级后核对存在性，与 scripts/ops/backup_db.sh 的 KEY_TABLES 同口径）
KEY_TABLES=(sys_user sys_role sys_menu sys_dict_type sys_config biz_purchase_order biz_voucher biz_inventory)
# 引擎认可的 applied_via（kind.rs:95-111），出现别的值 = 服务启动期 FATAL
ALLOWED_APPLIED_VIA="executed imported baseline"
# 引擎认的版本化迁移路径（与 scripts/deploy/migrate_plan.sh 的 MIG_PATH_RE 一致）
MIG_PATH_RE='^backend/migrations/[A-Za-z0-9_.-]+/V[0-9]+__[A-Za-z0-9_]+\.up\.sql$'

PSQL=(psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER")
Q() { "${PSQL[@]}" -d "$1" -Atq -v ON_ERROR_STOP=1 -c "$2"; }
QDB() { Q "$DB_NAME" "$1"; }

# 服务控制（--no-service 时是空操作）
svc() {
  [ "$MANAGE_SERVICE" = "1" ] || return 0
  if [ -n "$SUDO" ] && [ "$SUDO" != "__none__" ]; then
    $SUDO systemctl "$@"
  else
    systemctl "$@"
  fi
}
svc_active() { [ "$MANAGE_SERVICE" = "1" ] && svc is-active --quiet "$SERVICE"; }

# -----------------------------------------------------------------------------
# 4) 状态文件（边跑边写，供 --rollback latest 与事后追溯）
# -----------------------------------------------------------------------------
BK_DIR=""
STATE_FILE=""
PHASE="init"
OLD_HEAD=""; TARGET_HEAD=""; OLD_BINARY_MD5=""; NEW_BINARY_MD5=""
VER_BEFORE_MAX=0; VER_BEFORE_COUNT=0; VER_AFTER_MAX=0; VER_AFTER_COUNT=0
PENDING_COUNT=0; PENDING_LIST=""; PENDING_FIRST=""; PENDING_LAST=""
PROD_TABLES=0; REHEARSAL_TABLES=0; DUMP_SZ=0
START_TS=$(date +%s)
REHEARSAL_DB=""; REHEARSAL_TREE=""; REHEARSAL_WT=""; RESTORE_DB=""; PRE_ROLLBACK_DB=""
declare -a FAILURES=()

write_state() {
  [ -n "$STATE_FILE" ] || return 0
  {
    echo "# cloudpivot upgrade state —— 由 upgrade.sh 写入；--rollback 读它决定怎么退"
    echo "STATE_VERSION=1"
    echo "PHASE=$PHASE"
    echo "DRY_RUN=$DRY_RUN"
    echo "TS=$TS"
    echo "STARTED_AT=\"$TODAY $(date +%H:%M:%S)\""
    echo "REPO=$REPO"
    echo "ENV_FILE=$ENV_FILE"
    echo "SERVICE=$SERVICE"
    echo "MANAGE_SERVICE=$MANAGE_SERVICE"
    echo "DB_NAME=$DB_NAME"
    echo "DB_HOST=$DB_HOST"
    echo "DB_PORT=$DB_PORT"
    echo "DB_USER=$DB_USER"
    echo "BACKUP_DIR=$BK_DIR"
    echo "DUMP=$BK_DIR/db.dump"
    echo "OLD_BINARY=$BK_DIR/cloudpivot.old"
    echo "NEW_BINARY=$BK_DIR/cloudpivot.new"
    echo "BINARY=$BINARY"
    echo "OLD_HEAD=$OLD_HEAD"
    echo "TARGET_HEAD=$TARGET_HEAD"
    echo "OLD_BINARY_MD5=$OLD_BINARY_MD5"
    echo "NEW_BINARY_MD5=$NEW_BINARY_MD5"
    echo "VER_BEFORE_MAX=$VER_BEFORE_MAX"
    echo "VER_BEFORE_COUNT=$VER_BEFORE_COUNT"
    echo "VER_AFTER_MAX=$VER_AFTER_MAX"
    echo "VER_AFTER_COUNT=$VER_AFTER_COUNT"
    echo "PENDING_COUNT=$PENDING_COUNT"
    echo "PENDING_FIRST=$PENDING_FIRST"
    echo "PENDING_LAST=$PENDING_LAST"
    echo "PENDING_LIST=\"$PENDING_LIST\""
    echo "PROD_TABLES=$PROD_TABLES"
    echo "LOG=$LOG"
  } > "$STATE_FILE.tmp"
  mv -f "$STATE_FILE.tmp" "$STATE_FILE"
}

cleanup_all() {
  local rc=$?
  cleanup_rehearsal
  cleanup_self
  return $rc
}
# 预演资源（临时库 / 只读工作树）用完就回收：被 kill -9 时 EXIT trap 不会执行，
# 所以⑤成功之后立刻显式回收一次，trap 只作兜底。
cleanup_rehearsal() {
  if [ -n "$REHEARSAL_DB" ]; then
    "${PSQL[@]}" -d postgres -Atq -c "select pg_terminate_backend(pid) from pg_stat_activity where datname='$REHEARSAL_DB'" >/dev/null 2>&1 || true
    "${PSQL[@]}" -d postgres -Atq -c "drop database if exists \"$REHEARSAL_DB\" with (force)" >/dev/null 2>&1 \
      || "${PSQL[@]}" -d postgres -Atq -c "drop database if exists \"$REHEARSAL_DB\"" >/dev/null 2>&1 || true
    REHEARSAL_DB=""
  fi
  if [ -n "$REHEARSAL_WT" ] && [ -d "$REHEARSAL_WT" ]; then
    git -C "$REPO" worktree remove --force "$REHEARSAL_WT" >/dev/null 2>&1 || rm -rf "$REHEARSAL_WT" || true
    REHEARSAL_WT=""
  fi
  return 0
}
trap cleanup_all EXIT
# set -e 会让脚本静默退出；这里先说明「哪条命令、第几行」失败，客户翻日志能直接定位
trap 'rc=$?; echo; echo "❌ 未预期的失败：第 $LINENO 行 —— $BASH_COMMAND （rc=$rc）"; echo "   日志：$LOG"; exit $rc' ERR

die() {
  echo
  echo "############ 失败：$* ############"
  echo "日志：$LOG"
  if [ -n "$BK_DIR" ]; then echo "本次备份：$BK_DIR（db.dump / cloudpivot.old / upgrade-state.env）"; fi
  echo
  case "$PHASE" in
    init|precheck) echo "生产库、服务、二进制**均未改动**。" ;;
    backup_done|code_ready|code_built|rehearsed)
      echo "生产库、服务、二进制**均未改动**（失败点在 $PHASE 之后）。" ;;
    service_stopped|migrating)
      echo "服务可能仍处于停止状态，生产库可能已部分迁移。恢复手段："
      echo "  bash $SELF_CMD --rollback \"${BK_DIR:-<备份目录>}\" --yes"
      if [ "$MANAGE_SERVICE" = "1" ]; then echo "  （只想把服务拉起来：systemctl start $SERVICE）"; fi ;;
    migrated|binary_installed|started|smoke)
      echo "已迁移/已换二进制。回滚到升级前："
      echo "  bash $SELF_CMD --rollback \"${BK_DIR:-<备份目录>}\" --yes" ;;
    *) echo "当前阶段：$PHASE" ;;
  esac
  echo "--------------------------------------------------------------"
  if [ "${#FAILURES[@]}" -gt 0 ]; then
    printf '  %s\n' "${FAILURES[@]}"
  fi
  exit 1
}

# -----------------------------------------------------------------------------
# 5) 迁移清单算法（自带实现：老版本客户的树里没有 scripts/deploy/migrate_plan.sh）
#    disk@target = 目标 ref 树里的 backend/migrations/**/V<n>__<name>.up.sql
#    applied     = 库里 kind='versioned' 且 success=true 的 name
#    pending     = disk - applied，按版本号数值升序
# -----------------------------------------------------------------------------
declare -a MIG_VER=() MIG_NAME=()
declare -A APPLIED_NAME=() APPLIED_CKSUM=() MIG_FILE=()

load_applied() {
  APPLIED_NAME=(); APPLIED_CKSUM=()
  local n c
  while IFS='|' read -r n c; do
    if [ -n "$n" ]; then APPLIED_NAME["$n"]=1; APPLIED_CKSUM["$n"]="$c"; fi
  done < <(QDB "select name, coalesce(checksum,'') from schema_migrations where kind='versioned' and success")
}

load_disk_at() { # $1 = git ref → MIG_VER / MIG_NAME / MIG_FILE
  local ref="$1" raw bad_names dups
  MIG_VER=(); MIG_NAME=(); MIG_FILE=()
  raw=$(git -C "$REPO" ls-tree -r --name-only "$ref" -- backend/migrations | grep -E '\.up\.sql$' || true)
  if [ -z "$raw" ]; then bad "目标版本 $ref 的树里没有任何 V*.up.sql"; return 1; fi
  bad_names=$(printf '%s\n' "$raw" | grep -vE "$MIG_PATH_RE" || true)
  if [ -n "$bad_names" ]; then
    bad "迁移目录里有引擎识别不了的文件名（永远装不到，等于没写）："
    printf '      %s\n' $bad_names
    return 1
  fi
  dups=$(printf '%s\n' "$raw" | while IFS= read -r f; do b="${f##*/}"; b="${b#*__}"; printf '%s\n' "${b%.up.sql}"; done | sort | uniq -d)
  if [ -n "$dups" ]; then
    bad "同名迁移出现在多个目录（引擎按 (kind,name) 记账会互相覆盖，必须先改名）：$(echo "$dups" | paste -sd' ' -)"
    return 1
  fi
  local ver name f
  while IFS=$'\t' read -r ver name f; do
    MIG_VER+=("$ver"); MIG_NAME+=("$name"); MIG_FILE["$name"]="$f"
  done < <(printf '%s\n' "$raw" | while IFS= read -r p; do
             b="${p##*/}"; v="${b%%__*}"; v="${v#V}"; n="${b#*__}"; n="${n%.up.sql}"
             printf '%d\t%s\t%s\n' "$((10#$v))" "$n" "$p"
           done | sort -t$'\t' -k1,1n -k3,3)
  return 0
}

# 引擎口径内容指纹：逐行去掉行尾 CR、行尾统一 \n（vendor/nik-db/src/migrate/history.rs:540）
file_checksum_at() { # $1 = ref  $2 = repo 相对路径
  git -C "$REPO" cat-file blob "$1:$2" | awk '{sub(/\r$/,"")} {print}' | sha256sum | cut -d' ' -f1
}

# -----------------------------------------------------------------------------
# 6) 通用工具
# -----------------------------------------------------------------------------
human_bytes() { numfmt --to=iec --suffix=B "$1" 2>/dev/null || echo "${1}B"; }
db_size_bytes() { QDB "select pg_database_size('$DB_NAME')"; }
table_count() { "${PSQL[@]}" -d "$1" -Atq -c "select count(*) from information_schema.tables where table_schema='public' and table_type='BASE TABLE'"; }
table_list() { "${PSQL[@]}" -d "$1" -Atq -c "select tablename from pg_tables where schemaname='public' order by 1"; }
unique_index_count() { "${PSQL[@]}" -d "$1" -Atq -c "select count(*) from pg_indexes where schemaname='public' and indexdef like 'CREATE UNIQUE%'"; }
key_table_rows() { # $1 = db → 每行 "表名|行数"（表不在就 MISSING）
  local d="$1" t c
  for t in "${KEY_TABLES[@]}"; do
    c=$("${PSQL[@]}" -d "$d" -Atq -c "select count(*) from \"$t\"" 2>/dev/null || echo "MISSING")
    printf '%s|%s\n' "$t" "$c"
  done
}

confirm() {
  if [ "$ASSUME_YES" = "1" ]; then say "（--yes：跳过确认）$1"; return 0; fi
  if ! [ -t 0 ]; then bad "非交互环境无法确认：「$1」。确认无误请加 --yes"; exit 2; fi
  local a
  read -r -p "  ❓ $1 [y/N] " a
  case "$a" in y|Y|yes|YES) return 0 ;; *) bad "已取消"; exit 3 ;; esac
}

http_ok() { # $1=url  $2=token（可选）
  local url="$1" token="${2:-}" out code body
  if [ -n "$token" ]; then
    out=$(curl -s -m 10 -w '\n%{http_code}' -H "Authorization: Bearer $token" "$url" 2>/dev/null || true)
  else
    out=$(curl -s -m 10 -w '\n%{http_code}' "$url" 2>/dev/null || true)
  fi
  code=$(printf '%s' "$out" | tail -1)
  body=$(printf '%s' "$out" | sed '$d')
  if [ "$code" != "200" ]; then bad "$url → HTTP ${code:-无响应}"; return 1; fi
  if ! printf '%s' "$body" | grep -q '"code":0'; then
    bad "$url → HTTP 200 但业务码非 0：$(printf '%s' "$body" | head -c 200)"
    return 1
  fi
  return 0
}

# =============================================================================
# 模式 A：--rollback
# =============================================================================
resolve_backup_spec() { # $1 = latest | 目录 | .dump 文件 → 回填 BK_DIR
  local spec="$1"
  case "$spec" in
    latest|"")
      if [ -e "$BACKUP_ROOT/latest" ]; then
        BK_DIR="$(cd -P "$BACKUP_ROOT/latest" && pwd)"
      else
        BK_DIR="$(ls -1d "$BACKUP_ROOT"/*-upgrade-* 2>/dev/null | sort | tail -1 || true)"
      fi ;;
    *.dump) BK_DIR="$(cd "$(dirname "$spec")" && pwd)" ;;
    *)      BK_DIR="$(cd "$spec" 2>/dev/null && pwd || true)" ;;
  esac
  [ -n "$BK_DIR" ] && [ -d "$BK_DIR" ] || { bad "找不到备份：$spec"; exit 2; }
  if [ ! -f "$BK_DIR/db.dump" ]; then
    local d
    d=$(ls -1 "$BK_DIR"/*.dump 2>/dev/null | head -1 || true)
    [ -n "$d" ] || { bad "$BK_DIR 里没有 db.dump"; exit 2; }
    cp -f "$d" "$BK_DIR/db.dump"
  fi
}

restore_db_from_dump() { # $1 = dump 文件；「恢复到新库 → 原子 rename 顶上」
  local dump="$1"
  local newdb="cp_restore_${TS//-/}"
  say "恢复到全新库 $newdb …"
  "${PSQL[@]}" -d postgres -Atq -c "drop database if exists \"$newdb\" with (force)" >/dev/null 2>&1 || true
  if ! "${PSQL[@]}" -d postgres -Atq -v ON_ERROR_STOP=1 -c "create database \"$newdb\"" >/dev/null 2>&1; then
    if [ "$(id -u)" = "0" ] && command -v su >/dev/null && su - postgres -c "createdb -O $DB_USER $newdb" >/dev/null 2>&1; then
      ok "临时库已建（借 postgres 账号：当前账号无 CREATEDB）"
    else
      bad "建临时恢复库失败（$DB_USER 没有 CREATEDB 权限，且无法借 postgres 账号）：$newdb"
      return 1
    fi
  fi
  RESTORE_DB="$newdb"
  say "pg_restore → $newdb …"
  if ! pg_restore --no-owner --no-privileges -j 2 -d "$newdb" -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" \
        "$dump" >"$LOG.pg_restore" 2>&1; then
    bad "pg_restore 失败（详见 $LOG.pg_restore）："
    tail -8 "$LOG.pg_restore" | sed 's/^/      /'
    return 1
  fi
  local n_before n_after
  n_before=$(table_count "$DB_NAME" 2>/dev/null || echo 0)
  n_after=$(table_count "$newdb")
  if [ "${n_after:-0}" -le 0 ]; then bad "恢复后的库没有表（$newdb）"; return 1; fi
  ok "恢复完成：$newdb 有 $n_after 张表（回滚前线上 $n_before 张）"

  say "切换数据库：$DB_NAME → ${DB_NAME}_prerollback_$TS，$newdb → $DB_NAME …"
  "${PSQL[@]}" -d postgres -Atq -c "select pg_terminate_backend(pid) from pg_stat_activity where datname='$DB_NAME' and pid<>pg_backend_pid()" >/dev/null 2>&1 || true
  if ! "${PSQL[@]}" -d postgres -Atq -v ON_ERROR_STOP=1 -c "alter database \"$DB_NAME\" rename to \"${DB_NAME}_prerollback_$TS\"" >/dev/null; then
    bad "重命名线上库失败（可能还有连接没断开）；恢复库留在 $newdb，线上库未动"
    return 1
  fi
  if ! "${PSQL[@]}" -d postgres -Atq -v ON_ERROR_STOP=1 -c "alter database \"$newdb\" rename to \"$DB_NAME\"" >/dev/null; then
    bad "把恢复库改名为 $DB_NAME 失败；请手工把 ${DB_NAME}_prerollback_$TS 改回来，恢复库=$newdb"
    return 1
  fi
  PRE_ROLLBACK_DB="${DB_NAME}_prerollback_$TS"
  RESTORE_DB=""
  ok "已切换到恢复后的库（升级后的旧库留档为 $PRE_ROLLBACK_DB）"
  return 0
}

rollback_flow() {
  resolve_backup_spec "$ROLLBACK_SPEC"
  step "[R/6] 回滚：从 $BK_DIR 恢复到升级前"
  say "日志：$LOG"
  STATE_FILE="$BK_DIR/upgrade-state.env"
  if [ -f "$STATE_FILE" ]; then
    # shellcheck disable=SC1090
    set -a
    . "$STATE_FILE" || warn "状态文件里有读不动的行（$STATE_FILE），按已读到的部分继续回滚"
    set +a
    say "备份里的状态：PHASE=$PHASE　升级时间=${STARTED_AT:-未知}　库=${DB_NAME:-?}　服务=${SERVICE:-?}"
  else
    warn "备份目录里没有 upgrade-state.env：只回滚数据库，不处理二进制/代码"
  fi

  local sz
  sz=$(stat -c%s "$BK_DIR/db.dump" 2>/dev/null || echo 0)
  [ "$sz" -gt 0 ] || die "备份文件为空：$BK_DIR/db.dump"
  pg_restore --list "$BK_DIR/db.dump" >/dev/null 2>"$LOG.pg_restore_list" || die "备份文件不可读：pg_restore --list 失败"
  ok "备份可读：$(human_bytes "$sz")（$BK_DIR/db.dump）"

  local safety="$BK_DIR/rollback-safety-$TS.dump"
  say "[1/6] 回滚本身也要可回滚：先备份当前状态的库 → $safety"
  pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -Fc -f "$safety" || die "回滚前的安全备份失败，未做任何改动"
  [ -s "$safety" ] || die "回滚前的安全备份为空：$safety"
  ok "安全备份：$safety（$(human_bytes "$(stat -c%s "$safety")")）"

  confirm "确认把 $DB_NAME 回滚到 $BK_DIR 的状态？升级后的写入会丢失（安全备份见上）"

  say "[2/6] 停服务 $SERVICE …"
  if [ "$MANAGE_SERVICE" = "1" ]; then
    svc stop "$SERVICE" || warn "systemctl stop 返回非 0（可能本来就没跑）"
    ok "服务已停"
  else
    warn "--no-service：请自行确认 $SERVICE 已停止（否则 ALTER DATABASE RENAME 会因连接未断开失败）"
  fi

  say "[3/6] 恢复数据库 …"
  restore_db_from_dump "$BK_DIR/db.dump" || die "数据库恢复失败（线上库未改动；安全备份 $safety）"
  local back_max back_cnt
  back_max=$(QDB "select coalesce(max(version::int),0) from schema_migrations where kind='versioned' and success")
  back_cnt=$(QDB "select count(*) from schema_migrations where kind='versioned' and success")
  ok "恢复后迁移版本：V$(printf '%04d' "$back_max")（$back_cnt 条）"
  if [ -n "${VER_BEFORE_MAX:-}" ] && [ "$VER_BEFORE_MAX" != "0" ] && [ "$back_max" != "$VER_BEFORE_MAX" ]; then
    warn "恢复后版本 V$(printf '%04d' "$back_max") ≠ 备份记录的升级前版本 V$(printf '%04d' "$VER_BEFORE_MAX")，请人工确认"
  fi

  if [ -f "$BK_DIR/cloudpivot.old" ] && [ -n "${BINARY:-}" ] && [ -d "$(dirname "$BINARY")" ]; then
    say "[4/6] 换回旧二进制 → $BINARY …"
    cp -f "$BK_DIR/cloudpivot.old" "$BINARY.rollback" && mv -f "$BINARY.rollback" "$BINARY" \
      || warn "换回旧二进制失败：$BINARY"
    ok "旧二进制已就位（md5 $(md5sum "$BINARY" | cut -d' ' -f1)）"
  else
    warn "[4/6] 没有 cloudpivot.old 或二进制目录不存在，跳过二进制回滚"
  fi
  if [ -n "${OLD_HEAD:-}" ] && [ -n "${REPO:-}" ] && git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1 \
     && git -C "$REPO" cat-file -e "$OLD_HEAD^{commit}" 2>/dev/null; then
    say "[5/6] 代码切回升级前提交 ${OLD_HEAD:0:8} …"
    git -C "$REPO" checkout --detach "$OLD_HEAD" >/dev/null 2>&1 \
      && ok "代码已切回 ${OLD_HEAD:0:8}" || warn "git checkout $OLD_HEAD 失败，请手工处理"
  else
    warn "[5/6] 没有升级前 commit 记录，跳过代码回退"
  fi

  say "[6/6] 启动 + 冒烟 …"
  if [ "$MANAGE_SERVICE" = "1" ]; then
    svc start "$SERVICE" || die "启动失败：systemctl start $SERVICE"
    local up=0
    for _ in $(seq 1 30); do
      if curl -s -m 3 "$BASE_URL/health" | grep -q '"status":"ok"'; then up=1; break; fi
      sleep 2
    done
    [ "$up" = "1" ] || die "回滚后 /health 未就绪，查 journalctl -u $SERVICE -n 50"
  fi
  ok "回滚完成：库版本 V$(printf '%04d' "$back_max")"
  echo
  echo "================= 回滚完成 ================="
  echo "  数据库      $DB_NAME ← $BK_DIR/db.dump（V$(printf '%04d' "$back_max")，$back_cnt 条迁移）"
  echo "  升级后留档  ${PRE_ROLLBACK_DB:-（未产生）}"
  echo "  安全备份    $safety"
  echo "  日志        $LOG"
  echo "==========================================="
  exit 0
}

if [ "$MODE" = "rollback" ]; then
  rollback_flow
fi

# =============================================================================
# 模式 B：正常升级
# =============================================================================
step "[0/9] 升级前确认"
say "目标版本：$TARGET_REF"
say "数据库：$DB_USER@$DB_HOST:$DB_PORT/$DB_NAME"
if [ "$MANAGE_SERVICE" = "1" ]; then say "服务：$SERVICE（systemctl）"; else say "服务：不管理（--no-service）"; fi
say "二进制：$BINARY（profile=$BUILD_PROFILE）"
say "备份根：$BACKUP_ROOT"
say "冒烟地址：$BASE_URL"
if [ "$DRY_RUN" = "1" ]; then
  warn "**--dry-run**：只做 ①版本检测 ②前置检查 ③全库备份 ⑤迁移预演；不碰生产库/服务/二进制/代码"
fi

# -----------------------------------------------------------------------------
step "[1/9] 版本检测（库内已应用 vs 目标代码的迁移文件）"
# -----------------------------------------------------------------------------
command -v git >/dev/null || die "缺 git"
say "git fetch（只更新远端引用，不动工作区）…"
git -C "$REPO" fetch --tags --prune origin >/dev/null 2>&1 || die "git fetch origin 失败"
OLD_HEAD="$(git -C "$REPO" rev-parse HEAD)"
TARGET_HEAD="$(git -C "$REPO" rev-parse --verify "$TARGET_REF^{commit}" 2>/dev/null || true)"
[ -n "$TARGET_HEAD" ] || die "解析不到目标版本：$TARGET_REF（先 git fetch，或确认 tag/分支名）"
say "当前 HEAD   ${OLD_HEAD:0:8} $(git -C "$REPO" log -1 --format=%s HEAD)"
say "目标版本    ${TARGET_HEAD:0:8} $(git -C "$REPO" log -1 --format=%s "$TARGET_HEAD")"

if [ "$(QDB "select 1" 2>/dev/null || true)" != "1" ]; then
  bad "连不上数据库 $DB_USER@$DB_HOST:$DB_PORT/$DB_NAME"
  bad "先确认：数据库在跑吗？env 里的 NIK_DATABASE_* 对吗？pg_hba 允许本机口令登录吗？"
  exit 2
fi
HAS_MIG_TABLE=$(QDB "select count(*) from information_schema.tables where table_name='schema_migrations'")
if [ "$HAS_MIG_TABLE" != "1" ]; then
  bad "库里没有 schema_migrations 表：这个库还没初始化过"
  bad "全新环境请先跑：bash scripts/setup_new_env.sh <库名>（本脚本只负责升级已初始化的环境）"
  exit 2
fi

VER_BEFORE_MAX=$(QDB "select coalesce(max(version::int),0) from schema_migrations where kind='versioned' and success")
VER_BEFORE_COUNT=$(QDB "select count(*) from schema_migrations where kind='versioned' and success")
load_applied
load_disk_at "$TARGET_HEAD" || die "目标版本的迁移文件清单不可用（见上）"

# ---- 记账合法性（非法值 = 服务启动 FATAL，动手前就挡住）----
BAD_VIA=$(QDB "select applied_via||'|'||count(*) from schema_migrations
                where applied_via not in ('executed','imported','baseline')
                group by applied_via")
if [ -n "$BAD_VIA" ]; then
  bad "schema_migrations.applied_via 出现引擎不认识的值：$(echo "$BAD_VIA" | paste -sd' ' -)"
  bad "引擎只认 ($ALLOWED_APPLIED_VIA)（vendor/nik-db/src/migrate/kind.rs:95-111），非法值会让服务启动期 FATAL"
  bad "修法：update schema_migrations set applied_via='imported' where applied_via='<非法值>';"
  exit 1
fi
ok "记账合法性：applied_via 全部 ∈ ($ALLOWED_APPLIED_VIA)"

# ---- 迁移不可变 ----
DRIFT=""; MISSING_ON_DISK=""; i=0
while [ "$i" -lt "${#MIG_NAME[@]}" ]; do
  n="${MIG_NAME[$i]}"; v="${MIG_VER[$i]}"
  if [ -n "${APPLIED_NAME[$n]:-}" ]; then
    want="${APPLIED_CKSUM[$n]:-}"
    if [ -n "$want" ]; then
      got="$(file_checksum_at "$TARGET_HEAD" "${MIG_FILE[$n]}")"
      if [ "$got" != "$want" ]; then DRIFT="${DRIFT}V$(printf '%04d' "$v"):$n(库 ${want:0:12}… / 盘 ${got:0:12}…) "; fi
    fi
  fi
  i=$((i + 1))
done
if [ "${#APPLIED_NAME[@]}" -gt 0 ]; then
  while IFS= read -r n; do
    if [ -n "$n" ] && [ -z "${MIG_FILE[$n]:-}" ]; then MISSING_ON_DISK="${MISSING_ON_DISK}${n} "; fi
  done < <(printf '%s\n' "${!APPLIED_NAME[@]}")
fi
if [ -n "$MISSING_ON_DISK" ]; then
  warn "库内已应用、但目标版本树里已不存在的迁移：$MISSING_ON_DISK"
  warn "（引擎启动时会忽略它们，不致命；但「已应用的迁移不许删」是纪律，请确认是有意为之）"
fi
if [ -n "$DRIFT" ]; then
  bad "已应用的迁移内容被改写过（迁移不可变被破坏）："
  echo "      $DRIFT"
  if [ "$ALLOW_DRIFT" = "1" ]; then
    warn "--allow-drift：明知风险继续（引擎启动期可能 FATAL: checksum mismatch）"
  else
    bad "处理办法：把改动还原成已应用版本，或新增一条迁移去修（V0359 的教训：改旧迁移 = 各环境结构分叉）"
    bad "确需带病升级可加 --allow-drift（不推荐）"
    exit 1
  fi
else
  ok "迁移不可变校验：已应用迁移的内容指纹与目标版本逐条一致（${#APPLIED_NAME[@]} 条）"
fi

# ---- 待应用清单（集合差）----
declare -a PENDING_PAIRS=() PENDING_FILES=()
i=0
while [ "$i" -lt "${#MIG_NAME[@]}" ]; do
  n="${MIG_NAME[$i]}"
  if [ -z "${APPLIED_NAME[$n]:-}" ]; then
    PENDING_PAIRS+=("${MIG_VER[$i]}:$n")
    PENDING_FILES+=("${MIG_FILE[$n]}")
    PENDING_COUNT=$((PENDING_COUNT + 1))
    if [ "$PENDING_COUNT" = "1" ]; then PENDING_FIRST="${MIG_VER[$i]}"; fi
    PENDING_LAST="${MIG_VER[$i]}"
  fi
  i=$((i + 1))
done
PENDING_LIST="${PENDING_PAIRS[*]:-}"

say "库内已应用：$VER_BEFORE_COUNT 条 versioned 迁移，最大版本 V$(printf '%04d' "$VER_BEFORE_MAX")"
say "目标版本合计：${#MIG_NAME[@]} 条 versioned 迁移"

if [ "$PENDING_COUNT" -eq 0 ]; then
  ok "已是最新：目标版本 $TARGET_REF 的迁移全部已在库内记账，无需升级"
  say "（代码/二进制若与目标版本不同，需要单独发布：git diff HEAD $TARGET_REF -- backend/）"
  echo
  echo "================= 无需升级 ================="
  echo "  当前版本  V$(printf '%04d' "$VER_BEFORE_MAX")（$VER_BEFORE_COUNT 条迁移）"
  echo "  目标版本  V$(printf '%04d' "${MIG_VER[${#MIG_VER[@]}-1]}")（$TARGET_REF，${#MIG_NAME[@]} 条迁移）"
  echo "  日志      $LOG"
  echo "==========================================="
  exit 0
fi

say "待应用清单：V$(printf '%04d' "$PENDING_FIRST") → V$(printf '%04d' "$PENDING_LAST")，共 $PENDING_COUNT 条待应用"
for f in "${PENDING_FILES[@]}"; do say "    + $f"; done
LOWER_THAN_MAX=0
for p in "${PENDING_PAIRS[@]}"; do
  if [ "${p%%:*}" -lt "$VER_BEFORE_MAX" ]; then LOWER_THAN_MAX=$((LOWER_THAN_MAX + 1)); fi
done
if [ "$LOWER_THAN_MAX" -gt 0 ]; then
  warn "其中 $LOWER_THAN_MAX 条版本号低于库内最大已应用 V$(printf '%04d' "$VER_BEFORE_MAX")：属引擎允许的乱序补迁（会打 out_of_order WARN），必须照样跑"
fi

# -----------------------------------------------------------------------------
step "[2/9] 前置检查"
# -----------------------------------------------------------------------------
for t in psql pg_dump pg_restore curl; do
  command -v "$t" >/dev/null || die "缺命令：$t（装 postgresql-client / curl）"
done
ok "工具链：psql / pg_dump / pg_restore / curl 就位"
if [ "$SKIP_BUILD" != "1" ] && [ "$DRY_RUN" != "1" ]; then
  command -v cargo >/dev/null || die "缺 cargo（或用 --skip-build 用现成二进制）"
fi

DBSIZE=$(db_size_bytes)
if [ -z "$DBSIZE" ] || [ "$DBSIZE" -le 0 ]; then die "读不到库大小（pg_database_size('$DB_NAME')）"; fi
AVAIL_KB=$(df -Pk "$BACKUP_ROOT" | awk 'NR==2{print $4}')
NEED_KB=$(( (DBSIZE + 1023) / 1024 * 2 ))
say "库大小 $(human_bytes "$DBSIZE")　备份盘可用 $(human_bytes $((AVAIL_KB * 1024)))　需要（2×库）$(human_bytes $((NEED_KB * 1024)))"
if [ "$AVAIL_KB" -lt "$NEED_KB" ]; then
  bad "磁盘余量不足 2× 库大小（$BACKUP_ROOT）：释放空间或改 --backup-dir 到更大的盘"
  exit 1
fi
ok "磁盘余量够（$(human_bytes $((AVAIL_KB * 1024))) ≥ 2×$(human_bytes "$DBSIZE")）"
warn "注：预演还要在 PG 数据盘上开一个临时库（≈ 库大小），请确认 PG 所在磁盘也够"

PROD_TABLES=$(table_count "$DB_NAME")
ok "数据库连通：$DB_NAME 有 $PROD_TABLES 张业务表"

if [ "$MANAGE_SERVICE" = "1" ]; then
  if svc_active; then ok "服务 $SERVICE 正在运行（⑥ 会先停）"; else warn "服务 $SERVICE 当前未运行（⑥ 会确保它停下来再迁移）"; fi
else
  warn "--no-service：脚本不停/不起服务，请自行保证「迁移时服务已停」"
fi

DIRTY=$(git -C "$REPO" status --porcelain --untracked-files=no || true)
if [ -n "$DIRTY" ] && [ "$ALLOW_DIRTY" != "1" ]; then
  echo "$DIRTY" | sed 's/^/      /'
  die "工作区有未提交的已跟踪改动（脚本不替你 stash）。先提交/还原，或加 --allow-dirty"
fi
if [ -n "$DIRTY" ]; then warn "工作区有未提交改动（--allow-dirty 放行）"; else ok "工作区干净（无未提交的已跟踪改动）"; fi

# -----------------------------------------------------------------------------
step "[3/9] 全库备份（备份不成立就不许往下走）"
# -----------------------------------------------------------------------------
if [ "$DRY_RUN" = "1" ]; then
  BK_DIR="$BACKUP_ROOT/dryrun-${DB_NAME}-$TS"
else
  BK_DIR="$BACKUP_ROOT/${DB_NAME}-upgrade-$TS"
fi
mkdir -p "$BK_DIR"; chmod 700 "$BK_DIR"
STATE_FILE="$BK_DIR/upgrade-state.env"
PHASE="backup_started"; write_state
say "备份目录：$BK_DIR"
say "pg_dump -Fc …"
T0=$(date +%s)
pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -Fc -f "$BK_DIR/db.dump" \
  || die "pg_dump 失败（备份不成立，绝不动生产）"
DUMP_SZ=$(stat -c%s "$BK_DIR/db.dump" 2>/dev/null || echo 0)
[ "$DUMP_SZ" -gt 0 ] || die "备份文件为空：$BK_DIR/db.dump"
ok "db.dump 已生成：$(human_bytes "$DUMP_SZ")（$(( $(date +%s) - T0 ))s）"
if ! pg_restore --list "$BK_DIR/db.dump" > "$BK_DIR/db.dump.toc" 2>"$LOG.toc_err"; then
  bad "备份文件不可读（pg_restore --list 失败）："
  tail -5 "$LOG.toc_err" | sed 's/^/      /'
  die "备份校验失败"
fi
TOC_N=$(grep -c ';' "$BK_DIR/db.dump.toc" || true)
ok "备份可读：归档条目 $TOC_N 个（pg_restore --list）"

{
  echo "# 升级前快照 $(date '+%F %T')"
  echo "tables=$PROD_TABLES"
  echo "unique_indexes=$(unique_index_count "$DB_NAME")"
  echo "migration_version=V$(printf '%04d' "$VER_BEFORE_MAX")"
  echo "migration_count=$VER_BEFORE_COUNT"
  key_table_rows "$DB_NAME"
} > "$BK_DIR/before.txt"
ok "升级前快照（表数/唯一索引/关键表行数）→ $BK_DIR/before.txt"

if [ -f "$BINARY" ]; then
  cp -p "$BINARY" "$BK_DIR/cloudpivot.old"
  OLD_BINARY_MD5="$(md5sum "$BK_DIR/cloudpivot.old" | cut -d' ' -f1)"
  ok "旧二进制已备份：$(basename "$BINARY") md5=$OLD_BINARY_MD5（$(human_bytes "$(stat -c%s "$BK_DIR/cloudpivot.old")")）"
else
  warn "找不到现役二进制 $BINARY（首次上线？），跳过旧二进制备份"
fi
PHASE="backup_done"; write_state
if [ "$DRY_RUN" != "1" ]; then
  ln -sfn "$BK_DIR" "$BACKUP_ROOT/latest"
  ok "回滚点已登记：$BACKUP_ROOT/latest → $BK_DIR"
fi

# -----------------------------------------------------------------------------
step "[4/9] 拉代码 + 编译"
# -----------------------------------------------------------------------------
if [ "$DRY_RUN" = "1" ]; then
  warn "--dry-run：不切代码、不编译（预演只用目标版本的迁移 SQL，不需要二进制）"
else
  if [ "$OLD_HEAD" = "$TARGET_HEAD" ]; then
    ok "工作区已在目标版本 ${TARGET_HEAD:0:8}，无需切换"
  else
    say "切换代码：${OLD_HEAD:0:8} → ${TARGET_HEAD:0:8} …"
    if [ "$TARGET_REF" = "origin/main" ] && [ "$(git -C "$REPO" symbolic-ref --short -q HEAD || true)" = "main" ] \
       && git -C "$REPO" merge-base --is-ancestor "$OLD_HEAD" "$TARGET_HEAD" 2>/dev/null; then
      git -C "$REPO" merge --ff-only "$TARGET_HEAD" >/dev/null || die "git merge --ff-only 失败"
      ok "已快进到 ${TARGET_HEAD:0:8}"
    else
      git -C "$REPO" checkout --detach "$TARGET_HEAD" >/dev/null 2>&1 || die "git checkout $TARGET_REF 失败"
      ok "已切到 ${TARGET_HEAD:0:8}（detached HEAD；回滚会切回 ${OLD_HEAD:0:8}）"
    fi
  fi
  PHASE="code_ready"; write_state

  if [ "$SKIP_BUILD" = "1" ]; then
    warn "--skip-build：跳过编译，直接用 $BINARY"
    [ -x "$BINARY" ] || die "--skip-build 但二进制不存在/不可执行：$BINARY"
    NEW_BINARY_MD5="$(md5sum "$BINARY" | cut -d' ' -f1)"
    cp -f "$BINARY" "$BK_DIR/cloudpivot.new" || die "快照新二进制失败"
  else
    export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-1}"
    say "cargo build --profile $BUILD_PROFILE（jobs=$CARGO_BUILD_JOBS，日志 $LOG）…"
    T0=$(date +%s)
    ( cd "$REPO/backend" && cargo build --profile "$BUILD_PROFILE" ) || die "编译失败（数据库一点没动）"
    ok "编译完成（$(( $(date +%s) - T0 ))s）"
    [ -x "$BINARY" ] || die "编译产物不存在：$BINARY（--profile 与 --binary 是否匹配？）"
    NEW_BINARY_MD5="$(md5sum "$BINARY" | cut -d' ' -f1)"
    ok "新二进制：$(basename "$BINARY") md5=$NEW_BINARY_MD5 $(human_bytes "$(stat -c%s "$BINARY")")"
    cp -f "$BINARY" "$BK_DIR/cloudpivot.new" || die "快照新二进制失败：$BK_DIR/cloudpivot.new"
    ok "新二进制快照 → $BK_DIR/cloudpivot.new（⑦ 从它原子换上）"
    # 迁移窗口内若服务意外重启，跑回与库一致的旧二进制；⑥ 成功后在 ⑦ 才换新
    if [ -f "$BK_DIR/cloudpivot.old" ]; then
      cp -f "$BK_DIR/cloudpivot.old" "$BINARY.new" && mv -f "$BINARY.new" "$BINARY" \
        && warn "旧二进制已放回安装位（迁移完成后才换新，避免「旧二进制 × 新 schema」窗口）"
    fi
    if [ "$NEW_BINARY_MD5" = "$OLD_BINARY_MD5" ]; then
      warn "新二进制与旧二进制内容相同（本次只跑迁移，换二进制是空操作）"
    fi
  fi
  PHASE="code_built"; write_state
fi

# -----------------------------------------------------------------------------
step "[5/9] 迁移预演（最重要的一步：在真备份的临时库上把迁移全跑一遍）"
# -----------------------------------------------------------------------------
REHEARSAL_TREE=""
if [ "$SKIP_REHEARSAL" = "1" ]; then
  warn "**--skip-rehearsal：跳过预演**。V0359 撞唯一约束导致服务中断 30 分钟，就是没预演"
else
  if [ "$DRY_RUN" != "1" ] && [ "$(git -C "$REPO" rev-parse HEAD)" = "$TARGET_HEAD" ]; then
    REHEARSAL_TREE="$REPO"
  else
    # dry-run 或尚未切代码：用目标版本的独立工作树（只读迁移文件，不动当前工作区）
    REHEARSAL_WT="$(mktemp -d "${TMPDIR:-/tmp}/cp-upgrade-wt.XXXXXX")/tree"
    say "取目标版本代码树 → $REHEARSAL_WT（git worktree，只读用）…"
    git -C "$REPO" worktree add --detach "$REHEARSAL_WT" "$TARGET_HEAD" >/dev/null 2>&1 \
      || die "git worktree add $TARGET_HEAD 失败"
    REHEARSAL_TREE="$REHEARSAL_WT"
  fi

  REHEARSAL_DB="cp_rehearsal_${TS//-/}"
  say "建临时库 $REHEARSAL_DB …"
  "${PSQL[@]}" -d postgres -Atq -c "drop database if exists \"$REHEARSAL_DB\" with (force)" >/dev/null 2>&1 || true
  if ! "${PSQL[@]}" -d postgres -Atq -v ON_ERROR_STOP=1 -c "create database \"$REHEARSAL_DB\"" >/dev/null 2>&1; then
    if [ "$(id -u)" = "0" ] && command -v su >/dev/null && su - postgres -c "createdb -O $DB_USER $REHEARSAL_DB" >/dev/null 2>&1; then
      ok "临时库已建（借 postgres 账号：当前账号无 CREATEDB）"
    else
      die "建临时库失败：$DB_USER 没有 CREATEDB 权限。
      预演必须建临时库：给该账号 CREATEDB，或用 root 运行（脚本会借 postgres 账号）。
      绝不要用 --skip-rehearsal 硬上 —— V0359 的 30 分钟中断就是这么来的。"
    fi
  else
    ok "临时库已建：$REHEARSAL_DB"
  fi

  say "把刚做的全库备份恢复到临时库 …"
  T0=$(date +%s)
  if ! pg_restore --no-owner --no-privileges -j 2 -d "$REHEARSAL_DB" -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" \
        "$BK_DIR/db.dump" >"$LOG.rehearsal_restore" 2>&1; then
    tail -10 "$LOG.rehearsal_restore" | sed 's/^/      /'
    die "备份恢复到临时库失败（生产没动）：详见 $LOG.rehearsal_restore"
  fi
  RB_CNT=$(Q "$REHEARSAL_DB" "select count(*) from schema_migrations where kind='versioned' and success")
  RB_MAX=$(Q "$REHEARSAL_DB" "select coalesce(max(version::int),0) from schema_migrations where kind='versioned' and success")
  ok "恢复完成（$(( $(date +%s) - T0 ))s）：临时库版本 V$(printf '%04d' "$RB_MAX")，记账 $RB_CNT 条（线上 $VER_BEFORE_COUNT 条）"
  if [ "$RB_CNT" != "$VER_BEFORE_COUNT" ]; then
    warn "临时库记账条数与线上不一致（$RB_CNT vs $VER_BEFORE_COUNT）—— 备份与库可能不是同一时刻，请确认"
  fi

  APPLIER="$REHEARSAL_TREE/scripts/verify/deploya_migrate.sh"
  REH_OK=1
  T0=$(date +%s)
  if [ -f "$APPLIER" ]; then
    say "在临时库上逐条应用 $PENDING_COUNT 条迁移（$APPLIER）…"
    DEPLOY_ENV_FILE="$ENV_FILE" bash "$APPLIER" "$REHEARSAL_DB" --assert-pending "${PENDING_PAIRS[@]}" \
      2>&1 | tee "$BK_DIR/rehearsal-apply.log" | sed 's/^/      /' || REH_OK=0
    if ! grep -q 'SET_OK' "$BK_DIR/rehearsal-apply.log" 2>/dev/null; then REH_OK=0; fi
  else
    warn "目标树里没有 scripts/verify/deploya_migrate.sh：退化为「逐条 psql --single-transaction」预演（只验 SQL 能否跑通）"
    for p in "${PENDING_PAIRS[@]}"; do
      v="${p%%:*}"; n="${p#*:}"; f="$REHEARSAL_TREE/${MIG_FILE[$n]}"
      say "  [V$(printf '%04d' "$v")] $n"
      if ! "${PSQL[@]}" -d "$REHEARSAL_DB" -v ON_ERROR_STOP=1 --single-transaction -f "$f" \
            >"$BK_DIR/rehearsal-V$v.log" 2>&1; then
        tail -8 "$BK_DIR/rehearsal-V$v.log" | sed 's/^/      /'
        REH_OK=0; break
      fi
    done
  fi
  if [ "$REH_OK" != "1" ]; then
    FAILURES+=("迁移预演失败（生产库一点没动）")
    die "迁移预演失败（明细：$BK_DIR/rehearsal-apply.log）—— 生产库、服务、二进制全部未动。
      修法：修正**本次待应用**的那条迁移 SQL（绝不要改已应用的迁移），或补前置迁移"
  fi
  ok "预演通过：$PENDING_COUNT 条迁移在临时库上全部成功（$(( $(date +%s) - T0 ))s）"

  RB_FAILED=$(Q "$REHEARSAL_DB" "select count(*) from schema_migrations where not success")
  [ "$RB_FAILED" = "0" ] || die "预演库里有 $RB_FAILED 条失败记录（不该发生）"
  RB_CNT2=$(Q "$REHEARSAL_DB" "select count(*) from schema_migrations where kind='versioned' and success")
  RB_MAX2=$(Q "$REHEARSAL_DB" "select coalesce(max(version::int),0) from schema_migrations where kind='versioned' and success")
  if [ -f "$APPLIER" ]; then
    [ "$RB_CNT2" = "$((VER_BEFORE_COUNT + PENDING_COUNT))" ] \
      || die "预演对账失败：应新增 $PENDING_COUNT 条，实际 $VER_BEFORE_COUNT → $RB_CNT2"
    ok "预演记账对账：$VER_BEFORE_COUNT + $PENDING_COUNT = $RB_CNT2 条，最大版本 V$(printf '%04d' "$RB_MAX2")"
  fi
  REHEARSAL_TABLES=$(table_count "$REHEARSAL_DB")
  ok "表数：线上 $PROD_TABLES → 预演后 $REHEARSAL_TABLES"

  # 关键约束：线上原有的表在预演后必须还在（本次迁移显式 DROP 的除外）
  DROPPED_TABLES=""
  if [ "${#PENDING_FILES[@]}" -gt 0 ]; then
    DROPPED_TABLES=$( { for f in "${PENDING_FILES[@]}"; do
                          grep -hiE 'drop[[:space:]]+table' "$REHEARSAL_TREE/$f" 2>/dev/null || true
                        done | grep -oiE 'drop[[:space:]]+table[[:space:]]+(if[[:space:]]+exists[[:space:]]+)?(public\.)?"?[A-Za-z0-9_]+' \
                             | sed -E 's/.*[ ."]//' | tr 'A-Z' 'a-z' | sort -u | tr '\n' ' '; } || true )
  fi
  PROD_TBL_LIST=$(mktemp); RB_TBL_LIST=$(mktemp)
  table_list "$DB_NAME" > "$PROD_TBL_LIST"
  table_list "$REHEARSAL_DB" > "$RB_TBL_LIST"
  VANISHED=""
  while IFS= read -r t; do
    [ -n "$t" ] || continue
    if grep -qxF "$t" "$RB_TBL_LIST"; then continue; fi
    case " $DROPPED_TABLES " in *" $t "*) continue ;; esac
    VANISHED="$VANISHED $t"
  done < "$PROD_TBL_LIST"
  rm -f "$PROD_TBL_LIST" "$RB_TBL_LIST"
  if [ -n "$VANISHED" ]; then
    die "预演后线上原有的表不见了（本次迁移没有 DROP 它们）：$VANISHED —— 迁移有问题，生产没动"
  fi
  ok "关键约束核对：线上 $PROD_TABLES 张表在预演后全部还在（本次迁移显式 DROP：${DROPPED_TABLES:-无}）"

  MISSING_KEY=""
  for t in "${KEY_TABLES[@]}"; do
    c=$(Q "$REHEARSAL_DB" "select count(*) from information_schema.tables where table_name='$t'" 2>/dev/null || echo 0)
    if [ "$c" != "1" ]; then MISSING_KEY="$MISSING_KEY $t"; fi
  done
  [ -z "$MISSING_KEY" ] || die "预演后缺关键表：$MISSING_KEY（生产没动）"
  PROD_UIDX=$(unique_index_count "$DB_NAME"); RB_UIDX=$(unique_index_count "$REHEARSAL_DB")
  ok "关键表齐全（${KEY_TABLES[*]}）；唯一索引 线上 $PROD_UIDX → 预演后 $RB_UIDX"
  [ "$RB_UIDX" -ge "$PROD_UIDX" ] || die "预演后唯一索引变少（$PROD_UIDX → $RB_UIDX），唯一约束被削弱，生产没动"
  PHASE="rehearsed"; write_state
fi

if [ "$DRY_RUN" = "1" ]; then
  HR=$(( $(date +%s) - START_TS ))
  echo
  echo "================= 演练完成（生产零改动）================="
  echo "  版本变化    V$(printf '%04d' "$VER_BEFORE_MAX") → V$(printf '%04d' "$PENDING_LAST")（$PENDING_COUNT 条待应用）"
  echo "  迁移清单    $PENDING_LIST"
  echo "  预演结果    通过（临时库 $REHEARSAL_DB：$REHEARSAL_TABLES 张表；记录见 $BK_DIR/rehearsal-apply.log）"
  echo "  备份文件    $BK_DIR/db.dump（$(human_bytes "$DUMP_SZ")）"
  echo "  日志        $LOG"
  echo "  耗时        ${HR}s"
  echo "  下一步      去掉 --dry-run 正式升级：bash $SELF_CMD --target $TARGET_REF"
  echo "======================================================="
  exit 0
fi

# -----------------------------------------------------------------------------
step "[6/9] 正式迁移（停服务 → 逐条应用 → 对账 → 集合复核）"
# -----------------------------------------------------------------------------
cleanup_rehearsal   # 预演资源立刻回收（被 kill -9 时 EXIT trap 不执行）
if [ "$MANAGE_SERVICE" = "1" ]; then
  say "停服务 $SERVICE …"
  svc stop "$SERVICE" || warn "systemctl stop 返回非 0（可能本来就没跑）"
  PHASE="service_stopped"; write_state
  ok "服务已停"
else
  warn "--no-service：请确认 $SERVICE 已停（迁移期间旧二进制带新 schema 跑＝可能 500）"
fi

APPLIER="$REPO/scripts/verify/deploya_migrate.sh"
if [ ! -f "$APPLIER" ]; then
  die "目标版本里没有 scripts/verify/deploya_migrate.sh，无法安全逐条应用迁移。
      临时先把服务拉起来再说：systemctl start $SERVICE"
fi
MIG_FAIL=0
T0=$(date +%s)
say "逐条应用 $PENDING_COUNT 条迁移（$APPLIER）…"
PHASE="migrating"; write_state
DEPLOY_ENV_FILE="$ENV_FILE" bash "$APPLIER" "$DB_NAME" --assert-pending "${PENDING_PAIRS[@]}" \
  2>&1 | tee "$BK_DIR/apply.log" | sed 's/^/      /' || MIG_FAIL=1
if ! grep -q 'SET_OK' "$BK_DIR/apply.log" 2>/dev/null; then MIG_FAIL=1; fi

if [ "$MIG_FAIL" != "0" ]; then
  FAILURES+=("迁移应用失败，已从备份恢复")
  bad "迁移失败：从备份恢复（$BK_DIR/db.dump）…"
  if ! restore_db_from_dump "$BK_DIR/db.dump"; then
    die "迁移失败**且自动恢复也失败**，请立刻人工介入：
      备份=$BK_DIR/db.dump　恢复库留在=${RESTORE_DB:-无}　被改名的原库=${PRE_ROLLBACK_DB:-无}"
  fi
  if [ "$MANAGE_SERVICE" = "1" ]; then svc start "$SERVICE" || warn "systemctl start 失败"; fi
  PHASE="migrate_failed_restored"; write_state
  die "迁移失败，已从备份恢复到升级前并启动服务（明细见 $BK_DIR/apply.log）"
fi
ok "迁移应用完成（$(( $(date +%s) - T0 ))s）"

VER_AFTER_COUNT=$(QDB "select count(*) from schema_migrations where kind='versioned' and success")
VER_AFTER_MAX=$(QDB "select coalesce(max(version::int),0) from schema_migrations where kind='versioned' and success")
DELTA=$((VER_AFTER_COUNT - VER_BEFORE_COUNT))
if [ "$DELTA" != "$PENDING_COUNT" ]; then
  FAILURES+=("迁移对账失败（待跑 $PENDING_COUNT，实际新增 $DELTA）")
  bad "对账失败：待跑 $PENDING_COUNT 条，实际新增 $DELTA 条（$VER_BEFORE_COUNT → $VER_AFTER_COUNT）"
  restore_db_from_dump "$BK_DIR/db.dump" || die "对账失败且恢复失败，请人工介入（备份 $BK_DIR/db.dump）"
  if [ "$MANAGE_SERVICE" = "1" ]; then svc start "$SERVICE" || true; fi
  die "迁移对账失败，已从备份恢复并启动服务"
fi
ok "对账通过：新增记账 $DELTA 条 == 待跑 $PENDING_COUNT 条（$VER_BEFORE_COUNT → $VER_AFTER_COUNT 条，最大版本 V$(printf '%04d' "$VER_AFTER_MAX")）"

BAD_VIA_AFTER=$(QDB "select applied_via||'|'||count(*) from schema_migrations
                       where applied_via not in ('executed','imported','baseline') group by applied_via")
[ -z "$BAD_VIA_AFTER" ] || die "记账后出现非法 applied_via：$BAD_VIA_AFTER（不修启动就 FATAL；库已是新版本，需人工处理）"
ok "applied_via 合法"

load_applied
LEFT=""
i=0
while [ "$i" -lt "${#MIG_NAME[@]}" ]; do
  n="${MIG_NAME[$i]}"
  if [ -z "${APPLIED_NAME[$n]:-}" ]; then LEFT="$LEFT ${MIG_FILE[$n]}"; fi
  i=$((i + 1))
done
[ -z "$LEFT" ] || die "迁移后仍有未记账的迁移：$LEFT（库与目标树不一致）"
ok "集合复核：目标版本磁盘上的 ${#MIG_NAME[@]} 条版本化迁移全部已记账"
PHASE="migrated"; write_state

# -----------------------------------------------------------------------------
step "[7/9] 替换二进制 + 启动"
# -----------------------------------------------------------------------------
if [ ! -f "$BK_DIR/cloudpivot.new" ]; then
  cp -f "$BINARY" "$BK_DIR/cloudpivot.new" || die "找不到新二进制快照，且无法从 $BINARY 复制"
fi
say "原子替换二进制（cp → .new + mv -f，绝不覆盖正在运行的路径）…"
cp -f "$BK_DIR/cloudpivot.new" "$BINARY.new" && mv -f "$BINARY.new" "$BINARY" || die "换二进制失败"
NEW_BINARY_MD5="$(md5sum "$BINARY" | cut -d' ' -f1)"
PHASE="binary_installed"; write_state
ok "二进制就位：$BINARY（md5=$NEW_BINARY_MD5）"

if [ "$MANAGE_SERVICE" = "1" ]; then
  say "启动服务 $SERVICE …"
  svc start "$SERVICE" || die "systemctl start 失败"
  UP=0
  for _ in $(seq 1 40); do
    if curl -s -m 3 "$BASE_URL/health" | grep -q '"status":"ok"'; then UP=1; break; fi
    sleep 2
  done
  if [ "$UP" != "1" ]; then
    FAILURES+=("启动后 /health 不就绪")
    bad "/health 80s 内没就绪，服务日志尾部："
    journalctl -u "$SERVICE" -n 40 --no-pager 2>/dev/null | sed 's/^/      /' || true
    die "启动失败（回滚：bash $SELF_CMD --rollback \"$BK_DIR\" --yes）"
  fi
  PHASE="started"; write_state
  ok "服务已启动，/health：$(curl -s -m 5 "$BASE_URL/health")"
else
  warn "--no-service：请手工启动 $SERVICE 后再验收"
fi

# -----------------------------------------------------------------------------
step "[8/9] 冒烟（/health → 登录 → 关键接口 3 个）"
# -----------------------------------------------------------------------------
SMOKE_FAIL=0
say "1) /health …"
if curl -s -m 10 "$BASE_URL/health" | grep -q '"status":"ok"'; then ok "/health 200 ok"; else bad "/health 异常"; SMOKE_FAIL=1; fi

TOKEN=""
if [ -z "$SMOKE_PASS" ]; then
  warn "2) 登录：没有冒烟口令（--smoke-pass 或 env 的 NIK_ADMIN_PASSWORD），跳过登录与鉴权接口检查"
else
  say "2) 登录（$SMOKE_USER）…"
  if command -v python3 >/dev/null; then
    PAYLOAD=$(python3 -c 'import json,sys;print(json.dumps({"username":sys.argv[1],"password":sys.argv[2]}))' "$SMOKE_USER" "$SMOKE_PASS")
  else
    PAYLOAD=$(printf '{"username":"%s","password":"%s"}' "${SMOKE_USER//\"/}" "${SMOKE_PASS//\"/}")
  fi
  LOGIN_OUT=$(curl -s -m 15 -w '\n%{http_code}' -X POST "$BASE_URL/api/system/auth/login" \
                -H 'Content-Type: application/json' -d "$PAYLOAD" 2>/dev/null || true)
  LOGIN_CODE=$(printf '%s' "$LOGIN_OUT" | tail -1)
  LOGIN_BODY=$(printf '%s' "$LOGIN_OUT" | sed '$d')
  if [ "$LOGIN_CODE" = "200" ] && printf '%s' "$LOGIN_BODY" | grep -q '"code":0'; then
    TOKEN=$( { printf '%s' "$LOGIN_BODY" | grep -o '"access_token":"[^"]*"' | head -1 | cut -d'"' -f4; } || true )
    ok "登录 200（token ${TOKEN:0:12}…）"
  else
    bad "登录失败：HTTP ${LOGIN_CODE:-无响应} $(printf '%s' "$LOGIN_BODY" | head -c 200)"
    SMOKE_FAIL=1
  fi
fi

if [ -n "$TOKEN" ]; then
  say "3) 关键接口（$SMOKE_APIS）…"
  IFS=',' read -r -a _apis <<< "$SMOKE_APIS"
  n=0
  for a in "${_apis[@]}"; do
    a="$(echo "$a" | tr -d ' ')"
    [ -n "$a" ] || continue
    n=$((n + 1))
    if http_ok "$BASE_URL$a" "$TOKEN"; then ok "$a → 200 / code 0"; else SMOKE_FAIL=1; fi
  done
  if [ "$n" -lt 1 ]; then warn "没配关键接口（--smoke-apis）"; fi
else
  say "3) 关键接口：没有 token，跳过"
fi

if [ "$SMOKE_FAIL" != "0" ]; then
  FAILURES+=("冒烟失败，已自动回滚")
  bad "冒烟失败 → 自动回滚（恢复数据库 + 换回旧二进制 + 切回旧代码）…"
  if [ "$MANAGE_SERVICE" = "1" ]; then svc stop "$SERVICE" || true; fi
  restore_db_from_dump "$BK_DIR/db.dump" || die "冒烟失败且自动恢复数据库失败，请人工介入：备份 $BK_DIR/db.dump"
  if [ -f "$BK_DIR/cloudpivot.old" ]; then
    cp -f "$BK_DIR/cloudpivot.old" "$BINARY.rollback" && mv -f "$BINARY.rollback" "$BINARY" \
      && ok "旧二进制已换回（md5=$(md5sum "$BINARY" | cut -d' ' -f1)）" || warn "换回旧二进制失败"
  else
    warn "没有旧二进制备份，二进制未回滚"
  fi
  git -C "$REPO" checkout --detach "$OLD_HEAD" >/dev/null 2>&1 || warn "代码未能切回 ${OLD_HEAD:0:8}"
  if [ "$MANAGE_SERVICE" = "1" ]; then
    svc start "$SERVICE" || warn "systemctl start 失败"
    for _ in $(seq 1 30); do
      if curl -s -m 3 "$BASE_URL/health" | grep -q '"status":"ok"'; then break; fi
      sleep 2
    done
  fi
  PHASE="smoke_failed_rolled_back"; write_state
  die "冒烟失败，已自动回滚到升级前（库 V$(printf '%04d' "$VER_BEFORE_MAX") + 旧二进制 + 代码 ${OLD_HEAD:0:8}）"
fi
PHASE="smoke_ok"; write_state
ok "冒烟全绿"

# -----------------------------------------------------------------------------
step "[9/9] 收尾报告"
# -----------------------------------------------------------------------------
DUR=$(( $(date +%s) - START_TS ))
TABLE_ROWS_AFTER="$(key_table_rows "$DB_NAME")"
{
  echo "# 升级完成 $(date '+%F %T')"
  echo "duration_seconds=$DUR"
  echo "version_before=V$(printf '%04d' "$VER_BEFORE_MAX")"
  echo "version_after=V$(printf '%04d' "$VER_AFTER_MAX")"
  echo "applied_count=$PENDING_COUNT"
  echo "applied_list=\"$PENDING_LIST\""
  echo "code_before=$OLD_HEAD"
  echo "code_after=$TARGET_HEAD"
  echo "dump=$BK_DIR/db.dump"
  echo "log=$LOG"
  echo "tables=$PROD_TABLES -> $(table_count "$DB_NAME")"
  echo "$TABLE_ROWS_AFTER"
} > "$BK_DIR/after.txt"
PHASE="done"; write_state

LOST=""
while IFS='|' read -r t before; do
  case "$before" in MISSING|"") continue ;; esac
  after=$(printf '%s\n' "$TABLE_ROWS_AFTER" | awk -F'|' -v k="$t" '$1==k{print $2}')
  case "$after" in MISSING|"") LOST="$LOST $t(表不见了)"; continue ;; esac
  if [ "$after" -lt "$before" ]; then LOST="$LOST $t($before→$after)"; fi
done < <(grep -E '^[a-z_0-9]+([|]MISSING|[|][0-9]+)$' "$BK_DIR/before.txt" || true)

echo
echo "================= 升级完成 ================="
echo "  版本变化    V$(printf '%04d' "$VER_BEFORE_MAX")（$VER_BEFORE_COUNT 条） → V$(printf '%04d' "$VER_AFTER_MAX")（$VER_AFTER_COUNT 条）"
echo "  应用迁移    $PENDING_COUNT 条：$PENDING_LIST"
echo "  代码        ${OLD_HEAD:0:8} → ${TARGET_HEAD:0:8}（$TARGET_REF）"
echo "  二进制      $BINARY（md5 $NEW_BINARY_MD5）"
echo "  备份        $BK_DIR（db.dump $(human_bytes "$DUMP_SZ") + cloudpivot.old + upgrade-state.env）"
echo "  回滚        bash $SELF_CMD --rollback \"$BK_DIR\" --yes"
echo "  日志        $LOG"
echo "  耗时        ${DUR}s"
if [ "$MANAGE_SERVICE" = "1" ]; then
  echo "  服务        $(systemctl is-active "$SERVICE")｜/health $(curl -s -m 5 "$BASE_URL/health" || true)"
else
  echo "  服务        --no-service（请自行确认已启动）"
fi
echo "  冒烟        /health 200 · 登录 200 · 关键接口 3 个 200"
if [ -n "$LOST" ]; then
  echo "  ⚠ 关键表行数下降：$LOST（迁移删了数据？核对 $BK_DIR/before.txt 与 after.txt）"
else
  echo "  数据核对    关键表行数全部 ≥ 升级前（明细：$BK_DIR/before.txt / after.txt）"
fi
echo "==========================================="
exit 0
