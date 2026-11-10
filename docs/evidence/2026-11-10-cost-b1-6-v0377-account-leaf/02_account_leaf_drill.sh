#!/usr/bin/env bash
# ============================================================================
# V0377（成本科目翻末级 + 9 个末级科目）真库往返演练
#
# 卡：t_486e652d（COST-B1-6）—— 卡面验收：
#   ① 迁移内置检查：三科目历史分录非 0 → RAISE EXCEPTION（不许翻）
#   ② 实测：干净库能翻，有分录的库被拦
#   ③ 9 个末级科目编码与设计 §3 一致
#
# 做法：建一个临时库，把 V0377 之前的所有迁移按版本号升序跑完（= 干净库的基线），
# 然后在这个库上做八段演练：
#   S1  有分录被拦   —— 对 4001 / 4101 / 2211 逐个造一条分录，每次 up 必须失败，
#                       且**失败后库必须一个字节都没变**（单事务回滚：仍是 3 个末级、0 个子科目）
#   S2  干净库能翻   —— 清掉探针分录后 up 成功，打印 9 个末级科目的完整形态
#   S3  幂等         —— 再 up 一次，科目行数/形态指纹完全不变
#   S4  down 护栏    —— 往 4001.01 记一条分录，down 必须拒绝并点名
#   S5  down 正常    —— 清掉分录后 down 成功：9 个科目删净、三科目翻回末级、无裂树
#   S6  down 幂等 + 回滚后重放 —— 再 down 一次不报错；再 up 一次回到 S2 的形态
#
# 用法：
#   bash docs/evidence/2026-11-10-cost-b1-6-v0377-account-leaf/02_account_leaf_drill.sh
#   KEEP_DB=1 bash .../.sh        # 跑完保留演练库（排查用，默认删）
#
# 退出码：0 = 全部断言通过；1 = 有断言失败
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
MROOT="$ROOT/backend/migrations"
DBHOST="${DBHOST:-127.0.0.1}"
DBPORT="${DBPORT:-5432}"
DBUSER="${DBUSER:-alun}"
DBPASS="${DBPASS:-alun}"
CUTOFF="${CUTOFF:-377}"                       # 本迁移版本号
DB="${DB:-mig_costb16_drill_$(date +%m%d%H%M%S)}"
UP="$MROOT/business/V0377__cost_accounts.up.sql"     # 文件名跟设计 §7.1 的 `cost_accounts`（号段从 V0375 顺延）
DOWN="$MROOT/business/V0377__cost_accounts.down.sql"

LEAVES=('4001.01' '4001.02' '4001.03' '4101.01' '4101.02' '4101.03' '4101.04' '4101.09' '2211.01')
LEAF_SQL="'4001.01','4001.02','4001.03','4101.01','4101.02','4101.03','4101.04','4101.09','2211.01'"

export PGPASSWORD="$DBPASS"
q()  { psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "${2:-$DB}" -tAc "$1" 2>&1; }
qq() { psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -q -X -v ON_ERROR_STOP=1 \
             --single-transaction -f "$1" 2>&1 >/dev/null; }

FAILS=0
ok()   { printf '  ✓ %s\n' "$1"; }
bad()  { printf '  ✗ %s\n' "$1"; FAILS=$((FAILS + 1)); }
head1(){ printf '\n== %s\n' "$1"; }
eq()   { if [ "$2" = "$3" ]; then ok "$1（$2）"; else bad "$1：期望 [$3] 实际 [$2]"; fi; }
neq()  { if [ "$2" != "$3" ]; then ok "$1（$2）"; else bad "$1：不该是 [$3]"; fi; }

cleanup() {
    if [ "${KEEP_DB:-0}" = "1" ]; then
        printf '\n==> KEEP_DB=1：保留演练库 %s\n' "$DB"
        return
    fi
    su - postgres -c "dropdb --force $DB" >/dev/null 2>&1 \
        || q "DROP DATABASE IF EXISTS $DB" postgres >/dev/null 2>&1 \
        || printf '⚠ 演练库 %s 没删掉（无权限），请手工清理\n' "$DB"
}
trap cleanup EXIT

# ---------- 建库 ----------
if q "select 1 from pg_database where datname='$DB'" postgres | grep -q 1; then
    printf 'FAIL: 临时库已存在：%s\n' "$DB" >&2
    exit 2
fi
su - postgres -c "createdb -O $DBUSER $DB" >/dev/null 2>&1 \
    || { printf 'FAIL: 建库失败（%s）\n' "$DB" >&2; exit 2; }
printf '==> 演练库：%s\n' "$DB"

# ---------- 基线：V0377 之前的所有迁移（按版本号升序） ----------
head1 "S0 基线：跑到 V0376 为止"
applied=0
while IFS=$'\t' read -r ver f; do
    [ "$ver" -lt "$CUTOFF" ] || continue
    # 基线那段 296 条迁移的 NOTICE 会把输出淹掉（我们要看的是 V0377 自己的日志），
    # 所以只在跑基线时把 client_min_messages 压到 warning；V0377 的那两次调用照常显示 NOTICE。
    if ! PGOPTIONS='-c client_min_messages=warning' qq "$f"; then
        printf 'FAIL: 基线迁移失败 v%s %s\n' "$ver" "$(basename "$f")" >&2
        PGOPTIONS='-c client_min_messages=warning' qq "$f" | grep -E "ERROR|FATAL" | head -3 >&2
        exit 2
    fi
    applied=$((applied + 1))
done < <(find "$MROOT" -name 'V*__*.up.sql' \
         | awk -F'/V' '{split($NF, a, "__"); print a[1] + 0 "\t" $0}' | sort -n -k1,1)
printf '  基线迁移数：%s 条（< V%s）\n' "$applied" "$CUTOFF"

eq "4001/4101/2211 都在且都是末级" \
   "$(q "select count(*) from biz_account where account_code in ('4001','4101','2211') and deleted_at is null and is_leaf = 1")" "3"
eq "三科目下子科目数（基线应为 0）" \
   "$(q "select count(*) from biz_account c join biz_account p on p.id=c.parent_id where p.account_code in ('4001','4101','2211') and c.deleted_at is null")" "0"
eq "9 个末级编码（基线上应全不存在）" \
   "$(q "select count(*) from biz_account where account_code in ($LEAF_SQL)")" "0"

# 造一条「记在某个汇总科目上的」历史分录（凭证 + 两条分录，借贷各 100）
seed_entry() { # $1 = 科目编码
    q "\
INSERT INTO biz_voucher (voucher_no, period, voucher_date, voucher_type, source_type,
                         summary, total_debit, total_credit, entry_count, status)
VALUES ('V0377-PROBE-$1', '2026-11', DATE '2026-11-10', 'MANUAL', 'MANUAL',
        'V0377 演练：历史分录护栏', 100, 100, 2, 'POSTED');
INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code,
                              account_name, debit_amount, credit_amount)
SELECT v.id, 1, 'V0377 演练：记在汇总科目上', a.id, a.account_code, a.account_name, 100, 0
  FROM biz_voucher v, biz_account a
 WHERE v.voucher_no = 'V0377-PROBE-$1' AND a.account_code = '$1';
INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code,
                              account_name, debit_amount, credit_amount)
SELECT v.id, 2, 'V0377 演练：对方科目', a.id, a.account_code, a.account_name, 0, 100
  FROM biz_voucher v, biz_account a
 WHERE v.voucher_no = 'V0377-PROBE-$1' AND a.account_code = '1002';" >/dev/null
}
clear_entries() {
    q "DELETE FROM biz_voucher WHERE voucher_no LIKE 'V0377-PROBE-%'" >/dev/null
}

# ---------- S1：有分录的库被拦 ----------
head1 "S1 有历史分录 ⇒ 迁移必须 RAISE EXCEPTION 且库零变化"
for code in '4001' '4101' '2211'; do
    clear_entries
    seed_entry "$code"
    eq "已造历史分录：$code" \
       "$(q "select count(*) from biz_voucher_entry where account_code = '$code'")" "1"
    out="$(qq "$UP")"; rc=$?
    neq "up 在 $code 有分录时失败（exit=$rc）" "$rc" "0"
    printf '    ┌ 迁移自身抛的错（原文）\n'
    printf '%s\n' "$out" | grep -E "ERROR|DETAIL|HINT" | sed 's/^/    │ /'
    printf '    └\n'
    printf '%s\n' "$out" | grep -q "V0377 拒绝执行" \
        && ok "拒绝信息点名（含科目与条数）" \
        || bad "拒绝信息里没有「V0377 拒绝执行」：$out"
    printf '%s\n' "$out" | grep -q "$code 有 1 条分录" \
        && ok "拒绝信息点名了 $code 的具体条数" \
        || bad "拒绝信息没点名 $code 有 1 条分录：$out"
    # 单事务回滚：三科目仍是末级、9 个末级科目一个都没种下
    eq "失败后三个汇总科目仍是末级（整份迁移回滚）" \
       "$(q "select count(*) from biz_account where account_code in ('4001','4101','2211') and is_leaf = 1")" "3"
    eq "失败后 9 个末级编码仍未种下" \
       "$(q "select count(*) from biz_account where account_code in ($LEAF_SQL)")" "0"
done
clear_entries
eq "清掉探针分录后分录表归零" \
   "$(q "select count(*) from biz_voucher where voucher_no like 'V0377-PROBE-%'")" "0"

# ---------- S2：干净库能翻 ----------
head1 "S2 干净库 ⇒ up 成功，9 个末级科目形态正确"
out="$(qq "$UP")"; rc=$?
eq "up 退出码" "$rc" "0"
printf '%s\n' "$out" | grep -q "V0377 形态断言通过" && ok "形态自检通过" || bad "形态自检没通过：$out"
printf '%s\n' "$out" | grep -q "V0377 口径断言通过" && ok "口径自检通过" || bad "口径自检没通过：$out"
printf '    ┌ 迁移自身的三条日志（原文）\n'
printf '%s\n' "$out" | grep -E "NOTICE|WARNING" | sed 's/^/    │ /'
printf '    └\n'

eq "翻非末级的科目数" \
   "$(q "select count(*) from biz_account where account_code in ('4001','4101','2211') and is_leaf = 0 and deleted_at is null")" "3"
eq "9 个末级科目全部种下且是末级" \
   "$(q "select count(*) from biz_account where account_code in ($LEAF_SQL) and is_leaf = 1 and status = 1 and deleted_at is null")" "9"
eq "9 个末级科目的父级/级次/类别/方向全部正确" \
   "$(q "select count(*) from biz_account c join biz_account p on p.id = c.parent_id
          where c.account_code in ($LEAF_SQL)
            and c.level = p.level + 1
            and c.account_category = p.account_category
            and c.balance_direction = p.balance_direction")" "9"

printf '\n  -- 9 个末级科目（编码 / 名称 / 父级 / 级次 / 类别 / 方向 / 末级）\n'
q "select '     ' || c.account_code || '  ' || rpad(c.account_name, 14) || '  ← ' || p.account_code
        || '  level=' || c.level || '  ' || c.account_category || '/' || c.balance_direction
        || '  is_leaf=' || c.is_leaf
     from biz_account c join biz_account p on p.id = c.parent_id
    where c.account_code in ($LEAF_SQL) order by c.account_code"

# 与设计 §3.3 ③ / §1 D-14 的编码-名称-父级三元组逐条比对
printf '\n  -- 与设计稿的编码/名称/父级三元组比对\n'
design_mismatch="$(q "select count(*) from biz_account c join biz_account p on p.id = c.parent_id
   where (c.account_code, c.account_name, p.account_code) not in (values
     ('4001.01','生产成本-直接材料','4001'), ('4001.02','生产成本-直接人工','4001'),
     ('4001.03','生产成本-制造费用','4001'), ('4101.01','制造费用-折旧费','4101'),
     ('4101.02','制造费用-水电费','4101'),   ('4101.03','制造费用-修理费','4101'),
     ('4101.04','制造费用-车间管理费','4101'),('4101.09','制造费用-其他','4101'),
     ('2211.01','应付职工薪酬-工资','2211'))
   and c.account_code in ($LEAF_SQL)")"
eq "编码/名称/父级与 §3.3 逐字一致（不符条数）" "$design_mismatch" "0"

eq "记账闸：汇总科目 3 个都会被拒" \
   "$(q "select count(*) from biz_account where deleted_at is null and is_leaf = 0 and account_code in ('4001','4101','2211')")" "3"
eq "记账闸：9 个末级科目都可记账" \
   "$(q "select count(*) from biz_account where deleted_at is null and is_leaf = 1 and status = 1 and account_code in ($LEAF_SQL)")" "9"

FINGER1="$(q "select md5(string_agg(account_code || '|' || account_name || '|' || coalesce(parent_id::text,'-')
                                        || '|' || level || '|' || account_category || '|' || balance_direction
                                        || '|' || is_leaf || '|' || status, ',' order by account_code))
                  from biz_account where account_code in ('4001','4101','2211',$LEAF_SQL)")"
printf '  科目形态指纹（S2）：%s\n' "$FINGER1"

# ---------- S3：幂等 ----------
head1 "S3 幂等：再 up 一次，形态一字节不变"
out="$(qq "$UP")"; rc=$?
eq "第二次 up 退出码" "$rc" "0"
FINGER2="$(q "select md5(string_agg(account_code || '|' || account_name || '|' || coalesce(parent_id::text,'-')
                                        || '|' || level || '|' || account_category || '|' || balance_direction
                                        || '|' || is_leaf || '|' || status, ',' order by account_code))
                  from biz_account where account_code in ('4001','4101','2211',$LEAF_SQL)")"
eq "两次 up 后科目形态指纹一致" "$FINGER2" "$FINGER1"
eq "9 个末级编码没有重复行" \
   "$(q "select count(*) from biz_account where account_code in ($LEAF_SQL)")" "9"
eq "整表裂树节点数（末级挂子）" \
   "$(q "select count(*) from biz_account p join biz_account c on c.parent_id = p.id and c.deleted_at is null
          where p.deleted_at is null and p.is_leaf = 1")" "0"

# ---------- S4：down 护栏 ----------
head1 "S4 down 护栏：末级科目上有分录 ⇒ 拒绝回滚"
seed_entry '4001.01'
out="$(qq "$DOWN")"; rc=$?
neq "down 在 4001.01 有分录时失败（exit=$rc）" "$rc" "0"
printf '%s\n' "$out" | grep -q "V0377 down 拒绝执行" && ok "拒绝信息点名" || bad "拒绝信息缺失：$out"
printf '%s\n' "$out" | grep -q "4001.01 有 1 条" && ok "点名 4001.01 的条数" || bad "没点名条数：$out"
eq "拒绝后 9 个末级科目仍在" \
   "$(q "select count(*) from biz_account where account_code in ($LEAF_SQL)")" "9"
clear_entries

# ---------- S5：down 正常 ----------
head1 "S5 down 正常：9 个科目删净、三科目翻回末级"
out="$(qq "$DOWN")"; rc=$?
eq "down 退出码" "$rc" "0"
printf '%s\n' "$out" | grep -q "V0377 down 完成" && ok "down 反向自检通过" || bad "down 自检没通过：$out"
eq "9 个末级编码全部删除" \
   "$(q "select count(*) from biz_account where account_code in ($LEAF_SQL)")" "0"
eq "4001/4101/2211 翻回末级" \
   "$(q "select count(*) from biz_account where account_code in ('4001','4101','2211') and is_leaf = 1 and deleted_at is null")" "3"
eq "回滚后裂树节点数" \
   "$(q "select count(*) from biz_account p join biz_account c on c.parent_id = p.id and c.deleted_at is null
          where p.deleted_at is null and p.is_leaf = 1")" "0"
eq "回滚后孤立分录数" \
   "$(q "select count(*) from biz_voucher_entry where account_code in ($LEAF_SQL)")" "0"

# ---------- S6：down 幂等 + 回滚后重放 ----------
head1 "S6 down 幂等 + 回滚后重放"
out="$(qq "$DOWN")"; rc=$?
eq "第二次 down 退出码（幂等）" "$rc" "0"
out="$(qq "$UP")"; rc=$?
eq "回滚后重放 up 退出码" "$rc" "0"
FINGER3="$(q "select md5(string_agg(account_code || '|' || account_name || '|' || coalesce(parent_id::text,'-')
                                        || '|' || level || '|' || account_category || '|' || balance_direction
                                        || '|' || is_leaf || '|' || status, ',' order by account_code))
                  from biz_account where account_code in ('4001','4101','2211',$LEAF_SQL)")"
eq "重放后的形态指纹与 S2 一致" "$FINGER3" "$FINGER1"
printf '  科目形态指纹（S6 重放后）：%s\n' "$FINGER3"

# ---------- S7：客户已自建「形态正确」的同码科目 ⇒ 复用，不重复种 ----------
head1 "S7 客户已自建形态正确的 4001.01 ⇒ 复用（ON CONFLICT 幂等，不覆盖客户数据）"
qq "$DOWN" >/dev/null                       # 先回到「无 9 个叶子」的基线
q "INSERT INTO biz_account (account_code, account_name, parent_id, level,
                            account_category, balance_direction, is_leaf, status, remark)
   SELECT '4001.01', '客户自建-直接材料', p.id, p.level + 1,
          p.account_category, p.balance_direction, 1, 1, '客户手工建（演练用）'
     FROM biz_account p WHERE p.account_code = '4001'" >/dev/null
out="$(qq "$UP")"; rc=$?
eq "已有形态正确的 4001.01 时 up 退出码" "$rc" "0"
eq "up 后 4001.01 仍只有 1 行（没种出重复科目）" \
   "$(q "select count(*) from biz_account where account_code = '4001.01'")" "1"
eq "客户那行没被本迁移覆盖（remark 仍是客户的）" \
   "$(q "select remark from biz_account where account_code = '4001.01'")" "客户手工建（演练用）"
eq "9 个末级编码齐备" \
   "$(q "select count(*) from biz_account where account_code in ($LEAF_SQL)")" "9"

# ---------- S8：客户自建的同码科目形态不对（挂错父级）⇒ 前置护栏拒绝并点名 ----------
head1 "S8 客户自建的同码科目形态不对 ⇒ 拒绝并点名（不改已有科目行）"
qq "$DOWN" >/dev/null                       # 回到基线（down 按编码删，客户那行也一起删掉）
eq "down 后 4001.01 已清空（为 S8 造反例）" \
   "$(q "select count(*) from biz_account where account_code = '4001.01'")" "0"
q "INSERT INTO biz_account (account_code, account_name, parent_id, level,
                            account_category, balance_direction, is_leaf, status, remark)
   SELECT '4001.01', '客户自建-挂错父级', p.id, p.level + 1,
          p.account_category, p.balance_direction, 1, 1, '客户手工建（演练用·错挂）'
     FROM biz_account p WHERE p.account_code = '4101'" >/dev/null
out="$(qq "$UP")"; rc=$?
neq "形态不符时 up 失败（exit=$rc）" "$rc" "0"
printf '    ┌ 迁移自身抛的错（原文）\n'
printf '%s\n' "$out" | grep -E "ERROR|DETAIL|HINT" | sed 's/^/    │ /'
printf '    └\n'
printf '%s\n' "$out" | grep -q "末级科目编码已被占用且形态不符" \
    && ok "拒绝信息点名「编码已被占用且形态不符」" \
    || bad "拒绝信息缺失：$out"
printf '%s\n' "$out" | grep -q "4001.01(父级=4101" \
    && ok "拒绝信息指出 4001.01 挂在了 4101 下" \
    || bad "拒绝信息没指出错挂的父级：$out"
eq "拒绝后 4001 仍是末级（库零变化）" \
   "$(q "select count(*) from biz_account where account_code = '4001' and is_leaf = 1")" "1"
eq "拒绝后客户那行原样保留（本迁移不改已有科目行）" \
   "$(q "select remark from biz_account where account_code = '4001.01'")" "客户手工建（演练用·错挂）"

# 收尾：删掉反例行 → 再 up 一次 → 形态回到 S2
q "DELETE FROM biz_account WHERE account_code = '4001.01'" >/dev/null
out="$(qq "$UP")"; rc=$?
eq "清掉反例后重放 up 退出码" "$rc" "0"
FINGER4="$(q "select md5(string_agg(account_code || '|' || account_name || '|' || coalesce(parent_id::text,'-')
                                        || '|' || level || '|' || account_category || '|' || balance_direction
                                        || '|' || is_leaf || '|' || status, ',' order by account_code))
                  from biz_account where account_code in ('4001','4101','2211',$LEAF_SQL)")"
eq "S8 收尾后的形态指纹与 S2 一致" "$FINGER4" "$FINGER1"

printf '\n==> 演练结论：%s\n' "$([ "$FAILS" -eq 0 ] && echo '全部断言通过' || echo "失败 $FAILS 项")"
exit "$([ "$FAILS" -eq 0 ] && echo 0 || echo 1)"