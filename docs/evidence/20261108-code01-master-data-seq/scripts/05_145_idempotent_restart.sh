#!/bin/bash
# CODE-01 幂等验证：再重启一次 145 实例，确认「应用 0 个迁移」+ 六条规则不重复
set -u
systemctl restart cloudpivot-test
for i in $(seq 1 30); do
  code=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8033/health 2>/dev/null)
  [ "$code" = "200" ] && break
  sleep 1
done
echo "== 重启后 health=$code"
journalctl -u cloudpivot-test --since '-2 min' --no-pager | grep -E '迁移|V0337' | tail -5
set -a
. /etc/cloudpivot/cloudpivot-dev.env
set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
echo "== 第二次启动后六类行数（应仍为 6）"
psql -h 127.0.0.1 -p "${NIK_DATABASE_PORT:-5432}" -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -Atc \
  "select count(*) from biz_seq where seq_type in ('SUP','CUS','MAT','WH','AC','ET') and deleted_at is null"
echo "== schema_migrations 里 version=337 的记录数（应仍为 1）"
psql -h 127.0.0.1 -p "${NIK_DATABASE_PORT:-5432}" -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -Atc \
  "select count(*) from schema_migrations where version='337'"
