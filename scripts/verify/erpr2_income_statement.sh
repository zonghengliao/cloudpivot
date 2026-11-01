#!/usr/bin/env bash
# ============================================================
# ERP-R2 验收：利润表（真 HTTP + 真库 + 真两表勾稽）
#
# 用法：
#   bash scripts/verify/erpr2_income_statement.sh
#
# 可覆盖的环境变量：
#   REPO=/opt/cloudpivot              后台仓根
#   R2_DB=cloudpivot_erpr2v           隔离库名（不存在则自建；**不 DROP**，方便复跑/评审）
#   R2_PORT=18097                     监听端口（被占用时自动 +1 找空）
#   NIK_DATABASE_USER/PASSWORD        连库角色（默认 alun/alun，需能建库）
#
# 做的事：
#   ① 自建隔离库 → 起一次实例把迁移跑到最新（含 V0184：利润表 12 个报表项 + 权限节点）
#   ② 造夹具：7 张 POSTED 凭证（资本投入 / 9 月收入 / 10 月收入 / 红冲收入 / 成本 / 管理费用 / 所得税）
#   ③ 打利润表接口：本期 + 本年累计逐行对金额，勾稽对「资产负债表未分配利润本年变动」
#   ④ 同一份数据打资产负债表接口：未分配利润 = 4400、资产总计 = 14700（两表互相印证）
#   ⑤ 空期间（2026-07）全 0；无发生额月份（2026-11）本期 0 / 累计不变
#   ⑥ 反例：非法期间（400 + 10001）、空月份没开账（400 + 66010）、无令牌（401/403）
#   ⑦ 未归类损益（借 1002 / 贷 5111 投资收益）→ 必须报不平、点名科目、差额精确；删掉后回绿
#   ⑧ 导出 xlsx（200 + PK 魔数 + 附件头）
#   ⑨ psql 侧核对：报表项 12 个 / 权限节点 / 标准损益科目映射
#
# 期望值（夹具决定，写死在这里当契约）：
#   9 月：收入 3,000；10 月：收入 5,000 − 红冲 500 = 4,500；成本 2,000；管理费用 800；所得税 300
#   ⇒ 本期：营业收入 4,500 / 营业利润 1,700 / 净利润 1,400
#   ⇒ 本年累计：营业收入 7,500 / 营业利润 4,700 / 净利润 4,400
#   ⇒ 勾稽：净利润（累计）4,400 = 资产负债表「未分配利润」本年变动 4,400 ⇒ diff 0
# ============================================================
set -uo pipefail

REPO=${REPO:-/opt/cloudpivot}
DB=${R2_DB:-cloudpivot_erpr2v}
BIN="$REPO/backend/target/debug/cloudpivot"
LOG=/tmp/erpr2_verify_server.log
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
# JSON 点路径取值：data.check.net_profit_ytd / data.items.0.current …
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
# 利润表某一行（items 是平铺列表，按 item_code 取）
jamount() {
    python3 - "$1" "$2" "$3" <<'PY'
import json, sys
raw, code, field = sys.argv[1], sys.argv[2], sys.argv[3]
data = json.loads(raw)["data"]
for row in data["items"]:
    if row["item_code"] == code:
        value = row.get(field)
        print("null" if value is None else value); raise SystemExit
print("<missing>")
PY
}
jflag() {
    python3 - "$1" "$2" "$3" <<'PY'
import json, sys
raw, code, field = sys.argv[1], sys.argv[2], sys.argv[3]
data = json.loads(raw)["data"]
for row in data["items"]:
    if row["item_code"] == code:
        print(json.dumps(row.get(field), ensure_ascii=False)); raise SystemExit
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

# ---------- 0. 前置 ----------
[ -x "$BIN" ] || { say "缺二进制 $BIN（先 cd backend && cargo build）"; exit 1; }
say "== 二进制 $BIN（$(stat -c %y "$BIN" 2>/dev/null)）"

su - postgres -c "psql -Atc \"select 1 from pg_database where datname='$DB'\"" | grep -q 1 \
    || su - postgres -c "createdb -O $DBUSER $DB"
say "== 隔离库 $DB 就绪（owner=$DBUSER）"

PORT=${R2_PORT:-18097}
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
export NIK_ADMIN_PASSWORD="Erpr2_verify_2026"
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="erpr2_verify_secret_0123456789abcdef"
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
DELETE FROM biz_voucher WHERE voucher_word = 'ERPR2V';

-- 期间：2026-07（早于任何凭证，用于「空期间」）、2026-09（已结账，累计来源）、
--       2026-10（编制期间）、2026-11（无发生额，验证「本期 0 / 累计不变」）
INSERT INTO biz_accounting_period (period_year, period_month, period_code, start_date, end_date, status)
VALUES (2026, 7, '2026-07', '2026-07-01', '2026-07-31', 'OPEN'),
       (2026, 9, '2026-09', '2026-09-01', '2026-09-30', 'CLOSED'),
       (2026, 10, '2026-10', '2026-10-01', '2026-10-31', 'OPEN'),
       (2026, 11, '2026-11', '2026-11-01', '2026-11-30', 'OPEN')
ON CONFLICT (period_code) DO NOTHING;

-- 7 张 POSTED 凭证（借 = 贷）
-- 0004 是**真红字凭证**（`is_red = 1` + `reverse_of`，DB 的 `ck_biz_voucher_red_ref` 这么要求）：
-- 销售退回 500，红冲 0003 的一部分，分录是借贷互换的「借 5001 / 贷 1002」。
INSERT INTO biz_voucher (id, voucher_word, voucher_no, period, voucher_date, voucher_type,
                         source_type, source_no, summary, total_debit, total_credit,
                         entry_count, status, is_red, reverse_of)
VALUES
 ('e4f20001-0000-4000-8000-000000000001', 'ERPR2V', '0001', '2026-09', '2026-09-10', 'AUTO',
  'MANUAL', 'ERPR2V-1', '股东投入', 10000, 10000, 2, 'POSTED', 0, NULL),
 ('e4f20001-0000-4000-8000-000000000002', 'ERPR2V', '0002', '2026-09', '2026-09-20', 'AUTO',
  'MANUAL', 'ERPR2V-2', '9 月销售收入', 3000, 3000, 2, 'POSTED', 0, NULL),
 ('e4f20001-0000-4000-8000-000000000003', 'ERPR2V', '0003', '2026-10', '2026-10-05', 'AUTO',
  'MANUAL', 'ERPR2V-3', '10 月销售收入', 5000, 5000, 2, 'POSTED', 0, NULL),
 ('e4f20001-0000-4000-8000-000000000004', 'ERPR2V', '0004', '2026-10', '2026-10-06', 'AUTO',
  'MANUAL', 'ERPR2V-4', '销售退回（红字冲销 0003 的 500）', 500, 500, 2, 'POSTED', 1,
  'e4f20001-0000-4000-8000-000000000003'::uuid),
 ('e4f20001-0000-4000-8000-000000000005', 'ERPR2V', '0005', '2026-10', '2026-10-08', 'AUTO',
  'MANUAL', 'ERPR2V-5', '结转销售成本', 2000, 2000, 2, 'POSTED', 0, NULL),
 ('e4f20001-0000-4000-8000-000000000006', 'ERPR2V', '0006', '2026-10', '2026-10-10', 'AUTO',
  'MANUAL', 'ERPR2V-6', '管理费用', 800, 800, 2, 'POSTED', 0, NULL),
 ('e4f20001-0000-4000-8000-000000000007', 'ERPR2V', '0007', '2026-10', '2026-10-12', 'AUTO',
  'MANUAL', 'ERPR2V-7', '计提所得税', 300, 300, 2, 'POSTED', 0, NULL);

INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code,
                               account_name, debit_amount, credit_amount)
SELECT 'e4f20001-0000-4000-8000-000000000001'::uuid, 1, '投入', a.id, a.account_code, a.account_name, 10000, 0
  FROM biz_account a WHERE a.account_code = '1002' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f20001-0000-4000-8000-000000000001'::uuid, 2, '投入', a.id, a.account_code, a.account_name, 0, 10000
  FROM biz_account a WHERE a.account_code = '3001' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f20001-0000-4000-8000-000000000002'::uuid, 1, '收款', a.id, a.account_code, a.account_name, 3000, 0
  FROM biz_account a WHERE a.account_code = '1002' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f20001-0000-4000-8000-000000000002'::uuid, 2, '收入', a.id, a.account_code, a.account_name, 0, 3000
  FROM biz_account a WHERE a.account_code = '5001' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f20001-0000-4000-8000-000000000003'::uuid, 1, '收款', a.id, a.account_code, a.account_name, 5000, 0
  FROM biz_account a WHERE a.account_code = '1002' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f20001-0000-4000-8000-000000000003'::uuid, 2, '收入', a.id, a.account_code, a.account_name, 0, 5000
  FROM biz_account a WHERE a.account_code = '5001' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f20001-0000-4000-8000-000000000004'::uuid, 1, '退回', a.id, a.account_code, a.account_name, 500, 0
  FROM biz_account a WHERE a.account_code = '5001' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f20001-0000-4000-8000-000000000004'::uuid, 2, '退款', a.id, a.account_code, a.account_name, 0, 500
  FROM biz_account a WHERE a.account_code = '1002' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f20001-0000-4000-8000-000000000005'::uuid, 1, '成本', a.id, a.account_code, a.account_name, 2000, 0
  FROM biz_account a WHERE a.account_code = '5401' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f20001-0000-4000-8000-000000000005'::uuid, 2, '出库', a.id, a.account_code, a.account_name, 0, 2000
  FROM biz_account a WHERE a.account_code = '1405' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f20001-0000-4000-8000-000000000006'::uuid, 1, '管理费', a.id, a.account_code, a.account_name, 800, 0
  FROM biz_account a WHERE a.account_code = '5602' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f20001-0000-4000-8000-000000000006'::uuid, 2, '付款', a.id, a.account_code, a.account_name, 0, 800
  FROM biz_account a WHERE a.account_code = '1002' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f20001-0000-4000-8000-000000000007'::uuid, 1, '所得税', a.id, a.account_code, a.account_name, 300, 0
  FROM biz_account a WHERE a.account_code = '5801' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f20001-0000-4000-8000-000000000007'::uuid, 2, '应交', a.id, a.account_code, a.account_name, 0, 300
  FROM biz_account a WHERE a.account_code = '2221.02' AND a.deleted_at IS NULL;
SQL
[ $? -eq 0 ] && ok "夹具写入（4 期间 / 7 凭证 / 14 分录）" || { bad "夹具写入失败"; exit 1; }

# ---------- 2. 登录（首登强制改密；复跑要用改过之后的口令） ----------
NEWPASS="Erpr2_verify_2026_new"
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
    # 首次登录必须先改初始口令，否则业务接口一律 403「首次登录必须先修改初始密码」
    curl -s -X PUT "$BASE/system/auth/password" -H "Authorization: Bearer $TOKEN" \
        -H 'Content-Type: application/json' \
        -d "{\"old_password\":\"$NIK_ADMIN_PASSWORD\",\"new_password\":\"$NEWPASS\"}" >/dev/null
    LOGIN=$(curl -s -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
        -d "{\"username\":\"admin\",\"password\":\"$NEWPASS\"}")
    TOKEN=$(jget "$LOGIN" "data.access_token")
    NIK_ADMIN_PASSWORD=$NEWPASS
fi
[ "${#TOKEN}" -gt 20 ] && ok "admin 登录成功（令牌 ${#TOKEN} 字节）" || { bad "拿不到令牌"; exit 1; }
AUTH="Authorization: Bearer $TOKEN"

# ---------- 3. 配置落库 ----------
expect "利润表报表项 12 个（V0184 种子）" "12" \
    "$(psql_at "select count(*) from biz_report_item where report_code='INCOME_STATEMENT' and is_active=1")"
expect "利润表科目取数项 9 个" "9" \
    "$(psql_at "select count(*) from biz_report_item where report_code='INCOME_STATEMENT' and account_pattern is not null")"
expect "权限节点 business:erp:report:income-statement" "1" \
    "$(psql_at "select count(*) from sys_menu where auth_code='business:erp:report:income-statement' and menu_type=2 and is_visible and component='business/erp/report/income-statement/index'")"
expect "净利润公式" "TOTAL_PROFIT - INCOME_TAX" \
    "$(psql_at "select formula from biz_report_item where report_code='INCOME_STATEMENT' and item_code='NET_PROFIT'")"

# ---------- 4. 利润表主体 ----------
IS=$(curl -s -H "$AUTH" "$BASE/business/erp/report/income-statement?period=2026-10")
expect "2026-10 报表 code" "0" "$(jget "$IS" "code")"
expect "period" "2026-10" "$(jget "$IS" "data.period")"
expect "period_name" "2026年10月" "$(jget "$IS" "data.period_name")"
expect "period_status" "OPEN" "$(jget "$IS" "data.period_status")"
expect "本年累计起算日" "2026-01-01" "$(jget "$IS" "data.ytd_start")"
expect "本期列名" "本期金额" "$(jget "$IS" "data.current_label")"
expect "累计列名" "本年累计" "$(jget "$IS" "data.ytd_label")"
expect "行数 = 12 + 未归类行" "13" \
    "$(python3 - "$IS" <<'PY'
import json, sys
print(len(json.loads(sys.argv[1])["data"]["items"]))
PY
)"

# 本期（10 月）
expect_num "本期 营业收入 = 4,500（5,000 − 红冲 500）" "4500" "$(jamount "$IS" REVENUE current)"
expect_num "本期 营业成本 = 2,000" "2000" "$(jamount "$IS" COST_OF_SALES current)"
expect_num "本期 管理费用 = 800" "800" "$(jamount "$IS" ADMIN_EXPENSE current)"
expect_num "本期 营业利润 = 1,700" "1700" "$(jamount "$IS" OPERATING_PROFIT current)"
expect_num "本期 利润总额 = 1,700" "1700" "$(jamount "$IS" TOTAL_PROFIT current)"
expect_num "本期 所得税 = 300" "300" "$(jamount "$IS" INCOME_TAX current)"
expect_num "本期 净利润 = 1,400" "1400" "$(jamount "$IS" NET_PROFIT current)"

# 本年累计（1 月 1 日 → 10 月 31 日，含 9 月收入 3,000）
expect_num "累计 营业收入 = 7,500" "7500" "$(jamount "$IS" REVENUE ytd)"
expect_num "累计 营业成本 = 2,000" "2000" "$(jamount "$IS" COST_OF_SALES ytd)"
expect_num "累计 营业利润 = 4,700" "4700" "$(jamount "$IS" OPERATING_PROFIT ytd)"
expect_num "累计 净利润 = 4,400" "4400" "$(jamount "$IS" NET_PROFIT ytd)"
expect "净利润是合计行（加粗）" "true" "$(jflag "$IS" NET_PROFIT is_total)"
expect "营业利润是合计行（加粗）" "true" "$(jflag "$IS" OPERATING_PROFIT is_total)"
expect "营业成本带「减：」前缀" '"减：营业成本"' "$(jflag "$IS" COST_OF_SALES item_name)"
expect "营业成本映射到的科目（可审计：5401 + 5402 + 6401）" '["5401", "5402", "6401"]' "$(jflag "$IS" COST_OF_SALES accounts)"

# 勾稽
expect "勾稽 balanced" "true" "$(jget "$IS" "data.check.balanced")"
expect_num "勾稽 净利润（本年累计）" "4400" "$(jget "$IS" "data.check.net_profit_ytd")"
expect_num "勾稽 未分配利润本年变动" "4400" "$(jget "$IS" "data.check.equity_change")"
expect_num "勾稽 diff" "0" "$(jget "$IS" "data.check.diff")"
expect "勾稽 diff_text" "0.00" "$(jget "$IS" "data.check.diff_text")"
case "$(jget "$IS" "data.check.message")" in
    勾稽一致*) ok "勾稽结论是「勾稽一致」：$(jget "$IS" "data.check.message")" ;;
    *) bad "勾稽结论应为「勾稽一致…」，实际：$(jget "$IS" "data.check.message")" ;;
esac
expect "无未归类损益" "[]" "$(jget "$IS" "data.check.unmapped")"
expect "平的时候不吐提示" "[]" "$(jget "$IS" "data.check.hints")"

# ---------- 5. 两表互证：同一份数据打资产负债表 ----------
BS=$(curl -s -H "$AUTH" "$BASE/business/erp/report/balance-sheet?period=2026-10")
expect "资产负债表 code" "0" "$(jget "$BS" "code")"
expect_num "资产负债表「未分配利润」= 4,400" "4400" "$(bs_amount "$BS" equity UNDISTRIBUTED_PROFIT)"
expect_num "资产负债表 资产总计 = 14,700" "14700" "$(bs_amount "$BS" assets TOTAL_ASSETS)"
expect_num "资产负债表 负债+权益 = 14,700" "14700" "$(bs_amount "$BS" equity TOTAL_LIABILITIES_AND_EQUITY)"
expect_num "两表互证：净利润（累计）= 资产负债表「未分配利润」" "4400" "$(bs_amount "$BS" equity UNDISTRIBUTED_PROFIT)"

# ---------- 6. 空期间 / 无发生额期间 ----------
EMPTY=$(curl -s -H "$AUTH" "$BASE/business/erp/report/income-statement?period=2026-07")
expect "空期间 2026-07 code" "0" "$(jget "$EMPTY" "code")"
expect_num "空期间 净利润 = 0" "0" "$(jget "$EMPTY" "data.check.net_profit_ytd")"
expect "空期间 仍勾稽一致" "true" "$(jget "$EMPTY" "data.check.balanced")"
expect_num "空期间 营业收入 = 0" "0" "$(jamount "$EMPTY" REVENUE ytd)"

NOV=$(curl -s -H "$AUTH" "$BASE/business/erp/report/income-statement?period=2026-11")
expect_num "无发生额月份 2026-11 本期净利润 = 0" "0" "$(jamount "$NOV" NET_PROFIT current)"
expect_num "无发生额月份 2026-11 累计净利润 = 4,400（累计不因换月归零）" "4400" "$(jamount "$NOV" NET_PROFIT ytd)"
expect "无发生额月份 仍勾稽一致" "true" "$(jget "$NOV" "data.check.balanced")"

# ---------- 7. 反例 ----------
C1=$(curl -s -o /dev/null -w "%{http_code}" -H "$AUTH" "$BASE/business/erp/report/income-statement?period=2026-13")
expect "非法期间（2026-13）→ 400" "400" "$C1"
C2=$(curl -s -H "$AUTH" "$BASE/business/erp/report/income-statement?period=2026-13")
expect "非法期间 业务码 = 10001" "10001" "$(jget "$C2" "code")"
C3=$(curl -s -o /dev/null -w "%{http_code}" -H "$AUTH" "$BASE/business/erp/report/income-statement?period=2026-12")
expect "空月份 + 没开账（2026-12）→ 400" "400" "$C3"
C4=$(curl -s -H "$AUTH" "$BASE/business/erp/report/income-statement?period=2026-12")
expect "空月份 + 没开账 业务码 = 66010" "66010" "$(jget "$C4" "code")"
case "$(jget "$C4" "msg")" in
    *不存在*) ok "空月份 + 没开账 消息说清原因" ;;
    *) bad "空月份 + 没开账 消息应含「不存在」：$(jget "$C4" "msg")" ;;
esac
C5=$(curl -s -o /dev/null -w "%{http_code}" "$BASE/business/erp/report/income-statement?period=2026-10")
case "$C5" in
    401|403) ok "无令牌 → $C5" ;;
    *) bad "无令牌应为 401/403，实际 $C5" ;;
esac

# ---------- 8. 未归类损益必须报不平（硬红线 3） ----------
# 投资收益 5111 在卡面结构里没有对应行 → 报表漏钱 → 必须红，且点名科目、差额精确
PSQL <<'SQL'
INSERT INTO biz_voucher (id, voucher_word, voucher_no, period, voucher_date, voucher_type,
                         source_type, source_no, summary, total_debit, total_credit,
                         entry_count, status, is_red)
VALUES ('e4f20001-0000-4000-8000-000000000008', 'ERPR2V', '0008', '2026-10', '2026-10-20', 'AUTO',
        'MANUAL', 'ERPR2V-8', '投资收益', 250, 250, 2, 'POSTED', 0);
INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code,
                               account_name, debit_amount, credit_amount)
SELECT 'e4f20001-0000-4000-8000-000000000008'::uuid, 1, '收款', a.id, a.account_code, a.account_name, 250, 0
  FROM biz_account a WHERE a.account_code = '1002' AND a.deleted_at IS NULL;
INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code,
                               account_name, debit_amount, credit_amount)
SELECT 'e4f20001-0000-4000-8000-000000000008'::uuid, 2, '投资收益', a.id, a.account_code, a.account_name, 0, 250
  FROM biz_account a WHERE a.account_code = '5111' AND a.deleted_at IS NULL;
SQL
UNM=$(curl -s -H "$AUTH" "$BASE/business/erp/report/income-statement?period=2026-10")
expect "未归类损益时 仍返回 200（报表要能出，但要报警）" "0" "$(jget "$UNM" "code")"
expect "未归类损益时 balanced = false" "false" "$(jget "$UNM" "data.check.balanced")"
expect_num "未归类损益 净利润仍是 4,400（未归类的钱不进净利润）" "4400" "$(jget "$UNM" "data.check.net_profit_ytd")"
expect_num "未归类损益 权益变动 = 4,650" "4650" "$(jget "$UNM" "data.check.equity_change")"
expect_num "未归类损益 diff = −250" "-250" "$(jget "$UNM" "data.check.diff")"
expect "未归类损益 diff_text = -250.00" "-250.00" "$(jget "$UNM" "data.check.diff_text")"
case "$(jget "$UNM" "data.check.message")" in
    不平*差*) ok "结论是「不平 + 差额」：$(jget "$UNM" "data.check.message")" ;;
    *) bad "结论应为「不平…差…」，实际：$(jget "$UNM" "data.check.message")" ;;
esac
if jget "$UNM" "data.check.hints" | grep -q "5111"; then ok "提示里点名 5111 投资收益"; else bad "提示里没点名 5111"; fi
expect_num "报表里出现 UNMAPPED_PROFIT_LOSS 行（未归类的钱看得见）" "250" "$(jamount "$UNM" UNMAPPED_PROFIT_LOSS ytd)"

# 删掉那张投资收益凭证 → 回绿
PSQL <<'SQL'
DELETE FROM biz_voucher WHERE id = 'e4f20001-0000-4000-8000-000000000008'::uuid;
SQL
BACK=$(curl -s -H "$AUTH" "$BASE/business/erp/report/income-statement?period=2026-10")
expect "删掉未归类凭证后 balanced = true" "true" "$(jget "$BACK" "data.check.balanced")"
expect_num "删掉后 未分配利润变动 = 4,400" "4400" "$(jget "$BACK" "data.check.equity_change")"

# ---------- 9. 导出 xlsx ----------
curl -s -H "$AUTH" "$BASE/business/erp/report/income-statement/export?period=2026-10" \
    -o /tmp/erpr2_is.xlsx -D /tmp/erpr2_is.hdr
CT=$(grep -i '^content-type' /tmp/erpr2_is.hdr | tr -d '\r')
case "$CT" in
    *spreadsheetml*) ok "导出 Content-Type = xlsx（${CT#*: }）" ;;
    *) bad "导出 Content-Type 不对：$CT" ;;
esac
if [ "$(head -c 2 /tmp/erpr2_is.xlsx)" = "PK" ]; then
    ok "导出文件是合法 xlsx（PK 魔数，$(stat -c %s /tmp/erpr2_is.xlsx) 字节）"
else
    bad "导出文件不是 xlsx"
fi
if grep -qi 'attachment' /tmp/erpr2_is.hdr; then ok "导出带 Content-Disposition: attachment"; else bad "导出缺 attachment 头"; fi
if grep -qi 'filename\*' /tmp/erpr2_is.hdr; then ok "导出文件名是中文（RFC 5987）"; else bad "导出文件名不是中文"; fi

# ---------- 10. 收尾 ----------
say ""
say "=========================================="
say "  PASS=$PASS  FAIL=$FAIL"
say "  隔离库 $DB 保留（复核：psql -d $DB）；删除：su - postgres -c \"dropdb $DB\""
say "=========================================="
[ "$FAIL" -eq 0 ] || exit 1
