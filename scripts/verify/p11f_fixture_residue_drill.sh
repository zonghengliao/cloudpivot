#!/usr/bin/env bash
# ============================================================================
# P11-followup · 夹具残留「注入 → 检出 → 清理 → 幂等 → 回滚」全链路演练
# ----------------------------------------------------------------------------
# 为什么要有这个脚本：
#   2026-10-31 11:34 prod 被客户端打了一次 POST /business/erp/serial/batch-generate，
#   造出「无单据、零价值」的库存 ⇒ B1（BLOCKER）/ C3（WARN）同时红。P10 的 10_ 只认
#   物料编码 M-W1A-004，换个物料就又看不见了。本脚本在 **prod 的副本库** 上重放这件事，
#   证明 20/21 两个新分支能检出、能清、能幂等、能回滚 —— 全程不碰 prod（cloudpivot）本身。
#
# 覆盖的断言（18 条）：
#   注入前后不变量由绿转红（B1 + C3）→ 检出脚本 exit 1 且点名该物料
#   → 通用清理把三条账清干净 → 全绿 → 第二遍 0 行 → down 精确还原 → 又回到红
#
# 用法：
#   bash scripts/verify/p11f_fixture_residue_drill.sh          # 跑完自动删副本库
#   KEEP=1 bash scripts/verify/p11f_fixture_residue_drill.sh   # 留下副本库和备份供查看
# 环境变量：P11F_DB（副本库名，默认 cloudpivot_p11f）/ P11F_SRC（源库，默认 cloudpivot）
# ============================================================================
set -uo pipefail

REPO="${REPO:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
ENV_FILE="${CLOUDPIVOT_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}"
# shellcheck disable=SC1090
set -a; . "$ENV_FILE"; set +a

export PGPASSWORD="$NIK_DATABASE_PASSWORD"
DB_HOST="${NIK_DATABASE_HOST:-127.0.0.1}"
DB_PORT="${NIK_DATABASE_PORT:-5432}"
DB_USER="${NIK_DATABASE_USER:-cloudpivot}"
SRC="${P11F_SRC:-${NIK_DATABASE_NAME:-cloudpivot}}"
DB="${P11F_DB:-cloudpivot_p11f}"
MAT="M-P11F-GEN"          # 演练专用物料（**不是** M-W1A-004：证明判据不绑物料编码）
WH01="d1370001-0000-4000-8000-000000000001"

PASS=0; FAIL=0
ok()  { PASS=$((PASS+1)); echo "  [PASS] $1"; }
bad() { FAIL=$((FAIL+1)); echo "  [FAIL] $1"; [ $# -gt 1 ] && echo "         期望：$2"; [ $# -gt 2 ] && echo "         实际：$3"; true; }

psql_q() { psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB" -At -v ON_ERROR_STOP=1 -c "$1" 2>&1 | head -1; }
psql_f() { psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB" -q -v ON_ERROR_STOP=1 -f "$1" 2>&1; }

echo "=============================================================================="
echo "P11-followup 夹具残留演练：$DB（源库 $SRC @ $DB_HOST:$DB_PORT）"
echo "=============================================================================="

echo "== 0) 建副本库（prod 副本，prod 本体全程只读）"
su - postgres -c "dropdb --if-exists $DB" >/dev/null 2>&1
su - postgres -c "createdb -O $DB_USER $DB" >/dev/null 2>&1 || { echo "✗ createdb 失败"; exit 1; }
pg_dump -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$SRC" 2>/dev/null | psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB" -q -v ON_ERROR_STOP=1 >/dev/null 2>&1
n=$(psql_q "SELECT count(*) FROM biz_material")
[ "${n:-0}" -ge 5 ] && ok "副本库就绪（biz_material=$n）" || bad "副本库导出" ">=5" "$n"

echo "== 1) 备份副本库（apply.sh 的前置；演练库也要走备份闸）"
bk="/root/datafix-backup-p10-drill-p11f"     # 00_backup.sh 的标签固定，按路径取（不按 mtime 猜最新的）
rm -rf "$bk"
P10_DB="$DB" bash "$REPO/scripts/datafix/p10/00_backup.sh" "drill-p11f" >/dev/null 2>&1
if [ -s "$bk/full.dump" ] && grep -q "database  : $DB on " "$bk/MANIFEST.txt"; then
    ok "备份落在 $bk（MANIFEST 库名 = $DB）"
else
    bad "备份/MANIFEST 库名" "$DB" "${bk:-<none>}"
fi

echo "== 2) 注入 11:34 的同款夹具（批量生成 3 件：无单据、零价值、无库位账）"
inj_err=$(psql_f /dev/stdin 2>&1 <<SQL
\\set ON_ERROR_STOP 1
INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type, serial_required)
VALUES ('00000000-0000-4000-8000-0000000f0001'::uuid, '$MAT', '演练夹具物料（P11F）', gen_random_uuid(), 'FINISHED', true)
ON CONFLICT DO NOTHING;

INSERT INTO biz_material_serial (id, serial_no, material_id, warehouse_id, status, source_type, source_id, created_at, updated_at)
SELECT ('00000000-0000-4000-8000-0000000f01' || lpad(g::text, 2, '0'))::uuid, 'SN-P11F-000' || g,
       m.id, '$WH01'::uuid, 'IN_STOCK', 'PURCHASE', NULL, now(), now()
  FROM biz_material m, generate_series(1,3) g
 WHERE m.material_code = '$MAT'
ON CONFLICT DO NOTHING;

INSERT INTO biz_inventory_txn
    (id, material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no, line_no,
     qty, unit_cost, amount, before_qty, before_amount, before_cost,
     after_qty, after_amount, after_cost, period_code, remark, created_at)
SELECT '00000000-0000-4000-8000-0000000f0201'::uuid, m.id, '$WH01'::uuid, 'IN', 'SERIAL_IN',
       'SERIAL_BATCH', '00000000-0000-4000-8000-0000000f0101'::uuid, 'SN-P11F-0001', 1,
       3, 0, 0, 0, 0, 0, 3, 0, 0, '2026-10', '演练：批量生成 3 件（SN-P11F-0001 起）', now()
  FROM biz_material m WHERE m.material_code = '$MAT'
ON CONFLICT DO NOTHING;

INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty, locked_qty, avg_cost, amount, last_txn_at, created_at, updated_at)
SELECT '00000000-0000-4000-8000-0000000f0301'::uuid, m.id, '$WH01'::uuid, NULL, 3, 0, 0, 0, now(), now(), now()
  FROM biz_material m WHERE m.material_code = '$MAT'
ON CONFLICT DO NOTHING;
SQL
)
case "$inj_err" in
    *ERROR*|*错误*) bad "注入失败" "无 ERROR" "$(echo "$inj_err" | head -3)" ;;
    *) ok "注入三条账（物料 / 序列号 / 流水 / 成本账）" ;;
esac
inv=$(psql_q "SELECT qty::text FROM biz_inventory i JOIN biz_material m ON m.id=i.material_id WHERE m.material_code='$MAT'")
ser=$(psql_q "SELECT count(*) FROM biz_material_serial s JOIN biz_material m ON m.id=s.material_id WHERE m.material_code='$MAT' AND s.deleted_at IS NULL")
[ "$inv" = "3.0000" ] && ok "注入：成本账 qty=3.0000" || bad "注入成本账" "3.0000" "$inv"
[ "$ser" = "3" ] && ok "注入：3 条存活序列号（source_id IS NULL）" || bad "注入序列号" "3" "$ser"
loc=$(psql_q "SELECT coalesce(sum(qty),0)::numeric(18,4)::text FROM biz_inventory_location il JOIN biz_material m ON m.id=il.material_id WHERE m.material_code='$MAT'")
[ "$loc" = "0.0000" ] && ok "注入：库位账 0（正是 B1 破的形状）" || bad "注入库位账为空" "0.0000" "$loc"

echo "== 3) 不变量应当转红（B1 BLOCKER + C3 WARN，与 11:34 那次一模一样）"
out=$(NIK_DATABASE_NAME="$DB" python3 "$REPO/scripts/verify/erp_invariants.py" 2>&1); rc=$?
echo "$out" | grep -E "结果：|B1 |C3 " | sed 's/^/    /'
[ "$rc" = "1" ] && ok "不变量 exit=1（有 BLOCKER）" || bad "不变量应 exit=1" "1" "$rc"
echo "$out" | grep -qF "🚨 [账账] B1 库位账合计 = 成本账 qty" && ok "B1 红（BLOCKER）" || bad "B1 未转红"
echo "$out" | grep -qF "⚠️ [账实] C3 有库存则成本价非零" && ok "C3 红（WARN）" || bad "C3 未转红"
pass_cnt=$(echo "$out" | grep -oE "结果：[0-9]+/[0-9]+" | head -1)
echo "    基线：${pass_cnt:-?}"

echo "== 4) 检出脚本（只读）应当 exit 1 并点名 $MAT"
d=$(NIK_DATABASE_NAME="$DB" python3 "$REPO/scripts/verify/e2e_fixture_residue.py" 2>&1); rc=$?
echo "$d" | grep -E "命中|结论" | sed 's/^/    /'
[ "$rc" = "1" ] && ok "e2e_fixture_residue.py exit=1" || bad "检出脚本 exit" "1" "$rc"
echo "$d" | grep -q "$MAT" && ok "检出脚本点名 $MAT" || bad "检出脚本未点名"

echo "== 5) 通用清理（P10_GENERIC_APPLY=1 apply.sh，含 20 报告 + 21 清理）"
cl=$(P10_DB="$DB" P10_GENERIC_APPLY=1 bash "$REPO/scripts/datafix/p10/apply.sh" 2>&1); rc=$?
echo "$cl" | grep -E "P10-20:|P10-21:|库存数量合计|库位账数量合计" | sed 's/^/    /'
[ "$rc" = "0" ] && ok "apply.sh exit=0" || bad "apply.sh exit" "0" "$rc"
echo "$cl" | grep -q "已清理 $MAT" && ok "21 清理了 $MAT" || bad "21 未清理 $MAT"
inv=$(psql_q "SELECT coalesce(sum(qty),0)::text FROM biz_inventory i JOIN biz_material m ON m.id=i.material_id WHERE m.material_code='$MAT'")
[ "$inv" = "0" ] && ok "清理后该物料成本账 0 行" || bad "清理后成本账" "0" "$inv"
ser=$(psql_q "SELECT count(*) FROM biz_material_serial s JOIN biz_material m ON m.id=s.material_id WHERE m.material_code='$MAT' AND s.deleted_at IS NULL")
[ "$ser" = "0" ] && ok "清理后存活序列号 0（软删，行还在）" || bad "清理后存活序列号" "0" "$ser"
alls=$(psql_q "SELECT count(*) FROM biz_material_serial s JOIN biz_material m ON m.id=s.material_id WHERE m.material_code='$MAT'")
[ "$alls" = "3" ] && ok "序列号行仍在（软删，审计痕迹不丢）" || bad "序列号行数" "3" "$alls"
txn=$(psql_q "SELECT count(*) FROM biz_inventory_txn t JOIN biz_material m ON m.id=t.material_id WHERE m.material_code='$MAT'")
[ "$txn" = "0" ] && ok "清理后流水 0 条" || bad "清理后流水" "0" "$txn"
snap=$(psql_q "SELECT count(*) FROM datafix_residue_batch WHERE restored_at IS NULL")
[ "$snap" = "1" ] && ok "快照批次 1 批（未还原）" || bad "快照批次" "1" "$snap"

echo "== 6) 清理后不变量必须全绿"
out=$(NIK_DATABASE_NAME="$DB" python3 "$REPO/scripts/verify/erp_invariants.py" 2>&1); rc=$?
res=$(echo "$out" | grep -oE "结果：[0-9]+/[0-9]+ +.*" | head -1)
echo "    ${res:-?}"
[ "$rc" = "0" ] && ok "不变量 exit=0" || bad "不变量 exit" "0" "$rc"
# 条数会随新增检查增长（32 → 33 …），这里断言「通过数 == 总条数」而不是硬编码条数，
# 免得以后每加一条不变量就得回来改这里（历史上 32/32 被写死了 5 处）。
pair=$(echo "$res" | sed -nE 's/.*结果：([0-9]+)\/([0-9]+) .*/\1 \2/p')
if [ -n "$pair" ] && [ "${pair% *}" = "${pair#* }" ]; then
  ok "不变量全绿（$(echo "$pair" | tr ' ' '/')）"
else
  bad "套件全绿（通过数应等于总条数）" "X/X" "${res:-<无结果>}"
fi
echo "$res" | grep -q "BLOCKER 0" && ok "BLOCKER 0" || bad "BLOCKER" "0"
echo "$res" | grep -q "WARN 0" && ok "WARN 0" || bad "WARN" "0"

echo "== 7) 幂等：第二遍 0 行"
cl2=$(P10_DB="$DB" P10_GENERIC_APPLY=1 bash "$REPO/scripts/datafix/p10/apply.sh" 2>&1)
echo "$cl2" | grep -E "P10-20:|P10-21:" | sed 's/^/    /'
echo "$cl2" | grep -q "P10-20: 全库无「批量生成零成本夹具」残留" && ok "第二遍报告 0 命中" || bad "第二遍报告仍命中"
echo "$cl2" | grep -q "没有需要清理的夹具" && ok "第二遍清理 0 行（未改数据）" || bad "第二遍仍在删"
snap=$(psql_q "SELECT count(*) FROM datafix_residue_batch")
[ "$snap" = "1" ] && ok "第二遍没有新增快照批次（仍 1 批）" || bad "快照批次" "1" "$snap"

echo "== 8) 回滚：down 精确还原到修复前"
rb=$(P10_DB="$DB" P10_GENERIC_APPLY=1 bash "$REPO/scripts/datafix/p10/apply.sh" --rollback 2>&1)
echo "$rb" | grep -E "P10-21-down:" | sed 's/^/    /'
echo "$rb" | grep -q "已还原批次" && ok "down 还原了快照批次" || bad "down 未还原"
inv=$(psql_q "SELECT qty::text FROM biz_inventory i JOIN biz_material m ON m.id=i.material_id WHERE m.material_code='$MAT'")
[ "$inv" = "3.0000" ] && ok "成本账行写回 qty=3.0000（id 用快照原值）" || bad "还原成本账" "3.0000" "$inv"
ser=$(psql_q "SELECT count(*) FROM biz_material_serial s JOIN biz_material m ON m.id=s.material_id WHERE m.material_code='$MAT' AND s.deleted_at IS NULL")
[ "$ser" = "3" ] && ok "3 条序列号回到未软删" || bad "还原序列号" "3" "$ser"
txn=$(psql_q "SELECT count(*) FROM biz_inventory_txn t JOIN biz_material m ON m.id=t.material_id WHERE m.material_code='$MAT'")
[ "$txn" = "1" ] && ok "流水 1 条写回" || bad "还原流水" "1" "$txn"
out=$(NIK_DATABASE_NAME="$DB" python3 "$REPO/scripts/verify/erp_invariants.py" 2>&1); rc=$?
[ "$rc" = "1" ] && ok "回滚后不变量重新转红（= 修复前状态）" || bad "回滚后应转红" "1" "$rc"
rb2=$(P10_DB="$DB" P10_GENERIC_APPLY=1 bash "$REPO/scripts/datafix/p10/apply.sh" --rollback 2>&1)
echo "$rb2" | grep -q "没有未还原的批次" && ok "再回滚一遍：0 行（down 也幂等）" || bad "down 不幂等"

echo "== 9) 收尾"
if [ "${KEEP:-0}" = "1" ]; then
    echo "    保留副本库 $DB 与备份 $bk（KEEP=1）"
else
    su - postgres -c "dropdb --if-exists $DB" >/dev/null 2>&1
    echo "    已删副本库 $DB（prod 全程未动：$SRC）"
fi

echo "------------------------------------------------------------------------------"
echo "汇总：PASS=$PASS  FAIL=$FAIL"
[ "$FAIL" -eq 0 ] || { echo "✗ 有断言失败"; exit 1; }
echo "★ 演练通过：注入 → 红 → 检出 → 清 → 绿 → 幂等 → 回滚"
