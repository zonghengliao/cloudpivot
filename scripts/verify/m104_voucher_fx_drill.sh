#!/usr/bin/env bash
# =============================================================================
# m104_voucher_fx_drill.sh —— V0409（M1-04 分录原币金额）psql 往返实测
#
# 卡：t_b95e935d（M1-04 凭证录入：外币金额 + 汇率 + 本位币换算）
#
# 干什么（全部在**演练库**里，脚本自己 DROP）：
#   ① 克隆模板库 cloudpivot_test（停版 V0404）→ 顺序补 V0406/V0407/V0408/V0409
#   ② up 幂等：V0409 再跑一次必须零报错、零结构变化
#   ③ 结构断言：两列 numeric(20,4) 可空无默认 / 1 条 CHECK / 部分索引谓词对 / 零回填
#   ④ 行为断言：本位币行 NULL 可写；外币行可写；半对 / 不同边 / 负原币三种脏行必须被拒
#   ⑤ down：先 down 一次（成功）→ 再 down 一次（幂等）→ 重放 up（结构回来）
#   ⑥ down 护栏反证：留一行外币分录，down 必须 RAISE EXCEPTION 且结构**不动**
#
# 用法：bash scripts/verify/m104_voucher_fx_drill.sh [PG_HOST]
#   默认 PG_HOST=172.18.10.145（在 99 上跑）；在 145 上跑传 127.0.0.1。
#   （99 本机 PG 已卸载，禁止在 99 上连 127.0.0.1:5432）
# =============================================================================
set -uo pipefail

export PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4
PG_USER=cloudpivot
PG_HOST="${1:-172.18.10.145}"
DB=cp_m104_drill
REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
MIG="$REPO_ROOT/backend/app/migrations/business"

PASS=0
FAIL=0
ok()  { PASS=$((PASS+1)); echo "  PASS  $*"; }
bad() { FAIL=$((FAIL+1)); echo "  FAIL  $*"; }

q()   { psql -h "$PG_HOST" -U "$PG_USER" -d "$DB" -tAc "$1" 2>&1; }
qf()  { psql -h "$PG_HOST" -U "$PG_USER" -d "$DB" -q -v ON_ERROR_STOP=1 -1 -f "$1" 2>&1; }
admin(){ psql -h "$PG_HOST" -U "$PG_USER" -d postgres -tAc "$1" 2>&1; }

cleanup() { admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1; }
trap cleanup EXIT

admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
if ! admin "CREATE DATABASE $DB TEMPLATE cloudpivot_test" >/dev/null 2>&1; then
    echo "FATAL: 克隆模板库失败（模板可能被别的会话占用），5 秒后重试一次"
    sleep 5
    admin "CREATE DATABASE $DB TEMPLATE cloudpivot_test" >/dev/null || { echo "FATAL: 建库失败"; exit 2; }
fi
echo "== 演练库 $DB 就绪（模板 $(q "select count(*) from schema_migrations where kind='versioned'") 条 versioned，最大 V$(q "select max(version::int) from schema_migrations where kind='versioned'")）"

echo
echo "== ① 补前置迁移（V0406 / V0407 / V0408）+ 本卡 V0409 =="
for f in V0406__account_voucher_currency V0407__exchange_rate_effective_range V0408__account_foreign_currency V0409__voucher_entry_foreign_amounts; do
    OUT="$(qf "$MIG/$f.up.sql")"
    if [[ $? -ne 0 ]]; then
        echo "FATAL: $f.up.sql 执行失败："; echo "$OUT" | tail -20; exit 2
    fi
    echo "  applied $f.up.sql"
done
ok "V0406..V0409 全部应用成功"

echo
echo "== ② up 幂等（V0409 再跑一次）=="
OUT="$(qf "$MIG/V0409__voucher_entry_foreign_amounts.up.sql")"
[[ $? -eq 0 ]] && ok "V0409 up 第二次执行零报错（幂等）" || { bad "V0409 up 重放报错"; echo "$OUT" | tail -20; }
echo "$OUT" | grep -q "V0409 自检通过" && ok "重放时自检块也打印了「V0409 自检通过」" || bad "重放没有自检 NOTICE"

echo
echo "== ③ 结构断言 =="
COL="$(q "select string_agg(column_name||':'||data_type||'('||numeric_precision||','||numeric_scale||')/'||is_nullable||'/'||coalesce(column_default,'-'), ' | ' order by column_name)
           from information_schema.columns
          where table_schema='public' and table_name='biz_voucher_entry'
            and column_name in ('fc_debit_amount','fc_credit_amount')")"
echo "  $COL"
[[ "$COL" == "fc_credit_amount:numeric(20,4)/YES/- | fc_debit_amount:numeric(20,4)/YES/-" ]] \
    && ok "两列 = numeric(20,4) / 可空 / 无默认" || bad "列结构不对：[$COL]"

CK="$(q "select count(*) from pg_constraint where conrelid='public.biz_voucher_entry'::regclass and conname='ck_biz_voucher_entry_fc_side'")"
[[ "$CK" == "1" ]] && ok "ck_biz_voucher_entry_fc_side 在位" || bad "CHECK 计数 = $CK"

CKDEF="$(q "select pg_get_constraintdef(oid) from pg_constraint where conname='ck_biz_voucher_entry_fc_side'")"
echo "  $CKDEF"
case "$CKDEF" in
    *fc_debit_amount*fc_credit_amount*debit_amount*) ok "CHECK 定义同时引用原币两列与本位币借方" ;;
    *) bad "CHECK 定义不像预期：$CKDEF" ;;
esac

IDX="$(q "select count(*) from pg_indexes where schemaname='public' and indexname='idx_biz_voucher_entry_fc' and indexdef like '%WHERE%fc_debit_amount IS NOT NULL%'")"
[[ "$IDX" == "1" ]] && ok "部分索引 idx_biz_voucher_entry_fc 在位且谓词正确" || bad "部分索引不对（计数 $IDX）"

NFC="$(q "select count(*) from biz_voucher_entry where fc_debit_amount is not null or fc_credit_amount is not null")"
[[ "$NFC" == "0" ]] && ok "不做任何回填：存量分录原币列全 NULL" || bad "存量分录被回填了 $NFC 行"

echo
echo "== ④ 行为断言（探针数据，事务内跑完 ROLLBACK 前的清场由脚本自己做）=="
q "insert into biz_voucher (voucher_word, voucher_no, period, voucher_date, voucher_type, source_type, summary, total_debit, total_credit, entry_count, status)
   values ('记','M104DRILL','1999-01',date '1999-01-01','MANUAL','MANUAL','__m104_drill__',2,2,2,'DRAFT') on conflict do nothing" >/dev/null
VID="$(q "select id from biz_voucher where voucher_no='M104DRILL'")"
[[ -n "$VID" ]] && ok "探针凭证已建（$VID）" || { bad "探针凭证没建出来"; VID=""; }

if [[ -n "$VID" ]]; then
    # P1 本位币行：不给原币 ⇒ NULL
    q "insert into biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name, debit_amount, credit_amount)
       values ('$VID',1,'__m104_p1__',gen_random_uuid(),'1999','__m104_drill__',2,0)" >/dev/null
    [[ "$(q "select fc_debit_amount is null and fc_credit_amount is null from biz_voucher_entry where voucher_id='$VID' and line_no=1")" == "t" ]] \
        && ok "P1 本位币分录：原币两列留 NULL" || bad "P1 本位币分录原币列不是 NULL"

    # P2 正常外币行：借方原币 14.4 / 本位币 2（汇率 7.2）
    q "insert into biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name, debit_amount, credit_amount, fc_debit_amount, fc_credit_amount)
       values ('$VID',2,'__m104_p2__',gen_random_uuid(),'1999','__m104_drill__',2,0,14.4,0)" >/dev/null
    [[ "$(q "select fc_debit_amount from biz_voucher_entry where voucher_id='$VID' and line_no=2")" == "14.4000" ]] \
        && ok "P2 正常外币行可写（原币 14.4000 / 本位币 2.0000）" || bad "P2 正常外币行没写进去"

    # P3 半对原币 ⇒ 必须被 CHECK 拒
    ERR="$(q "insert into biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name, debit_amount, credit_amount, fc_debit_amount)
             values ('$VID',3,'__m104_p3__',gen_random_uuid(),'1999','__m104_drill__',1,0,7.2)" 2>&1)"
    case "$ERR" in
        *ck_biz_voucher_entry_fc_side*) ok "P3 半对原币被 CHECK 拒绝（约束名点名）" ;;
        *) bad "P3 半对原币没被拒：$ERR" ;;
    esac

    # P4 原币与本位币不同边 ⇒ 必须被拒
    ERR="$(q "insert into biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name, debit_amount, credit_amount, fc_debit_amount, fc_credit_amount)
             values ('$VID',4,'__m104_p4__',gen_random_uuid(),'1999','__m104_drill__',1,0,0,7.2)" 2>&1)"
    case "$ERR" in
        *ck_biz_voucher_entry_fc_side*) ok "P4 原币与本位币不同边被拒绝" ;;
        *) bad "P4 不同边没被拒：$ERR" ;;
    esac

    # P5 负原币 ⇒ 必须被拒
    ERR="$(q "insert into biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name, debit_amount, credit_amount, fc_debit_amount, fc_credit_amount)
             values ('$VID',5,'__m104_p5__',gen_random_uuid(),'1999','__m104_drill__',1,0,-7.2,0)" 2>&1)"
    case "$ERR" in
        *ck_biz_voucher_entry_fc_side*) ok "P5 负原币被拒绝" ;;
        *) bad "P5 负原币没被拒：$ERR" ;;
    esac
fi

echo
echo "== ⑤ down 往返（先清外币行 → down → down 幂等 → 重放 up）=="
q "delete from biz_voucher_entry where summary like '\_\_m104\_%'" >/dev/null
q "delete from biz_voucher where voucher_no='M104DRILL'" >/dev/null
OUT="$(qf "$MIG/V0409__voucher_entry_foreign_amounts.down.sql")"
[[ $? -eq 0 ]] && ok "down 第一次执行成功" || { bad "down 报错"; echo "$OUT" | tail -20; }
[[ "$(q "select count(*) from information_schema.columns where table_name='biz_voucher_entry' and column_name like 'fc\_%'")" == "0" ]] \
    && ok "down 后原币列已拆除" || bad "down 后列还在"
OUT="$(qf "$MIG/V0409__voucher_entry_foreign_amounts.down.sql")"
[[ $? -eq 0 ]] && ok "down 第二次执行成功（幂等）" || { bad "down 重放报错"; echo "$OUT" | tail -20; }
OUT="$(qf "$MIG/V0409__voucher_entry_foreign_amounts.up.sql")"
[[ $? -eq 0 ]] && ok "重放 up 成功（结构回来）" || { bad "重放 up 报错"; echo "$OUT" | tail -20; }

echo
echo "== ⑥ down 护栏反证（留一行外币分录，down 必须拒绝且结构不动）=="
q "insert into biz_voucher (voucher_word, voucher_no, period, voucher_date, voucher_type, source_type, summary, total_debit, total_credit, entry_count, status)
   values ('记','M104GUARD','1999-01',date '1999-01-01','MANUAL','MANUAL','__m104_guard__',2,2,2,'DRAFT')" >/dev/null
VID2="$(q "select id from biz_voucher where voucher_no='M104GUARD'")"
q "insert into biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name, debit_amount, credit_amount, fc_debit_amount, fc_credit_amount)
   values ('$VID2',1,'__m104_guard__',gen_random_uuid(),'1999','__m104_guard__',2,0,14.4,0)" >/dev/null
ERR="$(qf "$MIG/V0409__voucher_entry_foreign_amounts.down.sql")"
case "$ERR" in
    *"V0409 down 拒绝执行"*) ok "down 被数据护栏拒绝（有外币分录就不回滚）" ;;
    *) bad "down 护栏没生效：$ERR" ;;
esac
[[ "$(q "select count(*) from information_schema.columns where table_name='biz_voucher_entry' and column_name like 'fc\_%'")" == "2" ]] \
    && ok "护栏拒绝后结构未动（原币两列仍在）" || bad "护栏拒绝后半结构被拆了"

echo
echo "== 结果：PASS=$PASS FAIL=$FAIL =="
[[ $FAIL -eq 0 ]] || exit 1
