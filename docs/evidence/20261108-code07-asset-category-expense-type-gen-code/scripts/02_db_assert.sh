#!/bin/bash
# CODE-07 库侧断言（99 本机，库 cloudpivot_code07e2e）：curl 建出来的行是不是真落库、号段是不是按预期推进。
#
# 用法：bash 02_db_assert.sh
#   依赖 ../raw/09_fixtures.env（01_curl.sh 落下的 id / 编码）
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
. "$RAW/09_fixtures.env"

PSQL() { su - postgres -c "psql -d ${DB:-cloudpivot_code07e2e} -tAc \"$1\""; }

say "=== ① 建出来的行真落库（按 id / 编码直查表，不看接口返回值） ==="
AC1_CODE=$(PSQL "SELECT category_code FROM biz_asset_category WHERE id = '$AC_A1_ID' AND deleted_at IS NULL")
say "  AC_A1_ID → $AC1_CODE（② 字段缺席自动取号那行）"
if [[ "$AC1_CODE" =~ ^AC-[0-9]{2}$ ]]; then say "  PASS  AC 自动号形态（2 位纯流水）"; else say "  FAIL  AC1=[$AC1_CODE]"; FAILED=1; fi

AC2_HIT=$(PSQL "SELECT count(*) FROM biz_asset_category WHERE category_code = '$AC_A2' AND deleted_at IS NULL")
check "② 前端发 null 那行落库（$AC_A2）" "1" "$AC2_HIT"

AC_M_HIT=$(PSQL "SELECT category_code FROM biz_asset_category WHERE category_code = '$AC_MANUAL' AND deleted_at IS NULL")
check "③ 手工编码原样落库（小写提交 → 大写落库）" "$AC_MANUAL" "$AC_M_HIT"

AC_U_HIT=$(PSQL "SELECT category_code FROM biz_asset_category WHERE category_code = '$AC_U' AND deleted_at IS NULL")
check "④ 预览号当手工码建单，原样落库" "$AC_U" "$AC_U_HIT"

ET1_CODE=$(PSQL "SELECT type_code FROM biz_expense_type WHERE id = '$ET_A1_ID' AND deleted_at IS NULL")
say "  ET_A1_ID → $ET1_CODE（② 字段缺席自动取号那行）"
if [[ "$ET1_CODE" =~ ^ET-[0-9]{2}$ ]]; then say "  PASS  ET 自动号形态（2 位纯流水）"; else say "  FAIL  ET1=[$ET1_CODE]"; FAILED=1; fi

ET_M_HIT=$(PSQL "SELECT type_code FROM biz_expense_type WHERE type_code = '$ET_MANUAL' AND deleted_at IS NULL")
check "③ 手工编码原样落库（$ET_MANUAL）" "$ET_MANUAL" "$ET_M_HIT"

ET_U_HIT=$(PSQL "SELECT type_code FROM biz_expense_type WHERE type_code = '$ET_U' AND deleted_at IS NULL")
check "④ 预览号当手工码建单，原样落库" "$ET_U" "$ET_U_HIT"

say ""
say "=== ② 存量种子数据没被改（卡面硬要求：不动已有档案） ==="
LEGACY_AC=$(PSQL "SELECT count(*) FROM biz_asset_category WHERE deleted_at IS NULL AND category_code IN ('ELEC','FURNITURE','BUILDING','VEHICLE','MACHINE')")
check "存量 5 条标准分类仍在" "5" "$LEGACY_AC"
LEGACY_ET=$(PSQL "SELECT count(*) FROM biz_expense_type WHERE deleted_at IS NULL AND type_code IN ('TRAVEL','MEAL','OFFICE','COMM','TRAIN','WELFARE','OTHER','PENALTY')")
check "存量 8 条标准费用类型仍在" "8" "$LEGACY_ET"

say ""
say "=== ③ 号段推进（自动取号 2 次；第 3 个号是手工路径提交，不该推进计数器） ==="
AC_NO=$(PSQL "SELECT current_no FROM biz_seq WHERE seq_type='AC' AND deleted_at IS NULL")
ET_NO=$(PSQL "SELECT current_no FROM biz_seq WHERE seq_type='ET' AND deleted_at IS NULL")
AC_PAD=$(PSQL "SELECT pad_width FROM biz_seq WHERE seq_type='AC' AND deleted_at IS NULL")
ET_PAD=$(PSQL "SELECT pad_width FROM biz_seq WHERE seq_type='ET' AND deleted_at IS NULL")
say "  biz_seq：AC.current_no=$AC_NO pad=${AC_PAD}  ET.current_no=$ET_NO pad=${ET_PAD}"
check "AC 号段 = 自动取号次数 2" "2" "$AC_NO"
check "ET 号段 = 自动取号次数 2" "2" "$ET_NO"
check "AC pad_width = 2（AC-01 两位）" "2" "$AC_PAD"
check "ET pad_width = 2（ET-01 两位）" "2" "$ET_PAD"

AC_TAKEN=$(PSQL "SELECT count(*) FROM biz_asset_category WHERE category_code LIKE 'AC-%' AND deleted_at IS NULL")
ET_TAKEN=$(PSQL "SELECT count(*) FROM biz_expense_type WHERE type_code LIKE 'ET-%' AND deleted_at IS NULL")
say "  落库的 AC-NN 行 $AC_TAKEN 条 / ET-NN 行 $ET_TAKEN 条（含手工提交的预览号）"
check "AC-NN 共 3 行（AC-01/02 自动 + AC-03 手工）" "3" "$AC_TAKEN"
check "ET-NN 共 3 行（ET-01/02 自动 + ET-03 手工）" "3" "$ET_TAKEN"

say ""
say "=== ④ 唯一性约束还在（撞号 409 的根因） ==="
AC_IDX=$(PSQL "SELECT count(*) FROM pg_indexes WHERE tablename='biz_asset_category' AND indexdef ILIKE '%category_code%' AND indexdef ILIKE '%UNIQUE%'")
ET_IDX=$(PSQL "SELECT count(*) FROM pg_indexes WHERE tablename='biz_expense_type' AND indexdef ILIKE '%type_code%' AND indexdef ILIKE '%UNIQUE%'")
check "biz_asset_category 上 category_code 唯一索引存在" "1" "$AC_IDX"
check "biz_expense_type 上 type_code 唯一索引存在" "1" "$ET_IDX"

say ""
say "=== ⑤ 编码无重复（全表口径，含编辑过名的那行） ==="
AC_DUP=$(PSQL "SELECT count(*) FROM (SELECT category_code FROM biz_asset_category WHERE deleted_at IS NULL GROUP BY category_code HAVING count(*) > 1) t")
ET_DUP=$(PSQL "SELECT count(*) FROM (SELECT type_code FROM biz_expense_type WHERE deleted_at IS NULL GROUP BY type_code HAVING count(*) > 1) t")
check "分类编码无重复组" "0" "$AC_DUP"
check "费用类型编码无重复组" "0" "$ET_DUP"

say ""
if [ "$FAILED" = "0" ]; then
  say "RESULT: ALL PASS"
else
  say "RESULT: 有 FAIL，见上"
fi
exit "$FAILED"
