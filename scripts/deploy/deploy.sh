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
#   bash scripts/deploy/deploy.sh --tag mylabel   # 指定备份/构建标签（默认时间戳）
#
# 选项：
#   --skip-migrate    不跑新迁移（默认：自动检测并逐条跑）
#   --skip-backend    不编译/不重启后端
#   --skip-frontend   不构建/不替换前端 dist
#   --dry-run         只打印计划
#   --tag <label>     备份与构建目录标签
#   -h|--help
#
# 环境变量：
#   DEPLOY_REPO        仓库路径，默认 /opt/cloudpivot
#   DEPLOY_ENV_FILE    生产环境变量文件，默认 /etc/cloudpivot/cloudpivot.env
#   DEPLOY_SERVICE     systemd 服务名，默认 cloudpivot
#   CARGO_BUILD_JOBS   默认 1（7GB 机器上高并发会 OOM）
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
MIG_DIR="$REPO/backend/migrations/business"
MIGRATE_TOOL="$REPO/scripts/verify/deploya_migrate.sh"
HEALTH_URL="http://127.0.0.1:8023/health"
WEB_URL="http://127.0.0.1/"
TAG="$(date +%Y%m%d-%H%M%S)"

SKIP_MIGRATE=0
SKIP_BACKEND=0
SKIP_FRONTEND=0
DRY_RUN=0

while [ $# -gt 0 ]; do
  case "$1" in
    --skip-migrate)  SKIP_MIGRATE=1 ;;
    --skip-backend)  SKIP_BACKEND=1 ;;
    --skip-frontend) SKIP_FRONTEND=1 ;;
    --dry-run)       DRY_RUN=1 ;;
    --tag)           TAG="${2:?--tag 需要值}"; shift ;;
    -h|--help)       sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "未知参数：$1（--help 看用法）" >&2; exit 2 ;;
  esac
  shift
done

LOG="/var/log/cloudpivot-deploy-$TAG.log"
BACKUP_DIR="/root/deploy-backup-$TAG"
exec > >(tee -a "$LOG") 2>&1

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
    echo "  迁移：$APPLIED_MIGS 已应用（迁移不可逆，回滚要跑对应 .down.sql，见 $MIG_DIR）"
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
step "[0/7] 前置检查"
[ -d "$REPO/.git" ] || die "找不到仓库 $REPO（本脚本要在生产机上跑）"
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
say "仓库 $REPO，库 $DB_NAME，服务 $SERVICE，标签 $TAG"
[ "$DRY_RUN" = "1" ] && say ">>> dry-run：只体检与打印计划，不写文件、不动服务 <<<"

# ----------------------------------------------------------------------------
step "[1/7] 查清待上线内容（git fetch + 差异清单）"
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
# 口径：**磁盘上（$NEW_HEAD 的树）版本号 > 库里已应用最大版本** 的 business up 脚本。
#
# 为什么不再用 `git diff --diff-filter=A $OLD_HEAD..$NEW_HEAD`：
# 那个口径只看「这次同步新加的迁移文件」。一旦主仓只同步了代码、迁移没对库跑过
# （实操里发生过：prod 工作区已到 V0217 的文件，库还停在 V0211），待跑迁移根本
# 不在 diff 里 ⇒ 脚本报「本次无新迁移」，库与二进制长期不一致，而 v1 检测脚本
# 又只看 git 落后数，两边都显示"已部署"。按「库 vs 磁盘」比才是生产是否真生效。
APPLIED_MAX=$($PSQL -c "select coalesce(max(version::int),0) from schema_migrations where kind='versioned' and version ~ '^[0-9]+\$'")
say "库里已应用的最大迁移版本：V$(printf '%04d' "$APPLIED_MAX")"

mapfile -t MIG_FILES < <(
  git ls-tree --name-only "$NEW_HEAD" backend/migrations/business/ \
    | grep -E '^backend/migrations/business/V[0-9]+__[A-Za-z0-9_]+\.up\.sql$' \
    | awk -F'/' -v applied="$APPLIED_MAX" '{ v=substr($NF,2); sub(/__.*/,"",v); if ((v+0) > applied) print }' \
    | sort || true
)

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

[ "$SKIP_BACKEND" = "1" ] && BE_CHANGED="" && say "（--skip-backend：忽略后端改动）"
[ "$SKIP_FRONTEND" = "1" ] && FE_CHANGED="" && say "（--skip-frontend：忽略前端改动）"
say "后端改动：$([ -n "$BE_CHANGED" ] && echo 有 || echo 无)　　前端改动：$([ -n "$FE_CHANGED" ] && echo 有 || echo 无)　　待应用迁移：${#MIG_FILES[@]} 个"
for f in "${MIG_FILES[@]:-}"; do [ -n "${f:-}" ] && say "    + $f ($(git show "$NEW_HEAD:$f" >/dev/null 2>&1 || echo '⚠ 不在工作区'))"; done

PENDING_PAIRS=()
PENDING_NUMS=""
for f in "${MIG_FILES[@]:-}"; do
  [ -n "${f:-}" ] || continue
  base="$(basename "$f")"                                   # V0207__erp_period_remind.up.sql
  ver="${base%%__*}"; ver="${ver#V}"; ver=$((10#$ver))       # 207
  [ -n "$ver" ] || continue
  name="$(echo "$base" | sed -E 's/^V[0-9]+__(.*)\.up\.sql$/\1/')"
  if [ "$ver" -le "$APPLIED_MAX" ]; then
    say "    跳过 V$(printf '%04d' "$ver")（库里已应用）"
    continue
  fi
  PENDING_PAIRS+=("$ver:$name")
  PENDING_NUMS="$PENDING_NUMS $ver"
done

# ----------------------------------------------------------------------------
step "[2/7] 备份（库 dump + 线上二进制 + 现有 dist）"
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
step "[3/7] 同步代码（git merge --ff-only）"
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
step "[4/7] 数据库迁移（逐条跑 + 按引擎口径记账）"
if [ "$SKIP_MIGRATE" = "1" ]; then
  say "--skip-migrate：跳过"
elif [ "${#PENDING_PAIRS[@]}" -eq 0 ]; then
  ok "本次无新迁移（已应用最大版本 V$(printf '%04d' "$APPLIED_MAX")）"
elif [ "$DRY_RUN" = "1" ]; then
  say "dry-run：将依次应用 ${#PENDING_PAIRS[@]} 条：${PENDING_PAIRS[*]}"
else
  bash "$MIGRATE_TOOL" "$DB_NAME" "${PENDING_PAIRS[@]}" || die "迁移应用失败（上面的 APPLY_FAILED 行）"
  DID_MIGRATE=1
  APPLIED_MIGS="${PENDING_PAIRS[*]}"
  NEW_MAX=$($PSQL -c "select coalesce(max(version::int),0) from schema_migrations where kind='versioned' and version ~ '^[0-9]+\$'")
  ok "迁移后最大版本：V$(printf '%04d' "$NEW_MAX")"
fi

# ----------------------------------------------------------------------------
step "[5/7] 后端（只在有后端改动时编译并重启）"
if [ -z "$BE_CHANGED" ]; then
  say "无后端改动，跳过编译与重启"
elif [ "$DRY_RUN" = "1" ]; then
  say "dry-run：将 cargo build --release（CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-1}）→ 换二进制 → systemctl restart $SERVICE → 轮询 $HEALTH_URL"
else
  cd "$BACKEND_DIR" || die "cd backend 失败"
  export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-1}"
  say "cargo build --release（jobs=$CARGO_BUILD_JOBS，日志 $LOG）…"
  cargo build --release || die "cargo build --release 失败"
  NEW_MD5=$(md5sum target/release/cloudpivot | cut -d' ' -f1)
  ok "新二进制 md5=$NEW_MD5  $(stat -c%s target/release/cloudpivot) 字节"

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

  NMIG=$(journalctl -u "$SERVICE" --since "-3min" --no-pager | grep -c "数据库迁移完成: 应用 0 个迁移" || true)
  [ "${NMIG:-0}" -ge 1 ] && ok "启动日志确认「应用 0 个迁移」（记账与引擎一致）" || say "⚠️  启动日志里没看到「应用 0 个迁移」，请人工核对：journalctl -u $SERVICE -n 60"
fi

# ----------------------------------------------------------------------------
step "[6/7] 前端（只在有前端改动时 typecheck + build，并比对入口指纹）"
if [ -z "$FE_CHANGED" ]; then
  say "无前端改动，跳过 dist 构建"
elif [ "$DRY_RUN" = "1" ]; then
  say "dry-run：将 pnpm typecheck → vite build --outDir dist-$TAG → 双 rename → curl $WEB_URL 比对入口 chunk"
else
  cd "$FRONTEND_DIR" || die "cd frontend 失败"
  say "pnpm typecheck …"
  pnpm typecheck || die "pnpm typecheck 有错（按规范前端 0 错才允许上线）"
  ok "typecheck 0 错"

  say "vite build → dist-$TAG …"
  pnpm exec vite build --outDir "dist-$TAG" --emptyOutDir || die "vite build 失败"
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
step "[7/7] 收口核对"
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
say "迁移最大版本       V$(printf '%04d' "$($PSQL -c "select coalesce(max(version::int),0) from schema_migrations where kind='versioned' and version ~ '^[0-9]+\$'")")"
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
