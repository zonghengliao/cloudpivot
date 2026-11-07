#!/usr/bin/env bash
# P0-03 回归用例一键跑：SSH 隧道 → 145 cloudpivot_dev 真库 → 跑调拨结算回归 → 清夹具残留
# 用法：bash 20_verify_test.sh
set -uo pipefail
cd "$(dirname "$0")"
REPO=/opt/cloudpivot
# 库口令不落仓库：从 145 的 env 文件现取（也可外部 export PG_PASS 覆盖）
: "${PG_PASS:=$(ssh root@172.18.10.145 'grep -m1 ^NIK_DATABASE_PASSWORD /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2')}"
DB=cloudpivot_dev

echo "== 1) 建 SSH 隧道（本地 15432 → 145 的 127.0.0.1:5432）=="
# 端口可能被上一次没退干净的隧道占着：先收掉再建，避免「Could not request local forwarding」
pkill -f 'ssh .*-L 15432:127.0.0.1:5432' >/dev/null 2>&1
sleep 1
ssh -o StrictHostKeyChecking=no -o ExitOnForwardFailure=yes -N -L 15432:127.0.0.1:5432 root@172.18.10.145 &
TUNNEL=$!
trap 'kill $TUNNEL 2>/dev/null' EXIT
sleep 3
if ! PGPASSWORD=$PG_PASS psql -h 127.0.0.1 -p 15432 -U cloudpivot -d $DB -X -tAc 'select 1' >/dev/null 2>&1; then
  echo "FAIL: 隧道/数据库连不上"
  exit 1
fi

echo "== 2) 跑回归用例 =="
export PATH=/root/.cargo/bin:$PATH
export ERP_DB_URL="postgres://cloudpivot:$PG_PASS@127.0.0.1:15432/$DB"
(cd "$REPO/backend" && cargo test --lib p003_settle_deducts_the_same_bucket_in_both_books 2>&1 | tail -6)

echo "== 3) 清测试夹具残留（WH-W3B-* 的库存账/库位账基线）=="
scp -q 15_purge_test_residue.sql root@172.18.10.145:/tmp/p003_purge.sql
ssh root@172.18.10.145 "export PGPASSWORD=$PG_PASS; psql -h 127.0.0.1 -U cloudpivot -d $DB -X -f /tmp/p003_purge.sql" | tail -8
