#!/usr/bin/env bash
# V0260 结构与种子验收（只读断言 + 汇总，任一不符 exit 1）
set -uo pipefail
# 默认指向本卡隔离库；可以用 DB / PGHOST / PGPORT / PGUSER / PGPASSWORD 覆盖
DB=${DB:-cloudpivot_v0260}
PGHOST=${PGHOST:-127.0.0.1}
PGPORT=${PGPORT:-5432}
PGUSER=${PGUSER:-alun}
export PGPASSWORD=${PGPASSWORD:-alun}
q() { psql -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" -d "$DB" -Atc "$1"; }
fail=0
chk() { # chk <标签> <期望> <实际>
  if [ "$2" = "$3" ]; then printf '  [PASS] %s = %s\n' "$1" "$3"
  else printf '  [FAIL] %s：期望 [%s] 实际 [%s]\n' "$1" "$2" "$3"; fail=$((fail+1)); fi
}

echo "== 1) 表存在 =="
for t in biz_customer biz_customer_contact; do
  chk "to_regclass($t)" "$t" "$(q "select to_regclass('public.$t')")"
done

echo "== 2) 列（名/类型/可空/默认）=="
q "select table_name||'.'||column_name||' | '||data_type||coalesce('('||character_maximum_length||')','')||' | nullable='||is_nullable||' | default='||coalesce(column_default,'-')
     from information_schema.columns where table_schema='public'
      and table_name in ('biz_customer','biz_customer_contact') order by table_name, ordinal_position"

echo "== 3) 索引 =="
q "select tablename||' | '||indexname||' | '||indexdef from pg_indexes
    where schemaname='public' and tablename in ('biz_customer','biz_customer_contact') order by tablename, indexname"
chk "uk_biz_customer_code 是唯一部分索引" "1" \
    "$(q "select count(*) from pg_indexes where indexname='uk_biz_customer_code' and indexdef like 'CREATE UNIQUE INDEX%' and indexdef like '%deleted_at IS NULL%'")"

echo "== 4) 约束 =="
q "select conrelid::regclass||' | '||conname||' | '||pg_get_constraintdef(oid) as def from pg_constraint
    where conrelid in ('biz_customer'::regclass,'biz_customer_contact'::regclass) and contype in ('c','f') order by 1"
chk "外键指向 biz_customer(id) 且 RESTRICT" "1" \
    "$(q "select count(*) from pg_constraint where conname='fk_biz_customer_contact_customer' and contype='f' and pg_get_constraintdef(oid) like '%REFERENCES biz_customer(id)%' and pg_get_constraintdef(oid) like '%ON DELETE RESTRICT%'")"

echo "== 5) 种子 =="
q "select code||' | '||name||' | '||status||' | '||credit_limit||' | 主联系人='||coalesce((select x.name from biz_customer_contact x where x.customer_id=c.id and x.is_primary=1 and x.deleted_at is null),'(无)')
     from biz_customer c where deleted_at is null order by code"
chk "客户种子条数" "3" "$(q "select count(*) from biz_customer where deleted_at is null and code like 'CUS-%'")"
chk "主联系人条数" "3" "$(q "select count(*) from biz_customer_contact where deleted_at is null and is_primary=1")"
chk "孤儿联系人" "0" "$(q "select count(*) from biz_customer_contact x left join biz_customer c on c.id=x.customer_id where c.id is null")"
chk "探针残留" "0" "$(q "select count(*) from biz_customer where code like '__V0260%'")"

echo "== 6) 行为断言（负例必须被拒）=="
neg() { # neg <标签> <SQL>
  out=$(psql -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" -d "$DB" -Atc "begin; $2 rollback;" 2>&1)
  if printf '%s' "$out" | grep -qE 'violates (check|foreign key|unique) constraint'; then
    printf '  [PASS] %s 被拒：%s\n' "$1" "$(printf '%s' "$out" | grep -oE 'violates (check|foreign key|unique) constraint "[^"]+"')"
  else
    printf '  [FAIL] %s 未被拒：%s\n' "$1" "$out"; fail=$((fail+1))
  fi
}
neg "credit_limit = -1"  "insert into biz_customer(code,name,credit_limit) values ('__NEG__','负例',-1);"
neg "status = 'BOGUS'"   "insert into biz_customer(code,name,status) values ('__NEG2__','负例','BOGUS');"
neg "is_primary = 2"     "insert into biz_customer_contact(customer_id,name,is_primary) select id,'负例',2 from biz_customer where code='CUS-0001';"
neg "孤儿 customer_id"   "insert into biz_customer_contact(customer_id,name) values ('00000000-0000-4000-8000-0000000fffff','负例');"
neg "第二主联系人"       "insert into biz_customer_contact(customer_id,name,is_primary) select customer_id,'负例',1 from biz_customer_contact where id='d0260000-0000-4000-8000-000000000101';"
neg "同编码重复"         "insert into biz_customer(code,name) values ('CUS-0001','重复编码');"

echo "== 7) 物理删客户被联系人挡住（RESTRICT）=="
out=$(psql -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" -d "$DB" -Atc "begin; delete from biz_customer where code='CUS-0001'; rollback;" 2>&1)
if printf '%s' "$out" | grep -q 'violates RESTRICT setting of foreign key constraint'; then
  echo "  [PASS] DELETE 客户被 fk_biz_customer_contact_customer 挡住"
else
  echo "  [FAIL] DELETE 客户未被挡住：$out"; fail=$((fail+1))
fi

echo
if [ "$fail" -eq 0 ]; then echo "V0260 验收：全部通过"; else echo "V0260 验收：$fail 项失败"; fi
exit $fail
