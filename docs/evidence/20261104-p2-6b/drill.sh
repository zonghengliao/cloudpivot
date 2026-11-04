#!/usr/bin/env bash
# ============================================================
# P2-6b 脏数据演练 —— 序列号来源映射 + 附件 / 提醒 / 抄送三类补口
#
# **只在隔离库上跑，不碰生产**。本文件是「造脏 / 清脏的夹具」，
# 刻意放在 docs/evidence/ 而不是 scripts/verify/：
# 让「巡检链路零 DML」这个性质可以靠目录边界一眼验证
# （scripts/verify/snapshot_audit.py 的 grep -cwE 'UPDATE|INSERT|DELETE' = 0）。
#
# 用法：D1_DB=cloudpivot_p26b bash docs/evidence/20261104-p2-6b/drill.sh
# ============================================================
set -uo pipefail
DB=${D1_DB:-cloudpivot_p26b}
OUT=${P26B_DRILL_OUT:-/opt/cloudpivot/docs/evidence/20261104-p2-6b/drill}
TESTENV=/etc/cloudpivot/cloudpivot-test.env
PW=$(grep -m1 "^ALUN_DATABASE_PASSWORD=" "$TESTENV" | cut -d= -f2-)
export PGPASSWORD="$PW"
mkdir -p "$OUT"
PSQL="psql -X -q -h 127.0.0.1 -U cloudpivot -d $DB -v ON_ERROR_STOP=1"

export NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432 \
       NIK_DATABASE_USER=cloudpivot NIK_DATABASE_PASSWORD="$PW" NIK_DATABASE_NAME="$DB"

# 固定 UUID（可重跑；清场就是按这些 id / 单号删）
IN_ID=b2b60001-0000-4000-8000-000000000001
SN_OK=b2b60002-0000-4000-8000-000000000001
SN_DANGLING=b2b60002-0000-4000-8000-000000000002
SN_MISSING=b2b60002-0000-4000-8000-000000000003
FILE_ID=b2b60003-0000-4000-8000-000000000001
ATT_ID=b2b60004-0000-4000-8000-000000000001
REM_ID=b2b60005-0000-4000-8000-000000000001
INST_ID=b2b60006-0000-4000-8000-000000000001
CC_DANGLING=b2b60007-0000-4000-8000-000000000001
CC_MISSING=b2b60007-0000-4000-8000-000000000002
DEAD=b2b600ff-0000-4000-8000-0000000000ff

run() {  # run <文件名>
  python3 /opt/cloudpivot/scripts/verify/snapshot_audit.py --group 多态引用 > "$OUT/$1.txt" 2>&1
  local rc=$?
  echo "[$1] exit=$rc  $(grep -E '^汇总' "$OUT/$1.txt" | sed 's/^汇总: //')"
  grep -E "^  [⚠ℹ!✗✓] DI-(19|21|22|23|24|25)" "$OUT/$1.txt" | sed 's/^/    /'
}

clean_fixtures() {
  $PSQL <<'SQL' >/dev/null
DELETE FROM biz_material_serial WHERE serial_no LIKE 'P26B-%';
DELETE FROM biz_doc_attachment WHERE id = 'b2b60004-0000-4000-8000-000000000001';
DELETE FROM biz_reminder_log    WHERE id = 'b2b60005-0000-4000-8000-000000000001';
DELETE FROM biz_flow_cc         WHERE instance_id = 'b2b60006-0000-4000-8000-000000000001';
DELETE FROM biz_flow_instance   WHERE id = 'b2b60006-0000-4000-8000-000000000001';
UPDATE biz_stock_in SET deleted_at = NULL WHERE id = 'b2b60001-0000-4000-8000-000000000001';
DELETE FROM biz_stock_in        WHERE id = 'b2b60001-0000-4000-8000-000000000001';
DELETE FROM sys_file            WHERE id = 'b2b60003-0000-4000-8000-000000000001';
SQL
}
trap clean_fixtures EXIT

echo "== ① 基线（空库，25 条巡检应全绿）=="
clean_fixtures
run 00-baseline-green

echo "== ② 造脏 A：序列号三种形态 + 附件 + 提醒 + 抄送（收货单尚在）=="
$PSQL <<'SQL' >/dev/null
-- 收货单（PURCHASE 必须有供应商；无来源采购单 ⇒ is_sourceless=true，满足 ck_biz_si_sourceless_pair）
INSERT INTO biz_stock_in (id, in_no, in_type, supplier_id, warehouse_id, in_date, status, is_sourceless)
VALUES ('b2b60001-0000-4000-8000-000000000001', 'P26B-IN-1', 'PURCHASE',
        (SELECT id FROM biz_supplier ORDER BY supplier_code LIMIT 1),
        (SELECT id FROM biz_warehouse ORDER BY warehouse_code LIMIT 1),
        CURRENT_DATE, 'DRAFT', TRUE);

-- 序列号①：PURCHASE + 真实的收货单 id（形态正确，正常情况下不该报）
INSERT INTO biz_material_serial
  (id, serial_no, material_id, warehouse_id, status, inbound_at, source_type, source_id)
VALUES ('b2b60002-0000-4000-8000-000000000001', 'P26B-SN-OK',
        (SELECT id FROM biz_material WHERE serial_required ORDER BY material_code LIMIT 1),
        (SELECT id FROM biz_warehouse ORDER BY warehouse_code LIMIT 1),
        'IN_STOCK', now(), 'PURCHASE', 'b2b60001-0000-4000-8000-000000000001');

-- 序列号②：TRANSFER + 不存在的调拨单 id ⇒ DI-21 DANGLING（故意造成的悬空）
INSERT INTO biz_material_serial
  (id, serial_no, material_id, warehouse_id, status, inbound_at, source_type, source_id)
VALUES ('b2b60002-0000-4000-8000-000000000002', 'P26B-SN-DANGLING',
        (SELECT id FROM biz_material WHERE serial_required ORDER BY material_code LIMIT 1),
        (SELECT id FROM biz_warehouse ORDER BY warehouse_code LIMIT 1),
        'IN_STOCK', now(), 'TRANSFER', 'b2b600ff-0000-4000-8000-0000000000ff');

-- 序列号③：有类型无 id（历史残缺形态）⇒ DI-21 MISSING_ID
INSERT INTO biz_material_serial
  (id, serial_no, material_id, warehouse_id, status, inbound_at, source_type)
VALUES ('b2b60002-0000-4000-8000-000000000003', 'P26B-SN-MISSING',
        (SELECT id FROM biz_material WHERE serial_required ORDER BY material_code LIMIT 1),
        (SELECT id FROM biz_warehouse ORDER BY warehouse_code LIMIT 1),
        'IN_STOCK', now(), 'PURCHASE');

-- 附件：挂在收货单上（doc_type=STOCK_IN）
INSERT INTO sys_file (id, file_name, file_key, file_size)
VALUES ('b2b60003-0000-4000-8000-000000000001', 'p26b-drill.pdf', 'p26b/drill.pdf', 1024);
INSERT INTO biz_doc_attachment (id, doc_type, doc_id, file_id, file_role)
VALUES ('b2b60004-0000-4000-8000-000000000001', 'STOCK_IN',
        'b2b60001-0000-4000-8000-000000000001',
        'b2b60003-0000-4000-8000-000000000001', 'INVOICE');

-- 提醒台账：指向不存在的采购单 ⇒ DI-24
INSERT INTO biz_reminder_log (id, doc_type, doc_id, doc_no, remind_kind, stage, remind_date)
VALUES ('b2b60005-0000-4000-8000-000000000001', 'PURCHASE_ORDER',
        'b2b600ff-0000-4000-8000-0000000000ff', 'P26B-REM-1', 'PO_OVERDUE', 'D1', CURRENT_DATE);

-- 抄送：借一条真实流程定义的实例（避免造整套流程图）
--   biz_type 固定写 OTHER：本演练只关心抄送，不想让 DI-19（流程实例单据）抢戏
INSERT INTO biz_flow_instance
  (id, instance_no, def_id, version_id, version_no, flow_code, flow_name, biz_type, title,
   status, initiator_id)
SELECT 'b2b60006-0000-4000-8000-000000000001', 'P26B-INST-1', d.id, v.id, v.version_no,
       d.flow_code, d.flow_name, 'OTHER', 'P2-6b 演练实例', 'PENDING',
       (SELECT id FROM sys_user ORDER BY username LIMIT 1)
  FROM biz_flow_def d
  JOIN biz_flow_def_version v ON v.def_id = d.id AND v.is_current = TRUE
 ORDER BY d.flow_code LIMIT 1;

-- 抄送①：ROLE 来源但 source_ref 指向不存在的角色 ⇒ DI-25 DANGLING
INSERT INTO biz_flow_cc (id, instance_id, node_key, round, receiver_id, source_type, source_ref, is_read)
VALUES ('b2b60007-0000-4000-8000-000000000001', 'b2b60006-0000-4000-8000-000000000001',
        'p26b_cc_dangling', 1, (SELECT id FROM sys_user ORDER BY username LIMIT 1),
        'ROLE', 'b2b600ff-0000-4000-8000-0000000000ff', FALSE);
-- 抄送②：ROLE 来源却没记 source_ref ⇒ DI-25 MISSING_ID
INSERT INTO biz_flow_cc (id, instance_id, node_key, round, receiver_id, source_type, source_ref, is_read)
VALUES ('b2b60007-0000-4000-8000-000000000002', 'b2b60006-0000-4000-8000-000000000001',
        'p26b_cc_missing', 1, (SELECT id FROM sys_user ORDER BY username LIMIT 1),
        'ROLE', NULL, FALSE);
SQL
echo "  （造脏完成）"
run 01-dirty-serial-and-refs

echo "== ③ 造脏 B：把这张收货单软删 ⇒ DI-21/DI-23 必须从「绿」变「悬空」=="
$PSQL -c "UPDATE biz_stock_in SET deleted_at = now() WHERE id = 'b2b60001-0000-4000-8000-000000000001'" >/dev/null
run 02-dirty-stock-in-soft-deleted

echo "== ④ 清场：恢复基线（再跑必须全绿）=="
clean_fixtures
run 03-cleanup-green

echo "== 结果 =="
for f in 00-baseline-green 01-dirty-serial-and-refs 02-dirty-stock-in-soft-deleted 03-cleanup-green; do
  printf '%-32s %s\n' "$f" "$(grep -E '^汇总' "$OUT/$f.txt" | sed 's/^汇总: //')"
done
