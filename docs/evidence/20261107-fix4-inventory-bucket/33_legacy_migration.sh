#!/usr/bin/env bash
# ============================================================================
# FIX-4（t_763677cd）145 真机验收 · 步骤 4：**存量数据**升级路径（V0324 在真实数据上跑）
#
# 库：cloudpivot_fix4legacy_*（= cloudpivot_dev 的完整拷贝，停在 V0323 之前）
# 口径：**不动共享的 cloudpivot_dev**（它的备份在 /root/backup/cloudpivot_dev_FIX4_*.dump）
#
# 覆盖卡面验收：
#   ④ 存量数据迁移后，库存总数不变（对账 0 差异）
#   ⑤ 全量对账：库存表 = 流水累计（用 (物料, 仓库) 聚合口径）
#   以及迁移幂等（up.sql 连跑两次结果不变）
# ============================================================================
set -uo pipefail
EV=/opt/cloudpivot/docs/evidence/20261107-fix4-inventory-bucket
RAW=$EV/raw
mkdir -p "$RAW"
DBL=$(cat /root/backup/.fix4_db_legacy)
export PGPASSWORD=$(awk -F= '/^NIK_DATABASE_PASSWORD/{print $2}' /etc/cloudpivot/cloudpivot-dev.env)

PASS=0
FAIL=0
ok()  { echo "PASS $*"; PASS=$((PASS + 1)); }
bad() { echo "FAIL $*"; FAIL=$((FAIL + 1)); }
chk() { if [ "$1" = "$2" ]; then ok "$3（$1）"; else bad "$3（期望 $2，实际 $1）"; fi }
sqlq() { psql -h 127.0.0.1 -U cloudpivot -d "$DBL" -X -tA -c "$1"; }

snap() { # 打印一份完整账目快照（供 before / after 对比）
  echo "cost_qty=$(sqlq "SELECT COALESCE(SUM(qty),0) FROM biz_inventory")"
  echo "cost_amount=$(sqlq "SELECT COALESCE(SUM(amount),0) FROM biz_inventory")"
  echo "loc_qty=$(sqlq "SELECT COALESCE(SUM(qty),0) FROM biz_inventory_location")"
  echo "null_bucket_rows=$(sqlq "SELECT COUNT(*) FROM biz_inventory_location WHERE location_id IS NULL")"
  echo "wh_with_default_location=$(sqlq "SELECT COUNT(DISTINCT warehouse_id) FROM biz_location WHERE is_default = 1 AND status = 1 AND deleted_at IS NULL")"
  echo "null_bucket_in_default_wh=$(sqlq "SELECT COUNT(*) FROM biz_inventory_location il WHERE il.location_id IS NULL AND EXISTS (SELECT 1 FROM biz_location l WHERE l.warehouse_id = il.warehouse_id AND l.is_default = 1 AND l.status = 1 AND l.deleted_at IS NULL)")"
  echo "two_books_broken=$(sqlq "WITH c AS (SELECT i.material_id AS m, i.warehouse_id AS w, SUM(i.qty) AS qty FROM biz_inventory i GROUP BY 1,2), l AS (SELECT il.material_id AS m, il.warehouse_id AS w, SUM(il.qty) AS qty FROM biz_inventory_location il GROUP BY 1,2) SELECT COUNT(*) FROM c FULL JOIN l ON c.m=l.m AND c.w=l.w WHERE COALESCE(c.qty,0) <> COALESCE(l.qty,0)")"
  echo "inv_vs_txn_broken=$(sqlq "WITH inv AS (SELECT material_id, warehouse_id, SUM(qty) AS qty FROM biz_inventory GROUP BY 1,2), txn AS (SELECT material_id, warehouse_id, SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END) AS qty FROM biz_inventory_txn GROUP BY 1,2) SELECT COUNT(*) FROM inv FULL JOIN txn USING (material_id, warehouse_id) WHERE COALESCE(inv.qty,0) <> COALESCE(txn.qty,0)")"
}

echo "===== 0. 迁移前快照（库 = $DBL）====="
sqlq "SELECT 'schema_version_max=' || COALESCE(MAX(version), '?') FROM schema_migrations" || true
snap | tee "$RAW/10_legacy_before.txt"

echo "===== 1. 用新二进制起实例 → 自动应用 V0324（真实数据升级路径）====="
ENVF=/etc/cloudpivot/cloudpivot-dev.env
set -a
# shellcheck disable=SC1090
. "$ENVF"
set +a
export NIK_DATABASE_NAME="$DBL"
export NIK_SERVER_LISTEN=127.0.0.1:8046
export NIK_SEED_DEMO_USERS=false
cd /opt/cloudpivot/backend
setsid nohup ./target/debug/cloudpivot-fix4 > /root/backup/fix4_legacy.log 2>&1 < /dev/null &
LEGACY_PID=$!
echo "pid=$LEGACY_PID"
READY=no
for _ in $(seq 1 90); do
  if [ "$(curl -s -m 2 -o /dev/null -w '%{http_code}' http://127.0.0.1:8046/health || true)" = "200" ]; then READY=yes; break; fi
  sleep 1
done
chk "$READY" "yes" "存量库实例启动成功（迁移已应用）"
grep -E "V0324|迁移|migration" /root/backup/fix4_legacy.log | tail -5 > "$RAW/10_legacy_migration.log" || true
kill "$LEGACY_PID" 2>/dev/null || true
sleep 1

echo "===== 2. 迁移后快照 ====="
snap | tee "$RAW/11_legacy_after.txt"

B=$(grep -E '^(cost_qty|cost_amount|loc_qty)=' "$RAW/10_legacy_before.txt")
A=$(grep -E '^(cost_qty|cost_amount|loc_qty)=' "$RAW/11_legacy_after.txt")
chk "$A" "$B" "④ 迁移前后「库存总数量 / 总金额 / 库位账合计」逐项相等"

chk "$(sqlq "SELECT COUNT(*) FROM biz_inventory_location il WHERE il.location_id IS NULL AND EXISTS (SELECT 1 FROM biz_location l WHERE l.warehouse_id = il.warehouse_id AND l.is_default = 1 AND l.status = 1 AND l.deleted_at IS NULL)")" "0" \
  "④ 有默认库位的仓库里不再有「未分配桶」库位账行"
chk "$(sqlq "WITH c AS (SELECT i.material_id AS m, i.warehouse_id AS w, SUM(i.qty) AS qty FROM biz_inventory i GROUP BY 1,2), l AS (SELECT il.material_id AS m, il.warehouse_id AS w, SUM(il.qty) AS qty FROM biz_inventory_location il GROUP BY 1,2) SELECT COUNT(*) FROM c FULL JOIN l ON c.m=l.m AND c.w=l.w WHERE COALESCE(c.qty,0) <> COALESCE(l.qty,0)")" "0" \
  "④ 迁移后逐 (物料, 仓库) 两本账 0 差异"
chk "$(sqlq "WITH inv AS (SELECT material_id, warehouse_id, SUM(qty) AS qty FROM biz_inventory GROUP BY 1,2), txn AS (SELECT material_id, warehouse_id, SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END) AS qty FROM biz_inventory_txn GROUP BY 1,2) SELECT COUNT(*) FROM inv FULL JOIN txn USING (material_id, warehouse_id) WHERE COALESCE(inv.qty,0) <> COALESCE(txn.qty,0)")" "0" \
  "⑤ 全量对账 0 差异（库存表 = 流水累计，(物料,仓库) 聚合口径）"

echo "===== 3. 迁移幂等：up.sql 连跑两次 ====="
BEFORE2=$(snap)
psql -h 127.0.0.1 -U cloudpivot -d "$DBL" -X -v ON_ERROR_STOP=1 \
  -f /opt/cloudpivot/backend/migrations/business/V0324__inventory_default_bucket.up.sql \
  > "$RAW/12_legacy_rerun1.log" 2>&1
chk "$?" "0" "④ up.sql 第 2 次执行无报错"
chk "$(grep -o '并入 [0-9]* 个' "$RAW/12_legacy_rerun1.log" | head -1)" "并入 0 个" "④ 第 2 次执行没有桶需要并入（幂等）"
psql -h 127.0.0.1 -U cloudpivot -d "$DBL" -X -v ON_ERROR_STOP=1 \
  -f /opt/cloudpivot/backend/migrations/business/V0324__inventory_default_bucket.up.sql \
  > "$RAW/12_legacy_rerun2.log" 2>&1
chk "$?" "0" "④ up.sql 第 3 次执行无报错"
AFTER2=$(snap)
chk "$AFTER2" "$BEFORE2" "④ 连跑两次后账目快照与第 1 次之后完全一致"

echo "===== 4. 重跑 P0-03 的对账脚本 13_final_reconcile.sql ====="
psql -h 127.0.0.1 -U cloudpivot -d "$DBL" -X -v ON_ERROR_STOP=1 \
  -f /opt/cloudpivot/docs/evidence/20261107-p003-inventory-chain/13_final_reconcile.sql \
  > "$RAW/13_p003_reconcile.txt" 2>&1
chk "$?" "0" "⑤ P0-03 对账脚本在迁移后仍能跑通"
cat "$RAW/13_p003_reconcile.txt"

echo
echo "===== 汇总：PASS=$PASS FAIL=$FAIL ====="
echo "db=$DBL"
if [ "$FAIL" -ne 0 ]; then exit 1; fi
exit 0
