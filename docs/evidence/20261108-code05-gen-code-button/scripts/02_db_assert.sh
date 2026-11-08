#!/bin/bash
# CODE-05 库侧断言（库 cloudpivot_code05，隔离实例 :8045 用的同一个库）
#
# 跑 curl 之后执行：核对**落库**真相（接口返回对不对是一回事，库里是不是那样是另一回事）
#   1. 自动号真落库且形态正确（SUP-NNNN / CUS-NNNN）
#   2. 手工号（GYS-001 / KH-001）真落库、原样
#   3. 存活行内编码无重复
#   4. biz_seq 推进到「最后一个被用掉的号」
#   5. 预览接口没推进 biz_seq（对比：当前 current_no 与已落库最大流水一致）
set -uo pipefail
export PATH=/usr/lib/postgresql/*/bin:$PATH
PGPASSWORD=${PGPASSWORD:-alun}
export PGPASSWORD
DB=${DB:-cloudpivot_code05a}
URL="postgresql://alun:alun@127.0.0.1:5432/$DB"
HERE=$(cd "$(dirname "$0")" && pwd)
OUT="$HERE/../raw/02_db_assert.txt"
: > "$OUT"

FAILED=0
say() { echo "$@" | tee -a "$OUT"; }
check() {
  if [ "$2" = "$3" ]; then say "  PASS  $1 = $3"; else say "  FAIL  $1 期望[$2] 实际[$3]"; FAILED=1; fi
}

q() { psql "$URL" -Atc "$1"; }

say "=== 1) biz_seq 号段（预览不占号 ⇒ current_no 只被保存推进） ==="
say "  biz_seq: $(q "select string_agg(seq_type||'='||current_no||'/'||prefix||'/'||pad_width, ' ' order by seq_type) from biz_seq where seq_type in ('SUP','CUS')")"
SUP_SEQ=$(q "select current_no from biz_seq where seq_type='SUP'")
CUS_SEQ=$(q "select current_no from biz_seq where seq_type='CUS'")
SUP_ROWS=$(q "select count(*) from biz_supplier where deleted_at is null and supplier_code ~ '^SUP-[0-9]+$'")
CUS_ROWS=$(q "select count(*) from biz_customer where deleted_at is null and code ~ '^CUS-[0-9]+$'")
say "  SUP: current_no=$SUP_SEQ  自动码存活行=$SUP_ROWS"
say "  CUS: current_no=$CUS_SEQ  自动码存活行=$CUS_ROWS"
check "SUP 号段被推进（>0）" "true" "$([ "$SUP_SEQ" -gt 0 ] && echo true || echo false)"
check "CUS 号段被推进（>0）" "true" "$([ "$CUS_SEQ" -gt 0 ] && echo true || echo false)"

say ""
say "=== 2) 供应商落库明细 ==="
psql "$URL" -c "select supplier_code, supplier_name, created_at::time(0) from biz_supplier where deleted_at is null order by supplier_code" | tee -a "$OUT"
check "自动号形态全部合法（SUP-NNNN）" "$SUP_ROWS" "$(q "select count(*) from biz_supplier where deleted_at is null and supplier_code ~ '^SUP-[0-9]{4}$'")"
check "手工码 GYS-001 原样落库" "1" "$(q "select count(*) from biz_supplier where deleted_at is null and supplier_code='GYS-001'")"
check "非法编码 BAD CODE 没落库" "0" "$(q "select count(*) from biz_supplier where supplier_code='BAD CODE'")"

say ""
say "=== 3) 客户落库明细 ==="
psql "$URL" -c "select code, name, created_at::time(0) from biz_customer where deleted_at is null order by code" | tee -a "$OUT"
check "自动号形态全部合法（CUS-NNNN）" "$CUS_ROWS" "$(q "select count(*) from biz_customer where deleted_at is null and code ~ '^CUS-[0-9]{4}$'")"
check "手工码 KH-001 原样落库" "1" "$(q "select count(*) from biz_customer where deleted_at is null and code='KH-001'")"

say ""
say "=== 4) 存活行内编码无重复（部分唯一索引 + 应用层预检双保险） ==="
SUP_DUP=$(q "select count(*) from (select supplier_code from biz_supplier where deleted_at is null group by 1 having count(*)>1) t")
CUS_DUP=$(q "select count(*) from (select code from biz_customer where deleted_at is null group by 1 having count(*)>1) t")
check "供应商存活编码无重复" "0" "$SUP_DUP"
check "客户存活编码无重复" "0" "$CUS_DUP"

say ""
say "=== 5) 预览不占号：号段值 == 已用掉的最大流水（没有为预览白跳的号） ==="
SUP_MAX=$(q "select coalesce(max(substring(supplier_code from '[0-9]+$')::bigint),0) from biz_supplier where deleted_at is null and supplier_code ~ '^SUP-[0-9]+$'")
CUS_MAX=$(q "select coalesce(max(substring(code from '[0-9]+$')::bigint),0) from biz_customer where deleted_at is null and code ~ '^CUS-[0-9]+$'")
say "  SUP 已用最大流水=$SUP_MAX / current_no=$SUP_SEQ ；CUS 已用最大流水=$CUS_MAX / current_no=$CUS_SEQ"
# 本脚本跑了 4 次供应商预览（P1 两次、P3、P4）+ 客户 3 次预览，若预览占号 current_no 会远超最大流水。
check "SUP current_no - 最大流水 <= 1（只被保存推进）" "true" "$([ $((SUP_SEQ - SUP_MAX)) -le 1 ] && echo true || echo false)"
check "CUS current_no - 最大流水 <= 1（只被保存推进）" "true" "$([ $((CUS_SEQ - CUS_MAX)) -le 1 ] && echo true || echo false)"

say ""
if [ "$FAILED" = "0" ]; then say "=== 库侧断言全部 PASS ==="; else say "=== 库侧有断言 FAIL ==="; fi
exit "$FAILED"
