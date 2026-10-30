#!/usr/bin/env bash
# ============================================================
# P2-C2 验收：应付单查询（/business/erp/payable/page + /{id}），真 HTTP + 真库
#
# 用法：bash scripts/verify/p2c2_payable.sh
#   REPO=/opt/cloudpivot        后台仓根
#   P2C2_DB=cloudpivot_p2c2v    隔离库名（脚本自建；已存在则复用）
#   P2C2_PORT=18130             监听端口（被占用时自动向上找空端口）
#
# 做的事：
#   ① 自建隔离库（`createdb -O cloudpivot`）→ 起一次实例把迁移跑到最新（含 V0151）
#   ② 造夹具：2 个供应商 + 1 张凭证 + 5 张应付单（OPEN / PARTIAL_PAID / SETTLED / CANCELLED）
#   ③ 登录 admin → 打 2 个接口（默认列表 / 状态 / 供应商 / 单号 / 分页 / 详情）+ 5 条反例
#   ④ psql 侧核对：V0151 的 2 个权限码、重放迁移幂等且角色授权补齐
#   ⑤ 收尾杀实例（**保留库**，方便评审复跑；DROP 命令打印在末尾）
# ============================================================
set -uo pipefail

REPO=${REPO:-/opt/cloudpivot}
DB=${P2C2_DB:-cloudpivot_p2c2v}
BIN="$REPO/backend/target/debug/cloudpivot"
LOG=/tmp/p2c2_verify_server.log
PASS=0
FAIL=0

say() { printf '%s\n' "$*"; }
ok() { PASS=$((PASS + 1)); say "[PASS] $*"; }
bad() { FAIL=$((FAIL + 1)); say "[FAIL] $*"; }
expect() { # expect <描述> <期望> <实际>
    if [ "$2" = "$3" ]; then ok "$1（$3）"; else bad "$1（期望 $2，实际 $3）"; fi
}
# 数值比较（NUMERIC 经 JSON 回来是字符串 "226.0000"）
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
# 数组长度
jlen() { python3 - "$1" "$2" <<'PY'
import json, sys
raw, path = sys.argv[1], sys.argv[2]
try:
    node = json.loads(raw)
except Exception:
    print(0); raise SystemExit
for part in path.split("."):
    if part == "":
        continue
    node = node[int(part)] if isinstance(node, list) else node.get(part)
    if node is None:
        print(0); raise SystemExit
print(len(node) if isinstance(node, list) else 0)
PY
}

# ---------- 0. 前置 ----------
[ -x "$BIN" ] || { say "缺二进制 $BIN（先 cargo build）"; exit 1; }
say "== 二进制 $BIN（$(stat -c %y "$BIN")）"

set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export NIK_DATABASE_NAME=$DB
export NIK_REDIS_URL=redis://127.0.0.1:6379/9
export NIK_ADMIN_PASSWORD="${NIK_ADMIN_PASSWORD:-P2c2_verify_2026}"

su - postgres -c "psql -Atc \"select 1 from pg_database where datname='$DB'\"" | grep -q 1 \
    || su - postgres -c "createdb -O cloudpivot $DB"
say "== 隔离库 $DB 就绪"

PORT=${P2C2_PORT:-18130}
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
psql_at() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$DB" -Atc "$1" 2>&1 | head -1; }

$PSQL <<'SQL'
DELETE FROM biz_payable WHERE payable_no LIKE 'P2C2E-%';
DELETE FROM biz_voucher WHERE voucher_word = 'P2C2E';
DELETE FROM biz_supplier WHERE supplier_code LIKE 'P2C2E-%';

INSERT INTO biz_supplier (id, supplier_code, supplier_name, credit_days)
VALUES ('c2c20002-0000-4000-8000-000000000001', 'P2C2E-SUP-A', 'P2C2E 供应商甲', 30),
       ('c2c20002-0000-4000-8000-000000000002', 'P2C2E-SUP-B', 'P2C2E 供应商乙', 0);

INSERT INTO biz_voucher (id, voucher_word, voucher_no, period, voucher_date, voucher_type,
                         source_type, summary, total_debit, total_credit, entry_count, status)
VALUES ('c2c20002-0000-4000-8000-0000000000a1', 'P2C2E', 'P2C2E-V-0001', '2026-10', '2026-10-31',
        'AUTO', 'STOCK_IN', 'P2C2E 测试凭证', 226, 226, 2, 'POSTED');

-- 5 张应付：创建时间递减（0005 最新 → 0001 最旧），供应商甲 3 张 / 乙 2 张
INSERT INTO biz_payable (id, payable_no, supplier_id, source_type, source_id, source_no,
                         currency_code, amount, paid_amount, unpaid_amount, credit_days,
                         due_date, period_code, status, settled_at, voucher_id, created_at, updated_at)
VALUES
 ('c2c20002-0000-4000-8000-0000c2c20001', 'P2C2E-AP-0001',
  'c2c20002-0000-4000-8000-000000000001', 'STOCK_IN', 'c2c20002-0000-4000-8000-000000020001',
  'P2C2E-IN-0001', 'CNY', 226, 0, 226, 30, '2026-11-30', '2026-10', 'OPEN', NULL,
  'c2c20002-0000-4000-8000-0000000000a1', now() - interval '5 days', now()),
 ('c2c20002-0000-4000-8000-0000c2c20002', 'P2C2E-AP-0002',
  'c2c20002-0000-4000-8000-000000000001', 'STOCK_IN', 'c2c20002-0000-4000-8000-000000020002',
  'P2C2E-IN-0002', 'CNY', 100, 40, 60, 30, '2026-11-30', '2026-10', 'PARTIAL_PAID', NULL,
  NULL, now() - interval '4 days', now()),
 ('c2c20002-0000-4000-8000-0000c2c20003', 'P2C2E-AP-0003',
  'c2c20002-0000-4000-8000-000000000002', 'STOCK_IN', 'c2c20002-0000-4000-8000-000000020003',
  'P2C2E-IN-0003', 'CNY', 50, 50, 0, 0, '2026-10-31', '2026-10', 'SETTLED', now(),
  NULL, now() - interval '3 days', now()),
 ('c2c20002-0000-4000-8000-0000c2c20004', 'P2C2E-AP-0004',
  'c2c20002-0000-4000-8000-000000000002', 'STOCK_IN', 'c2c20002-0000-4000-8000-000000020004',
  'P2C2E-IN-0004', 'CNY', 80, 0, 80, 0, '2026-10-31', '2026-10', 'CANCELLED', NULL,
  NULL, now() - interval '2 days', now()),
 ('c2c20002-0000-4000-8000-0000c2c20005', 'P2C2E-AP-0005',
  'c2c20002-0000-4000-8000-000000000001', 'STOCK_IN', 'c2c20002-0000-4000-8000-000000020005',
  'P2C2E-IN-0005', 'CNY', 30, 0, 30, 30, '2026-11-30', '2026-10', 'OPEN', NULL,
  NULL, now() - interval '1 day', now());
SQL
[ $? -eq 0 ] && ok "夹具写入（2 供应商 / 1 凭证 / 5 应付单）" || { bad "夹具写入失败"; exit 1; }

# ---------- 2. 登录（首登可能强制改密；复跑要用改过之后的口令） ----------
NEWPASS="P2c2_verify_new_2026"
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

SUP_A=c2c20002-0000-4000-8000-000000000001
SUP_B=c2c20002-0000-4000-8000-000000000002
AP1=c2c20002-0000-4000-8000-0000c2c20001
AP2=c2c20002-0000-4000-8000-0000c2c20002

# ---------- 3. ① 列表 ----------
R=$(curl -s -H "$AUTH" "$BASE/business/erp/payable/page?page=1&page_size=20")
expect "①-a code=0" "0" "$(jget "$R" "code")"
expect "①-b 默认返回全部（5 张夹具）" "5" "$(jget "$R" "data.total")"
expect "①-c 第 1 行是最新的 0005（创建时间倒序）" "P2C2E-AP-0005" "$(jget "$R" "data.list.0.payable_no")"
expect "①-d 状态标签（未付）" "未付" "$(jget "$R" "data.list.0.status_label")"
expect "①-e 供应商名称来自 LEFT JOIN" "P2C2E 供应商甲" "$(jget "$R" "data.list.0.supplier_name")"
expect "①-f 来源单号" "P2C2E-IN-0005" "$(jget "$R" "data.list.0.source_no")"
expect "①-g 来源类型" "STOCK_IN" "$(jget "$R" "data.list.0.source_type")"
expect_num "①-h 未付金额" "30" "$(jget "$R" "data.list.0.unpaid_amount")"

R=$(curl -s -H "$AUTH" "$BASE/business/erp/payable/page?status=OPEN&page=1&page_size=20")
expect "②-a status=OPEN 只回 2 张（0001 / 0005）" "2" "$(jget "$R" "data.total")"
expect "②-b 全部是 OPEN" "OPEN" "$(jget "$R" "data.list.0.status")"

R=$(curl -s -H "$AUTH" "$BASE/business/erp/payable/page?supplier_id=$SUP_B&page=1&page_size=20")
expect "③-a supplier_id=乙 回 2 张" "2" "$(jget "$R" "data.total")"
expect "③-b 供应商名称都是乙" "P2C2E 供应商乙" "$(jget "$R" "data.list.0.supplier_name")"
expect "③-c 最新的那张是已作废（0004，创建时间倒序）" "已作废" "$(jget "$R" "data.list.0.status_label")"
expect "③-d 第 2 张是已结清（0003）" "已结清" "$(jget "$R" "data.list.1.status_label")"

R=$(curl -s -H "$AUTH" "$BASE/business/erp/payable/page?payable_no=P2C2E-AP-0003")
expect "④-a 单号模糊筛选命中 1 张" "1" "$(jget "$R" "data.total")"
expect "④-b 命中的单号" "P2C2E-AP-0003" "$(jget "$R" "data.list.0.payable_no")"

R=$(curl -s -H "$AUTH" "$BASE/business/erp/payable/page?supplier_id=$SUP_A&page=1&page_size=2")
expect "⑤-a 分页 total（供应商甲 3 张）" "3" "$(jget "$R" "data.total")"
expect "⑤-b 第 1 页条数 = page_size" "2" "$(jlen "$R" "data.list")"
expect "⑤-c 第 1 页首行 0005" "P2C2E-AP-0005" "$(jget "$R" "data.list.0.payable_no")"
R=$(curl -s -H "$AUTH" "$BASE/business/erp/payable/page?supplier_id=$SUP_A&page=2&page_size=2")
expect "⑤-d 第 2 页只剩 1 张" "1" "$(jlen "$R" "data.list")"
expect "⑤-e 第 2 页是 0001（不重不漏）" "P2C2E-AP-0001" "$(jget "$R" "data.list.0.payable_no")"

# 到期日区间：P2-E2 前端（payable-logic.ts）真在发 due_date_from / due_date_to
R=$(curl -s -H "$AUTH" "$BASE/business/erp/payable/page?supplier_id=$SUP_B&due_date_to=2026-10-31&page=1&page_size=20")
expect "⑤-f 到期日上界含当天（乙 2 张都 2026-10-31）" "2" "$(jget "$R" "data.total")"
R=$(curl -s -H "$AUTH" "$BASE/business/erp/payable/page?supplier_id=$SUP_A&due_date_from=2026-11-01&due_date_to=2026-11-30&page=1&page_size=20")
expect "⑤-g 到期日区间（甲 3 张都 2026-11-30）" "3" "$(jget "$R" "data.total")"
R=$(curl -s -H "$AUTH" "$BASE/business/erp/payable/page?supplier_id=$SUP_B&due_date_from=2026-11-01&page=1&page_size=20")
expect "⑤-h 到期日下界排除（乙 0 张）" "0" "$(jget "$R" "data.total")"

# ---------- 4. ② 详情 ----------
R=$(curl -s -H "$AUTH" "$BASE/business/erp/payable/$AP1")
expect "⑥-a code=0" "0" "$(jget "$R" "code")"
expect "⑥-b 单号" "P2C2E-AP-0001" "$(jget "$R" "data.payable_no")"
expect "⑥-c 供应商" "$SUP_A" "$(jget "$R" "data.supplier_id")"
expect "⑥-d 供应商名称" "P2C2E 供应商甲" "$(jget "$R" "data.supplier_name")"
expect "⑥-e 来源单号" "P2C2E-IN-0001" "$(jget "$R" "data.source_no")"
expect_num "⑥-f 应付金额 226" "226" "$(jget "$R" "data.amount")"
expect_num "⑥-g 已付 0" "0" "$(jget "$R" "data.paid_amount")"
expect_num "⑥-h 未付 226" "226" "$(jget "$R" "data.unpaid_amount")"
expect "⑥-i 状态" "OPEN" "$(jget "$R" "data.status")"
expect "⑥-j 状态标签" "未付" "$(jget "$R" "data.status_label")"
expect "⑥-k 到期日" "2026-11-30" "$(jget "$R" "data.due_date")"
expect "⑥-l 关联凭证号" "P2C2E-V-0001" "$(jget "$R" "data.voucher_no")"
expect "⑥-m 有 created_at" "True" \
    "$([ "$(jget "$R" "data.created_at")" != "<null>" ] && echo True || echo False)"

R=$(curl -s -H "$AUTH" "$BASE/business/erp/payable/$AP2")
expect "⑦-a 部分已付的标签" "部分已付" "$(jget "$R" "data.status_label")"
expect "⑦-b 没有凭证 → voucher_no 为 null" "<null>" "$(jget "$R" "data.voucher_no")"

expect "⑧ 不存在的应付单 → 404" "404" \
    "$(curl -s -o /dev/null -w '%{http_code}' -H "$AUTH" "$BASE/business/erp/payable/c2c20002-0000-4000-8000-00000000ffff")"
R=$(curl -s -H "$AUTH" "$BASE/business/erp/payable/not-a-uuid")
expect "⑨ 非法 UUID → 400 + 66006" "66006" "$(jget "$R" "code")"

# ---------- 5. 反例（参数校验） ----------
H=$(curl -s -o /tmp/p2c2_bad_status.json -w '%{http_code}' -H "$AUTH" \
    "$BASE/business/erp/payable/page?status=PAID")
expect "⑩-a 非法 status → HTTP 400" "400" "$H"
expect "⑩-b 非法 status → 业务码 10001（不静默返回空列表）" "10001" \
    "$(jget "$(cat /tmp/p2c2_bad_status.json)" "code")"
H=$(curl -s -o /tmp/p2c2_bad_sup.json -w '%{http_code}' -H "$AUTH" \
    "$BASE/business/erp/payable/page?supplier_id=not-a-uuid")
expect "⑪-a 非法 supplier_id → HTTP 400" "400" "$H"
expect "⑪-b 非法 supplier_id → 业务码 66006" "66006" \
    "$(jget "$(cat /tmp/p2c2_bad_sup.json)" "code")"
H=$(curl -s -o /tmp/p2c2_bad_date.json -w '%{http_code}' -H "$AUTH" \
    "$BASE/business/erp/payable/page?due_date_from=2026-13-01")
expect "⑪-c 非法到期日 → HTTP 400" "400" "$H"
expect "⑪-d 非法到期日 → 业务码 10001" "10001" \
    "$(jget "$(cat /tmp/p2c2_bad_date.json)" "code")"

# ---------- 6. 权限 / 认证 ----------
expect "⑫-a 无令牌打列表 → 401" "401" \
    "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/business/erp/payable/page")"
expect "⑫-b 无令牌打详情 → 401" "401" \
    "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/business/erp/payable/$AP1")"
NPERM=$(psql_at "SELECT count(*) FROM sys_menu WHERE auth_code IN ('business:erp:payable:list','business:erp:payable:detail')")
expect "⑫-c V0151 的 2 个权限码已种" "2" "$NPERM"
SCHEMA=$(psql_at "SELECT max(version::int) FROM schema_migrations")
say "== schema_migrations max = $SCHEMA（期望 ≥151）"
[ "${SCHEMA:-0}" -ge 151 ] && ok "迁移已到 V0151" || bad "迁移未到 V0151（max=$SCHEMA）"

# 空库首启时迁移先于 bootstrap：角色还不存在 → 授权 0 行。重放 up 证明幂等 + 补齐授权。
GRANTS0=$(psql_at "SELECT count(*) FROM sys_role_menu WHERE menu_id IN ('20000000-0000-0000-0000-000000001009'::uuid,'20000000-0000-0000-0000-000000001010'::uuid)")
say "== 重放前授权行数 = $GRANTS0（空库首启应为 0）"
$PSQL -f "$REPO/backend/migrations/business/V0151__erp_payable_permissions.up.sql" >/tmp/p2c2_replay.log 2>&1 \
    && ok "V0151 重放（幂等）" || { bad "V0151 重放失败"; tail -5 /tmp/p2c2_replay.log; }
expect "⑬-a 重放后权限节点仍是 2 个（幂等）" "2" \
    "$(psql_at "SELECT count(*) FROM sys_menu WHERE right(id::text,4) IN ('1009','1010')")"
GRANTS1=$(psql_at "SELECT count(*) FROM sys_role_menu WHERE menu_id IN ('20000000-0000-0000-0000-000000001009'::uuid,'20000000-0000-0000-0000-000000001010'::uuid)")
say "== 重放后授权行数 = $GRANTS1"
[ "${GRANTS1:-0}" -ge 2 ] && ok "⑬-b 角色授权已补齐（超管至少 2 条）" \
    || bad "角色授权没补齐（$GRANTS1）"
expect "⑬-c 超管持有 list 码" "1" \
    "$(psql_at "SELECT count(*) FROM sys_role_menu rm JOIN sys_role r ON r.id=rm.role_id JOIN sys_menu m ON m.id=rm.menu_id WHERE r.role_code='SUPER_ADMIN' AND m.auth_code='business:erp:payable:list'")"

# ---------- 7. 汇总 ----------
say ""
say "==> 汇总：PASS=$PASS FAIL=$FAIL"
say "== 清理（按需执行）："
say "   DROP DATABASE $DB;   -- su - postgres -c 'dropdb $DB'"
[ "$FAIL" -eq 0 ] || exit 1
