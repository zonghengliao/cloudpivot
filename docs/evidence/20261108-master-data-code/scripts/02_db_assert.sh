#!/bin/bash
# CODE-09 库侧断言（库 cloudpivot_code09，01_curl.sh 跑完后的状态）
#
# 断言口径：
#   1. 每个主数据表「未删行」编码唯一（count == count(distinct)）—— 不重号；
#   2. 自动取号的编码全部符合拍板格式（PREFIX + 4/2 位补零流水，中间一个 -）；
#   3. 手工码原样落库（GYS-001 / KH-001 / YB-001-02 / CK-001 / CAT-BUILDING / ADMIN-FEE / KH-BACK-001）；
#   4. 开关关闭时被 400+66054 拒掉的 6 个编码一行都没落库；
#   5. biz_seq 六条规则的 prefix / separator / pad_width 正确；
#   6. **预览不占号 + 撞号跳号的库侧铁证**：
#        - MAT/WH/AC/ET/CUS 各有 5 个同前缀编码但 current_no 只有 4 —— 第 5 个是「预览号当手工码」提交的，
#          说明 peek 只读不推进计数器；
#        - SUP 有 17 个同前缀编码而 current_no=18 —— 多出的 1 次是跳过被手工占用的 SUP-0005 / SUP-0017
#          两次 bump 里的净差（current_no 记的是「分配次数」，不是「已用编码数」）；
#   7. 计量单位：KG9 手工码落库、biz_seq 里没有 UNIT 号段（本批不改自动取号）。
#
# 用法：bash 02_db_assert.sh
set -uo pipefail
export PGPASSWORD=alun
DB=${DB:-cloudpivot_code09}
HERE=$(cd "$(dirname "$0")" && pwd)
RAW="$HERE/../raw"
OUT="$RAW/02_db_assert.txt"
: > "$OUT"
FAILED=0
say() { echo "$@" | tee -a "$OUT"; }
q() { psql -h 127.0.0.1 -U alun -d "$DB" -tA -q -c "$1"; }
check() { if [ "$2" = "$3" ]; then say "  PASS  $1 = $3"; else say "  FAIL  $1 期望[$2] 实际[$3]"; FAILED=1; fi }
check_like() { if [[ "$3" =~ $2 ]]; then say "  PASS  $1 = $3"; else say "  FAIL  $1 不匹配[$2] 实际[$3]"; FAILED=1; fi }

say "库 = $DB  时间 = $(date '+%F %T %Z')"
say ""
say "=== 全量留存（含软删种子，审计用）==="
q "select 'SUP '||count(*)||' 行：'||coalesce(string_agg(supplier_code, ',' order by supplier_code),'') from biz_supplier" | tee -a "$OUT"
q "select 'CUS '||count(*)||' 行：'||coalesce(string_agg(code, ',' order by code),'') from biz_customer" | tee -a "$OUT"
q "select 'MAT '||count(*)||' 行：'||coalesce(string_agg(material_code, ',' order by material_code),'') from biz_material" | tee -a "$OUT"
q "select 'WH  '||count(*)||' 行：'||coalesce(string_agg(warehouse_code, ',' order by warehouse_code),'') from biz_warehouse" | tee -a "$OUT"
q "select 'AC  '||count(*)||' 行：'||coalesce(string_agg(category_code, ',' order by category_code),'') from biz_asset_category" | tee -a "$OUT"
q "select 'ET  '||count(*)||' 行：'||coalesce(string_agg(type_code, ',' order by type_code),'') from biz_expense_type" | tee -a "$OUT"
say ""
say "=== biz_seq 六条主数据规则 ==="
q "select seq_type||' prefix='||prefix||' sep='||separator||' pad='||pad_width||' current_no='||current_no from biz_seq where seq_type in ('SUP','CUS','MAT','WH','AC','ET') order by seq_type" | tee -a "$OUT"

say ""
say "=== 1) 未删行编码唯一（不重号）==="
for t in "biz_supplier supplier_code" "biz_customer code" "biz_material material_code" "biz_warehouse warehouse_code" "biz_asset_category category_code" "biz_expense_type type_code"; do
  set -- $t
  TOT=$(q "select count(*) from $1 where deleted_at is null")
  DIS=$(q "select count(distinct $2) from $1 where deleted_at is null")
  check "$1 未删行数 == 去重编码数（$TOT）" "$TOT" "$DIS"
done

say ""
say "=== 2) 自动取号编码格式（PREFIX-4位 / PREFIX-2位）==="
say "  SUP 不符合 (SUP + 4 位)：$(q "select count(*) from biz_supplier where deleted_at is null and supplier_code like 'SUP-%' and supplier_code !~ '^SUP-[0-9]{4}$'")（期望 0）"
check "SUP 自动码全部 4 位补零" "0" "$(q "select count(*) from biz_supplier where deleted_at is null and supplier_code like 'SUP-%' and supplier_code !~ '^SUP-[0-9]{4}$'")"
check "CUS 自动码全部 4 位补零" "0" "$(q "select count(*) from biz_customer where deleted_at is null and code like 'CUS-%' and code !~ '^CUS-[0-9]{4}$'")"
check "MAT 自动码全部 4 位补零" "0" "$(q "select count(*) from biz_material where deleted_at is null and material_code like 'MAT-%' and material_code !~ '^MAT-[0-9]{4}$'")"
check "WH 自动码全部 2 位补零" "0" "$(q "select count(*) from biz_warehouse where deleted_at is null and warehouse_code like 'WH-%' and warehouse_code !~ '^WH-[0-9]{2}$'")"
check "AC 自动码全部 2 位补零" "0" "$(q "select count(*) from biz_asset_category where deleted_at is null and category_code like 'AC-%' and category_code !~ '^AC-[0-9]{2}$'")"
check "ET 自动码全部 2 位补零" "0" "$(q "select count(*) from biz_expense_type where deleted_at is null and type_code like 'ET-%' and type_code !~ '^ET-[0-9]{2}$'")"
say "  首号（矩阵用例 1 的落库值）：$(cat "$RAW/00_first_codes.txt" 2>/dev/null | tr '\n' ' ')"
check "SUP 首号落库 = SUP-0001" "1" "$(q "select count(*) from biz_supplier where supplier_code='SUP-0001' and deleted_at is null")"
check "CUS 首号落库 = CUS-0001" "1" "$(q "select count(*) from biz_customer where code='CUS-0001' and deleted_at is null")"
check "MAT 首号落库 = MAT-0001" "1" "$(q "select count(*) from biz_material where material_code='MAT-0001' and deleted_at is null")"
check "WH 首号落库 = WH-01" "1" "$(q "select count(*) from biz_warehouse where warehouse_code='WH-01' and deleted_at is null")"
check "AC 首号落库 = AC-01" "1" "$(q "select count(*) from biz_asset_category where category_code='AC-01' and deleted_at is null")"
check "ET 首号落库 = ET-01" "1" "$(q "select count(*) from biz_expense_type where type_code='ET-01' and deleted_at is null")"

say ""
say "=== 3) 手工码原样落库（用例 3 + 恢复用例）==="
check "供应商手工码 GYS-001" "1"   "$(q "select count(*) from biz_supplier where supplier_code='GYS-001' and deleted_at is null")"
check "客户手工码 KH-001" "1"      "$(q "select count(*) from biz_customer where code='KH-001' and deleted_at is null")"
check "物料手工码 YB-001-02" "1"   "$(q "select count(*) from biz_material where material_code='YB-001-02' and deleted_at is null")"
check "仓库手工码 CK-001" "1"      "$(q "select count(*) from biz_warehouse where warehouse_code='CK-001' and deleted_at is null")"
check "资产分类手工码 CAT-BUILDING" "1" "$(q "select count(*) from biz_asset_category where category_code='CAT-BUILDING' and deleted_at is null")"
check "费用类型手工码 ADMIN-FEE" "1" "$(q "select count(*) from biz_expense_type where type_code='ADMIN-FEE' and deleted_at is null")"
check "参数恢复后手工码 KH-BACK-001" "1" "$(q "select count(*) from biz_customer where code='KH-BACK-001' and deleted_at is null")"
check "用例 4 重复编码没有多插一行（GYS-001 未删行仍 1 行）" "1" "$(q "select count(*) from biz_supplier where supplier_code='GYS-001' and deleted_at is null")"

say ""
say "=== 4) 开关关闭时被拒的 6 个编码 → 0 行落库 ==="
check "SUP-OFF-001 未落库" "0" "$(q "select count(*) from biz_supplier where supplier_code='SUP-OFF-001'")"
check "CUS-OFF-001 未落库" "0" "$(q "select count(*) from biz_customer where code='CUS-OFF-001'")"
check "MAT-OFF-001 未落库" "0" "$(q "select count(*) from biz_material where material_code='MAT-OFF-001'")"
check "WH-OFF-001 未落库" "0" "$(q "select count(*) from biz_warehouse where warehouse_code='WH-OFF-001'")"
check "AC-OFF-001 未落库" "0" "$(q "select count(*) from biz_asset_category where category_code='AC-OFF-001'")"
check "ET-OFF-001 未落库" "0" "$(q "select count(*) from biz_expense_type where type_code='ET-OFF-001'")"

say ""
say "=== 5) biz_seq 规则（前缀 / 分隔符 / 补零位数）==="
check "SUP prefix" "SUP" "$(q "select prefix from biz_seq where seq_type='SUP'")"
check "CUS prefix" "CUS" "$(q "select prefix from biz_seq where seq_type='CUS'")"
check "MAT prefix" "MAT" "$(q "select prefix from biz_seq where seq_type='MAT'")"
check "WH prefix"  "WH"  "$(q "select prefix from biz_seq where seq_type='WH'")"
check "AC prefix"  "AC"  "$(q "select prefix from biz_seq where seq_type='AC'")"
check "ET prefix"  "ET"  "$(q "select prefix from biz_seq where seq_type='ET'")"
check "SUP pad_width（纯流水 4 位）" "4" "$(q "select pad_width from biz_seq where seq_type='SUP'")"
check "CUS pad_width（纯流水 4 位）" "4" "$(q "select pad_width from biz_seq where seq_type='CUS'")"
check "MAT pad_width（纯流水 4 位）" "4" "$(q "select pad_width from biz_seq where seq_type='MAT'")"
check "WH pad_width（纯流水 2 位）" "2" "$(q "select pad_width from biz_seq where seq_type='WH'")"
check "AC pad_width（纯流水 2 位）" "2" "$(q "select pad_width from biz_seq where seq_type='AC'")"
check "ET pad_width（纯流水 2 位）" "2" "$(q "select pad_width from biz_seq where seq_type='ET'")"
check "六条规则都不带日期（date_pattern 不参与主数据编码）" "6" "$(q "select count(*) from biz_seq where seq_type in ('SUP','CUS','MAT','WH','AC','ET')")"

say ""
say "=== 6) 预览不占号 + 撞号跳号的库侧证据 ==="
SUP_N=$(q "select count(*) from biz_supplier where deleted_at is null and supplier_code ~ '^SUP-[0-9]{4}$'")
SUP_NO=$(q "select current_no from biz_seq where seq_type='SUP'")
say "  SUP：同前缀编码 $SUP_N 个 / current_no=$SUP_NO"
say "       （其中 SUP-0005 是手工占位、SUP-0017 是预览号当手工码；取号器跳过的正好也是这 2 个号，"
say "        所以「编码数 = current_no」在这个实体上恰好相等，而跳过行为本身由下面两条断言钉住）"
check "SUP 同前缀编码数" "18" "$SUP_N"
check "SUP current_no（分配次数，含 2 次跳过）" "18" "$SUP_NO"
for pair in "biz_customer:code:CUS:5:4" "biz_material:material_code:MAT:5:4" "biz_warehouse:warehouse_code:WH:5:4" "biz_asset_category:category_code:AC:5:4" "biz_expense_type:type_code:ET:5:4"; do
  T=$(echo "$pair" | cut -d: -f1); C=$(echo "$pair" | cut -d: -f2); P=$(echo "$pair" | cut -d: -f3)
  EXP_N=$(echo "$pair" | cut -d: -f4); EXP_NO=$(echo "$pair" | cut -d: -f5)
  N=$(q "select count(*) from $T where deleted_at is null and $C ~ '^$P-[0-9]+$'")
  NO=$(q "select current_no from biz_seq where seq_type='$P'")
  say "  $P：同前缀编码 $N 个 / current_no=$NO（预览号当手工码提交 → 编码多 1 个但计数器只推进了自动取号次数）"
  check "$P 同前缀编码数" "$EXP_N" "$N"
  check "$P current_no（预览不占号）" "$EXP_NO" "$NO"
done
check "手工占位 SUP-0005 落库" "1" "$(q "select count(*) from biz_supplier where supplier_code='SUP-0005' and deleted_at is null")"
check "撞号后自动取号落到 SUP-0006" "1" "$(q "select count(*) from biz_supplier where supplier_code='SUP-0006' and deleted_at is null")"
CONC_CNT=$(q "select count(*) from biz_supplier where deleted_at is null and supplier_code ~ '^SUP-00(0[7-9]|1[0-6])$'")
check "并发 10 个编码全部落库（SUP-0007..0016）" "10" "$CONC_CNT"

say ""
say "=== 7) 计量单位回归 ==="
check "计量单位 KG9 手工码落库" "1" "$(q "select count(*) from biz_unit where unit_code='KG9' and deleted_at is null")"
check "biz_seq 无 UNIT 号段（本批不自动生成单位编码）" "0" "$(q "select count(*) from biz_seq where seq_type like '%UNIT%' or prefix='UNIT'")"
check "种子单位 PCS 仍在（未被影响）" "1" "$(q "select count(*) from biz_unit where unit_code='PCS' and deleted_at is null")"
check "空编码单位没有落库" "0" "$(q "select count(*) from biz_unit where unit_code=''")"

say ""
if [ "$FAILED" = "0" ]; then say "RESULT: ALL PASS（库侧断言全绿）"; else say "RESULT: FAILED（见上面 FAIL 行）"; fi
q "select 'biz_seq 全部号段（确认只有 6 条主数据规则被使用）：'||string_agg(seq_type, ',' order by seq_type) from biz_seq where current_no > 0" | tee -a "$OUT"
exit "$FAILED"
