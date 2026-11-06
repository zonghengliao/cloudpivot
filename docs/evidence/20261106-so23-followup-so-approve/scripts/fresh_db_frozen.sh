#!/usr/bin/env bash
# 干净基线库（可复现）v2：把二进制跑在「冻结的 migrations」上。
#
# 背景：共享工作区 /opt/cloudpivot 里有另一位 worker 正在写**未提交且当前是坏的**
# 迁移（SO-25 的 V0267__sales_menu：直接跑会 `query failed: db error` 整批回滚，
# 实例起不来）。`NIK_DATABASE_MIGRATION_PATH` 实测**不被识别**（config.toml 的
# `[database.migration].path` 只在配置文件里生效），所以这里造一个独立的运行目录：
#   run_env/config/config.toml  → path 改成冻结目录的绝对路径
#   run_env/migrations          → 冻结目录的软链（仅作兜底）
# 然后以 run_env 为 CWD 启动同一个二进制。
set -uo pipefail
WS=/root/.hermes/kanban/boards/dev/workspaces/t_1dadde81
FROZEN=$WS/migrations_frozen
RUN=$WS/run_env
DB=${SO23_FRESH_DB:-cloudpivot_so23fresh}
PORT=${SO23_FRESH_PORT:-18223}
LOG=/tmp/t_1dadde81_fresh.log
PIDF=/tmp/t_1dadde81_fresh.pid
export PGPASSWORD=alun

# ① 冻结迁移目录：已提交的 + 本卡 V0301；剔掉工作区里未提交的（别人的在途迁移）
rm -rf "$FROZEN"; cp -a /opt/cloudpivot/backend/migrations "$FROZEN"
cd /opt/cloudpivot || exit 1
for f in $(git status --porcelain -- backend/migrations | awk '{print $2}'); do
  rel=${f#backend/migrations/}
  case "$rel" in
    business/V0301__*) ;;   # 本卡的迁移：未提交但要进基线
    *) [ -e "$FROZEN/$rel" ] && rm -f "$FROZEN/$rel" && echo "  剔除未提交迁移：$rel" ;;
  esac
done
# V0267__sales_menu（SO-25 卡，f61cf8a）在**空库**上会失败：它的自检 6.1 要求 17 个销售
# 权限码必须已有 sys_menu 承载行，而那些菜单行由 V0282/V0286/V0291/V0294/V0297/V0300 才建，
# 号段比 267 大 ⇒ 空库上必然先跑 267、后建菜单行，直接 RAISE EXCEPTION（整批迁移回滚，
# 实例起不来）。这不是本卡的缺陷，但会挡住任何人的「空库 + 全量迁移」验收，所以基线里
# 先把它剔掉，并在 raw/v0267-fresh-blocker.txt 里留了实测证据。
rm -f "$FROZEN"/business/V0267__sales_menu.*.sql && echo "  剔除 V0267（空库上自检 6.1 必红，号段应为 ≥ V0302）"

# ② 运行目录：config 拷贝 + 迁移路径改绝对 + migrations 软链
rm -rf "$RUN"; mkdir -p "$RUN"
cp -a /opt/cloudpivot/backend/config "$RUN/config"
sed -i "s|^path = \"migrations\"|path = \"$FROZEN\"|" "$RUN/config/config.toml"
ln -s "$FROZEN" "$RUN/migrations"
grep -n -A3 '^\[database.migration\]' "$RUN/config/config.toml" | head -6
echo "冻结后的 up 迁移数：$(ls "$FROZEN"/*/V*.up.sql | wc -l)，其中本卡 V0301 = $(ls "$FROZEN"/business/V0301* | wc -l) 个文件"

su_psql() { su postgres -s /bin/sh -c "psql -v ON_ERROR_STOP=1 -At $*"; }
kill "$(cat "$PIDF" 2>/dev/null)" 2>/dev/null || true
sleep 2
cat > /tmp/_fresh_recreate.sql <<SQL
DROP DATABASE IF EXISTS $DB;
CREATE DATABASE $DB OWNER alun;
SQL
su_psql "-f -" < /tmp/_fresh_recreate.sql >/dev/null || exit 1
echo "✅ 空库 $DB 建好（owner=alun）"

cd "$RUN" || exit 1
NIK_PROFILE=test \
NIK_DATABASE_NAME="$DB" \
NIK_DATABASE_USER=alun \
NIK_DATABASE_PASSWORD=alun \
NIK_SERVER_LISTEN=127.0.0.1:$PORT \
NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
setsid nohup /opt/cloudpivot/backend/target/debug/cloudpivot > "$LOG" 2>&1 < /dev/null &
echo $! > "$PIDF"

code=""
for _ in $(seq 1 90); do
  code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 "http://127.0.0.1:$PORT/health" || true)
  [ "$code" = "200" ] && break
  kill -0 "$(cat "$PIDF")" 2>/dev/null || { echo "FATAL: 实例提前退出"; grep -E "ERROR|FATAL" "$LOG" | tail -5; exit 1; }
  sleep 2
done
echo "health=$code"
grep -E "数据库迁移完成" "$LOG" | tail -1
kill "$(cat "$PIDF")" 2>/dev/null; sleep 2; kill -9 "$(cat "$PIDF")" 2>/dev/null; rm -f "$PIDF"

psql -h 127.0.0.1 -U alun -d "$DB" -tA -F'|' \
  -c "select '迁移数=' || count(*) || '，applied_via 非 executed/imported=' ||
             (select count(*) from schema_migrations where applied_via not in ('executed','imported'))
        from schema_migrations" \
  -c "select 'so_approve: is_system=' || is_system::text || ' status=' || status
        from biz_flow_def where flow_code='so_approve'" \
  -c "select 'v1 hash=' || btrim(graph_hash) from biz_flow_def_version where id='20000000-0000-4000-8000-0000000d0282'" \
  -c "select 'V0267 是否入库=' || count(*) from schema_migrations where version='267'"
