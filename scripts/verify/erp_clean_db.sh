#!/usr/bin/env bash
# ============================================================================
# erp_clean_db.sh —— 建一个「干净的 ERP 单测库」并把它迁到最新（一次实例启动）
#
# 为什么必须有这个脚本（TEST-FIX · t_6d815eda 的教训）：
#   同一个提交、同一条 `cargo test -j 1`，在两个「都自称干净」的库上跑出两种结果
#   （2087/0 vs 2030/57）。差别不在代码，而在**库里的种子/夹具状态**：
#   作废复核节点 `n_review` 的审批人来源是 `assignee_type = 'ADMIN'`（运行时展开
#   **全部超管**）+ `join_mode = AND`（会签）⇒ 库里多一个有 SUPER_ADMIN 角色的
#   夹具用户（例如 `payable_bridge` 的 p17_ap1/p17_ap2 没被清场），同一个流程就从
#   「1 人会签」变成「3 人会签」，只批一条待办的夹具立刻假红。
#
#   所以「干净库」的口径必须**唯一且可复现**：
#     ① `createdb -O <表 owner>`（owner 错 = 启动只打一行「数据库迁移失败」）；
#     ② 用同一个二进制起**一次**实例，让引擎把 `migrations/` 全部应用 + 完成 bootstrap；
#     ③ 打印 `schema_migrations` 最大版本 / 迁移条数 / 超管人数 —— 记进证据，
#        下次「同一提交两种结果」一眼就能比对是不是库状态不同。
#
# 用法：
#   bash scripts/verify/erp_clean_db.sh                       # 建 cloudpivot_erp_clean
#   ERP_DB=cloudpivot_x bash scripts/verify/erp_clean_db.sh    # 换库名
#   RECREATE=1 bash scripts/verify/erp_clean_db.sh             # 先删后建（推荐：全量单测前）
#   REPO=/root/t6d8-src BIN=/root/t6d8-target/debug/cloudpivot bash scripts/verify/erp_clean_db.sh
#
# 跑完之后：
#   export ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/<ERP_DB>'
#   cargo test -j 1
# ============================================================================
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

REPO=${REPO:-/opt/cloudpivot}
DB=${ERP_DB:-cloudpivot_erp_clean}
PORT=${PORT:-18096}
PGHOST=${PGHOST:-127.0.0.1}
PGPORT=${PGPORT:-5432}
DB_USER=${DB_USER:-alun}
DB_PASS=${DB_PASS:-alun}
OWNER=${OWNER:-}          # 留空 = 自动从既有库的 sys_menu 表 owner 推导
BIN=${BIN:-$REPO/backend/target/debug/cloudpivot}
LOG=${LOG:-/root/erp-clean-db.log}
PIDFILE=${PIDFILE:-/root/erp-clean-db.pid}

psql_q() { PGPASSWORD="$DB_PASS" psql -h "$PGHOST" -p "$PGPORT" -U "$DB_USER" -d "$1" -Atc "$2"; }

# ---------- 0) 并发提示 ----------
# 库名按卡取唯一值（`ERP_DB=cloudpivot_<卡号>`）：多个 agent 同时建库时，同名的
# dropdb/createdb 会互相打架。脚本不再额外加锁，靠「一卡一库名」的约定避免冲突。

# ---------- 1) owner 推导（role "cloudpivot" 在 99 上不存在，实际 owner 是 alun） ----------
if [ -z "$OWNER" ]; then
  for probe in cloudpivot_test cloudpivot_dev cloudpivot; do
    OWNER=$(psql_q "$probe" "select tableowner from pg_tables where tablename='sys_menu'" 2>/dev/null | head -1)
    [ -n "$OWNER" ] && break
  done
fi
if [ -z "$OWNER" ]; then
  echo "无法推导表 owner（探测了 cloudpivot_test / cloudpivot_dev / cloudpivot）" >&2
  echo "请显式传 OWNER=<库属主> 再跑" >&2
  exit 1
fi
echo "==> 表 owner = $OWNER"

if [ ! -x "$BIN" ]; then
  echo "二进制不存在：$BIN（先 cargo build 或传 BIN=…）" >&2
  exit 1
fi
if [ ! -d "$REPO/backend/migrations" ]; then
  echo "迁移目录不存在：$REPO/backend/migrations（REPO 指到仓根）" >&2
  exit 1
fi

# ---------- 2) 建库 ----------
if [ "${RECREATE:-0}" = "1" ] && psql_q postgres "select 1 from pg_database where datname='$DB'" | grep -q 1; then
  # `su - postgres` 是为了拿到 dropdb 权限；也能兜住「当前账号不是库属主」的机器
  su - postgres -c "dropdb --force $DB" && echo "==> dropped: $DB"
fi
if psql_q postgres "select 1 from pg_database where datname='$DB'" | grep -q 1; then
  echo "==> 已存在：$DB（要重建请加 RECREATE=1）"
else
  su - postgres -c "createdb -O $OWNER $DB" || { echo "createdb 失败" >&2; exit 1; }
  echo "==> created: $DB (owner=$OWNER)"
fi

# ---------- 3) 起一次实例：迁移 + bootstrap ----------
# profile=test 的安全守卫要求注入 JWT secret 与存储主密钥，缺一个就 FATAL（迁移一条都不跑）
export NIK_PROFILE=test
export NIK_DATABASE_NAME="$DB"
export NIK_DATABASE_HOST="$PGHOST"
export NIK_DATABASE_PORT="$PGPORT"
export NIK_DATABASE_USER="$DB_USER"
export NIK_DATABASE_PASSWORD="$DB_PASS"
export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(openssl rand -hex 32)"
export CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)"
export NIK_ADMIN_PASSWORD="${NIK_ADMIN_PASSWORD:-Admin@123456}"

: > "$LOG"
cd "$REPO/backend" || exit 1
# 必须 setsid：只 nohup 的话 ssh/终端会话一断实例就被带走
setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
echo $! > "$PIDFILE"
echo "==> 启动 pid=$(cat "$PIDFILE") 库=$DB 端口=$PORT 日志=$LOG（跑迁移 + bootstrap）"

ok=0
for _ in $(seq 1 180); do
  grep -q "数据库迁移完成" "$LOG" 2>/dev/null && { ok=1; break; }
  kill -0 "$(cat "$PIDFILE")" 2>/dev/null || break
  sleep 1
done
sleep 2   # 等 bootstrap（角色/菜单/种子用户）落库
kill "$(cat "$PIDFILE")" 2>/dev/null
sleep 1

echo "==> 启动日志尾部"
tail -5 "$LOG"
if [ "$ok" != "1" ]; then
  echo "!! 迁移没有完成（见 $LOG）" >&2
  exit 1
fi

# ---------- 4) 指纹：版本 / 迁移条数 / 超管人数（会签人数）/ 关键种子流程 ----------
echo "==> 库指纹"
psql_q "$DB" "select max(version::int), count(*) from schema_migrations" | sed 's/^/    schema_migrations(max|count) = /'
psql_q "$DB" "select count(distinct u.id) from sys_user u \
                join sys_user_role ur on ur.user_id = u.id join sys_role r on r.id = ur.role_id \
               where r.role_code = 'SUPER_ADMIN' and u.deleted_at is null" \
  | sed 's/^/    超管人数（= assignee_type=ADMIN 节点的会签人数） = /'
psql_q "$DB" "select d.flow_code || ' / ' || n.node_key || ' / ' || coalesce(n.assignee_type,'-') \
                     || ' / self_approval=' || coalesce(n.self_approval,'-') \
                     || ' / ' || coalesce(n.join_mode,'-') \
                from biz_flow_def d join biz_flow_def_version v on v.def_id = d.id and v.is_current \
                join biz_flow_node n on n.version_id = v.id and n.node_type = 'APPROVAL' \
               where d.deleted_at is null order by 1" | sed 's/^/    /'

echo "==> 完成。下一步："
echo "    export ERP_DB_URL='postgres://$DB_USER:$DB_PASS@$PGHOST:$PGPORT/$DB'"
echo "    cd $REPO/backend && cargo test -j 1"
