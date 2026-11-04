#!/usr/bin/env bash
# P2-6 多态引用巡检 —— 脏数据演练（**只在隔离库上跑，不碰生产**）
#
# 目的：证明「新加的 6 条巡检真的会红，不是恒绿的摆设」，并证明清场后能变回绿。
# 本脚本是**演练夹具**（造脏 / 清脏），不是巡检本身；巡检脚本 snapshot_audit.py
# 全文 0 处 DML（grep -cwE 'UPDATE|INSERT|DELETE' = 0），只读纪律没有被这次改动破坏。
#
# 用法：
#   D1_DB=cloudpivot_p26 bash scripts/verify/p26_poly_ref_drill.sh
#   输出落在 docs/evidence/20261104-p2-6/drill/
set -uo pipefail
DB=${D1_DB:-cloudpivot_p26}
PORT=${P26_PORT:-18096}
OUT=${P26_DRILL_OUT:-/opt/cloudpivot/docs/evidence/20261104-p2-6/drill}
mkdir -p "$OUT"
PSQL="psql -X -q -h 127.0.0.1 -U alun -d $DB -v ON_ERROR_STOP=1"
export PGPASSWORD=alun
export NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun NIK_DATABASE_NAME="$DB"

run() {  # run <文件名>
  python3 /opt/cloudpivot/scripts/verify/snapshot_audit.py --group 多态引用 > "$OUT/$1.txt" 2>&1
  local rc=$?
  echo "[$1] exit=$rc  $(grep -E '^汇总' "$OUT/$1.txt" | sed 's/^汇总: //')"
}

clean_fixtures() {
  $PSQL -c "
DELETE FROM biz_material_serial WHERE serial_no = 'P26-DRILL-SN1';
DELETE FROM biz_voucher       WHERE voucher_no = 'P26-DRILL-V1';
DELETE FROM biz_inventory_txn WHERE doc_no     = 'P26-DRILL-TXN';
DELETE FROM biz_status_log    WHERE entity_no  = 'P26-DRILL-LOG';
UPDATE biz_stock_in SET deleted_at = NULL WHERE in_no = 'IN-20261104-000001';" >/dev/null
}
trap clean_fixtures EXIT

echo "== ① 基线（真机造出来的收货/凭证/流水，全绿）=="
clean_fixtures
run 00-baseline-green

echo "== ② 脏数据 A：把一张已过账收货单软删（单据没了，引用它的凭证/流水/审批/日志还挂着）=="
$PSQL -c "UPDATE biz_stock_in SET deleted_at = now() WHERE in_no = 'IN-20261104-000001';" >/dev/null
run 01-dirty-softdeleted-doc

echo "== ③ 恢复软删（撤销脏数据 A）=="
$PSQL -c "UPDATE biz_stock_in SET deleted_at = NULL WHERE in_no = 'IN-20261104-000001';" >/dev/null
run 02-restored-green

echo "== ④ 脏数据 B：凭证「有来源类型、无来源 id」+ 流水 doc_id 指向不存在的收货单 =="
$PSQL -c "
INSERT INTO biz_voucher (voucher_no, period, voucher_date, voucher_type, source_type, source_id, summary,
                         total_debit, total_credit, entry_count)
VALUES ('P26-DRILL-V1', '2026-11', '2026-11-04', 'AUTO', 'STOCK_IN', NULL,
        'P2-6 脏数据演练：有来源类型无来源 id', 1, 1, 2);
INSERT INTO biz_inventory_txn (material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no, qty, period_code)
SELECT (SELECT id FROM biz_material LIMIT 1), (SELECT id FROM biz_warehouse LIMIT 1), 'IN', 'PURCHASE_IN',
       'STOCK_IN', '99999999-9999-4999-8999-999999999999'::uuid, 'P26-DRILL-TXN', 1, '2026-11';" >/dev/null
run 03-dirty-missing-id-and-row

echo "== ⑤ 脏数据 C：序列号「有来源类型、无 source_id」（= 生产库现状的复刻）=="
clean_fixtures
$PSQL -c "
INSERT INTO biz_material_serial (serial_no, material_id, status, source_type, source_id)
SELECT 'P26-DRILL-SN1', (SELECT id FROM biz_material LIMIT 1), 'IN_STOCK', 'PURCHASE', NULL;" >/dev/null
run 04-dirty-serial-source

echo "== ⑥ 脏数据 D：状态日志出现登记表没有的 entity_type（护栏自检必须报）=="
clean_fixtures
$PSQL -c "INSERT INTO biz_status_log (entity_type, entity_id, entity_no, to_status)
          VALUES ('P26_UNKNOWN_DOC', gen_random_uuid(), 'P26-DRILL-LOG', 'POSTED');" >/dev/null
run 05-dirty-unregistered-type

echo "== ⑦ 全部清场（应回到全绿）=="
clean_fixtures
run 06-clean-green

echo
echo "输出目录：$OUT"
