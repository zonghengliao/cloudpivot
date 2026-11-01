#!/usr/bin/env bash
# ============================================================
# ERP-R3 验收：现金流量表（真 HTTP + 真库 + 跨表勾稽货币资金）
#
# 用法：
#   bash scripts/verify/erpr3_cash_flow.sh
#
# 可覆盖的环境变量：
#   REPO=/opt/cloudpivot              后台仓根
#   R3_DB=cloudpivot_erpr3v           隔离库名（不存在则自建；**不 DROP**，方便复跑/评审）
#   R3_PORT=18096                     监听端口（被占用时自动 +1 找空）
#   NIK_DATABASE_USER/PASSWORD        连库角色（默认 alun/alun，需能建库）
#
# 做的事：
#   ① 自建隔离库 → 起一次实例把迁移跑到最新（含 V0185：现金流量表分类行 + 权限节点）
#   ② 造夹具：7 张 POSTED 凭证（9 月股东投入 / 10 月销售收款、采购、工资、交税、买设备、借款）
#   ③ 打现金流量表接口：三活动逐行对金额、净增加额、期初 / 期末、跨表勾稽货币资金
#   ④ 同一份数据打资产负债表接口：货币资金 = 14,400（两表互证）
#   ⑤ 空期间（2026-07）全 0 且期初 = 期末；无发生额月份（2026-11）本期 0 / 累计不变
#   ⑥ 反例：非法期间（400 + 10001）、空月份没开账（400 + 66010）、无令牌（401/403）
#   ⑦ 未分类（借 1002 / 贷 1012 其他货币资金）→ warnings 非空、勾稽照实变红、差额精确；删掉后回绿
#   ⑧ 导出 xlsx（200 + PK 魔数 + 附件头）
#   ⑨ psql 侧核对：分类行 12 条 / 权限节点 / 非现金科目全覆盖
#
# 期望值（夹具决定，写死在这里当契约）：
#   本期（2026-10）：经营流入 5,000；经营流出 2,000 + 800 + 300 = 3,100 → 净 1,900
#                    投资流出 1,500 → 净 −1,500；筹资流入 4,000 → 净 4,000
#                    净增加 4,400；期初 10,000（9 月股东投入）；期末 14,400
#   本年累计：经营 1,900 / 投资 −1,500 / 筹资 14,000（10,000 + 4,000）→ 净增加 14,400
#   勾稽：期末 14,400 = 资产负债表「货币资金」14,400 ⇒ diff 0
# ============================================================
set -uo pipefail

REPO=${REPO:-/opt/cloudpivot}
DB=${R3_DB:-cloudpivot_erpr3v}
BIN="$REPO/backend/target/debug/cloudpivot"
LOG=/tmp/erpr3_verify_server.log
DBUSER=${NIK_DATABASE_USER:-alun}
DBPASS=${NIK_DATABASE_PASSWORD:-alun}
PASS=0
FAIL=0

say() { printf '%s\n' "$*"; }
ok() { PASS=$((PASS + 1)); say "[PASS] $*"; }
bad() { FAIL=$((FAIL + 1)); say "[FAIL] $*"; }
expect() { if [ "$2" = "$3" ]; then ok "$1（$3）"; else bad "$1（期望 $2，实际 $3）"; fi; }
# 数值比较：JSON 里的 NUMERIC 回来是字符串（"4400.0000"），统一按数值比
expect_num() {
    if awk -v a="$2" -v b="$3" 'BEGIN{exit !(a+0==b+0)}'; then
        ok "$1（$3）"
    else
        bad "$1（期望 $2，实际 $3）"
    fi
}
# JSON 点路径取值
jget() {
    python3 - "$1" "$2" <<'PY'
import json, sys
raw, path = sys.argv[1], sys.argv[2]
try:
    node = json.loads(raw)
except Exception:
    print("<not-json>"); raise SystemExit
for part in path.split("."):
    if part == "":
        continue
    if isinstance(node, list):
        node = node[int(part)]
    elif isinstance(node, dict):
        node = node.get(part)
    else:
        print("<missing>"); raise SystemExit
    if node is None:
        print("<null>"); raise SystemExit
if isinstance(node, bool):
    print("true" if node else "false")
elif isinstance(node, (dict, list)):
    print(json.dumps(node, ensure_ascii=False))
else:
    print(node)
PY
}
# 现金流量表某一行（活动分区里按 item_code 取）
jline() {
    python3 - "$1" "$2" "$3" <<'PY'
import json, sys
raw, code, field = sys.argv[1], sys.argv[2], sys.argv[3]
data = json.loads(raw)["data"]
for key in ("operating", "investing", "financing", "other"):
    section = data[key]
    for row in section["inflow"] + section["outflow"]:
        if row["item_code"] == code:
            print(row.get(field)); raise SystemExit
print("<missing>")
PY
}
# 资产负债表某一侧某一项的金额（两表互证的用）
bs_amount() {
    python3 - "$1" "$2" "$3" <<'PY'
import json, sys
raw, side, code = sys.argv[1], sys.argv[2], sys.argv[3]
data = json.loads(raw)["data"]
for row in data[side]:
    if row["item_code"] == code:
        print("null" if row["amount"] is None else row["amount"]); raise SystemExit
print("<missing>")
PY
}
# warnings 数组里的条数
jwarn_len() {
    python3 - "$1" <<'PY'
import json, sys
print(len(json.loads(sys.argv[1])["data"]["warnings"]))
PY
}

# ---------- 0. 前置 ----------
[ -x "$BIN" ] || { say "缺二进制 $BIN（先 cd backend && cargo build）"; exit 1; }
say "== 二进制 $BIN（$(stat -c %y "$BIN" 2>/dev/null)）"

su - postgres -c "psql -Atc \"select 1 from pg_database where datname='$DB'\"" | grep -q 1 \
    || su - postgres -c "createdb -O $DBUSER $DB"
say "== 隔离库 $DB 就绪（owner=$DBUSER）"

PORT=${R3_PORT:-18096}
for _ in $(seq 1 40); do
    ss -ltn 2>/dev/null | grep -q ":$PORT " || break
    PORT=$((PORT + 1))
done
BASE="http://127.0.0.1:$PORT/api"

export NIK_PROFILE=test
export NIK_DATABASE_NAME=$DB
export NIK_DATABASE_USER=$DBUSER
export NIK_DATABASE_PASSWORD=$DBPASS
export NIK_SERVER_LISTEN=127.0.0.1:$PORT
export NIK_ADMIN_PASSWORD="Erpr3_verify_2026"
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="erpr3_verify_secret_0123456789abcdef"
export CLOUDPIVOT_STORAGE_MASTER_KEY="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
say "== 实例端口 $PORT（日志 $LOG）"

cd "$REPO/backend" || exit 1
setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
SRV=$!
for _ in $(seq 1 180); do
    grep -q "数据库迁移完成" "$LOG" 2>/dev/null && break
    kill -0 "$SRV" 2>/dev/null || { say "实例启动失败："; tail -20 "$LOG"; exit 1; }
    sleep 1
done
grep -E "数据库迁移完成|迁移失败" "$LOG" | tail -2
sleep 2
kill -0 "$SRV" 2>/dev/null || { say "实例已退出"; tail -20 "$LOG"; exit 1; }
cleanup() { kill "$SRV" 2>/dev/null; sleep 1; kill -9 "$SRV" 2>/dev/null; }
trap cleanup EXIT

psql_at() { PGPASSWORD=$DBPASS psql -h 127.0.0.1 -U "$DBUSER" -d "$DB" -Atc "$1" 2>&1; }
PSQL() { PGPASSWORD=$DBPASS psql -h 127.0.0.1 -U "$DBUSER" -d "$DB" -q -v ON_ERROR_STOP=1; }

# ---------- 1. 夹具 ----------
PSQL <<'SQL'
DELETE FROM biz_voucher WHERE voucher_word = 'ERPR3V';

-- 期间：2026-07（早于任何凭证，用于「空期间」）、2026-09（已结账，累计来源）、
--       2026-10（编制期间）、2026-11（无发生额，验证「本期 0 / 累计不变」）
INSERT INTO biz_accounting_period (period_year, period_month, period_code, start_date, end_date, status)
VALUES (2026, 7, '2026-07', '2026-07-01', '2026-07-31', 'OPEN'),
       (2026, 9, '2026-09', '2026-09-01', '2026-09-30', 'CLOSED'),
       (2026, 10, '2026-10', '2026-10-01', '2026-10-31', 'OPEN'),
       (2026, 11, '2026-11', '2026-11-01', '2026-11-30', 'OPEN')
ON CONFLICT (period_code) DO NOTHING;

-- 7 张 POSTED 凭证（借 = 贷）
INSERT INTO biz_voucher (id, voucher_word, voucher_no, period, voucher_date, voucher_type,
                         source_type, source_no, summary, total_debit, total_credit,
                         entry_count, status, is_red)
VALUES
 ('e4f30001-0000-4000-8000-000000000001', 'ERPR3V', '0001', '2026-09', '2026-09-10', 'AUTO',
  'MANUAL', 'ERPR3V-1', '股东投入（筹资流入）', 10000, 10000, 2, 'POSTED', 0),
 ('e4f30001-0000-4000-8000-000000000002', 'ERPR3V', '0002', '2026-10', '2026-10-05', 'AUTO',
  'MANUAL', 'ERPR3V-2', '销售收款（经营流入）', 5000, 5000, 2, 'POSTED', 0),
 ('e4f30001-0000-4000-8000-000000000003', 'ERPR3V', '0003', '2026-10', '2026-10-06', 'AUTO',
  'MANUAL', 'ERPR3V-3', '采购付款（经营流出）', 2000, 2000, 2, 'POSTED', 0),
 ('e4f30001-0000-4000-8000-000000000004', 'ERPR3V', '0004', '2026-10', '2026-10-07', 'AUTO',
  'MANUAL', 'ERPR3V-4', '发工资（经营流出）', 800, 800, 2, 'POSTED', 0),
 ('e4f30001-0000-4000-8000-000000000005', 'ERPR3V', '0005', '2026-10', '2026-10-08', 'AUTO',
  'MANUAL', 'ERPR3V-5', '交税（经营流出）', 300, 300, 2, 'POSTED', 0),
 ('e4f30001-0000-4000-8000-000000000006', 'ERPR3V', '0006', '2026-10', '2026-10-09', 'AUTO',
  'MANUAL', 'ERPR3V-6', '买设备（投资流出）', 1500, 1500, 2, 'POSTED', 0),
 ('e4f30001-0000-4000-8000-000000000007', 'ERPR3V', '0007', '2026-10', '2026-10-10', 'AUTO',
  'MANUAL', 'ERPR3V-7', '借款（筹资流入）', 4000, 4000, 2, 'POSTED', 0);

INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code,
                               account_name, debit_amount, credit_amount)
SELECT 'e4f30001-0000-4000-8000-000000000001'::uuid, 1, '投入', a.id, a.account_code, a.account_name, 10000, 0
  FROM biz_account a WHERE a.account_code = '1002' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f30001-0000-4000-8000-000000000001'::uuid, 2, '实收资本', a.id, a.account_code, a.account_name, 0, 10000
  FROM biz_account a WHERE a.account_code = '3001' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f30001-0000-4000-8000-000000000002'::uuid, 1, '收款', a.id, a.account_code, a.account_name, 5000, 0
  FROM biz_account a WHERE a.account_code = '1002' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f30001-0000-4000-8000-000000000002'::uuid, 2, '收入', a.id, a.account_code, a.account_name, 0, 5000
  FROM biz_account a WHERE a.account_code = '5001' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f30001-0000-4000-8000-000000000003'::uuid, 1, '成本', a.id, a.account_code, a.account_name, 2000, 0
  FROM biz_account a WHERE a.account_code = '5401' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f30001-0000-4000-8000-000000000003'::uuid, 2, '付款', a.id, a.account_code, a.account_name, 0, 2000
  FROM biz_account a WHERE a.account_code = '1002' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f30001-0000-4000-8000-000000000004'::uuid, 1, '工资', a.id, a.account_code, a.account_name, 800, 0
  FROM biz_account a WHERE a.account_code = '2211' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f30001-0000-4000-8000-000000000004'::uuid, 2, '付款', a.id, a.account_code, a.account_name, 0, 800
  FROM biz_account a WHERE a.account_code = '1002' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f30001-0000-4000-8000-000000000005'::uuid, 1, '交税', a.id, a.account_code, a.account_name, 300, 0
  FROM biz_account a WHERE a.account_code = '2221' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f30001-0000-4000-8000-000000000005'::uuid, 2, '付款', a.id, a.account_code, a.account_name, 0, 300
  FROM biz_account a WHERE a.account_code = '1002' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f30001-0000-4000-8000-000000000006'::uuid, 1, '设备', a.id, a.account_code, a.account_name, 1500, 0
  FROM biz_account a WHERE a.account_code = '1601' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f30001-0000-4000-8000-000000000006'::uuid, 2, '付款', a.id, a.account_code, a.account_name, 0, 1500
  FROM biz_account a WHERE a.account_code = '1002' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f30001-0000-4000-8000-000000000007'::uuid, 1, '借款', a.id, a.account_code, a.account_name, 4000, 0
  FROM biz_account a WHERE a.account_code = '1002' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f30001-0000-4000-8000-000000000007'::uuid, 2, '长期借款', a.id, a.account_code, a.account_name, 0, 4000
  FROM biz_account a WHERE a.account_code = '2501' AND a.deleted_at IS NULL;
SQL
[ $? -eq 0 ] && ok "夹具写入（4 期间 / 7 凭证 / 14 分录）" || { bad "夹具写入失败"; exit 1; }

# ---------- 2. 登录（首登强制改密；复跑要用改过之后的口令） ----------
NEWPASS="Erpr3_verify_2026_new"
LOGIN=""
for PW in "$NIK_ADMIN_PASSWORD" "$NEWPASS"; do
    LOGIN=$(curl -s -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
        -d "{\"username\":\"admin\",\"password\":\"$PW\"}")
    if [ "$(jget "$LOGIN" "code")" = "0" ]; then
        NIK_ADMIN_PASSWORD=$PW
        break
    fi
done
TOKEN=$(jget "$LOGIN" "data.access_token")
case "$TOKEN" in
    "<null>"|"<not-json>"|"<missing>") say "登录响应：$LOGIN"; bad "admin 登录失败"; exit 1 ;;
esac
if [ "$(jget "$LOGIN" "data.must_change_password")" = "true" ]; then
    curl -s -X PUT "$BASE/system/auth/password" -H "Authorization: Bearer ${TOKEN}" \
        -H 'Content-Type: application/json' \
        -d "{\"old_password\":\"$NIK_ADMIN_PASSWORD\",\"new_password\":\"$NEWPASS\"}" >/dev/null
    LOGIN=$(curl -s -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
        -d "{\"username\":\"admin\",\"password\":\"$NEWPASS\"}")
    TOKEN=$(jget "$LOGIN" "data.access_token")
    NIK_ADMIN_PASSWORD=$NEWPASS
fi
[ "${#TOKEN}" -gt 20 ] && ok "admin 登录成功（令牌 ${#TOKEN} 字节）" || { bad "拿不到令牌"; exit 1; }
AUTH="Authorization: Bearer ${TOKEN}"

# ---------- 3. 配置落库 ----------
expect "分类行 12 条（V0185 种子）" "12" \
    "$(psql_at "select count(*) from biz_cashflow_item where is_active=1")"
expect "权限节点 business:erp:report:cash-flow" "1" \
    "$(psql_at "select count(*) from sys_menu where auth_code='business:erp:report:cash-flow' and menu_type=2 and is_visible and component='business/erp/report/cash-flow/index'")"
expect "购买商品付款 的对方科目清单" "2202,1123,1401,1402,1403,1404,1405,1408,4001,4101,5401,5402,6401" \
    "$(psql_at "select account_pattern from biz_cashflow_item where item_code='OPER_PURCHASE_PAYMENTS'")"

# ---------- 4. 现金流量表主体 ----------
CF=$(curl -s -H "$AUTH" "$BASE/business/erp/report/cash-flow?period=2026-10")
expect "2026-10 报表 code" "0" "$(jget "$CF" "code")"
expect "period" "2026-10" "$(jget "$CF" "data.period")"
expect "period_name" "2026年10月" "$(jget "$CF" "data.period_name")"
expect "period_status" "OPEN" "$(jget "$CF" "data.period_status")"
expect "本年累计起算日" "2026-01-01" "$(jget "$CF" "data.ytd_start")"
expect "本期列名" "本期金额" "$(jget "$CF" "data.current_label")"
expect "累计列名" "本年累计" "$(jget "$CF" "data.ytd_label")"
expect "简化口径标记" "true" "$(jget "$CF" "data.simplified")"
case "$(jget "$CF" "data.simplified_note")" in
    *简化版*) ok "简化口径说明随报表返回（口径跟着报表走）" ;;
    *) bad "简化口径说明缺失：$(jget "$CF" "data.simplified_note")" ;;
esac

# 本期（10 月）
expect_num "本期 经营流入小计 = 5,000" "5000" "$(jget "$CF" "data.operating.inflow_total.current")"
expect_num "本期 经营流出小计 = 3,100（2,000 + 800 + 300）" "3100" "$(jget "$CF" "data.operating.outflow_total.current")"
expect_num "本期 经营净额 = 1,900" "1900" "$(jget "$CF" "data.operating.net.current")"
expect_num "本期 投资净额 = −1,500" "-1500" "$(jget "$CF" "data.investing.net.current")"
expect_num "本期 筹资净额 = 4,000" "4000" "$(jget "$CF" "data.financing.net.current")"
expect_num "本期 净增加额 = 4,400" "4400" "$(jget "$CF" "data.net_increase.current")"
expect_num "本期 期初余额 = 10,000（9 月股东投入）" "10000" "$(jget "$CF" "data.opening_balance.current")"
expect_num "本期 期末余额 = 14,400 = 期初 + 净增加" "14400" "$(jget "$CF" "data.closing_balance.current")"

# 逐行（对方科目 → 明细行）
expect_num "本期 销售商品收到的现金" "5000" "$(jline "$CF" OPER_SALES_RECEIPTS current)"
expect_num "本期 购买商品支付的现金" "2000" "$(jline "$CF" OPER_PURCHASE_PAYMENTS current)"
expect_num "本期 支付的职工薪酬" "800" "$(jline "$CF" OPER_STAFF_PAYMENTS current)"
expect_num "本期 支付的各项税费" "300" "$(jline "$CF" OPER_TAX_PAYMENTS current)"
expect_num "本期 购建长期资产支付的现金" "1500" "$(jline "$CF" INVEST_PURCHASES current)"
expect_num "本期 取得借款收到的现金" "4000" "$(jline "$CF" FIN_LOANS_IN current)"
expect_num "本期 吸收投资未发生（9 月才发生）" "0" "$(jline "$CF" FIN_CAPITAL_IN current)"

# 本年累计（含 9 月）
expect_num "累计 经营净额 = 1,900" "1900" "$(jget "$CF" "data.operating.net.ytd")"
expect_num "累计 筹资净额 = 14,000（10,000 + 4,000）" "14000" "$(jget "$CF" "data.financing.net.ytd")"
expect_num "累计 吸收投资收到的现金 = 10,000" "10000" "$(jline "$CF" FIN_CAPITAL_IN ytd)"
expect_num "累计 净增加额 = 14,400" "14400" "$(jget "$CF" "data.net_increase.ytd")"
expect_num "累计 期初余额 = 0（本年 1 月 1 日）" "0" "$(jget "$CF" "data.opening_balance.ytd")"
expect_num "累计 期末余额 = 14,400" "14400" "$(jget "$CF" "data.closing_balance.ytd")"

# 勾稽
expect "勾稽 balanced" "true" "$(jget "$CF" "data.check.balanced")"
expect_num "勾稽 期末现金余额" "14400" "$(jget "$CF" "data.check.closing_balance")"
expect_num "勾稽 资产负债表货币资金" "14400" "$(jget "$CF" "data.check.balance_sheet_cash")"
expect_num "勾稽 diff" "0" "$(jget "$CF" "data.check.diff")"
expect "勾稽 diff_text" "0.00" "$(jget "$CF" "data.check.diff_text")"
expect_num "勾稽 总账现金类科目余额 = 14,400" "14400" "$(jget "$CF" "data.check.closing_by_ledger")"
expect "本期现金分录 7 笔" "7" "$(jget "$CF" "data.check.cash_entry_count")"
expect "未分类 0 笔" "0" "$(jget "$CF" "data.check.unclassified_count")"
case "$(jget "$CF" "data.check.message")" in
    勾稽一致*) ok "勾稽结论是「勾稽一致」：$(jget "$CF" "data.check.message")" ;;
    *) bad "勾稽结论应为「勾稽一致…」，实际：$(jget "$CF" "data.check.message")" ;;
esac
expect "平的时候不吐提示" "[]" "$(jget "$CF" "data.check.hints")"
expect "无告警" "[]" "$(jget "$CF" "data.warnings")"

# ---------- 5. 两表互证：同一份数据打资产负债表 ----------
BS=$(curl -s -H "$AUTH" "$BASE/business/erp/report/balance-sheet?period=2026-10")
expect "资产负债表 code" "0" "$(jget "$BS" "code")"
expect_num "资产负债表「货币资金」= 14,400" "14400" "$(bs_amount "$BS" assets MONETARY_FUNDS)"
expect_num "两表互证：期末现金 = 货币资金" "14400" "$(bs_amount "$BS" assets MONETARY_FUNDS)"

# ---------- 6. 空期间 / 无发生额期间 ----------
EMPTY=$(curl -s -H "$AUTH" "$BASE/business/erp/report/cash-flow?period=2026-07")
expect "空期间 2026-07 code" "0" "$(jget "$EMPTY" "code")"
expect_num "空期间 净增加额 = 0" "0" "$(jget "$EMPTY" "data.net_increase.current")"
expect_num "空期间 期初 = 期末 = 0" "0" "$(jget "$EMPTY" "data.closing_balance.current")"
expect "空期间 仍勾稽一致" "true" "$(jget "$EMPTY" "data.check.balanced")"
expect "空期间 无告警" "[]" "$(jget "$EMPTY" "data.warnings")"

NOV=$(curl -s -H "$AUTH" "$BASE/business/erp/report/cash-flow?period=2026-11")
expect_num "无发生额月份 2026-11 本期净增加 = 0" "0" "$(jget "$NOV" "data.net_increase.current")"
expect_num "无发生额月份 累计净增加 = 14,400" "14400" "$(jget "$NOV" "data.net_increase.ytd")"
expect_num "无发生额月份 期末余额 = 14,400" "14400" "$(jget "$NOV" "data.closing_balance.current")"
expect "无发生额月份 仍勾稽一致" "true" "$(jget "$NOV" "data.check.balanced")"

# ---------- 7. 反例 ----------
C1=$(curl -s -o /dev/null -w "%{http_code}" -H "$AUTH" "$BASE/business/erp/report/cash-flow?period=2026-13")
expect "非法期间（2026-13）→ 400" "400" "$C1"
C2=$(curl -s -H "$AUTH" "$BASE/business/erp/report/cash-flow?period=2026-13")
expect "非法期间 业务码 = 10001" "10001" "$(jget "$C2" "code")"
C3=$(curl -s -o /dev/null -w "%{http_code}" -H "$AUTH" "$BASE/business/erp/report/cash-flow?period=2026-12")
expect "空月份 + 没开账（2026-12）→ 400" "400" "$C3"
C4=$(curl -s -H "$AUTH" "$BASE/business/erp/report/cash-flow?period=2026-12")
expect "空月份 + 没开账 业务码 = 66010" "66010" "$(jget "$C4" "code")"
case "$(jget "$C4" "msg")" in
    *不存在*) ok "空月份 + 没开账 消息说清原因" ;;
    *) bad "空月份 + 没开账 消息应含「不存在」：$(jget "$C4" "msg")" ;;
esac
C5=$(curl -s -o /dev/null -w "%{http_code}" "$BASE/business/erp/report/cash-flow?period=2026-10")
case "$C5" in
    401|403) ok "无令牌 → $C5" ;;
    *) bad "无令牌应为 401/403，实际 $C5" ;;
esac

# ---------- 8. 未分类必须进 warnings（硬红线 4）+ 勾稽照实报（硬红线 3） ----------
# 借 1012 其他货币资金 / 贷 1002：1012 不在现金类口径（1001/1002）里，
# 也没有任何分类行 → 必须归「其他」+ warnings 非空 + 勾稽差 200（货币资金不变、现金少了 200）
PSQL <<'SQL'
INSERT INTO biz_voucher (id, voucher_word, voucher_no, period, voucher_date, voucher_type,
                         source_type, source_no, summary, total_debit, total_credit,
                         entry_count, status, is_red)
VALUES ('e4f30001-0000-4000-8000-000000000008', 'ERPR3V', '0008', '2026-10', '2026-10-20', 'AUTO',
        'MANUAL', 'ERPR3V-8', '转出到其他货币资金（口径外的科目）', 200, 200, 2, 'POSTED', 0);
INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code,
                               account_name, debit_amount, credit_amount)
SELECT 'e4f30001-0000-4000-8000-000000000008'::uuid, 1, '其他货币资金', a.id, a.account_code, a.account_name, 200, 0
  FROM biz_account a WHERE a.account_code = '1012' AND a.deleted_at IS NULL;
INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code,
                               account_name, debit_amount, credit_amount)
SELECT 'e4f30001-0000-4000-8000-000000000008'::uuid, 2, '银行付款', a.id, a.account_code, a.account_name, 0, 200
  FROM biz_account a WHERE a.account_code = '1002' AND a.deleted_at IS NULL;
SQL
UNM=$(curl -s -H "$AUTH" "$BASE/business/erp/report/cash-flow?period=2026-10")
expect "未分类时 仍返回 200（报表要能出，但要报警）" "0" "$(jget "$UNM" "code")"
expect "未分类时 warnings 非空" "2" "$(jwarn_len "$UNM")"
if jget "$UNM" "data.warnings" | grep -q "未能分类"; then ok "warnings 点名「未能分类」并给出笔数/科目"; else bad "warnings 没提未分类"; fi
if jget "$UNM" "data.warnings" | grep -q "1012"; then ok "warnings 点名 1012（数量 + 科目编码）"; else bad "warnings 没点名 1012"; fi
expect "未分类 1 笔" "1" "$(jget "$UNM" "data.check.unclassified_count")"
expect_num "未分类流出金额 = 200" "200" "$(jget "$UNM" "data.check.unclassified_outflow")"
expect_num "「其他」分区流出 = 200" "200" "$(jget "$UNM" "data.other.outflow_total.current")"
expect_num "净增加额仍含这笔（报表自身加减不断）= 4,200" "4200" "$(jget "$UNM" "data.net_increase.current")"
expect "未分类时 balanced = false（口径差异必须红）" "false" "$(jget "$UNM" "data.check.balanced")"
# 本表现金类只认 1001/1002：钱转去 1012 就是现金流出了 200 → 期末 14,200；
# 而资产负债表「货币资金」含 1012，仍是 14,400 ⇒ 差 −200（口径差异照实报）
expect_num "diff = −200（期末 14,200 − 货币资金 14,400）" "-200" "$(jget "$UNM" "data.check.diff")"
expect "diff_text = -200.00" "-200.00" "$(jget "$UNM" "data.check.diff_text")"
expect_num "期末余额 = 14,200（= 期初 10,000 + 净增加 4,200）" "14200" "$(jget "$UNM" "data.closing_balance.current")"
case "$(jget "$UNM" "data.check.message")" in
    不平*差*) ok "结论是「不平 + 差额」：$(jget "$UNM" "data.check.message")" ;;
    *) bad "结论应为「不平…差…」，实际：$(jget "$UNM" "data.check.message")" ;;
esac
if jget "$UNM" "data.check.hints" | grep -q "1012"; then ok "hints 指出 1012 口径差异"; else bad "hints 没指出 1012"; fi

# 删掉那张凭证 → 回绿
PSQL <<'SQL'
DELETE FROM biz_voucher WHERE id = 'e4f30001-0000-4000-8000-000000000008'::uuid;
SQL
BACK=$(curl -s -H "$AUTH" "$BASE/business/erp/report/cash-flow?period=2026-10")
expect "删掉后 balanced = true" "true" "$(jget "$BACK" "data.check.balanced")"
expect_num "删掉后 期末余额回 14,400" "14400" "$(jget "$BACK" "data.closing_balance.current")"
expect "删掉后 无告警" "[]" "$(jget "$BACK" "data.warnings")"

# ---------- 9. 导出 xlsx ----------
curl -s -H "$AUTH" "$BASE/business/erp/report/cash-flow/export?period=2026-10" \
    -o /tmp/erpr3_cf.xlsx -D /tmp/erpr3_cf.hdr
CT=$(grep -i '^content-type' /tmp/erpr3_cf.hdr | tr -d '\r')
case "$CT" in
    *spreadsheetml*) ok "导出 Content-Type = xlsx（${CT#*: }）" ;;
    *) bad "导出 Content-Type 不对：$CT" ;;
esac
if [ "$(head -c 2 /tmp/erpr3_cf.xlsx)" = "PK" ]; then
    ok "导出文件是合法 xlsx（PK 魔数，$(stat -c %s /tmp/erpr3_cf.xlsx) 字节）"
else
    bad "导出文件不是 xlsx"
fi
if grep -qi 'attachment' /tmp/erpr3_cf.hdr; then ok "导出带 Content-Disposition: attachment"; else bad "导出缺 attachment 头"; fi
if grep -qi 'filename\*' /tmp/erpr3_cf.hdr; then ok "导出文件名是中文（RFC 5987）"; else bad "导出文件名不是中文"; fi

# ---------- 10. 收尾 ----------
say ""
say "=========================================="
say "  PASS=$PASS  FAIL=$FAIL"
say "  隔离库 $DB 保留（复核：psql -d $DB）；删除：su - postgres -c \"dropdb $DB\""
say "=========================================="
[ "$FAIL" -eq 0 ] || exit 1
