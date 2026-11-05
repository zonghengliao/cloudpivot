#!/usr/bin/env bash
# ============================================================
# t_485a84dd 收口 D 验收演练（五形态）—— **只在隔离库 cloudpivot_p26 上跑**
#   形态① DI-18 真悬空/非豁免行仍 ERROR（1a 真收货单 / 1b 同 doc_type 不同行 id）
#   形态② DI-19 PENDING 实例指向软删任务 ⇒ WARN
#   形态③ DI-19 APPROVED（终态）实例 ⇒ INFO
#   形态④ 豁免 expires_on = 昨天 ⇒ DI-18 回 ERROR（再用仓库 SQL 还原 ⇒ 回 INFO）
#   形态⑤ 全清场 ⇒ 25/25、BLOCKER/WARN/INFO 全 0、exit 0
# 造脏 → 跑 → 清场；生产库绝不执行本脚本。
# 用法： bash docs/evidence/20261104-p2-6-closeout/drill.sh
# ============================================================
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="${REPO:-$(cd "$HERE/../../.." && pwd)}"
OUT="${OUT:-$HERE/drill}"
DB_NAME="${DB_NAME:-cloudpivot_p26}"

export NIK_DATABASE_HOST=127.0.0.1
export NIK_DATABASE_PORT=5432
export NIK_DATABASE_USER=alun
export NIK_DATABASE_NAME="$DB_NAME"
export NIK_DATABASE_PASSWORD=alun
export PGPASSWORD=alun

PSQL=(psql -X -q -h 127.0.0.1 -p 5432 -U alun -d "$DB_NAME" -v ON_ERROR_STOP=1)
AUDIT=(python3 "$REPO/scripts/verify/snapshot_audit.py")

mkdir -p "$OUT"
step() { echo; echo "########## $* ##########"; }

# run <输出文件名> [audit 参数...]  —— 原始输出整段落盘，并回显 exit code
run() {
  local name="$1"; shift
  "${AUDIT[@]}" --log-file "$OUT/audit-run.txt" "$@" > "$OUT/$name.txt" 2>&1
  local rc=$?
  local sum
  sum=$(grep -E '^汇总' "$OUT/$name.txt" | tail -1 | sed 's/^汇总: //')
  local exitline
  exitline=$(grep -E '^退出码' "$OUT/$name.txt" | tail -1)
  printf 'EXIT=%s  %s\n' "$rc" "$sum" >> "$OUT/$name.txt"
  printf '[%-28s] EXIT=%s  %s  (%s)\n' "$name" "$rc" "$sum" "$exitline"
  return $rc
}

# 期望断言：run_expect <名字> <期望BLOCKER> <期望WARN> <期望INFO> [audit 参数...]
run_expect() {
  local name="$1" eb="$2" ew="$3" ei="$4"; shift 4
  "${AUDIT[@]}" --log-file "$OUT/audit-run.txt" "$@" > "$OUT/$name.txt" 2>&1
  local rc=$?
  local sum
  sum=$(grep -E '^汇总' "$OUT/$name.txt" | tail -1 | sed 's/^汇总: //')
  printf 'EXIT=%s  %s\n' "$rc" "$sum" >> "$OUT/$name.txt"
  local b w i
  b=$(sed -E 's/.*BLOCKER ([0-9]+).*/\1/' <<<"$sum")
  w=$(sed -E 's/.*WARN ([0-9]+).*/\1/' <<<"$sum")
  i=$(sed -E 's/.*INFO ([0-9]+).*/\1/' <<<"$sum")
  local verdict="PASS"
  [[ "$b" == "$eb" && "$w" == "$ew" && "$i" == "$ei" ]] || verdict="MISMATCH(期望 B/W/I=$eb/$ew/$ei)"
  printf '[%-28s] EXIT=%s  B/W/I=%s/%s/%s  %s\n' "$name" "$rc" "$b" "$w" "$i" "$verdict"
  return 0
}

cleanup_all() {
  "${PSQL[@]}" -tA >/dev/null <<'SQL'
BEGIN;
DELETE FROM biz_inventory_txn
 WHERE id IN ('01a19108-ee53-73c6-bdaa-0109614ea963','01a1f900-0000-7000-8000-0000000000e2');
DELETE FROM biz_flow_instance
 WHERE id IN ('01a1f900-0000-7000-8000-0000000000a1','01a1f900-0000-7000-8000-0000000000a2');
DELETE FROM biz_task
 WHERE id IN ('01a1f900-0000-7000-8000-000000000001','01a1f900-0000-7000-8000-000000000002');
DELETE FROM biz_material_serial WHERE id = '01a1f900-0000-7000-8000-0000000000d1';
UPDATE biz_stock_in SET deleted_at = NULL, updated_at = now() WHERE in_no = 'IN-20261104-000001';
COMMIT;
SQL
}

trap cleanup_all EXIT

# ---------- 0. 基线（干净 p26，--install-views 全量 25 条）----------
step "0. 基线：干净隔离库"
cleanup_all
"${AUDIT[@]}" --install-views --log-file "$OUT/audit-run.txt" > "$OUT/00-baseline-full.txt" 2>&1
rc=$?; sum=$(grep -E '^汇总' "$OUT/00-baseline-full.txt" | tail -1 | sed 's/^汇总: //')
printf 'EXIT=%s  %s\n' "$rc" "$sum" >> "$OUT/00-baseline-full.txt"
echo "[00-baseline-full] EXIT=$rc  $sum"

# ---------- 形态①a：软删一张已过账收货单（DI-18 真悬空，非豁免行）----------
step "形态①a：软删已过账收货单 IN-20261104-000001（真悬空，非豁免行）"
"${PSQL[@]}" -tAc "UPDATE biz_stock_in SET deleted_at = now(), updated_at = now() WHERE in_no = 'IN-20261104-000001';" | tee "$OUT/01a-fixture.txt"
"${PSQL[@]}" -tAc "select 'in_no='||in_no||' status='||status||' deleted_at='||deleted_at from biz_stock_in where in_no='IN-20261104-000001';" | tee -a "$OUT/01a-fixture.txt"
run_expect 01a-dirty-real-receipt 3 0 4

# ---------- 还原收货单 ----------
step "还原：撤销形态①a 的软删"
"${PSQL[@]}" -tAc "UPDATE biz_stock_in SET deleted_at = NULL, updated_at = now() WHERE in_no = 'IN-20261104-000001';
                   select '还原后: in_no='||in_no||' status='||status||' deleted_at='||coalesce(deleted_at::text,'NULL') from biz_stock_in where in_no='IN-20261104-000001';" | tee "$OUT/01a-restored.txt"
run_expect 01a-restored-green 0 0 0

# ---------- 形态①b：同 doc_type、同目标表，行 id 不在豁免台账 ⇒ 仍 ERROR ----------
step "形态①b：豁免精确性反证（同 doc_type=SERIAL_BATCH，行 id 未登记）"
"${PSQL[@]}" -f "$HERE/fixture-18.sql" 2>&1 | tee "$OUT/01b-fixture.txt"
"${PSQL[@]}" -tAc "select '夹具行: '||id||' doc_no='||doc_no||' doc_type='||doc_type||' doc_id='||doc_id||'  目标已软删='||(select (deleted_at is not null)::text from biz_material_serial s where s.id=t.doc_id)||'  在豁免台账='||exists(select 1 from di.v_poly_ref_waiver w where w.src_id=t.id::text)::text from biz_inventory_txn t where doc_no like 'D4-DI18%' order by doc_no;" | tee -a "$OUT/01b-fixture.txt"
run_expect 01b-waived-plus-nowaiver 1 0 1

# ---------- 形态④-1：只留被豁免的那一行 ⇒ 有效期内 INFO（带「已核查豁免#」）----------
step "形态④-1：删掉未登记的反证行，只留被豁免行（有效期内 ⇒ INFO）"
"${PSQL[@]}" -tAc "DELETE FROM biz_inventory_txn WHERE id = '01a1f900-0000-7000-8000-0000000000e2';" | tee "$OUT/04a-fixture.txt"
"${PSQL[@]}" -tAc "select '豁免台账: '||check_id||' src_id='||src_id||' expires_on='||expires_on||' 未过期='||(expires_on > current_date)::text from di.v_poly_ref_waiver;" | tee -a "$OUT/04a-fixture.txt"
run_expect 04a-waiver-active-info 0 0 1

# ---------- 形态④-2：expires_on 改成昨天 ⇒ DI-18 自动回 ERROR ----------
step "形态④-2：豁免 expires_on = current_date - 1 ⇒ DI-18 回 ERROR"
"${PSQL[@]}" -tAc "select '改前: expires_on='||expires_on||'  未过期='||(expires_on > current_date)::text from di.v_poly_ref_waiver;" | tee "$OUT/04b-expiry-before.txt"
"${PSQL[@]}" > /dev/null <<'SQL'
CREATE OR REPLACE VIEW di.v_poly_ref_waiver AS
SELECT check_id, src_table, src_id, reason, checked_by, checked_on, expires_on
  FROM (VALUES
    ('DI-18', 'biz_inventory_txn', '01a19108-ee53-73c6-bdaa-0109614ea963',
     '收口D 验收：把 expires_on 改成昨天，验证「到期自动回红」',
     'manong', DATE '2026-11-04', (current_date - 1))
  ) AS t(check_id, src_table, src_id, reason, checked_by, checked_on, expires_on);
SQL
"${PSQL[@]}" -tAc "select '过期后: expires_on='||expires_on||'  未过期='||(expires_on > current_date)::text from di.v_poly_ref_waiver;" | tee "$OUT/04b-expiry-after.txt"
run_expect 04c-waiver-expired-error 1 0 0

# ---------- 形态④-3：用仓库 SQL 重新安装台账 ⇒ 回到 INFO ----------
step "形态④-3：--install-views 从仓库 SQL 还原台账 ⇒ 回 INFO"
run_expect 04d-waiver-restored-info 0 0 1 --install-views

# ---------- 清掉形态④夹具 ----------
step "清场：形态④夹具"
"${PSQL[@]}" -tA <<'SQL' | tee "$OUT/04e-cleanup.txt"
DELETE FROM biz_inventory_txn WHERE id = '01a19108-ee53-73c6-bdaa-0109614ea963';
DELETE FROM biz_material_serial WHERE id = '01a1f900-0000-7000-8000-0000000000d1';
SELECT '残留: txn='||(SELECT count(*) FROM biz_inventory_txn WHERE doc_no LIKE 'D4-DI18%')
     ||' serial='||(SELECT count(*) FROM biz_material_serial WHERE serial_no LIKE 'SN-D4-%');
SQL

# ---------- 形态②：PENDING 实例 → 软删任务 ⇒ WARN ----------
step "形态②：软删任务 + 审批实例 PENDING（活待办指向死对象）"
"${PSQL[@]}" -f "$HERE/fixture-19.sql" 2>&1 | tee "$OUT/02-fixture.txt"
"${PSQL[@]}" -tAc "select '实例: '||instance_no||' status='||status||' biz_id='||biz_id||'  任务已软删='||(select (deleted_at is not null)::text from biz_task t where t.id=f.biz_id) from biz_flow_instance f where instance_no like 'FLW-D4-%' order by instance_no;" | tee -a "$OUT/02-fixture.txt"
run_expect 02-di19-pending-warn 0 1 1

# ---------- 形态③：同一个实例改成 APPROVED（终态）⇒ INFO ----------
step "形态③：把实例 ① 的 status 改成 APPROVED（终态）⇒ 降 INFO"
"${PSQL[@]}" -tAc "UPDATE biz_flow_instance SET status='APPROVED', ended_at=now(), updated_at=now() WHERE instance_no='FLW-D4-0001';" | tee "$OUT/03-fixture.txt"
"${PSQL[@]}" -tAc "select '实例: '||instance_no||' status='||status from biz_flow_instance where instance_no like 'FLW-D4-%' order by instance_no;" | tee -a "$OUT/03-fixture.txt"
run_expect 03-di19-approved-info 0 0 2

# ---------- 形态⑤：全清场 ⇒ 25/25 全绿 ----------
step "形态⑤：全清场 ⇒ 全量复跑"
cleanup_all
"${PSQL[@]}" -tA <<'SQL' | tee "$OUT/05-residue.txt"
SELECT '残留: txn='      ||(SELECT count(*) FROM biz_inventory_txn WHERE doc_no LIKE 'D4-%')
     ||' inst='          ||(SELECT count(*) FROM biz_flow_instance WHERE instance_no LIKE 'FLW-D4-%')
     ||' task='          ||(SELECT count(*) FROM biz_task WHERE task_no LIKE 'TSK-D4-%')
     ||' serial='        ||(SELECT count(*) FROM biz_material_serial WHERE serial_no LIKE 'SN-D4-%')
     ||' stock_in_del='  ||(SELECT count(*) FROM biz_stock_in WHERE deleted_at IS NOT NULL);
SQL
"${AUDIT[@]}" --install-views --log-file "$OUT/audit-run.txt" > "$OUT/05-final-full-green.txt" 2>&1
rc=$?; sum=$(grep -E '^汇总' "$OUT/05-final-full-green.txt" | tail -1 | sed 's/^汇总: //')
printf 'EXIT=%s  %s\n' "$rc" "$sum" >> "$OUT/05-final-full-green.txt"
echo "[05-final-full-green] EXIT=$rc  $sum"

echo
echo "输出目录：$OUT"
