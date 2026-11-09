#!/usr/bin/env bash
# ============================================================================
# acceptance_gate.sh —— 「巡检全绿」验收闸（演练/上线前置条件）
#
# 为什么要有这个脚本（E2E-14 评审 I-5）：
#   共享演练库 cloudpivot_dev 长期被 E2E 夹具反复污染，80 项不变量 60 绿 / 20 红，
#   「全量巡检一片红」⇒ 真问题被夹具噪音淹没，演练库不能再当验收证据。
#   口径改成：**验收只看 cloudpivot_dev_clean（只跑迁移 + 种子，禁跑 E2E 夹具）**。
#
# 判据（三条全过才算绿）：
#   ① scripts/verify/erp_invariants.py 全量 80/80（BLOCKER 0 / WARN 0）
#   ② 孤儿应收 0（source_type='SALES_OUT' 且 source_id 指向的出库单不存在）
#   ③ 库存 vs 流水 0 差异（物料×仓库，**两侧先聚合再 FULL JOIN**）
#      + 逐桶（物料×仓库×库位）成本账 vs 库位账 0 差异
#
# 用法：
#   bash scripts/ops/acceptance_gate.sh                  # 默认 145 cloudpivot_dev_clean
#   DB=cloudpivot_x bash scripts/ops/acceptance_gate.sh  # 换库（生产库 cloudpivot 会被拒）
#   ENV_FILE=/etc/cloudpivot/cloudpivot.env bash scripts/ops/acceptance_gate.sh
#
# 退出码：0 全绿 / 1 有红（可接 CI 或演练脚本前置）
#
# 注意：**不要**把它指向 cloudpivot_dev —— 那是造夹具的演练场，本来就该有残留，
#       它的红说明不了账对不对。要验收就用干净库或恢复演练重建的库。
# ============================================================================
set -uo pipefail

REPO=${REPO:-/opt/cloudpivot}
DB=${DB:-${NIK_DATABASE_NAME:-cloudpivot_dev_clean}}
ENV_FILE=${ENV_FILE:-/etc/cloudpivot/cloudpivot-dev.env}
INVARIANTS=${INVARIANTS:-$REPO/scripts/verify/erp_invariants.py}
EXPECT_TOTAL=${EXPECT_TOTAL:-80}

if [ -f "$ENV_FILE" ]; then set -a; . "$ENV_FILE"; set +a; fi
export NIK_DATABASE_NAME="$DB"

case "$DB" in
  cloudpivot|cloudpivot_prod|cloudpivot_saas)
    echo "拒绝：$DB 是生产库。验收闸只跑演练/干净库（要跑生产巡检请直接调 erp_invariants.py）。" >&2
    exit 2 ;;
esac

PSQL() {
  PGPASSWORD="${NIK_DATABASE_PASSWORD:-}" psql -h "${NIK_DATABASE_HOST:-127.0.0.1}" \
    -p "${NIK_DATABASE_PORT:-5432}" -U "${NIK_DATABASE_USER:-cloudpivot}" -d "$DB" -Atc "$1"
}

FAIL=0
echo "=================================================================="
echo "验收闸 · 库=$DB"
echo "=================================================================="

# ---- ① 不变量全量 -----------------------------------------------------------
OUT=$(cd "$REPO" && python3 "$INVARIANTS" 2>&1)
RC=$?
LINE=$(printf '%s\n' "$OUT" | grep -E '^结果：' | tail -1)
BLOCKERS=$(printf '%s\n' "$OUT" | grep -c '^  · \[')
echo
echo "① 不变量：${LINE:-（没拿到结果行）}"
if [ $RC -ne 0 ] || [ "$BLOCKERS" -ne 0 ]; then
  echo "   ❌ 不变量未全绿（rc=$RC，问题行=$BLOCKERS）"
  printf '%s\n' "$OUT" | sed -n '/必须立刻修/,$p' | head -40
  FAIL=1
elif ! printf '%s' "$LINE" | grep -q "结果：${EXPECT_TOTAL}/${EXPECT_TOTAL}"; then
  echo "   ❌ 期望 ${EXPECT_TOTAL}/${EXPECT_TOTAL}，实际：$LINE"
  FAIL=1
else
  echo "   ✅ ${EXPECT_TOTAL}/${EXPECT_TOTAL}，BLOCKER 0 / WARN 0"
fi

# ---- ② 孤儿应收 -------------------------------------------------------------
ORPHAN=$(PSQL "SELECT count(*) FROM biz_receivable r
                WHERE r.deleted_at IS NULL AND r.source_type='SALES_OUT' AND r.source_id IS NOT NULL
                  AND NOT EXISTS (SELECT 1 FROM biz_stock_out s WHERE s.id=r.source_id AND s.deleted_at IS NULL)")
echo
echo "② 孤儿应收（SALES_OUT 指向不存在出库单）：${ORPHAN:-?}"
if [ "${ORPHAN:-1}" != "0" ]; then echo "   ❌ 应 0"; FAIL=1; else echo "   ✅ 0"; fi

# ---- ③ 库存 vs 流水 + 逐桶 --------------------------------------------------
DIFF=$(PSQL "
WITH inv AS (SELECT material_id, warehouse_id, SUM(qty) qty FROM biz_inventory GROUP BY 1,2),
     txn AS (SELECT material_id, warehouse_id,
                    SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END) qty
               FROM biz_inventory_txn GROUP BY 1,2)
SELECT count(*) FROM inv FULL JOIN txn USING (material_id, warehouse_id)
 WHERE COALESCE(inv.qty,0) <> COALESCE(txn.qty,0)")
BUCKET=$(PSQL "
WITH cost AS (SELECT i.material_id, i.warehouse_id,
                     COALESCE(il.location_id::text,'(未分配)') bucket, SUM(i.qty) qty
                FROM biz_inventory i LEFT JOIN biz_inventory_location il ON il.id=i.location_id
               GROUP BY 1,2,3),
     loc AS (SELECT material_id, warehouse_id,
                    COALESCE(location_id::text,'(未分配)') bucket, SUM(qty) qty
               FROM biz_inventory_location GROUP BY 1,2,3)
SELECT count(*) FROM cost FULL JOIN loc
  ON cost.material_id=loc.material_id AND cost.warehouse_id=loc.warehouse_id AND cost.bucket=loc.bucket
 WHERE COALESCE(cost.qty,0) <> COALESCE(loc.qty,0)")
echo
echo "③ 库存 vs 流水 差异组：${DIFF:-?}   逐桶成本账 vs 库位账 差异组：${BUCKET:-?}"
if [ "${DIFF:-1}" != "0" ] || [ "${BUCKET:-1}" != "0" ]; then
  echo "   ❌ 存在差异（注意：汇总口径必须两侧先聚合再 FULL JOIN，直接 LEFT JOIN 会因多桶行重复计流水而假红）"
  FAIL=1
else
  echo "   ✅ 0 / 0"
fi

echo
if [ "$FAIL" -ne 0 ]; then
  echo "验收闸：❌ 未通过 —— 不许拿这个库当验收证据，先查上面红项。"
  exit 1
fi
echo "验收闸：✅ 全绿（可作演练/上线验收基线）"
