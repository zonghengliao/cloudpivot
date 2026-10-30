#!/usr/bin/env bash
# ============================================================
# P2-D1 验收：凭证查询 + 总账 + 科目余额表（真 HTTP + 真库）
#
# 用法：bash scripts/verify/p2d1_ledger.sh
#   REPO=/opt/cloudpivot  后台仓根
#   P2D1_DB=cloudpivot_p2d1v  隔离库名（脚本会自建；已存在则复用）
#   P2D1_PORT=18094  监听端口（被占用时自动向上找空端口）
#
# 做的事：
#   ① 自建隔离库（`createdb -O cloudpivot`）→ 起一次实例把迁移跑到最新（含 V0150）
#   ② 造夹具：2 个科目 + 4 张凭证（POSTED / DRAFT / 红冲 / 跨期间）
#   ③ 登录 admin → 打 4 个接口 + 4 条反例（无令牌 / 缺必填 / 不存在 / 非法期间）
#   ④ psql 侧核对：权限码 4 个、Σ借 = Σ贷、DRAFT 未进总账
#   ⑤ 收尾杀实例（**保留库**，方便评审复跑；DROP 命令打印在末尾）
# ============================================================
set -uo pipefail

REPO=${REPO:-/opt/cloudpivot}
DB=${P2D1_DB:-cloudpivot_p2d1v}
BIN="$REPO/backend/target/debug/cloudpivot"
LOG=/tmp/p2d1_verify_server.log
PASS=0
FAIL=0

say() { printf '%s\n' "$*"; }
ok() { PASS=$((PASS + 1)); say "[PASS] $*"; }
bad() { FAIL=$((FAIL + 1)); say "[FAIL] $*"; }
expect() { # expect <描述> <期望> <实际>
    if [ "$2" = "$3" ]; then ok "$1（$3）"; else bad "$1（期望 $2，实际 $3）"; fi
}
# 数值比较（NUMERIC 经 JSON 回来是字符串 "100.0000"）
expect_num() {
    if awk -v a="$2" -v b="$3" 'BEGIN{exit !(a+0==b+0)}'; then
        ok "$1（$3）"
    else
        bad "$1（期望 $2，实际 $3）"
    fi
}
# 从 JSON 里取字段（点路径：data.total / data.list.0.id …）
jget() { python3 - "$1" "$2" <<'PY'
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
print(node if not isinstance(node, (dict, list)) else json.dumps(node, ensure_ascii=False))
PY
}

# ---------- 0. 前置 ----------
[ -x "$BIN" ] || { say "缺二进制 $BIN（先 cargo build）"; exit 1; }
say "== 二进制 $BIN（$(stat -c %y "$BIN")）"

set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export NIK_DATABASE_NAME=$DB
export NIK_REDIS_URL=redis://127.0.0.1:6379/9
export NIK_ADMIN_PASSWORD="${NIK_ADMIN_PASSWORD:-P2d1_verify_2026}"

su - postgres -c "psql -Atc \"select 1 from pg_database where datname='$DB'\"" | grep -q 1 \
    || su - postgres -c "createdb -O cloudpivot $DB"
say "== 隔离库 $DB 就绪"

# 端口：18094 起向上找空
PORT=${P2D1_PORT:-18094}
for _ in $(seq 1 40); do
    if ! ss -ltn 2>/dev/null | grep -q ":$PORT "; then break; fi
    PORT=$((PORT + 1))
done
export NIK_SERVER_LISTEN=127.0.0.1:$PORT
BASE="http://127.0.0.1:$PORT/api"
say "== 实例端口 $PORT（日志 $LOG）"

cd "$REPO/backend" || exit 1
setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
SRV=$!
for _ in $(seq 1 120); do
    if grep -q "数据库迁移完成" "$LOG" 2>/dev/null; then break; fi
    if ! kill -0 "$SRV" 2>/dev/null; then say "实例启动失败："; tail -20 "$LOG"; exit 1; fi
    sleep 1
done
grep -E "数据库迁移完成|迁移失败" "$LOG" | tail -2
sleep 2
kill -0 "$SRV" 2>/dev/null || { say "实例已退出"; tail -20 "$LOG"; exit 1; }

cleanup() { kill "$SRV" 2>/dev/null; sleep 1; kill -9 "$SRV" 2>/dev/null; }
trap cleanup EXIT

# ---------- 1. 夹具 ----------
export PGPASSWORD=$NIK_DATABASE_PASSWORD
PSQL="psql -h 127.0.0.1 -U $NIK_DATABASE_USER -d $DB -q -v ON_ERROR_STOP=1"
psql_at() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$DB" -Atc "$1" 2>&1; }

$PSQL <<'SQL'
DELETE FROM biz_voucher WHERE voucher_word = 'P2D1V';
DELETE FROM biz_account WHERE account_code IN ('P2D1V-A', 'P2D1V-B');

INSERT INTO biz_account (id, account_code, account_name, parent_id, level,
                         account_category, balance_direction, is_leaf, status)
VALUES ('d2d10002-0000-4000-8000-000000000001', 'P2D1V-A', 'P2D1V 测试现金', NULL, 1,
        'ASSET', 'DEBIT', 1, 1),
       ('d2d10002-0000-4000-8000-000000000002', 'P2D1V-B', 'P2D1V 测试应付', NULL, 1,
        'LIABILITY', 'CREDIT', 1, 1);

-- 期间 2026-10 由 V0137 种下（OPEN）；这里兜底一次，避免种子变动让脚本假红
INSERT INTO biz_accounting_period (period_year, period_month, period_code,
                                   start_date, end_date, status)
VALUES (2026, 10, '2026-10', '2026-10-01', '2026-10-31', 'OPEN')
ON CONFLICT DO NOTHING;

-- 4 张凭证：1 张 9 月（期初）、1 张草稿、1 张 10 月正常、1 张 10 月红冲
INSERT INTO biz_voucher (id, voucher_word, voucher_no, period, voucher_date, voucher_type,
                         source_type, source_no, summary, total_debit, total_credit,
                         entry_count, status, is_red, reverse_of)
VALUES ('d2d10002-0000-4000-8000-0000000000a1', 'P2D1V', 'P2D1V-0001', '2026-09', '2026-09-10',
        'AUTO', 'STOCK_IN', 'P2D1V-S1', 'P2D1V 9 月凭证', 100, 100, 2, 'POSTED', 0, NULL),
       ('d2d10002-0000-4000-8000-0000000000a2', 'P2D1V', 'P2D1V-0002', '2026-10', '2026-10-05',
        'AUTO', 'STOCK_IN', 'P2D1V-S2', 'P2D1V 10 月凭证', 50, 50, 2, 'POSTED', 0, NULL),
       ('d2d10002-0000-4000-8000-0000000000a3', 'P2D1V', 'P2D1V-0003', '2026-10', '2026-10-12',
        'MANUAL', 'MANUAL', NULL, 'P2D1V 草稿凭证', 999, 999, 2, 'DRAFT', 0, NULL),
       ('d2d10002-0000-4000-8000-0000000000a4', 'P2D1V', 'P2D1V-0004', '2026-10', '2026-10-15',
        'AUTO', 'STOCK_IN', 'P2D1V-S2', '红冲 P2D1V 10 月凭证', 50, 50, 2, 'POSTED', 1,
        'd2d10002-0000-4000-8000-0000000000a2');

INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code,
                               account_name, debit_amount, credit_amount)
VALUES
 ('d2d10002-0000-4000-8000-0000000000a1', 1, 'P2D1V 9 月 借', 'd2d10002-0000-4000-8000-000000000001',
  'P2D1V-A', 'P2D1V 测试现金', 100, 0),
 ('d2d10002-0000-4000-8000-0000000000a1', 2, 'P2D1V 9 月 贷', 'd2d10002-0000-4000-8000-000000000002',
  'P2D1V-B', 'P2D1V 测试应付', 0, 100),
 ('d2d10002-0000-4000-8000-0000000000a2', 1, 'P2D1V 10 月 借', 'd2d10002-0000-4000-8000-000000000001',
  'P2D1V-A', 'P2D1V 测试现金', 50, 0),
 ('d2d10002-0000-4000-8000-0000000000a2', 2, 'P2D1V 10 月 贷', 'd2d10002-0000-4000-8000-000000000002',
  'P2D1V-B', 'P2D1V 测试应付', 0, 50),
 ('d2d10002-0000-4000-8000-0000000000a3', 1, 'P2D1V 草稿 借', 'd2d10002-0000-4000-8000-000000000001',
  'P2D1V-A', 'P2D1V 测试现金', 999, 0),
 ('d2d10002-0000-4000-8000-0000000000a3', 2, 'P2D1V 草稿 贷', 'd2d10002-0000-4000-8000-000000000002',
  'P2D1V-B', 'P2D1V 测试应付', 0, 999),
 ('d2d10002-0000-4000-8000-0000000000a4', 1, '红冲 P2D1V 10 月 贷', 'd2d10002-0000-4000-8000-000000000001',
  'P2D1V-A', 'P2D1V 测试现金', 0, 50),
 ('d2d10002-0000-4000-8000-0000000000a4', 2, '红冲 P2D1V 10 月 借', 'd2d10002-0000-4000-8000-000000000002',
  'P2D1V-B', 'P2D1V 测试应付', 50, 0);
SQL
[ $? -eq 0 ] && ok "夹具写入（2 科目 / 4 凭证 / 8 分录）" || { bad "夹具写入失败"; exit 1; }

# ---------- 2. 登录（首登可能强制改密；复跑要用改过之后的口令） ----------
NEWPASS="P2d1_verify_new_2026"
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
if [ "$TOKEN" = "<null>" ] || [ "$TOKEN" = "<not-json>" ]; then
    say "登录响应：$LOGIN"; bad "admin 登录失败"; exit 1
fi
if [ "$(jget "$LOGIN" "data.must_change_password")" = "True" ]; then
    curl -s -X PUT "$BASE/system/auth/password" -H "Authorization: Bearer $TOKEN" \
        -H 'Content-Type: application/json' \
        -d "{\"old_password\":\"$NIK_ADMIN_PASSWORD\",\"new_password\":\"$NEWPASS\"}" >/dev/null
    LOGIN=$(curl -s -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
        -d "{\"username\":\"admin\",\"password\":\"$NEWPASS\"}")
    TOKEN=$(jget "$LOGIN" "data.access_token")
    NIK_ADMIN_PASSWORD=$NEWPASS
fi
[ ${#TOKEN} -gt 20 ] && ok "admin 登录（令牌 ${#TOKEN} 字节）" || { bad "拿不到令牌"; exit 1; }
AUTH="Authorization: Bearer $TOKEN"

# ---------- 3. ① 凭证列表 ----------
R=$(curl -s -H "$AUTH" "$BASE/business/erp/voucher/page?period=2026-10&page=1&page_size=10")
expect "①-a 凭证列表按期间筛选（2026-10 共 3 张：POSTED×2 + DRAFT×1）" "3" "$(jget "$R" "data.total")"
expect "①-b code=0" "0" "$(jget "$R" "code")"

R=$(curl -s -H "$AUTH" "$BASE/business/erp/voucher/page?status=DRAFT")
expect "①-c 按状态筛选（DRAFT 1 张）" "1" "$(jget "$R" "data.total")"
expect "①-d DRAFT 的中文标签" "草稿" "$(jget "$R" "data.list.0.status_label")"

R=$(curl -s -H "$AUTH" "$BASE/business/erp/voucher/page?source_type=MANUAL")
expect "①-e 按来源类型筛选（MANUAL 1 张）" "1" "$(jget "$R" "data.total")"

R=$(curl -s -H "$AUTH" "$BASE/business/erp/voucher/page?voucher_no=P2D1V-0002")
expect "①-f 按凭证号模糊筛选" "1" "$(jget "$R" "data.total")"
expect "①-g 命中的凭证号" "P2D1V-0002" "$(jget "$R" "data.list.0.voucher_no")"

H=$(curl -s -o /tmp/p2d1_bad_status.json -w '%{http_code}' -H "$AUTH" "$BASE/business/erp/voucher/page?status=POSTEDD")
expect "①-h 非法状态 → HTTP 400" "400" "$H"
expect "①-h2 非法状态 → 业务码 10001（不静默返回空列表）" "10001" "$(jget "$(cat /tmp/p2d1_bad_status.json)" "code")"

H=$(curl -s -o /tmp/p2d1_bad_period.json -w '%{http_code}' -H "$AUTH" "$BASE/business/erp/voucher/page?period=2026-13")
expect "①-i 非法期间 → HTTP 400" "400" "$H"
expect "①-i2 非法期间 → 业务码 10001" "10001" "$(jget "$(cat /tmp/p2d1_bad_period.json)" "code")"

# ---------- 4. ② 凭证详情 ----------
R=$(curl -s -H "$AUTH" "$BASE/business/erp/voucher/d2d10002-0000-4000-8000-0000000000a2")
expect "②-a 详情分行数（2 行）" "2" "$(jget "$R" "data.entries" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))' 2>/dev/null || echo 0)"
expect_num "②-b 分录借方合计 = 单头 total_debit" "50" "$(jget "$R" "data.entry_total_debit")"
expect_num "②-c 单头 total_debit" "50" "$(jget "$R" "data.total_debit")"
expect "②-d 一致性闸 balanced" "True" "$(jget "$R" "data.balanced")"
expect "②-e 分录带科目编码快照" "P2D1V-A" "$(jget "$R" "data.entries.0.account_code")"
expect "②-f 分录带科目名称快照" "P2D1V 测试现金" "$(jget "$R" "data.entries.0.account_name")"

R=$(curl -s -o /dev/null -w '%{http_code}' -H "$AUTH" "$BASE/business/erp/voucher/d2d10002-0000-4000-8000-0000000000ff")
expect "②-g 不存在的凭证 → 404" "404" "$R"

R=$(curl -s -H "$AUTH" "$BASE/business/erp/voucher/not-a-uuid")
expect "②-h 非法 UUID → 400 + 66006" "66006" "$(jget "$R" "code")"

# ---------- 5. ③ 总账（流水 + 逐行余额） ----------
R=$(curl -s -H "$AUTH" "$BASE/business/erp/ledger/page?account_code=P2D1V-A&page=1&page_size=20")
expect "③-a 科目 A 流水行数（9 月 + 10 月 + 红冲，DRAFT 不算）" "3" "$(jget "$R" "data.total")"
expect_num "③-b 第 1 行余额（100）" "100" "$(jget "$R" "data.list.0.running_balance")"
expect_num "③-c 第 2 行余额（150）" "150" "$(jget "$R" "data.list.1.running_balance")"
expect_num "③-d 第 3 行余额（红冲后 100）" "100" "$(jget "$R" "data.list.2.running_balance")"
expect "③-e 红字行 is_red" "True" "$(jget "$R" "data.list.2.is_red")"

R=$(curl -s -H "$AUTH" "$BASE/business/erp/ledger/page?account_code=P2D1V-A&period_from=2026-10&period_to=2026-10")
expect "③-f 期间过滤（2026-10 只有 2 行）" "2" "$(jget "$R" "data.total")"
expect_num "③-g 区间首行余额不含期初（50）" "50" "$(jget "$R" "data.list.0.running_balance")"
expect_num "③-h 区间末行余额（0）" "0" "$(jget "$R" "data.list.1.running_balance")"

R=$(curl -s -H "$AUTH" "$BASE/business/erp/ledger/page?account_code=P2D1V-A&page=2&page_size=2")
expect_num "③-i 跨页余额接续（第 2 页第 1 行 = 100）" "100" "$(jget "$R" "data.list.0.running_balance")"

R=$(curl -s -H "$AUTH" "$BASE/business/erp/ledger/page?page=1")
expect "③-j account_id / account_code 都不给 → 400" "66006" "$(jget "$R" "code")"

R=$(curl -s -o /dev/null -w '%{http_code}' -H "$AUTH" "$BASE/business/erp/ledger/page?account_code=P2D1V-NOPE")
expect "③-k 科目不存在 → 404" "404" "$R"

DRAFT_IN_LEDGER=$(psql_at "SELECT count(*) FROM biz_voucher_entry e JOIN biz_voucher v ON v.id=e.voucher_id WHERE e.account_code='P2D1V-A' AND v.status='POSTED'")
expect "③-l 库里 POSTED 的科目 A 分录数（DRAFT 之外）" "3" "$DRAFT_IN_LEDGER"

# ---------- 6. ④ 科目余额表 ----------
RB=$(curl -s -H "$AUTH" "$BASE/business/erp/ledger/trial-balance?period=2026-10")
expect "④-a code=0" "0" "$(jget "$RB" "code")"
expect "④-b balanced" "True" "$(jget "$RB" "data.balanced")"
expect_num "④-c Σ本期借 = Σ本期贷" \
    "$(jget "$RB" "data.total_period_debit")" "$(jget "$RB" "data.total_period_credit")"
expect_num "④-d 期初借方合计 = 期初贷方合计" \
    "$(jget "$RB" "data.total_opening_debit")" "$(jget "$RB" "data.total_opening_credit")"
expect_num "④-e 期末借方合计 = 期末贷方合计" \
    "$(jget "$RB" "data.total_closing_debit")" "$(jget "$RB" "data.total_closing_credit")"

# 用 python 从 rows 里挑 P2D1V-A / P2D1V-B 两组数（避免依赖 list 下标）
ROW_A=$(python3 - "$RB" <<'PY'
import json, sys
node = json.loads(sys.argv[1])["data"]["rows"]
row = next(r for r in node if r["account_code"] == "P2D1V-A")
print(f'{row["opening_debit"]}|{row["opening_credit"]}|{row["period_debit"]}|{row["period_credit"]}|{row["closing_debit"]}|{row["closing_credit"]}')
PY
)
IFS='|' read -r OA_OD OA_OC OA_PD OA_PC OA_CD OA_CC <<< "$ROW_A"
expect_num "④-f 科目 A 期初借方（9 月的 100）" "100" "$OA_OD"
expect_num "④-g 科目 A 本期借方（50）" "50" "$OA_PD"
expect_num "④-h 科目 A 本期贷方（红冲 50）" "50" "$OA_PC"
expect_num "④-i 科目 A 期末借方（100 + 50 − 50）" "100" "$OA_CD"
expect_num "④-j 期初 + 本期借 − 本期贷 = 期末" \
    "$(awk -v a="$OA_OD" -v b="$OA_PD" -v c="$OA_PC" 'BEGIN{printf "%.4f", a+b-c}')" "$OA_CD"

ROW_B=$(python3 - "$RB" <<'PY'
import json, sys
node = json.loads(sys.argv[1])["data"]["rows"]
row = next(r for r in node if r["account_code"] == "P2D1V-B")
print(f'{row["opening_credit"]}|{row["period_debit"]}|{row["period_credit"]}|{row["closing_credit"]}')
PY
)
IFS='|' read -r OB_OC OB_PD OB_PC OB_CC <<< "$ROW_B"
expect_num "④-k 科目 B 期初贷方（100）" "100" "$OB_OC"
expect_num "④-l 科目 B 期末贷方（100）" "100" "$OB_CC"

H=$(curl -s -o /tmp/p2d1_no_period.json -w '%{http_code}' -H "$AUTH" "$BASE/business/erp/ledger/trial-balance")
expect "④-m period 缺失 → HTTP 400" "400" "$H"
expect "④-m2 period 缺失 → 业务码 10001" "10001" "$(jget "$(cat /tmp/p2d1_no_period.json)" "code")"

# ---------- 7. 权限 / 认证 ----------
expect "权限-a 无令牌打凭证列表 → 401" "401" \
    "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/business/erp/voucher/page")"
expect "权限-b 无令牌打余额表 → 401" "401" \
    "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/business/erp/ledger/trial-balance?period=2026-10")"
NPERM=$(psql_at "SELECT count(*) FROM sys_menu WHERE auth_code IN ('business:erp:voucher:list','business:erp:voucher:detail','business:erp:ledger:list','business:erp:ledger:trial-balance')")
expect "权限-c V0150 的 4 个权限码已种" "4" "$NPERM"
SCHEMA=$(psql_at "SELECT max(version::int) FROM schema_migrations")
say "== schema_migrations max = $SCHEMA（期望 ≥150）"

# ---------- 8. 汇总 ----------
say ""
say "==> 汇总：PASS=$PASS FAIL=$FAIL"
say "== 隔离库 $DB 保留给评审复跑；清理：su - postgres -c 'dropdb $DB'"
[ "$FAIL" -eq 0 ] || exit 1
