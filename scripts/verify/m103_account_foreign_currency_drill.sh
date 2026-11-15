#!/usr/bin/env bash
# ============================================================================
# m103_account_foreign_currency_drill.sh —— V0408（M1-03 科目「是否外币核算」标记）真库往返演练
#
# 卡：t_52b02f6a。演练库名含 `drill`（硬性要求），脚本自己建、自己 DROP。
#
# 覆盖（对应迁移的每一处风险）：
#   ① up 首次执行成功（自检 DO 块自己会 RAISE EXCEPTION，跑过 = 自检过）
#   ② 列型 / NOT NULL / 默认值 / 两条 CHECK / 部分索引 / 权限码菜单行 逐项对拍
#   ③ up 重复执行幂等（整表指纹不变）
#   ④ 行为反证：值 2 / -1 / NULL 被拒；非末级标记被拒；已标记科目改成非末级被拒
#   ⑤ 行为正证：末级标记 → 读回 1；关闭 → 读回 0
#   ⑥ down 幂等（无外币科目时连跑两次）+ 结构和权限行都真的没了
#   ⑦ down 数据护栏：有外币科目时**必须**拒绝
#   ⑧ 回滚后重放 up，整表指纹与第一次完全一致
#
# 用法：bash scripts/verify/m103_account_foreign_currency_drill.sh [HOST]
#   默认 HOST=172.18.10.145（在 99 上跑）；在 145 上跑传 127.0.0.1。
# ============================================================================
set -uo pipefail

HOST="${1:-172.18.10.145}"
export PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4
USER_DB=cloudpivot
TEMPLATE=cloudpivot_test
DB=cp_m103_drill
UP=/opt/cloudpivot/backend/app/migrations/business/V0408__account_foreign_currency.up.sql
DOWN=/opt/cloudpivot/backend/app/migrations/business/V0408__account_foreign_currency.down.sql

PASS=0
FAIL=0
ok()   { PASS=$((PASS+1)); echo "  PASS  $*"; }
bad()  { FAIL=$((FAIL+1)); echo "  FAIL  $*"; }
q()    { psql -h "$HOST" -U "$USER_DB" -d "$DB" -tAc "$1" 2>&1; }

expect() { # expect <期望> <SQL> <说明>
    local want="$1" sql="$2" why="$3" got
    got="$(q "$sql" | tr -d '\r' | sed -e 's/[[:space:]]*$//')"
    if [ "$got" = "$want" ]; then ok "$why（$got）"; else bad "$why —— 期望 [$want] 实得 [$got]"; fi
}

# 反证助手：SQL 必须**报错**，且错误里含指定片段（约束名 / 关键字）。
#
# ⚠️ 为什么不用 `q ... | grep -q`：本脚本开了 `set -o pipefail`，`grep -q` 命中即退出会让
# psql 收到 SIGPIPE（退出码 141），pipefail 把整条管道判成失败 ⇒ 明明拒了却报「没拒」。
# 这里改成先取变量再匹配，不依赖管道退出码。
must_reject() { # must_reject <SQL> <错误片段> <说明>
    local sql="$1" needle="$2" why="$3" out
    out="$(q "$sql")"
    case "$out" in
        *"$needle"*) ok "$why（$needle）" ;;
        *)           bad "$why —— 期望报错含 [$needle]，实得 [$out]" ;;
    esac
}

must_accept() { # must_accept <SQL> <期望读回值> <读回 SQL> <说明>
    local sql="$1" want="$2" verify="$3" why="$4" out got
    out="$(q "$sql")"
    case "$out" in
        *ERROR*) bad "$why —— 本该放行却报错：[$out]"; return ;;
    esac
    got="$(q "$verify")"
    if [ "$got" = "$want" ]; then ok "$why（读回 $got）"; else bad "$why —— 读回 [$got]，期望 [$want]"; fi
}

echo "== 0) 建演练库（克隆 $TEMPLATE @ $HOST）=="
psql -h "$HOST" -U "$USER_DB" -d postgres -c "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
if ! psql -h "$HOST" -U "$USER_DB" -d postgres -c "CREATE DATABASE $DB TEMPLATE $TEMPLATE" >/dev/null 2>&1; then
    echo "  FATAL 建库失败（模板库可能被别的会话占用）："
    psql -h "$HOST" -U "$USER_DB" -d postgres -c "CREATE DATABASE $DB TEMPLATE $TEMPLATE" 2>&1 | sed 's/^/    /'
    exit 2
fi
ok "演练库 $DB 已建"

echo "== 1) up 首次执行（自检块：任一断言不符即整体回滚）=="
if psql -h "$HOST" -U "$USER_DB" -d "$DB" -v ON_ERROR_STOP=1 -q -f "$UP" >/tmp/m103_up1.log 2>&1; then
    ok "up 首次执行成功"
    grep -E "V0408" /tmp/m103_up1.log | sed 's/^NOTICE:  /    NOTICE /' | head -5
else
    bad "up 首次执行失败"; sed 's/^/    /' /tmp/m103_up1.log | head -20; exit 2
fi

echo "== 2) 结构对拍 =="
expect "smallint|NO|0" "select data_type||'|'||is_nullable||'|'||coalesce(column_default,'-') from information_schema.columns where table_schema='public' and table_name='biz_account' and column_name='is_foreign_currency'" "列型 / NOT NULL / 默认值"
expect "2" "select count(*) from pg_constraint where conrelid='public.biz_account'::regclass and conname in ('ck_biz_account_foreign_currency','ck_biz_account_fc_leaf')" "两条 CHECK 在位"
expect "t" "select pg_get_constraintdef(oid) like '%is_foreign_currency = 0%is_leaf = 1%' from pg_constraint where conname='ck_biz_account_fc_leaf'" "fc_leaf 约束定义含两个列"
expect "1" "select count(*) from pg_indexes where schemaname='public' and indexname='idx_biz_account_foreign_currency' and indexdef like '%WHERE%is_foreign_currency = 1%'" "部分索引 + 谓词"
expect "3|business:erp:account:update|20000000-0000-0000-0000-000000000950|10" "select menu_type||'|'||auth_code||'|'||parent_id||'|'||sort_order from sys_menu where id='20000000-0000-0000-0000-000000001720'::uuid" "权限码按钮行（type/code/parent/sort）"
expect "0" "select count(*) from sys_role r join sys_role_menu rm on rm.role_id=r.id where rm.menu_id='20000000-0000-0000-0000-000000001720'::uuid and r.role_code not in ('SUPER_ADMIN','FINANCE_MANAGER')" "授权只给 2 个角色（ADMIN 不是生产角色码）"
expect "1" "select count(*) from sys_role r join sys_role_menu rm on rm.role_id=r.id where rm.menu_id='20000000-0000-0000-0000-000000001720'::uuid and r.role_code='FINANCE_MANAGER'" "财务经理确实被授权（可授出去）"
expect "0" "select count(*) from biz_account where is_foreign_currency <> 0" "存量行全部回填 0"

echo "== 3) up 重复执行幂等（整表指纹不变）=="
FP1="$(q "select count(*)||'/'||md5(string_agg(id::text||is_foreign_currency::text, ',' order by id)) from biz_account")"
psql -h "$HOST" -U "$USER_DB" -d "$DB" -v ON_ERROR_STOP=1 -q -f "$UP" >/tmp/m103_up2.log 2>&1 \
    && ok "up 第二次执行成功（幂等）" || { bad "up 第二次执行失败"; sed 's/^/    /' /tmp/m103_up2.log | head -10; }
FP2="$(q "select count(*)||'/'||md5(string_agg(id::text||is_foreign_currency::text, ',' order by id)) from biz_account")"
[ "$FP1" = "$FP2" ] && ok "重放后整表指纹不变（$FP2）" || bad "重放后指纹变了：[$FP1] → [$FP2]"

echo "== 4) 行为反证（脏数据必须被拒）=="
psql -h "$HOST" -U "$USER_DB" -d "$DB" -q -c "insert into biz_account(account_code,account_name,level,account_category,balance_direction) values ('DRILL-M103-1','__drill_leaf__',1,'ASSET','DEBIT'),('DRILL-M103-0','__drill_parent__',1,'ASSET','DEBIT')" >/dev/null
psql -h "$HOST" -U "$USER_DB" -d "$DB" -q -c "update biz_account set is_leaf=0 where account_code='DRILL-M103-0'" >/dev/null

must_reject "update biz_account set is_foreign_currency=2 where account_code='DRILL-M103-1'" "ck_biz_account_foreign_currency" "值 2 被 ck_biz_account_foreign_currency 拒"
must_reject "update biz_account set is_foreign_currency=-1 where account_code='DRILL-M103-1'" "ck_biz_account_foreign_currency" "值 -1 被拒"
must_reject "update biz_account set is_foreign_currency=null where account_code='DRILL-M103-1'" "null value in column" "NULL 被 NOT NULL 拒"
must_reject "update biz_account set is_foreign_currency=1 where account_code='DRILL-M103-0'" "ck_biz_account_fc_leaf" "非末级科目标记被 ck_biz_account_fc_leaf 拒"

echo "== 5) 行为正证（正常业务必须放行）=="
must_accept "update biz_account set is_foreign_currency=1 where account_code='DRILL-M103-1'" "1" \
    "select is_foreign_currency from biz_account where account_code='DRILL-M103-1'" "末级科目标记外币核算成功"
must_reject "update biz_account set is_leaf=0 where account_code='DRILL-M103-1'" "ck_biz_account_fc_leaf" "已标记科目改成非末级被拒（反向也挡住）"
must_accept "update biz_account set is_foreign_currency=0 where account_code='DRILL-M103-1'" "0" \
    "select is_foreign_currency from biz_account where account_code='DRILL-M103-1'" "关闭外币核算成功"

echo "== 6) down 数据护栏（有外币科目必须拒绝）=="
q "update biz_account set is_foreign_currency=1 where account_code='DRILL-M103-1'" >/dev/null
must_reject "$(cat "$DOWN")" "V0408 down 拒绝执行" "有外币科目时 down 被拒（业务信息不许回滚丢掉）"
expect "1" "select count(*) from information_schema.columns where table_schema='public' and table_name='biz_account' and column_name='is_foreign_currency'" "被拒后列仍在（没被半途删掉）"

echo "== 7) down 幂等（先清标记再连跑两次）=="
q "update biz_account set is_foreign_currency=0 where account_code like 'DRILL-M103-%'" >/dev/null
for i in 1 2; do
    if psql -h "$HOST" -U "$USER_DB" -d "$DB" -v ON_ERROR_STOP=1 -q -f "$DOWN" >/tmp/m103_down$i.log 2>&1; then
        ok "down 第 $i 次执行成功"
    else
        bad "down 第 $i 次执行失败"; sed 's/^/    /' /tmp/m103_down$i.log | head -10
    fi
done
expect "0" "select count(*) from information_schema.columns where table_schema='public' and table_name='biz_account' and column_name='is_foreign_currency'" "down 后列已删"
expect "0" "select count(*) from pg_constraint where conname in ('ck_biz_account_foreign_currency','ck_biz_account_fc_leaf')" "down 后约束已删"
expect "0" "select count(*) from pg_indexes where indexname='idx_biz_account_foreign_currency'" "down 后索引已删"
expect "0" "select count(*) from sys_menu where id='20000000-0000-0000-0000-000000001720'::uuid" "down 后权限码行已删"
expect "0" "select count(*) from sys_role_menu where menu_id='20000000-0000-0000-0000-000000001720'::uuid" "down 后授权行已删"
expect "2" "select count(*) from biz_account where account_code like 'DRILL-M103-%'" "down 不动业务行（2 行演练科目还在）"

echo "== 8) 回滚后重放 up（指纹与第一次一致）=="
q "delete from biz_account where account_code like 'DRILL-M103-%'" >/dev/null
FP3="$(q "select count(*)||'/'||md5(string_agg(id::text||account_code, ',' order by id)) from biz_account")"
psql -h "$HOST" -U "$USER_DB" -d "$DB" -v ON_ERROR_STOP=1 -q -f "$UP" >/tmp/m103_up3.log 2>&1 \
    && ok "回滚后重放 up 成功" || { bad "回滚后重放 up 失败"; sed 's/^/    /' /tmp/m103_up3.log | head -10; }
expect "0" "select count(*) from biz_account where is_foreign_currency <> 0" "重放后仍全部为 0"
expect "1" "select count(*) from sys_menu where id='20000000-0000-0000-0000-000000001720'::uuid" "重放后权限码行回来了（且只有一行）"
FP4="$(q "select count(*)||'/'||md5(string_agg(id::text||account_code, ',' order by id)) from biz_account")"
[ "$FP3" = "$FP4" ] && ok "重放不动业务行（指纹一致 $FP4）" || bad "重放改了业务行：[$FP3] → [$FP4]"

echo "== 9) 收尾：DROP 演练库 =="
psql -h "$HOST" -U "$USER_DB" -d postgres -c "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1 && ok "演练库已删"
LEFT="$(psql -h "$HOST" -U "$USER_DB" -d postgres -tAc "select count(*) from pg_database where datname like 'cp_m103%'")"
if [ "$LEFT" = "0" ]; then ok "cp_m103* 残留库计数 = 0"; else bad "cp_m103* 残留库计数 = $LEFT"; fi

echo
echo "== 汇总：PASS=$PASS FAIL=$FAIL =="
[ "$FAIL" -eq 0 ] || exit 1
