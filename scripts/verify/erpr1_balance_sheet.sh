#!/usr/bin/env bash
# ============================================================
# ERP-R1 验收：资产负债表（真 HTTP + 真库 + 真勾稽）
#
# 用法：
#   bash scripts/verify/erpr1_balance_sheet.sh
#
# 可覆盖的环境变量：
#   REPO=/opt/cloudpivot          后台仓根
#   R1_DB=cloudpivot_erpr1v       隔离库名（不存在则自建；**不 DROP**，方便复跑/评审）
#   R1_PORT=18096                 监听端口（被占用时自动 +1 找空）
#   NIK_DATABASE_USER/PASSWORD    连库角色（默认 alun/alun，需能建库）
#
# 做的事：
#   ① 自建隔离库 → 起一次实例把迁移跑到最新（含 V0183：科目补齐 + 报表项 + 权限码）
#   ② 造夹具：5 张 POSTED 凭证（期初投入 / 固定资产赊购 / 存货赊购 / 费用付现 / 计提折旧）
#   ③ 登录 admin → 打报表接口，逐项对金额 + 对勾稽（资产 ≡ 负债+权益）
#   ④ 反例：非法期间格式（400）、期间没开账（400 + 66010）、无令牌（401/403）
#   ⑤ 空期间（2026-07，早于任何凭证）→ 全 0 且平衡；2026-11（无发生额）→ 期初结转仍平衡
#   ⑥ 人为删一条分录 → 必须报不平、差额精确、并点名那张凭证
#   ⑦ 导出 xlsx（200 + PK 魔数 + 列头）
#   ⑧ psql 侧核对：科目数 / 报表项数 / 权限码 / 权限落点
#
# 期望值（夹具决定，写死在这里当契约）：
#   1002 银行存款 9,500   1405 库存商品 2,000   1601 固定资产 6,000   1602 累计折旧 1,000（减项）
#   2202 应付账款 8,000   3001 实收资本 10,000  5602 管理费用 1,500
#   ⇒ 资产 = 9,500 + 2,000 + (6,000 − 1,000) = 16,500
#   ⇒ 负债+权益 = 8,000 + (10,000 − 1,500) = 16,500
# ============================================================
set -uo pipefail

REPO=${REPO:-/opt/cloudpivot}
DB=${R1_DB:-cloudpivot_erpr1v}
BIN="$REPO/backend/target/debug/cloudpivot"
LOG=/tmp/erpr1_verify_server.log
DBUSER=${NIK_DATABASE_USER:-alun}
DBPASS=${NIK_DATABASE_PASSWORD:-alun}
PASS=0
FAIL=0

say() { printf '%s\n' "$*"; }
ok() { PASS=$((PASS + 1)); say "[PASS] $*"; }
bad() { FAIL=$((FAIL + 1)); say "[FAIL] $*"; }
expect() { if [ "$2" = "$3" ]; then ok "$1（$3）"; else bad "$1（期望 $2，实际 $3）"; fi; }
# 数值比较：JSON 里的 NUMERIC 回来是字符串（"16500.0000"），统一按数值比
expect_num() {
    if awk -v a="$2" -v b="$3" 'BEGIN{exit !(a+0==b+0)}'; then
        ok "$1（$3）"
    else
        bad "$1（期望 $2，实际 $3）"
    fi
}
# JSON 点路径取值：data.check.assets_total / data.assets.0.amount …
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
# 布尔按 JSON 字面量（true/false）打印，和接口返回体一致，便于 shell 里直接比字符串
if isinstance(node, bool):
    print("true" if node else "false")
elif isinstance(node, (dict, list)):
    print(json.dumps(node, ensure_ascii=False))
else:
    print(node)
PY
}
# 在报表某一侧里找 item_code 的金额
jamount() {
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
jflag() {
    python3 - "$1" "$2" "$3" "$4" <<'PY'
import json, sys
raw, side, code, field = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
data = json.loads(raw)["data"]
for row in data[side]:
    if row["item_code"] == code:
        print(json.dumps(row[field])); raise SystemExit
print("<missing>")
PY
}

# ---------- 0. 前置 ----------
[ -x "$BIN" ] || { say "缺二进制 $BIN（先 cd backend && cargo build）"; exit 1; }
say "== 二进制 $BIN（$(stat -c %y "$BIN" 2>/dev/null)）"

su - postgres -c "psql -Atc \"select 1 from pg_database where datname='$DB'\"" | grep -q 1 \
    || su - postgres -c "createdb -O $DBUSER $DB"
say "== 隔离库 $DB 就绪（owner=$DBUSER）"

PORT=${R1_PORT:-18096}
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
export NIK_ADMIN_PASSWORD="Erpr1_verify_2026"
export NIK_MIDDLEWARE_AUTH_JWT_SECRET="erpr1_verify_secret_0123456789abcdef"
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
DELETE FROM biz_voucher WHERE voucher_word = 'ERPR1V';

-- 期间：2026-07（早于任何凭证，用于"空期间"）、2026-09（已结账，期初来源）、
--       2026-10（编制期间）、2026-11（无发生额，验证期初结转）
INSERT INTO biz_accounting_period (period_year, period_month, period_code, start_date, end_date, status)
VALUES (2026, 7, '2026-07', '2026-07-01', '2026-07-31', 'OPEN'),
       (2026, 9, '2026-09', '2026-09-01', '2026-09-30', 'CLOSED'),
       (2026, 10, '2026-10', '2026-10-01', '2026-10-31', 'OPEN'),
       (2026, 11, '2026-11', '2026-11-01', '2026-11-30', 'OPEN')
ON CONFLICT (period_code) DO NOTHING;

-- 5 张 POSTED 凭证（借 = 贷）
WITH v AS (
  INSERT INTO biz_voucher (id, voucher_word, voucher_no, period, voucher_date, voucher_type,
                           source_type, source_no, summary, total_debit, total_credit,
                           entry_count, status, is_red)
  VALUES
   ('e4f10001-0000-4000-8000-000000000001', 'ERPR1V', '0001', '2026-09', '2026-09-10', 'AUTO',
    'MANUAL', 'ERPR1V-1', '股东投入', 10000, 10000, 2, 'POSTED', 0),
   ('e4f10001-0000-4000-8000-000000000002', 'ERPR1V', '0002', '2026-10', '2026-10-08', 'AUTO',
    'MANUAL', 'ERPR1V-2', '赊购固定资产', 6000, 6000, 2, 'POSTED', 0),
   ('e4f10001-0000-4000-8000-000000000003', 'ERPR1V', '0003', '2026-10', '2026-10-12', 'AUTO',
    'MANUAL', 'ERPR1V-3', '赊购存货', 2000, 2000, 2, 'POSTED', 0),
   ('e4f10001-0000-4000-8000-000000000004', 'ERPR1V', '0004', '2026-10', '2026-10-25', 'AUTO',
    'MANUAL', 'ERPR1V-4', '付现费用', 500, 500, 2, 'POSTED', 0),
   ('e4f10001-0000-4000-8000-000000000005', 'ERPR1V', '0005', '2026-10', '2026-10-28', 'AUTO',
    'MANUAL', 'ERPR1V-5', '计提折旧', 1000, 1000, 2, 'POSTED', 0)
  RETURNING id
)
SELECT count(*) FROM v;

INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code,
                               account_name, debit_amount, credit_amount)
SELECT 'e4f10001-0000-4000-8000-000000000001'::uuid, 1, '投入', a.id, a.account_code, a.account_name, 10000, 0
  FROM biz_account a WHERE a.account_code = '1002' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f10001-0000-4000-8000-000000000001'::uuid, 2, '投入', a.id, a.account_code, a.account_name, 0, 10000
  FROM biz_account a WHERE a.account_code = '3001' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f10001-0000-4000-8000-000000000002'::uuid, 1, '固定资产', a.id, a.account_code, a.account_name, 6000, 0
  FROM biz_account a WHERE a.account_code = '1601' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f10001-0000-4000-8000-000000000002'::uuid, 2, '应付', a.id, a.account_code, a.account_name, 0, 6000
  FROM biz_account a WHERE a.account_code = '2202' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f10001-0000-4000-8000-000000000003'::uuid, 1, '存货', a.id, a.account_code, a.account_name, 2000, 0
  FROM biz_account a WHERE a.account_code = '1405' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f10001-0000-4000-8000-000000000003'::uuid, 2, '应付', a.id, a.account_code, a.account_name, 0, 2000
  FROM biz_account a WHERE a.account_code = '2202' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f10001-0000-4000-8000-000000000004'::uuid, 1, '管理费用', a.id, a.account_code, a.account_name, 500, 0
  FROM biz_account a WHERE a.account_code = '5602' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f10001-0000-4000-8000-000000000004'::uuid, 2, '银行付款', a.id, a.account_code, a.account_name, 0, 500
  FROM biz_account a WHERE a.account_code = '1002' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f10001-0000-4000-8000-000000000005'::uuid, 1, '折旧费用', a.id, a.account_code, a.account_name, 1000, 0
  FROM biz_account a WHERE a.account_code = '5602' AND a.deleted_at IS NULL
UNION ALL
SELECT 'e4f10001-0000-4000-8000-000000000005'::uuid, 2, '累计折旧', a.id, a.account_code, a.account_name, 0, 1000
  FROM biz_account a WHERE a.account_code = '1602' AND a.deleted_at IS NULL;
SQL
[ $? -eq 0 ] && ok "夹具写入（4 期间 / 5 凭证 / 10 分录）" || { bad "夹具写入失败"; exit 1; }

# ---------- 2. 登录（首登强制改密；复跑要用改过之后的口令） ----------
NEWPASS="Erpr1_verify_2026_new"
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
if [ "$(jget "$LOGIN" "data.must_change_password")" = "True" ]; then
    # 首次登录必须先改初始口令，否则所有业务接口一律 403「首次登录必须先修改初始密码」
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

# ---------- 3. 权限码落库 ----------
expect "报表项 41 个（V0183 种子）" "41" "$(psql_at "select count(*) from biz_report_item where report_code='BALANCE_SHEET' and is_active=1")"
ACCT_N=$(psql_at "select count(*) from biz_account where deleted_at is null")
if [ "${ACCT_N:-0}" -ge 56 ]; then ok "科目 ≥ 56 个（实际 $ACCT_N）"; else bad "科目应 ≥ 56，实际 $ACCT_N"; fi
expect "权限节点 2 个（:balance-sheet / :export）" "2" \
    "$(psql_at "select count(*) from sys_menu where auth_code in ('business:erp:report:balance-sheet','business:erp:report:export')")"
expect "资产负债表菜单是可见菜单（menu_type=2 + 组件已配）" "1" \
    "$(psql_at "select count(*) from sys_menu where auth_code='business:erp:report:balance-sheet' and menu_type=2 and is_visible and component='business/erp/report/balance-sheet/index'")"
# 权限落点：
#   · 已部署库（生产）：迁移跑时角色都在 → 真授权（实测生产副本 SUPER_ADMIN 拿到 2 条）；
#   · 空库首启：迁移先于 bootstrap（此刻还没有角色）→ 授权是空操作，
#     与 V0141–V0150 完全同口径，SUPER_ADMIN 走通配短路照样能用，角色建好后重放 V0183 即补齐。
REPORT_GRANTS=$(psql_at "select count(*) from sys_role_menu rm join sys_menu m on m.id=rm.menu_id where m.auth_code like 'business:erp:report:%'")
ERP_GRANTS=$(psql_at "select count(*) from sys_role_menu rm join sys_menu m on m.id=rm.menu_id where m.auth_code like 'business:erp:%'")
if [ "${REPORT_GRANTS:-0}" -ge 1 ]; then
    ok "权限已授予 $REPORT_GRANTS 条"
elif [ "${ERP_GRANTS:-0}" -eq 0 ]; then
    ok "空库首启：授权留空（与 V0141–V0150 同口径；SUPER_ADMIN 通配短路可用，角色建好后重放 V0183 即补齐）"
else
    bad "其它 ERP 权限有授权（$ERP_GRANTS 条），本报表却 0 条 —— 授权段失效"
fi
expect "累计折旧是减项" "t" \
    "$(psql_at "select is_deduction from biz_report_item where report_code='BALANCE_SHEET' and item_code='ACCUM_DEPRECIATION'")"

# ---------- 4. 报表主体 ----------
BS=$(curl -s -H "$AUTH" "$BASE/business/erp/report/balance-sheet?period=2026-10")
expect "2026-10 报表 HTTP code" "0" "$(jget "$BS" "code")"
expect "period" "2026-10" "$(jget "$BS" "data.period")"
expect "period_name" "2026年10月" "$(jget "$BS" "data.period_name")"
expect "period_status" "OPEN" "$(jget "$BS" "data.period_status")"
expect "编制单位非空" "1" "$([ -n "$(jget "$BS" "data.company_name")" ] && echo 1 || echo 0)"
expect "编制时间非空" "1" "$(if [ "$(jget "$BS" "data.generated_at")" != "<null>" ]; then echo 1; else echo 0; fi)"
expect "上期对比（2026-09 已建）" "2026-09" "$(jget "$BS" "data.previous_period")"

expect_num "货币资金 = 9,500（10,000 − 500）" "9500" "$(jamount "$BS" assets MONETARY_FUNDS)"
expect_num "存货 = 2,000" "2000" "$(jamount "$BS" assets INVENTORY)"
expect_num "固定资产原价 = 6,000" "6000" "$(jamount "$BS" assets FIXED_ASSETS_ORIGINAL)"
expect_num "累计折旧 = 1,000（贷方余额，正数显示）" "1000" "$(jamount "$BS" assets ACCUM_DEPRECIATION)"
expect "累计折旧标记 is_deduction" "true" "$(jflag "$BS" assets ACCUM_DEPRECIATION is_deduction)"
expect_num "固定资产净值 = 5,000（原价 − 折旧）" "5000" "$(jamount "$BS" assets FIXED_ASSETS_NET)"
expect_num "流动资产合计 = 11,500" "11500" "$(jamount "$BS" assets CURRENT_ASSETS_TOTAL)"
expect_num "非流动资产合计 = 5,000" "5000" "$(jamount "$BS" assets NONCURRENT_ASSETS_TOTAL)"
expect_num "资产总计 = 16,500" "16500" "$(jamount "$BS" assets TOTAL_ASSETS)"

expect_num "应付账款 = 8,000" "8000" "$(jamount "$BS" liabilities ACCOUNTS_PAYABLE)"
expect_num "流动负债合计 = 8,000" "8000" "$(jamount "$BS" liabilities CURRENT_LIABILITIES_TOTAL)"
expect_num "负债合计 = 8,000" "8000" "$(jamount "$BS" liabilities TOTAL_LIABILITIES)"

expect_num "实收资本 = 10,000" "10000" "$(jamount "$BS" equity PAID_IN_CAPITAL)"
expect_num "未分配利润 = −1,500（本期费用 1,500）" "-1500" "$(jamount "$BS" equity UNDISTRIBUTED_PROFIT)"
expect_num "所有者权益合计 = 8,500" "8500" "$(jamount "$BS" equity TOTAL_EQUITY)"
expect_num "负债和所有者权益总计 = 16,500" "16500" "$(jamount "$BS" equity TOTAL_LIABILITIES_AND_EQUITY)"

expect "勾稽 balanced" "true" "$(jget "$BS" "data.check.balanced")"
expect_num "勾稽 assets_total" "16500" "$(jget "$BS" "data.check.assets_total")"
expect_num "勾稽 liability_equity_total" "16500" "$(jget "$BS" "data.check.liability_equity_total")"
expect_num "勾稽 diff" "0" "$(jget "$BS" "data.check.diff")"
expect "勾稽 diff_text" "0.00" "$(jget "$BS" "data.check.diff_text")"
case "$(jget "$BS" "data.check.message")" in
    平衡*) ok "勾稽结论是「平衡」：$(jget "$BS" "data.check.message")" ;;
    *) bad "勾稽结论应为「平衡…」，实际：$(jget "$BS" "data.check.message")" ;;
esac
expect "无未归类科目" "[]" "$(jget "$BS" "data.check.unmapped")"
expect "无分录不平凭证" "[]" "$(jget "$BS" "data.check.unbalanced_vouchers")"
expect "平的时候不吐提示（映射完整）" "[]" "$(jget "$BS" "data.check.hints")"

# ---------- 5. 空期间 / 无发生额期间 ----------
EMPTY=$(curl -s -H "$AUTH" "$BASE/business/erp/report/balance-sheet?period=2026-07")
expect "空期间 2026-07 code" "0" "$(jget "$EMPTY" "code")"
expect_num "空期间 资产总计 = 0" "0" "$(jget "$EMPTY" "data.check.assets_total")"
expect_num "空期间 负债+权益 = 0" "0" "$(jget "$EMPTY" "data.check.liability_equity_total")"
expect "空期间 仍平衡" "true" "$(jget "$EMPTY" "data.check.balanced")"
expect "空期间 无上期" "<null>" "$(jget "$EMPTY" "data.previous_period")"

NOV=$(curl -s -H "$AUTH" "$BASE/business/erp/report/balance-sheet?period=2026-11")
expect_num "无发生额期间 2026-11 资产总计 = 16,500（期初结转）" "16500" "$(jget "$NOV" "data.check.assets_total")"
expect "无发生额期间 仍平衡" "true" "$(jget "$NOV" "data.check.balanced")"
expect_num "上期对比（2026-10 期末）资产总计" "16500" \
    "$(python3 - "$NOV" <<'PY'
import json, sys
data = json.loads(sys.argv[1])["data"]
for row in data["assets"]:
    if row["item_code"] == "TOTAL_ASSETS":
        print(row["previous_amount"]); raise SystemExit
print("<missing>")
PY
)"

# ---------- 6. 反例 ----------
C1=$(curl -s -o /dev/null -w "%{http_code}" -H "$AUTH" "$BASE/business/erp/report/balance-sheet?period=2026-13")
expect "非法期间（2026-13）→ 400" "400" "$C1"
C2=$(curl -s -H "$AUTH" "$BASE/business/erp/report/balance-sheet?period=2026-13")
expect "非法期间 业务码 = 10001" "10001" "$(jget "$C2" "code")"
C3=$(curl -s -o /dev/null -w "%{http_code}" -H "$AUTH" "$BASE/business/erp/report/balance-sheet?period=2026-12")
expect "空月份 + 没开账（2026-12）→ 400" "400" "$C3"
C4=$(curl -s -H "$AUTH" "$BASE/business/erp/report/balance-sheet?period=2026-12")
expect "空月份 + 没开账 业务码 = 66010" "66010" "$(jget "$C4" "code")"
case "$(jget "$C4" "msg")" in
    *不存在*) ok "空月份 + 没开账 消息说清原因" ;;
    *) bad "空月份 + 没开账 消息应含「不存在」：$(jget "$C4" "msg")" ;;
esac

# 2026-08：**没有期间行，但有一张 8 月的凭证** → 按自然月推算（DERIVED），并在 hints 里要求开账。
# 这条是「有账的月份不能出不了表」的回归用例（生产库实测 2026-11 就是这种状态）。
PSQL <<'SQL'
INSERT INTO biz_voucher (id, voucher_word, voucher_no, period, voucher_date, voucher_type,
                         source_type, source_no, summary, total_debit, total_credit,
                         entry_count, status, is_red)
VALUES ('e4f10001-0000-4000-8000-000000000009', 'ERPR1V', '0009', '2026-08', '2026-08-15', 'AUTO',
        'MANUAL', 'ERPR1V-9', '无期间行的月份凭证', 400, 400, 2, 'POSTED', 0);
INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code,
                               account_name, debit_amount, credit_amount)
SELECT 'e4f10001-0000-4000-8000-000000000009'::uuid, 1, '银行', a.id, a.account_code, a.account_name, 400, 0
  FROM biz_account a WHERE a.account_code = '1002' AND a.deleted_at IS NULL;
INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code,
                               account_name, debit_amount, credit_amount)
SELECT 'e4f10001-0000-4000-8000-000000000009'::uuid, 2, '资本', a.id, a.account_code, a.account_name, 0, 400
  FROM biz_account a WHERE a.account_code = '3001' AND a.deleted_at IS NULL;
SQL
DERIVED=$(curl -s -H "$AUTH" "$BASE/business/erp/report/balance-sheet?period=2026-08")
expect "有账无期间（2026-08）仍能出表" "0" "$(jget "$DERIVED" "code")"
expect "有账无期间 状态 = DERIVED" "DERIVED" "$(jget "$DERIVED" "data.period_status")"
expect "有账无期间 仍平衡" "true" "$(jget "$DERIVED" "data.check.balanced")"
if jget "$DERIVED" "data.check.hints" | grep -q "请尽快开账"; then
    ok "有账无期间 提示里要求尽快开账"
else
    bad "有账无期间 提示里应要求开账"
fi
C5=$(curl -s -o /dev/null -w "%{http_code}" "$BASE/business/erp/report/balance-sheet?period=2026-10")
case "$C5" in
    401|403) ok "无令牌 → $C5" ;;
    *) bad "无令牌应为 401/403，实际 $C5" ;;
esac

# ---------- 7. 导出 xlsx ----------
curl -s -H "$AUTH" "$BASE/business/erp/report/balance-sheet/export?period=2026-10" -o /tmp/erpr1_bs.xlsx -D /tmp/erpr1_bs.hdr
CT=$(grep -i '^content-type' /tmp/erpr1_bs.hdr | tr -d '\r')
case "$CT" in
    *spreadsheetml*) ok "导出 Content-Type = xlsx（${CT#*: }）" ;;
    *) bad "导出 Content-Type 不对：$CT" ;;
esac
if [ "$(head -c 2 /tmp/erpr1_bs.xlsx)" = "PK" ]; then
    ok "导出文件是合法 xlsx（PK 魔数，$(stat -c %s /tmp/erpr1_bs.xlsx) 字节）"
else
    bad "导出文件不是 xlsx"
fi
if grep -qi 'attachment' /tmp/erpr1_bs.hdr; then ok "导出带 Content-Disposition: attachment"; else bad "导出缺 attachment 头"; fi

# ---------- 8. 人为删一条分录 → 必须报不平 ----------
PSQL <<'SQL'
DELETE FROM biz_voucher_entry
 WHERE voucher_id = 'e4f10001-0000-4000-8000-000000000003'::uuid AND credit_amount > 0;
SQL
BROKEN=$(curl -s -H "$AUTH" "$BASE/business/erp/report/balance-sheet?period=2026-10")
expect "删分录后 仍返回 200（报表要能出，但要报不平）" "0" "$(jget "$BROKEN" "code")"
expect "删分录后 balanced = false" "false" "$(jget "$BROKEN" "data.check.balanced")"
expect_num "删分录后 diff = 2,000（资产多 2,000）" "2000" "$(jget "$BROKEN" "data.check.diff")"
expect "删分录后 diff_text = 2000.00" "2000.00" "$(jget "$BROKEN" "data.check.diff_text")"
case "$(jget "$BROKEN" "data.check.message")" in
    不平*差*) ok "结论是「不平 + 差额」：$(jget "$BROKEN" "data.check.message")" ;;
    *) bad "结论应为「不平…差…」，实际：$(jget "$BROKEN" "data.check.message")" ;;
esac
expect "点名了被删分录的凭证 0003" "0003" \
    "$(python3 - "$BROKEN" <<'PY'
import json, sys
data = json.loads(sys.argv[1])["data"]
nos = [row["voucher_no"] for row in data["check"]["unbalanced_vouchers"]]
print(nos[0] if nos else "<none>")
PY
)"
if jget "$BROKEN" "data.check.hints" | grep -q "0003"; then ok "提示里点名了 0003"; else bad "提示里没点名 0003"; fi
expect_num "分录借贷合计差 = 2,000" "2000" \
    "$(python3 - "$BROKEN" <<'PY'
import json, sys
from decimal import Decimal
c = json.loads(sys.argv[1])["data"]["check"]
print(Decimal(c["ledger_debit_total"]) - Decimal(c["ledger_credit_total"]))
PY
)"

# 恢复（让隔离库回到一致状态，便于人工复核）：被删的是 0003 的贷方分录（line_no=2），重新插回去
PSQL <<'SQL'
INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code,
                               account_name, debit_amount, credit_amount)
SELECT v.id, 2, '应付', a.id, a.account_code, a.account_name, 0, 2000
  FROM biz_voucher v, biz_account a
 WHERE v.voucher_word = 'ERPR1V' AND v.voucher_no = '0003'
   AND a.account_code = '2202' AND a.deleted_at IS NULL
ON CONFLICT (voucher_id, line_no) DO NOTHING;
SQL
RESTORED=$(curl -s -H "$AUTH" "$BASE/business/erp/report/balance-sheet?period=2026-10")
expect "恢复分录后 balanced = true" "true" "$(jget "$RESTORED" "data.check.balanced")"

# ---------- 9. 收尾 ----------
say ""
say "=========================================="
say "  PASS=$PASS  FAIL=$FAIL"
say "  隔离库 $DB 保留（复核：psql -d $DB）；删除：su - postgres -c \"dropdb $DB\""
say "=========================================="
[ "$FAIL" -eq 0 ] || exit 1
