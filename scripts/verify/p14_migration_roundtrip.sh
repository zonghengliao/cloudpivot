#!/usr/bin/env bash
# P1-4：V0212 单据附件迁移 up / down 三轮往返（空库，零残留）。
#
#   bash scripts/verify/p14_migration_roundtrip.sh
#
# 口径：
#   · 库 cp_p14_mig 由本脚本 createdb（**owner 必须是应用账号 alun**）；
#   · 直接 psql -f 回放 up/down（与应用账号同口径，表 owner 才是 alun）；
#   · 每轮回放后核对：表 / 索引 / 约束 / 3 个权限节点 / 角色授权。
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

ROOT=${REPO:-/opt/cloudpivot}
DB=${P14_MIG_DB:-cp_p14_mig}
URL="postgres://alun:alun@127.0.0.1:5432/$DB"
UP="$ROOT/backend/migrations/business/V0212__erp_doc_attachment.up.sql"
DOWN="$ROOT/backend/migrations/business/V0212__erp_doc_attachment.down.sql"

echo "== 建空库 $DB =="
su - postgres -c "dropdb --if-exists $DB" >/dev/null 2>&1
su - postgres -c "createdb -O alun $DB" || { echo "createdb 失败"; exit 1; }

# 迁移只依赖 sys_file / sys_user / sys_menu / sys_role / sys_role_menu，先补齐这三张底座表
echo "== 先跑一次完整启动链把底座迁移都应用上（临时实例，端口 18097）=="
P14_DB="$DB" P14_PORT=18097 P14_LOG=/root/p14-mig-server.log \
  P14_PIDFILE=/root/p14-mig-server.pid bash "$ROOT/scripts/verify/p14_instance.sh" start || exit 1
P14_DB="$DB" P14_PORT=18097 P14_PIDFILE=/root/p14-mig-server.pid \
  bash "$ROOT/scripts/verify/p14_instance.sh" stop

# 启动链已经把 V0212 应用过一次 → 先 down 回初始态，再开始三轮
q() { psql "$URL" -Atc "$1"; }

echo "== 回放到初始态（down）=="
psql "$URL" -q -f "$DOWN" 2>&1 | grep -v '^$' || true

check_state() {
  local phase="$1"
  local tbl idx ck menu grant_seq
  tbl=$(q "select count(*) from information_schema.tables where table_schema='public' and table_name='biz_doc_attachment'")
  idx=$(q "select count(*) from pg_indexes where tablename='biz_doc_attachment' and indexname in ('idx_biz_doc_attachment_doc','idx_biz_doc_attachment_file','uk_biz_doc_attachment')")
  ck=$(q "select count(*) from pg_constraint where conname in ('ck_biz_doc_attachment_type','ck_biz_doc_attachment_role')")
  menu=$(q "select count(*) from sys_menu where id in ('20000000-0000-0000-0000-000000001221'::uuid,'20000000-0000-0000-0000-000000001222'::uuid,'20000000-0000-0000-0000-000000001223'::uuid)")
  grant_seq=$(q "select count(*) from sys_role_menu where menu_id in ('20000000-0000-0000-0000-000000001221'::uuid,'20000000-0000-0000-0000-000000001222'::uuid,'20000000-0000-0000-0000-000000001223'::uuid)")
  echo "[$phase] 表=$tbl 索引=$idx 约束=$ck 权限节点=$menu 授权行=$grant_seq"
  if [ "$phase" = "up" ]; then
    [ "$tbl" = "1" ] && [ "$idx" = "3" ] && [ "$ck" = "2" ] && [ "$menu" = "3" ] \
      || { echo "  ✗ up 后状态不符（应 1/3/2/3）"; return 1; }
  else
    [ "$tbl" = "0" ] && [ "$menu" = "0" ] && [ "$grant_seq" = "0" ] \
      || { echo "  ✗ down 后仍有残留"; return 1; }
  fi
  return 0
}

fail=0
for round in 1 2 3; do
  echo "===== 第 $round 轮 ====="
  echo "-- up --"
  psql "$URL" -v ON_ERROR_STOP=1 -q -f "$UP" 2>&1 | grep -Ev '^$' || true
  psql "$URL" -Atc "select 1" >/dev/null || { echo "up 失败"; fail=1; break; }
  check_state "up" || fail=1
  echo "-- down --"
  psql "$URL" -v ON_ERROR_STOP=1 -q -f "$DOWN" 2>&1 | grep -Ev '^$' || true
  check_state "down" || fail=1
done

echo "== 终态：up 一次留作巡检库 =="
psql "$URL" -v ON_ERROR_STOP=1 -q -f "$UP" >/dev/null 2>&1
check_state "up"

if [ "$fail" = "0" ]; then
  echo "结论：V0212 up/down 三轮往返全部通过，零残留"
else
  echo "结论：存在失败，见上"
fi
exit "$fail"
