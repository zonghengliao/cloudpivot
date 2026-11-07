#!/usr/bin/env bash
# ①② 建资产分类 + 资产购入（台账 + 购入凭证）
set -uo pipefail
source /opt/cloudpivot/docs/evidence/20261107-p004-fixed-asset-chain/00_env.sh
ADMIN=$(cat "$EV/raw/admin.token")
DEPT=$(cat "$EV/raw/dept.id")

ACC_ASSET=8423d3f0-e8c5-44e6-ae0a-86f8606b4913   # 1601 固定资产
ACC_ACCUM=e7ca2e07-a8ae-4b12-935f-ba08d56b7e87   # 1602 累计折旧
ACC_EXP=117bcd6e-6ff9-4c7c-b5da-a65b387a6e73     # 5602.01 管理费用-折旧费

echo "===== ①-A 建分类 DRILL_EQ（60 月 / 残值率 5%）====="
api POST "/business/erp/asset-category" "$ADMIN" "{\"category_code\":\"DRILL_EQ\",\"category_name\":\"演练设备（60月）\",\"useful_life_months\":60,\"salvage_rate\":0.05,\"asset_account_id\":\"$ACC_ASSET\",\"accum_account_id\":\"$ACC_ACCUM\",\"expense_account_id\":\"$ACC_EXP\",\"status\":1,\"remark\":\"P0-04 演练分类\"}" > "$EV/raw/c1_cat.json"
echo "http=$HTTP $(jq -c '{code,msg}' "$EV/raw/c1_cat.json")"
CAT=$(jq -r '.data.id // empty' "$EV/raw/c1_cat.json")
[ -n "$CAT" ] || { echo "FATAL: 分类创建失败"; cat "$EV/raw/c1_cat.json"; exit 1; }
echo "$CAT" > "$EV/raw/cat.id"

echo "===== ①-B 建分类 DRILL_3M（3 月 / 残值率 5%，用来验「提足即停 + 尾差补齐」）====="
api POST "/business/erp/asset-category" "$ADMIN" "{\"category_code\":\"DRILL_3M\",\"category_name\":\"演练设备（3月尾差）\",\"useful_life_months\":3,\"salvage_rate\":0.05,\"asset_account_id\":\"$ACC_ASSET\",\"accum_account_id\":\"$ACC_ACCUM\",\"expense_account_id\":\"$ACC_EXP\",\"status\":1,\"remark\":\"P0-04 尾差演练分类\"}" > "$EV/raw/c1_cat3.json"
echo "http=$HTTP $(jq -c '{code,msg}' "$EV/raw/c1_cat3.json")"
CAT3=$(jq -r '.data.id // empty' "$EV/raw/c1_cat3.json")
echo "$CAT3" > "$EV/raw/cat3.id"

echo "===== ①-C 分类落库核对 ====="
psqlq "select category_code, useful_life_months, salvage_rate, status from biz_asset_category where category_code in ('DRILL_EQ','DRILL_3M') order by category_code"

echo "===== ②-A 资产购入：演练设备A 原值 120000（购入日 2025-12-01 ⇒ 起提 2026-01）====="
api POST "/business/erp/asset" "$ADMIN" "{\"asset_name\":\"演练设备A\",\"category_id\":\"$CAT\",\"spec\":\"P004-A\",\"use_dept_id\":\"$DEPT\",\"location\":\"演练车间 1 号位\",\"original_value\":\"120000\",\"tax_amount\":\"0\",\"purchase_date\":\"2025-12-01\",\"source_type\":\"MANUAL\",\"buy_mode\":\"DIRECT_BUY\",\"pay_mode\":\"CREDIT\",\"remark\":\"P0-04 演练资产 A\"}" > "$EV/raw/c2_asset.json"
echo "http=$HTTP $(jq -c '{code,msg,data}' "$EV/raw/c2_asset.json")"
ASSET=$(jq -r '.data.id // empty' "$EV/raw/c2_asset.json")
[ -n "$ASSET" ] || { echo "FATAL: 建卡失败"; cat "$EV/raw/c2_asset.json"; exit 1; }
echo "$ASSET" > "$EV/raw/asset.id"

echo "===== ②-B 资产购入：演练设备B 原值 1000（3 月期，验尾差）====="
api POST "/business/erp/asset" "$ADMIN" "{\"asset_name\":\"演练设备B\",\"category_id\":\"$CAT3\",\"spec\":\"P004-B\",\"use_dept_id\":\"$DEPT\",\"original_value\":\"1000\",\"tax_amount\":\"0\",\"purchase_date\":\"2025-12-01\",\"source_type\":\"MANUAL\",\"buy_mode\":\"DIRECT_BUY\",\"pay_mode\":\"CREDIT\",\"remark\":\"P0-04 演练资产 B（3 月尾差）\"}" > "$EV/raw/c2_asset_b.json"
echo "http=$HTTP $(jq -c '{code,msg,data}' "$EV/raw/c2_asset_b.json")"
ASSETB=$(jq -r '.data.id // empty' "$EV/raw/c2_asset_b.json")
echo "$ASSETB" > "$EV/raw/assetb.id"

echo "===== ②-C 资产卡片落库核对（金额字段全部打出来）====="
psqlq "select asset_no, asset_name, status, original_value, residual_value, depreciable_base, monthly_depreciation, useful_life_months, salvage_rate, start_depreciation_date, accumulated_depreciation, net_value, depreciation_months_done, last_depreciation_period, purchase_date, category_code, voucher_id from biz_fixed_asset order by asset_no"
