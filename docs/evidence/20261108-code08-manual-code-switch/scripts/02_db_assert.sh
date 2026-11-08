#!/bin/bash
# CODE-08 库侧断言（同一个库 cloudpivot_code08d，curl 跑完之后执行）
#
# 断言的是「HTTP 结果背后真的这么落了库」：
#   ① 参数行与迁移 V0342 的口径一致（值 / 类型 / 分类 / 可修改 / 启用 / 选项）
#   ② 参数分类字典项 `erp_master_data` 在（配置页页签译名靠它）
#   ③ 开关为 true 时提交的 6 个手工码**真的落库**（原样，不改写）
#   ④ 开关为 false 时提交的 6 个手工码**一条都不许落库**（400 是真的没写）
#   ⑤ 留空提交的 6 行是系统号段产物（形态正确）
#   ⑥ 手工码提交**不推进** biz_seq（号段只因自动取号前进）
#
# 用法：bash 02_db_assert.sh
set -uo pipefail

DB=${DB:-cloudpivot_code08d}
PSQL=(env PGPASSWORD="${PGPASSWORD:-alun}" psql -h 127.0.0.1 -U "${PGUSER:-alun}" -d "$DB" -Atq)
HERE=$(cd "$(dirname "$0")" && pwd)
RAW="$HERE/../raw"
mkdir -p "$RAW"
OUT="$RAW/02_db_assert.txt"
: > "$OUT"

say() { echo "$@" | tee -a "$OUT"; }
FAILED=0
q() { "${PSQL[@]}" -c "$1"; }
check() { # $1=描述 $2=期望 $3=实际
  if [ "$2" = "$3" ]; then say "  PASS  $1 = $3"; else say "  FAIL  $1 期望[$2] 实际[$3]"; FAILED=1; fi
}

say "== CODE-08 库侧断言（db=$DB） =="

say ""
say "-- ① 参数行（V0342：默认 true / BOOLEAN / erp_master_data / 可修改） --"
check "参数值" "true" "$(q "SELECT config_value FROM sys_config WHERE config_key='erp.master_data.allow_manual_code' AND deleted_at IS NULL")"
check "参数类型" "BOOLEAN" "$(q "SELECT config_type FROM sys_config WHERE config_key='erp.master_data.allow_manual_code'")"
check "分类" "erp_master_data" "$(q "SELECT category FROM sys_config WHERE config_key='erp.master_data.allow_manual_code'")"
check "可修改" "t" "$(q "SELECT is_modifiable FROM sys_config WHERE config_key='erp.master_data.allow_manual_code'")"
check "未加密" "f" "$(q "SELECT is_encrypted FROM sys_config WHERE config_key='erp.master_data.allow_manual_code'")"
check "启用" "1" "$(q "SELECT status FROM sys_config WHERE config_key='erp.master_data.allow_manual_code'")"
check "排序" "10" "$(q "SELECT sort_order FROM sys_config WHERE config_key='erp.master_data.allow_manual_code'")"
check "选项" "true|开启,false|关闭" "$(q "SELECT select_value FROM sys_config WHERE config_key='erp.master_data.allow_manual_code'")"
check "固定 id（幂等靠它）" "e2b00000-0000-4000-8000-000000000020" "$(q "SELECT id FROM sys_config WHERE config_key='erp.master_data.allow_manual_code'")"
check "同名行只有 1 条（无重复种入）" "1" "$(q "SELECT count(*) FROM sys_config WHERE config_key='erp.master_data.allow_manual_code' AND deleted_at IS NULL")"

say ""
say "-- ② 参数分类字典项 --"
check "字典项 erp_master_data" "1" "$(q "SELECT count(*) FROM sys_dict_item i JOIN sys_dict_type t ON t.id=i.dict_type_id WHERE t.dict_code='config_category' AND t.deleted_at IS NULL AND i.item_value='erp_master_data' AND i.deleted_at IS NULL")"
check "字典项名称" "ERP 主数据" "$(q "SELECT i.item_label FROM sys_dict_item i JOIN sys_dict_type t ON t.id=i.dict_type_id WHERE t.dict_code='config_category' AND i.item_value='erp_master_data'")"

say ""
say "-- ③ 开关 true 时提交的手工码：真的落库且原样 --"
check "供应商 GYS-001" "1" "$(q "SELECT count(*) FROM biz_supplier WHERE supplier_code='GYS-001' AND deleted_at IS NULL")"
check "客户 KH-001" "1" "$(q "SELECT count(*) FROM biz_customer WHERE code='KH-001' AND deleted_at IS NULL")"
check "物料 YB-001-02" "1" "$(q "SELECT count(*) FROM biz_material WHERE material_code='YB-001-02' AND deleted_at IS NULL")"
check "仓库 CK-001" "1" "$(q "SELECT count(*) FROM biz_warehouse WHERE warehouse_code='CK-001' AND deleted_at IS NULL")"
check "资产分类 CAT-BUILDING" "1" "$(q "SELECT count(*) FROM biz_asset_category WHERE category_code='CAT-BUILDING' AND deleted_at IS NULL")"
check "费用类型 ADMIN-FEE" "1" "$(q "SELECT count(*) FROM biz_expense_type WHERE type_code='ADMIN-FEE' AND deleted_at IS NULL")"
check "供应商 GYS-BACK-001（改回 true 后）" "1" "$(q "SELECT count(*) FROM biz_supplier WHERE supplier_code='GYS-BACK-001' AND deleted_at IS NULL")"
check "资产分类 CAT-BACK-001（改回 true 后）" "1" "$(q "SELECT count(*) FROM biz_asset_category WHERE category_code='CAT-BACK-001' AND deleted_at IS NULL")"

say ""
say "-- ④ 开关 false 时提交的手工码：一条都没落库（400 = 真的没写） --"
check "供应商 GYS-OFF-001 不存在" "0" "$(q "SELECT count(*) FROM biz_supplier WHERE supplier_code='GYS-OFF-001'")"
check "客户 KH-OFF-001 不存在" "0" "$(q "SELECT count(*) FROM biz_customer WHERE code='KH-OFF-001'")"
check "物料 MAT-OFF-001 不存在" "0" "$(q "SELECT count(*) FROM biz_material WHERE material_code='MAT-OFF-001'")"
check "仓库 WH-OFF-001 不存在" "0" "$(q "SELECT count(*) FROM biz_warehouse WHERE warehouse_code='WH-OFF-001'")"
check "资产分类 CAT-OFF-001 不存在" "0" "$(q "SELECT count(*) FROM biz_asset_category WHERE category_code='CAT-OFF-001'")"
check "费用类型 ET-OFF-001 不存在" "0" "$(q "SELECT count(*) FROM biz_expense_type WHERE type_code='ET-OFF-001'")"

say ""
say "-- ⑤ 留空提交的 6 行是号段产物（形态对；只数本卡造的 CODE08* 行） --"
# 只数本脚本造的行：种子库里本来就有 SUP-0001 / CUS-0001..0003，用「形态」数会把它们算进来
check "供应商自动码形态" "1" "$(q "SELECT count(*) FROM biz_supplier WHERE supplier_name LIKE 'CODE08%' AND supplier_code ~ '^SUP-[0-9]{4,}$' AND deleted_at IS NULL")"
check "客户自动码形态" "1" "$(q "SELECT count(*) FROM biz_customer WHERE name LIKE 'CODE08%' AND code ~ '^CUS-[0-9]{4,}$' AND deleted_at IS NULL")"
check "物料自动码形态" "1" "$(q "SELECT count(*) FROM biz_material WHERE material_name LIKE 'CODE08%' AND material_code ~ '^MAT-[0-9]{4,}$' AND deleted_at IS NULL")"
check "仓库自动码形态" "1" "$(q "SELECT count(*) FROM biz_warehouse WHERE warehouse_name LIKE 'CODE08%' AND warehouse_code ~ '^WH-[0-9]{2,}$' AND deleted_at IS NULL")"
check "资产分类自动码形态" "1" "$(q "SELECT count(*) FROM biz_asset_category WHERE category_name LIKE 'CODE08%' AND category_code ~ '^AC-[0-9]{2,}$' AND deleted_at IS NULL")"
check "费用类型自动码形态" "1" "$(q "SELECT count(*) FROM biz_expense_type WHERE type_name LIKE 'CODE08%' AND type_code ~ '^ET-[0-9]{2,}$' AND deleted_at IS NULL")"

say ""
say "-- ⑥ 手工码提交不推进 biz_seq（每类只前进了 1 号 = 只有那次自动取号） --"
say "     biz_seq：$(q "SELECT string_agg(seq_type||'='||current_no, ' ' ORDER BY seq_type) FROM biz_seq WHERE seq_type IN ('SUP','CUS','MAT','WH','AC','ET')")"
check "SUP current_no（种子 1 + 自动 1）" "2" "$(q "SELECT current_no FROM biz_seq WHERE seq_type='SUP'")"
check "MAT current_no（自动 1 次）" "1" "$(q "SELECT current_no FROM biz_seq WHERE seq_type='MAT'")"
check "WH current_no（自动 1 次）" "1" "$(q "SELECT current_no FROM biz_seq WHERE seq_type='WH'")"
check "AC current_no（自动 1 次）" "1" "$(q "SELECT current_no FROM biz_seq WHERE seq_type='AC'")"
check "ET current_no（自动 1 次）" "1" "$(q "SELECT current_no FROM biz_seq WHERE seq_type='ET'")"

say ""
say "-- ⑦ 存量行没有因本卡产生重复编码 --"
check "供应商存活行编码无重复" "0" "$(q "SELECT count(*) FROM (SELECT supplier_code FROM biz_supplier WHERE deleted_at IS NULL GROUP BY 1 HAVING count(*)>1) t")"
check "物料存活行编码无重复" "0" "$(q "SELECT count(*) FROM (SELECT material_code FROM biz_material WHERE deleted_at IS NULL GROUP BY 1 HAVING count(*)>1) t")"
check "仓库存活行编码无重复" "0" "$(q "SELECT count(*) FROM (SELECT warehouse_code FROM biz_warehouse WHERE deleted_at IS NULL GROUP BY 1 HAVING count(*)>1) t")"
check "资产分类存活行编码无重复" "0" "$(q "SELECT count(*) FROM (SELECT category_code FROM biz_asset_category WHERE deleted_at IS NULL GROUP BY 1 HAVING count(*)>1) t")"

say ""
if [ "$FAILED" = "0" ]; then say "RESULT: ALL PASS"; else say "RESULT: FAILED（见上面 FAIL 行）"; fi
exit "$FAILED"
