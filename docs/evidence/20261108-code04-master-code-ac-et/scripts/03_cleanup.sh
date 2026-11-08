#!/bin/bash
# CODE-04 夹具清理（**在 145 上跑**）：物理删掉 01_curl.sh 建的行。
#
# **不回退 biz_seq.current_no**：主数据是档案，号一旦发出去就永久占用
# （与 CODE-02/03 的用例口径一致）。所以清理后 AC/ET 号段停在 3，
# 145 上「下一个真实新建」会拿到 AC-04 / ET-04 —— 这是设计，不是 bug。
#
# 用法：bash 03_cleanup.sh
set -uo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
RAW="$HERE/../raw"
SUM="$RAW/20_cleanup.txt"
: > "$SUM"
say() { echo "$@" | tee -a "$SUM"; }

# shellcheck disable=SC1091
. "$RAW/09_ids.env"
PSQL() { su - postgres -c "psql -d ${DB:-cloudpivot_dev} -tAc \"$1\""; }

say "=== 删除本次夹具（按 id，业务接口才是软删） ==="
for id in "$AC1_ID" "$AC2_ID" "$AC3_ID" "$AC4_ID"; do
  PSQL "DELETE FROM biz_asset_category WHERE id = '$id'" >/dev/null && say "  deleted 分类 $id"
done
for id in "$ET1_ID" "$ET2_ID" "$ET3_ID" "$ET4_ID"; do
  PSQL "DELETE FROM biz_expense_type WHERE id = '$id'" >/dev/null && say "  deleted 类型 $id"
done

say ""
say "=== 复核：夹具已清、存量未动、号段不回退 ==="
LEFT_AC=$(PSQL "SELECT count(*) FROM biz_asset_category WHERE category_code IN ('$AC1_CODE','$AC2_CODE','$AC3_CODE') OR category_code = '$CAT_MANUAL'")
LEFT_ET=$(PSQL "SELECT count(*) FROM biz_expense_type WHERE type_code IN ('$ET1_CODE','$ET2_CODE','$ET3_CODE') OR type_code = '$TYPE_MANUAL'")
say "  残留 分类=$LEFT_AC 类型=$LEFT_ET"
AC_TOTAL=$(PSQL "SELECT count(*) FROM biz_asset_category WHERE deleted_at IS NULL")
ET_TOTAL=$(PSQL "SELECT count(*) FROM biz_expense_type WHERE deleted_at IS NULL")
AC_NO=$(PSQL "SELECT current_no FROM biz_seq WHERE seq_type='AC' AND deleted_at IS NULL")
ET_NO=$(PSQL "SELECT current_no FROM biz_seq WHERE seq_type='ET' AND deleted_at IS NULL")
say "  清理后：分类 $AC_TOTAL 条、类型 $ET_TOTAL 条；AC 号段=$AC_NO、ET 号段=$ET_NO（不回退）"
if [ "$LEFT_AC" = "0" ] && [ "$LEFT_ET" = "0" ]; then say "RESULT: 夹具已清"; else say "RESULT: 有残留"; exit 1; fi
