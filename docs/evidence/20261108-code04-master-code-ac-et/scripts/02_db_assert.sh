#!/bin/bash
# CODE-04 库侧断言（**在 145 上跑**）：curl 建出来的行是不是真落库、号段是不是按预期推进。
#
# 用法：bash 02_db_assert.sh
#   依赖 ../raw/09_ids.env（01_curl.sh 落下的 id 与手工编码）
#   落证：../raw/10_db_summary.txt
set -uo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
RAW="$HERE/../raw"
SUM="$RAW/10_db_summary.txt"
: > "$SUM"
say() { echo "$@" | tee -a "$SUM"; }
FAILED=0
check() { # $1=描述 $2=期望 $3=实际
  if [ "$2" = "$3" ]; then say "  PASS  $1 = $3"; else say "  FAIL  $1 期望[$2] 实际[$3]"; FAILED=1; fi
}

# shellcheck disable=SC1091
. "$RAW/09_ids.env"

PSQL() { su - postgres -c "psql -d ${DB:-cloudpivot_dev} -tAc \"$1\""; }

say "=== ① 建出来的行真落库（按 id 查，不看返回值） ==="
AC1_CODE=$(PSQL "SELECT category_code FROM biz_asset_category WHERE id = '$AC1_ID' AND deleted_at IS NULL")
AC2_CODE=$(PSQL "SELECT category_code FROM biz_asset_category WHERE id = '$AC2_ID' AND deleted_at IS NULL")
AC3_CODE=$(PSQL "SELECT category_code FROM biz_asset_category WHERE id = '$AC3_ID' AND deleted_at IS NULL")
AC4_CODE=$(PSQL "SELECT category_code FROM biz_asset_category WHERE id = '$AC4_ID' AND deleted_at IS NULL")
say "  分类：$AC1_CODE / $AC2_CODE / $AC3_CODE / $AC4_CODE"
if [[ "$AC1_CODE" =~ ^AC-[0-9]{2,}$ ]]; then say "  PASS  AC1 自动号形态"; else say "  FAIL  AC1=[$AC1_CODE]"; FAILED=1; fi
if [[ "$AC2_CODE" =~ ^AC-[0-9]{2,}$ ]]; then say "  PASS  AC2 自动号形态"; else say "  FAIL  AC2=[$AC2_CODE]"; FAILED=1; fi
if [[ "$AC3_CODE" =~ ^AC-[0-9]{2,}$ ]]; then say "  PASS  AC3 自动号形态"; else say "  FAIL  AC3=[$AC3_CODE]"; FAILED=1; fi
check "④ 手工编码原样落库" "$CAT_MANUAL" "$AC4_CODE"

ET1_CODE=$(PSQL "SELECT type_code FROM biz_expense_type WHERE id = '$ET1_ID' AND deleted_at IS NULL")
ET2_CODE=$(PSQL "SELECT type_code FROM biz_expense_type WHERE id = '$ET2_ID' AND deleted_at IS NULL")
ET3_CODE=$(PSQL "SELECT type_code FROM biz_expense_type WHERE id = '$ET3_ID' AND deleted_at IS NULL")
ET4_CODE=$(PSQL "SELECT type_code FROM biz_expense_type WHERE id = '$ET4_ID' AND deleted_at IS NULL")
say "  类型：$ET1_CODE / $ET2_CODE / $ET3_CODE / $ET4_CODE"
if [[ "$ET1_CODE" =~ ^ET-[0-9]{2,}$ ]]; then say "  PASS  ET1 自动号形态"; else say "  FAIL  ET1=[$ET1_CODE]"; FAILED=1; fi
if [[ "$ET2_CODE" =~ ^ET-[0-9]{2,}$ ]]; then say "  PASS  ET2 自动号形态"; else say "  FAIL  ET2=[$ET2_CODE]"; FAILED=1; fi
if [[ "$ET3_CODE" =~ ^ET-[0-9]{2,}$ ]]; then say "  PASS  ET3 自动号形态"; else say "  FAIL  ET3=[$ET3_CODE]"; FAILED=1; fi
check "④ 手工编码原样落库" "$TYPE_MANUAL" "$ET4_CODE"

say ""
say "=== ② 存量数据没被改（卡面硬要求） ==="
LEGACY_AC=$(PSQL "SELECT count(*) FROM biz_asset_category WHERE deleted_at IS NULL AND category_code IN ('ELEC','FURNITURE','BUILDING','VEHICLE','MACHINE')")
check "存量 5 条标准分类仍在" "5" "$LEGACY_AC"
LEGACY_ET=$(PSQL "SELECT count(*) FROM biz_expense_type WHERE deleted_at IS NULL AND type_code IN ('TRAVEL','MEAL','OFFICE','COMM','TRAIN','WELFARE','OTHER','PENALTY')")
check "存量 8 条标准费用类型仍在" "8" "$LEGACY_ET"

say ""
say "=== ③ 号段推进（本次消耗 3 个 AC + 3 个 ET） ==="
AC_NO=$(PSQL "SELECT current_no FROM biz_seq WHERE seq_type='AC' AND deleted_at IS NULL")
ET_NO=$(PSQL "SELECT current_no FROM biz_seq WHERE seq_type='ET' AND deleted_at IS NULL")
say "  biz_seq：AC.current_no=$AC_NO  ET.current_no=$ET_NO"
check "AC 号段推进 = 3（三次留空取号）" "3" "$AC_NO"
check "ET 号段推进 = 3（三次留空取号）" "3" "$ET_NO"
AC_PAD=$(PSQL "SELECT pad_width FROM biz_seq WHERE seq_type='AC' AND deleted_at IS NULL")
ET_PAD=$(PSQL "SELECT pad_width FROM biz_seq WHERE seq_type='ET' AND deleted_at IS NULL")
check "AC pad_width = 2（AC-01 形态）" "2" "$AC_PAD"
check "ET pad_width = 2（ET-01 形态）" "2" "$ET_PAD"

say ""
if [ "$FAILED" = "0" ]; then say "RESULT: ALL PASS"; else say "RESULT: 有 FAIL，见上"; fi

# 把实际落库的编码回写给清理脚本
{
  echo "AC1_CODE=$AC1_CODE"; echo "AC2_CODE=$AC2_CODE"; echo "AC3_CODE=$AC3_CODE"
  echo "ET1_CODE=$ET1_CODE"; echo "ET2_CODE=$ET2_CODE"; echo "ET3_CODE=$ET3_CODE"
} >> "$RAW/09_ids.env"

exit "$FAILED"
