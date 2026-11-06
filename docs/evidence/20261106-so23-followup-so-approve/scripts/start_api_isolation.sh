#!/usr/bin/env bash
# 验收 ③ 的隔离库：从 cloudpivot_so23fresh 克隆一份（不污染基线库），
# 只改 admin 的 must_change_password（首登强制改密会挡 API 探针）。
set -uo pipefail
DB=${1:-cloudpivot_tplprobe}
PORT=${2:-18224}
kill "$(cat /tmp/t_1dadde81_api.pid 2>/dev/null)" 2>/dev/null || true
sleep 2
export PGPASSWORD=alun
su_psql() { su postgres -s /bin/sh -c "psql -v ON_ERROR_STOP=1 -At $*"; }
su_psql "-c \"select pg_terminate_backend(pid) from pg_stat_activity where datname='$DB'\"" >/dev/null
cat > /tmp/_tplprobe.sql <<SQL
DROP DATABASE IF EXISTS $DB;
CREATE DATABASE $DB TEMPLATE cloudpivot_so23fresh OWNER alun;
SQL
su_psql "-f -" < /tmp/_tplprobe.sql >/dev/null || exit 1
psql -h 127.0.0.1 -U alun -d "$DB" -c \
  "UPDATE sys_user SET must_change_password = FALSE WHERE username = 'admin'" \
  -c "select username, must_change_password from sys_user where username='admin'"
# 共享工作区里别的 worker 正在改未提交的迁移文件（SO-25 的 V0267），
# 探针库按当前文件同步 checksum，否则 ChecksumMismatch 拒绝启动。
python3 /root/.hermes/kanban/boards/dev/workspaces/t_1dadde81/sync_probe_checksums.py "$DB"
cd /opt/cloudpivot/backend || exit 1
NIK_PROFILE=test NIK_DATABASE_NAME="$DB" NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
NIK_SERVER_LISTEN=127.0.0.1:$PORT NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
setsid nohup ./target/debug/cloudpivot > /tmp/t_1dadde81_api.log 2>&1 < /dev/null &
echo $! > /tmp/t_1dadde81_api.pid
for _ in $(seq 1 90); do
  code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 "http://127.0.0.1:$PORT/health" || true)
  [ "$code" = "200" ] && echo "health=200 pid=$(cat /tmp/t_1dadde81_api.pid) port=$PORT db=$DB" && exit 0
  kill -0 "$(cat /tmp/t_1dadde81_api.pid)" 2>/dev/null || { echo "FATAL: 实例提前退出"; tail -20 /tmp/t_1dadde81_api.log; exit 1; }
  sleep 2
done
echo "FATAL: health 超时"; tail -20 /tmp/t_1dadde81_api.log; exit 1
