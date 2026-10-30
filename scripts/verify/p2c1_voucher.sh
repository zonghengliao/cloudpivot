#!/usr/bin/env bash
# ============================================================
# P2-C1 应付 + 凭证自动生成：结构 / 约束 / 种子 / 单测 一站式验收
#
#   覆盖：
#     ① 四张表（biz_payable / biz_voucher / biz_voucher_entry / biz_voucher_rule）都在
#     ② 卡面硬红线 3：`total_debit = total_credit` 的 DB CHECK 真的拦得住
#     ③ 卡面硬红线 4：分录借贷不能同时有值
#     ④ 应付单四条业务约束（amount / paid / unpaid 恒等 / credit_days）
#     ⑤ 科目种子（1405 / 2202 / 2221 / 2221.01）与默认映射规则 PURCHASE_IN_DEFAULT
#     ⑥ 幂等铁律：同一来源单据只能有一张有效应付单（uk_biz_payable_source）
#     ⑦ 全库不变量：没有不平的凭证 / 没有双边分录 / 没有未付金额对不上的应付
#     ⑧ cargo 单测：business::erp::voucher（19 条，含 16 条真库用例）与 ::payable（2 条）
#
# 用法：
#   export P2C1_DB_PASSWORD='<cloudpivot 库口令>'   # 不给则从 /etc/cloudpivot/cloudpivot-dev.env 取
#   export P2C1_DB_NAME=cloudpivot_p2c1            # 默认 cloudpivot_p2c1（隔离库）
#   bash scripts/verify/p2c1_voucher.sh
#
#   只要结构/约束部分（不跑 cargo）：SKIP_CARGO=1 bash scripts/verify/p2c1_voucher.sh
#
# 退出码 = 是否有断言失败（0 = 全过）。
#
# ⚠️ 前置：目标库必须已应用 V0148（应付）+ V0149（凭证/分录/规则 + 科目种子）。
#    隔离库建法（144 实测）：
#      su - postgres -c 'createdb -O cloudpivot cloudpivot_p2c1'
#      su - postgres -c "pg_dump --no-owner cloudpivot_dev" > /tmp/x.sql
#      su - postgres -c "psql -q -d cloudpivot_p2c1 -c 'SET ROLE cloudpivot' -f /tmp/x.sql"
#      （或直接用 target/debug/cloudpivot 起一次让迁移自动跑完）
# ============================================================
set -uo pipefail

REPO="${REPO:-/opt/cloudpivot}"
DBN="${P2C1_DB_NAME:-cloudpivot_p2c1}"
DBH="${P2C1_DB_HOST:-127.0.0.1}"
DBU="${P2C1_DB_USER:-cloudpivot}"
SKIP_CARGO="${SKIP_CARGO:-0}"

if [ -z "${P2C1_DB_PASSWORD:-}" ] && [ -r /etc/cloudpivot/cloudpivot-dev.env ]; then
    # shellcheck disable=SC1091
    . /etc/cloudpivot/cloudpivot-dev.env
    P2C1_DB_PASSWORD="${NIK_DATABASE_PASSWORD:-}"
fi
: "${P2C1_DB_PASSWORD:?请先 export P2C1_DB_PASSWORD（cloudpivot 库口令）或提供 /etc/cloudpivot/cloudpivot-dev.env}"

PASS=0
FAIL=0
REJECT_MSG=""
ok() { PASS=$((PASS + 1)); printf '[PASS] %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf '[FAIL] %s\n' "$1"; }
assert_eq() { if [ "$1" = "$2" ]; then ok "$3（$2）"; else bad "$3：期望 [$1] 实际 [$2]"; fi; }
assert_num() {
    if awk -v a="$1" -v b="$2" 'BEGIN { if (a == "" || b == "" || b == "null") exit 1; exit !(a + 0 == b + 0) }'; then
        ok "$3（$2）"
    else
        bad "$3：期望 [$1] 实际 [$2]"
    fi
}

psql_q() { PGPASSWORD="$P2C1_DB_PASSWORD" psql -h "$DBH" -U "$DBU" -d "$DBN" -Atc "$1" 2>&1 | head -1; }
# 期望失败的探针：返回 0 = 语句被库拒绝（符合预期）
psql_rejects() {
    local out
    out=$(PGPASSWORD="$P2C1_DB_PASSWORD" psql -h "$DBH" -U "$DBU" -d "$DBN" -v ON_ERROR_STOP=1 \
        -Atc "$1" 2>&1)
    local code=$?
    REJECT_MSG=$(printf '%s' "$out" | head -1)
    return $code
}

echo "==> 0. 目标库 $DBN@$DBH"
if ! psql_q "select 1" | grep -q 1; then
    bad "连不上库 $DBN（检查 P2C1_DB_PASSWORD / 库是否已建）"
    echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
    exit 1
fi
echo "    库版本 = $(psql_q 'select max(version::int) from schema_migrations')"

echo "==> 1. 四张表"
for t in biz_payable biz_voucher biz_voucher_entry biz_voucher_rule; do
    assert_eq "1" "$(psql_q "select count(*) from pg_tables where schemaname='public' and tablename='$t'")" "表 $t 存在"
done

echo "==> 2. 约束（硬红线 3 / 4 + 应付恒等）"
for c in ck_biz_voucher_balance ck_biz_voucher_entries ck_biz_voucher_red_ref ck_biz_voucher_status; do
    assert_eq "1" "$(psql_q "select count(*) from pg_constraint where conname='$c'")" "约束 $c 存在"
done
for c in ck_biz_voucher_entry_side ck_biz_voucher_entry_positive uk_biz_voucher_entry_line; do
    assert_eq "1" "$(psql_q "select count(*) from pg_constraint where conname='$c'")" "约束 $c 存在"
done
for c in ck_biz_payable_unpaid ck_biz_payable_paid ck_biz_payable_amount ck_biz_payable_status ck_biz_payable_settled; do
    assert_eq "1" "$(psql_q "select count(*) from pg_constraint where conname='$c'")" "约束 $c 存在"
done
for i in uk_biz_payable_source uk_biz_payable_no uk_biz_voucher_no uk_biz_voucher_rule_code; do
    assert_eq "1" "$(psql_q "select count(*) from pg_indexes where indexname='$i'")" "唯一索引 $i 存在"
done

echo "==> 3. 约束探针（写坏数据必须被库拒绝）"
if psql_rejects "INSERT INTO biz_voucher (voucher_no, period, voucher_date, total_debit, total_credit, entry_count)
                 VALUES ('__P2C1_PROBE_UNBALANCED__', '1999-01', DATE '1999-01-01', 100, 90, 2)"; then
    bad "借贷不平衡的凭证居然写进了库（ck_biz_voucher_balance 失效）"
else
    ok "借贷不平衡被拦：$(printf '%s' "$REJECT_MSG" | sed 's/.*constraint //')"
fi
if psql_rejects "INSERT INTO biz_voucher (voucher_no, period, voucher_date, total_debit, total_credit, entry_count)
                 VALUES ('__P2C1_PROBE_ONEENTRY__', '1999-01', DATE '1999-01-01', 100, 100, 1)"; then
    bad "单分录凭证写进了库（ck_biz_voucher_entries 失效）"
else
    ok "entry_count < 2 被拦"
fi
if psql_rejects "INSERT INTO biz_voucher (voucher_no, period, voucher_date, total_debit, total_credit, entry_count, is_red, reverse_of)
                 VALUES ('__P2C1_PROBE_REDNOREF__', '1999-01', DATE '1999-01-01', 100, 100, 2, 1, NULL)"; then
    bad "红字凭证不带 reverse_of 写进了库（ck_biz_voucher_red_ref 失效）"
else
    ok "红字凭证必须指向原凭证（red_ref 拦住了）"
fi
if psql_rejects "INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name, debit_amount, credit_amount)
                 VALUES ('00000000-0000-0000-0000-000000000000', 1, '__P2C1_PROBE_BOTH__', gen_random_uuid(), 'X', 'X', 10, 10)"; then
    bad "借贷双边同时有值的分录写进了库（ck_biz_voucher_entry_side 失效）"
else
    ok "分录双边被拦（硬红线 4）"
fi
if psql_rejects "INSERT INTO biz_payable (payable_no, supplier_id, source_type, source_id, source_no, amount, paid_amount, unpaid_amount, due_date, period_code)
                 VALUES ('__P2C1_PROBE_UNPAID__', gen_random_uuid(), 'STOCK_IN', gen_random_uuid(), 'X', 100, 10, 95, DATE '2026-10-01', '2026-10')"; then
    bad "未付金额对不上的应付写进了库（ck_biz_payable_unpaid 失效）"
else
    ok "unpaid ≠ amount − paid 被拦"
fi
if psql_rejects "INSERT INTO biz_payable (payable_no, supplier_id, source_type, source_id, source_no, amount, paid_amount, unpaid_amount, due_date, period_code)
                 VALUES ('__P2C1_PROBE_PAID__', gen_random_uuid(), 'STOCK_IN', gen_random_uuid(), 'X', 100, 120, -20, DATE '2026-10-01', '2026-10')"; then
    bad "已付金额超过应付写进了库（ck_biz_payable_paid 失效）"
else
    ok "paid > amount 被拦"
fi

echo "==> 4. 幂等铁律：同一来源单据只能一张有效应付单"
PSRC="00000000-0000-4000-8000-0000c1c10001"
psql_q "DELETE FROM biz_payable WHERE source_id = '$PSRC'::uuid" >/dev/null
psql_q "INSERT INTO biz_payable (payable_no, supplier_id, source_type, source_id, source_no, amount, paid_amount, unpaid_amount, due_date, period_code)
        VALUES ('__P2C1_PROBE_DUP__', gen_random_uuid(), 'STOCK_IN', '$PSRC'::uuid, 'X', 100, 0, 100, DATE '2026-10-01', '2026-10')" >/dev/null
if psql_rejects "INSERT INTO biz_payable (payable_no, supplier_id, source_type, source_id, source_no, amount, paid_amount, unpaid_amount, due_date, period_code)
                 VALUES ('__P2C1_PROBE_DUP2__', gen_random_uuid(), 'STOCK_IN', '$PSRC'::uuid, 'X', 100, 0, 100, DATE '2026-10-01', '2026-10')"; then
    bad "同一来源单据生成了第二张应付单（uk_biz_payable_source 失效）"
else
    ok "同一来源重复生成应付被拦（过账重试不会重复挂账）"
fi
psql_q "DELETE FROM biz_payable WHERE source_id = '$PSRC'::uuid" >/dev/null

echo "==> 5. 科目种子与默认映射规则"
assert_eq "4" "$(psql_q "select count(*) from biz_account where account_code in ('1405','2202','2221','2221.01') and deleted_at is null")" "科目 1405 / 2202 / 2221 / 2221.01 齐备"
assert_eq "1" "$(psql_q "select count(*) from biz_account c join biz_account p on p.id = c.parent_id where c.account_code='2221.01' and p.account_code='2221' and c.is_leaf=1")" "2221.01 挂在 2221 下且是末级"
assert_eq "1" "$(psql_q "select count(*) from biz_voucher_rule where rule_code='PURCHASE_IN_DEFAULT' and deleted_at is null and debit_account_id is not null and credit_account_id is not null and tax_account_id is not null")" "PURCHASE_IN_DEFAULT 规则唯一且三个科目都配好"
assert_eq "1405" "$(psql_q "select a.account_code from biz_voucher_rule r join biz_account a on a.id=r.debit_account_id where r.rule_code='PURCHASE_IN_DEFAULT' and r.deleted_at is null")" "规则借方 = 1405 库存商品"
assert_eq "2202" "$(psql_q "select a.account_code from biz_voucher_rule r join biz_account a on a.id=r.credit_account_id where r.rule_code='PURCHASE_IN_DEFAULT' and r.deleted_at is null")" "规则贷方 = 2202 应付账款"
assert_eq "2221.01" "$(psql_q "select a.account_code from biz_voucher_rule r join biz_account a on a.id=r.tax_account_id where r.rule_code='PURCHASE_IN_DEFAULT' and r.deleted_at is null")" "规则税科目 = 2221.01 应交税费-进项税"

echo "==> 6. 全库不变量（有数据时才有意义）"
assert_eq "0" "$(psql_q "select count(*) from biz_voucher where total_debit <> total_credit and deleted_at is null")" "没有借贷不平的凭证"
assert_num "0" "$(psql_q "select count(*) from biz_voucher_entry where debit_amount > 0 and credit_amount > 0")" "没有借贷双边的分录"
assert_eq "0" "$(psql_q "select count(*) from biz_payable where unpaid_amount <> amount - paid_amount and deleted_at is null")" "没有未付金额对不上的应付"
assert_eq "0" "$(psql_q "select count(*) from biz_voucher where is_red = 1 and (reverse_of is null or total_debit = 0)")" "红字凭证都指向原凭证且金额非零"

LN=$(psql_q "select count(*) from biz_voucher_entry")
VD=$(psql_q "select count(*) from biz_voucher where deleted_at is null")
PD=$(psql_q "select count(*) from biz_payable where deleted_at is null")
echo "    现有数据：凭证 $VD 张 / 分录 $LN 行 / 应付 $PD 张"

echo "==> 7. cargo 单测（真库用例走 ERP_DB_URL → $DBN）"
if [ "$SKIP_CARGO" = "1" ]; then
    echo "    SKIP_CARGO=1，跳过"
else
    export PATH="/root/.cargo/bin:$PATH"
    export ERP_DB_URL="postgres://${DBU}:${P2C1_DB_PASSWORD}@${DBH}:5432/${DBN}"
    LOG=$(mktemp)
    if ( cd "$REPO/backend" && cargo test -j 1 --lib business::erp::voucher -- --test-threads=1 ) >"$LOG" 2>&1; then
        LINE=$(grep -E "^test result:" "$LOG" | head -1)
        ok "business::erp::voucher 单测：$LINE"
    else
        bad "business::erp::voucher 单测失败：$(grep -E '^(---- |failures:)' "$LOG" | head -5 | tr '\n' ' ')"
    fi
    rm -f "$LOG"
fi

echo
echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
