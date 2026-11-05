#!/usr/bin/env bash
# t_3a641ac8 复现脚本：起隔离实例（后端 18191 + 前端 15191）后跑真机两轮
#
# 前置：/opt/cloudpivot 已 pnpm install；PostgreSQL 本机 5432；库 cloudpivot_st9 存在（父卡验收留下的隔离库）
# 说明：口令哈希只写进隔离库 cloudpivot_fix9b，绝不碰生产库
set -u
REPO=/opt/cloudpivot
DB=cloudpivot_fix9b
BIN=/root/fix9b-bin/cloudpivot
PASS="${FIX9B_PASSWORD:?需要 FIX9B_PASSWORD（本轮口令，勿入库文档）}"

# 1) 克隆隔离库
su - postgres -c "psql -qc \"select pg_terminate_backend(pid) from pg_stat_activity where datname='$DB'\"" >/dev/null 2>&1
su - postgres -c "dropdb --if-exists $DB"
su - postgres -c "createdb -O alun -T cloudpivot_st9 $DB"

# 2) 重置 admin / st_wh 口令哈希（哈希由 backend/examples/hash_password 生成，写进临时 SQL 后删除）
HASH=$(cd "$REPO/backend" && cargo run -q --example hash_password "$PASS" 2>/dev/null | tail -1)
printf "update sys_user set password='%s', must_change_password=false, is_locked=false, login_fail_count=0 where username in ('admin','st_wh');\n" "$HASH" > /tmp/fix9b_pw.sql
chmod 644 /tmp/fix9b_pw.sql
su - postgres -c "psql -q -d $DB -f /tmp/fix9b_pw.sql"

# 3) 起后端（当前工作树二进制）
cd "$REPO/backend" && cargo build --bin cloudpivot >/dev/null
mkdir -p /root/fix9b-bin && cp -f "$REPO/backend/target/debug/cloudpivot" "$BIN"
NIK_PROFILE=test NIK_DATABASE_NAME=$DB NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432 \
NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun NIK_SERVER_LISTEN=127.0.0.1:18191 \
NIK_ADMIN_PASSWORD="$PASS" NIK_MIDDLEWARE_AUTH_JWT_SECRET='fix9b-e2e-secret-key-0123456789abcdef' \
CLOUDPIVOT_STORAGE_MASTER_KEY='0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef' \
  nohup "$BIN" > /root/fix9b-instance.log 2>&1 &
for i in $(seq 1 40); do [ "$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:18191/health)" = 200 ] && break; sleep 2; done

# 4) 起前端
cd "$REPO/frontend" && BACKEND_ORIGIN=http://127.0.0.1:18191 nohup node node_modules/vite/bin/vite.js --port 15191 --host 127.0.0.1 > /root/fix9b-vite.log 2>&1 &
for i in $(seq 1 30); do [ "$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:15191/)" = 200 ] && break; sleep 2; done

# 5) 真机两轮 + 后端契约核对
cd "$REPO/frontend" && node "$(dirname "$0")/fix9b_e2e.cjs" round1 && node "$(dirname "$0")/fix9b_e2e.cjs" round2
python3 "$(dirname "$0")/fix9b_api_check.py"
