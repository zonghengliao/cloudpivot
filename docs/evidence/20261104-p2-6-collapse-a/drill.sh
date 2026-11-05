#!/usr/bin/env bash
# ============================================================
# t_237f1caa 隔离库演练：DI-19 按状态分级 + DI-18 精确豁免（含到期回红 / 精确性反证）
#
# 只在隔离库 cloudpivot_p26 上跑。生产库绝不执行本脚本（生产只读复跑见 README §4）。
# 用法：  bash drill.sh            # 全部步骤，输出落在 $OUT
# ============================================================
set -uo pipefail

REPO="${REPO:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)}"
OUT="${OUT:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
DB_NAME="${DB_NAME:-cloudpivot_p26}"
PSQL=(psql -X -q -h 127.0.0.1 -p 5432 -U alun -d "$DB_NAME" -v ON_ERROR_STOP=1)

export NIK_DATABASE_HOST=127.0.0.1
export NIK_DATABASE_PORT=5432
export NIK_DATABASE_USER=alun
export NIK_DATABASE_NAME="$DB_NAME"
export NIK_DATABASE_PASSWORD=alun
export PGPASSWORD=alun

AUDIT=(python3 "$REPO/scripts/verify/snapshot_audit.py")

step() { echo; echo "########## $* ##########"; }

cleanup_fixture() {
  "${PSQL[@]}" -tA <<'SQL' >/dev/null
BEGIN;
DELETE FROM biz_inventory_txn WHERE id IN ('01a19108-ee53-73c6-bdaa-0109614ea963','01a1f900-0000-7000-8000-0000000000e1');
DELETE FROM biz_flow_instance WHERE id IN ('01a1f900-0000-7000-8000-0000000000a1','01a1f900-0000-7000-8000-0000000000a2');
DELETE FROM biz_task WHERE id IN ('01a1f900-0000-7000-8000-000000000001','01a1f900-0000-7000-8000-000000000002');
DELETE FROM biz_material_serial WHERE id = '01a1f900-0000-7000-8000-0000000000d1';
COMMIT;
SQL
}

# ---------- 0. 干净基线：全量 25 条（22 条本卡口径 + P2-6b 新增 3 条）----------
step "0. 基线（干净隔离库，--install-views 全量）"
cleanup_fixture
"${AUDIT[@]}" --install-views 2>&1 | tee "$OUT/iso-00-baseline-green.txt" | tail -6

# ---------- 1. 造脏：① PENDING 实例 / ② 终态实例 + ③ 指向软删序列号的流水 ----------
step "1. 造三种脏数据"
"${PSQL[@]}" -f "$OUT/iso-fixture.sql" 2>&1 | tee "$OUT/iso-01-fixture-apply.txt"
"${PSQL[@]}" -tAc "select '夹具行数: txn='||(select count(*) from biz_inventory_txn where doc_no like 'ISO-DI18%')
   ||' inst='||(select count(*) from biz_flow_instance where instance_no like 'FLW-ISO-%')
   ||' task='||(select count(*) from biz_task where task_no like 'TSK-ISO-%')
   ||' serial='||(select count(*) from biz_material_serial where serial_no like 'SN-ISO-%')
   ||'  (实例状态: '||(select string_agg(instance_no||'='||status, ', ' order by instance_no) from biz_flow_instance where instance_no like 'FLW-ISO-%')||')'" \
  | tee -a "$OUT/iso-01-fixture-apply.txt"

# ---------- 2. ①② DI-19 分级 + ③ DI-18 豁免生效 ----------
step "2. ①DI-19 PENDING⇒WARN / ②DI-19 终态⇒INFO / ③DI-18 豁免⇒INFO"
"${AUDIT[@]}" --group 多态引用 2>&1 | tee "$OUT/iso-02-graded-and-waived.txt" | sed -n '/多态引用/,/^汇总/p'

# ---------- 3. 把豁免 expires_on 改成昨天 ⇒ DI-18 自动回红 ----------
step "3. 豁免过期（expires_on = current_date - 1）⇒ DI-18 回 ERROR"
"${PSQL[@]}" -tAc "select '过期前: expires_on=' || expires_on || '  expires_on > current_date = ' || (expires_on > current_date)::text from di.v_poly_ref_waiver" | tee "$OUT/iso-03-waiver-expiry-before.txt"
"${PSQL[@]}" <<'SQL'
CREATE OR REPLACE VIEW di.v_poly_ref_waiver AS
SELECT check_id, src_table, src_id, reason, checked_by, checked_on, expires_on
  FROM (VALUES
    ('DI-18', 'biz_inventory_txn', '01a19108-ee53-73c6-bdaa-0109614ea963',
     '隔离库演练：把 expires_on 改成昨天，验证「到期自动回红」',
     'manong', DATE '2026-11-04', (current_date - 1))
  ) AS t(check_id, src_table, src_id, reason, checked_by, checked_on, expires_on);
SQL
"${PSQL[@]}" -tAc "select '过期后: expires_on=' || expires_on || '  expires_on > current_date = ' || (expires_on > current_date)::text from di.v_poly_ref_waiver" | tee "$OUT/iso-03-waiver-expiry-after.txt"
"${AUDIT[@]}" --group 多态引用 2>&1 | tee "$OUT/iso-04-waiver-expired-error.txt" | sed -n '/DI-18/,/真相来源/p'

# ---------- 4. 还原台账（重新按仓库 SQL 安装）⇒ DI-18 回到 INFO ----------
step "4. 还原台账 ⇒ DI-18 回到 INFO"
"${AUDIT[@]}" --install-views --group 多态引用 2>&1 | tee "$OUT/iso-05-waiver-restored-info.txt" | sed -n '/DI-18/,/真相来源/p'

# ---------- 5. 精确性反证：同一 doc_type / 同一目标表，行 id 不在台账 ⇒ 仍 ERROR ----------
step "5. 反证：同 doc_type 同目标表、行 id 不在台账 ⇒ 仍 ERROR"
"${PSQL[@]}" -tA <<'SQL' | tee "$OUT/iso-06-precision-fixture.txt"
INSERT INTO biz_inventory_txn (id, material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no, line_no,
   qty, unit_cost, amount, before_qty, before_amount, before_cost, after_qty, after_amount, after_cost,
   period_code, remark, created_at)
VALUES ('01a1f900-0000-7000-8000-0000000000e1','e2040000-0000-4000-8000-000000000011',
   'e2040000-0000-4000-8000-000000000003','IN','PURCHASE_IN','SERIAL_BATCH',
   '01a1f900-0000-7000-8000-0000000000d1','ISO-DI18-NOWAIVER',1,5,0,0,0,0,0,5,0,0,'2026-11',
   '隔离库反证：同一 doc_type/同一目标表，但行 id 不在豁免台账 ⇒ 必须仍报 ERROR', now());
SQL
"${PSQL[@]}" -tAc "select '反证流水已插入: '||id||' doc_no='||doc_no||' doc_type='||doc_type||' doc_id='||doc_id from biz_inventory_txn where doc_no='ISO-DI18-NOWAIVER'" | tee -a "$OUT/iso-06-precision-fixture.txt"
"${AUDIT[@]}" --group 多态引用 2>&1 | tee "$OUT/iso-07-precision-error.txt" | sed -n '/DI-18/,/DI-19/p'

# ---------- 6. 清场 ⇒ 回到 25/25 ----------
step "6. 清场后全量复跑"
cleanup_fixture
"${PSQL[@]}" -tAc "select '残留: txn='||(select count(*) from biz_inventory_txn where doc_no like 'ISO-DI18%')
   ||' inst='||(select count(*) from biz_flow_instance where instance_no like 'FLW-ISO-%')
   ||' task='||(select count(*) from biz_task where task_no like 'TSK-ISO-%')
   ||' serial='||(select count(*) from biz_material_serial where serial_no like 'SN-ISO-%')" | tee "$OUT/iso-08-residue.txt"
"${AUDIT[@]}" 2>&1 | tee "$OUT/iso-09-cleanup-green.txt" | tail -5
