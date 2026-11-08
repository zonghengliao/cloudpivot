#!/bin/bash
# CODE-06 库侧断言：落库明细 / 形态正则 / 存活无重复 / 号段推进
# 用法：bash 02_db_assert.sh   （实例已 start，01_curl.sh 已跑过）
set -uo pipefail
DB=${DB:-cloudpivot_code06a}
HERE=$(cd "$(dirname "$0")" && pwd)
RAW="$HERE/../raw"
OUT="$RAW/02_db_assert.txt"
: > "$OUT"
FAILED=0
q() { PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -tAc "$1"; }
say() { echo "$@" | tee -a "$OUT"; }
check() { if [ "$2" = "$3" ]; then say "  PASS  $1 = $3"; else say "  FAIL  $1 期望[$2] 实际[$3]"; FAILED=1; fi }
check_like() { if [[ "$3" =~ $2 ]]; then say "  PASS  $1 = $3"; else say "  FAIL  $1 不匹配[$2] 实际[$3]"; FAILED=1; fi }

say "=== ① 号段规则（CODE-01 V0337 提供；本卡只读不改） ==="
say "  biz_seq MAT/WH：$(q "select seq_type||' prefix='||prefix||' sep='||separator||' pad='||pad_width||' current='||current_no from biz_seq where seq_type in ('MAT','WH') order by seq_type" | tr '\n' ' | ')"

say ""
say "=== ② 落库明细：本卡建的三类物料 / 三类仓库 ==="
q "select material_code||' | '||material_name||' | '||material_type from biz_material where material_name like 'CODE06 物料%' order by created_at" | while IFS= read -r line; do say "  $line"; done
q "select warehouse_code||' | '||warehouse_name from biz_warehouse where warehouse_name like 'CODE06 仓库%' order by created_at" | while IFS= read -r line; do say "  $line"; done

say ""
say "=== ③ 自动取号形态（4 位 MAT / 2 位 WH） ==="
MAT_AUTO=$(q "select material_code from biz_material where material_name='CODE06 物料（字段缺席）'")
MAT_NULL=$(q "select material_code from biz_material where material_name='CODE06 物料（null）'")
WH_AUTO=$(q "select warehouse_code from biz_warehouse where warehouse_name='CODE06 仓库（字段缺席）'")
WH_EMPTY=$(q "select warehouse_code from biz_warehouse where warehouse_name='CODE06 仓库（空串）'")
check_like "物料自动号（字段缺席）" '^MAT-[0-9]{4,}$' "$MAT_AUTO"
check_like "物料自动号（null）" '^MAT-[0-9]{4,}$' "$MAT_NULL"
check_like "仓库自动号（字段缺席）" '^WH-[0-9]{2,}$' "$WH_AUTO"
check_like "仓库自动号（空串）" '^WH-[0-9]{2,}$' "$WH_EMPTY"

say ""
say "=== ④ 手工编码原样落库（不转成 MAT-/WH-） ==="
check "物料手工码" "YB-001-02" "$(q "select material_code from biz_material where material_name='CODE06 物料（手工码）'")"
check "仓库手工码" "CK-001" "$(q "select warehouse_code from biz_warehouse where warehouse_name='CODE06 仓库（手工码）'")"

say ""
say "=== ⑤ 未删行内编码无重复（唯一性兜底生效） ==="
check "物料 MAT- 前缀无重复" "0" "$(q "select count(*) from (select material_code from biz_material where deleted_at is null and material_code like 'MAT-%' group by material_code having count(*)>1) t")"
check "仓库 WH- 前缀无重复" "0" "$(q "select count(*) from (select warehouse_code from biz_warehouse where deleted_at is null and warehouse_code like 'WH-%' group by warehouse_code having count(*)>1) t")"

say ""
say "=== ⑥ 预览不占号：号段 current_no 只被「自动取号」推进 ==="
say "  本轮 MAT：自动取号 2 次（MAT-0001 / MAT-0002）+ 手工提交预览号 MAT-0003 ⇒ current_no 应为 2"
say "  本轮 WH ：自动取号 2 次（WH-01 / WH-02）    + 手工提交预览号 WH-03    ⇒ current_no 应为 2"
say "  说明：手工提交（含把预览号填进编码框）走的是「手工编码」分支，不推进号段 —— 这正是「预览不占号」的直接证据。"
check "MAT current_no = 2" "2" "$(q "select current_no from biz_seq where seq_type='MAT'")"
check "WH current_no = 2" "2" "$(q "select current_no from biz_seq where seq_type='WH'")"

say ""
if [ "$FAILED" = "0" ]; then say "RESULT: ALL PASS"; else say "RESULT: FAILED"; fi
exit "$FAILED"
