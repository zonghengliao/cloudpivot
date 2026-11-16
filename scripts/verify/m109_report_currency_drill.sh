#!/usr/bin/env bash
# =============================================================================
# m109_report_currency_drill.sh —— M1-09（报表取数按币种）真引擎 + 真 HTTP
#
# 卡：t_7f95005f。**不是** psql 手动跑 SQL：用编译出的后端二进制真启动，走引擎自己的
# 迁移器（schema_migrations 指纹 / 幂等 / 顺序）与真 HTTP 路由。
#
# 四段：
#   A) 克隆库升级（模板 cloudpivot_test 停版 V0404 → 真启动补齐 V0405..V0413）+ 重启幂等
#   B) 空库首启（全量迁移；本卡**没有新迁移**，这里只验证「加了个接口不会让首启挂掉」）
#   C) 真 HTTP：资产负债表 / 利润表的币种切换
#      ① 外币科目标记走 M1-03 接口（不许用 SQL 绕过）
#      ② 六张凭证（USD 三张 / CNY 一张 / EUR 一张 / HKD 一张 / USD 草稿一张）走 HTTP
#         建单 → 审核 → 记账（内控开着，制单 ≠ 审核）
#      ③ 本位币口径：1001 = 1979（外币按本位币金额算，与本位币凭证一起）
#      ④ USD 口径：1001 = 170 / 1122 = 100 / 2202 = 200 / 折合 1207 / 勾稽平
#      ⑤ EUR 口径：只算 EUR 分录；HKD 口径：缺期末牌价 ⇒ 折合列 null + rate_missing
#      ⑥ 利润表：收入 100 / 管理费用 30 / 净利润 70；本位币口径 710 / 213 / 497
#      ⑦ 入参：本位币 200（切回本位币不是错）、小写认、非法格式 400+10001、未知币种 400+66084
#      ⑧ 与外币余额表（M1-07）对拍：1001 的 closing_fc = 170
#      ⑨ 导出 xlsx（外币口径）200 + 真 zip 魔数；无码账号 403
#
# 用法：bash scripts/verify/m109_report_currency_drill.sh [PG_HOST]
#   默认 PG_HOST=172.18.10.145（99 上跑）；145 上跑传 127.0.0.1。
# 端口固定 18097（隔离段 18000-18999），库名含 e2e/upgrade，脚本自己 DROP。
# =============================================================================
set -uo pipefail

PG_HOST="${1:-172.18.10.145}"
export PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4
PG_USER=cloudpivot
TEMPLATE=cloudpivot_test
DB_UP=cp_m109_upgrade
DB_E2E=cp_m109_e2e
PORT=18097
BASE="http://127.0.0.1:$PORT/api"
REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
BACKEND_DIR="$REPO_ROOT/backend"
BIN="$BACKEND_DIR/target/debug/cloudpivot"
RUN_DIR="${M109_RUN_DIR:-/root/.hermes/profiles/backend-developer/cache/scratch/m109-run}"
mkdir -p "$RUN_DIR"

PASS=0
FAIL=0
ok()  { PASS=$((PASS+1)); echo "  PASS  $*"; }
bad() { FAIL=$((FAIL+1)); echo "  FAIL  $*"; }

psql_admin() { psql -h "$PG_HOST" -U "$PG_USER" -d postgres -tAc "$1"; }
q_db()       { psql -h "$PG_HOST" -U "$PG_USER" -d "$1" -tAc "$2"; }

[[ -x "$BIN" ]] || { echo "FATAL: 缺 $BIN（cd backend && cargo build --bin cloudpivot）"; exit 2; }

JWT_FILE="$RUN_DIR/jwt_secret"
MASTER_FILE="$RUN_DIR/storage_master_key"
[[ -s "$JWT_FILE" ]] || openssl rand -hex 32 > "$JWT_FILE"
[[ -s "$MASTER_FILE" ]] || openssl rand -hex 32 > "$MASTER_FILE"

ADMIN_PASSWORD='M109@Drill#2026'

start_backend() { # start_backend <db> <logfile>
    local db="$1" log="$2"
    (
        cd "$BACKEND_DIR" || exit 1
        export NIK_PROFILE=test
        export NIK_DATABASE_HOST="$PG_HOST" NIK_DATABASE_PORT=5432
        export NIK_DATABASE_NAME="$db" NIK_DATABASE_USER="$PG_USER"
        export NIK_DATABASE_PASSWORD="$PGPASSWORD" NIK_DATABASE_SSL_MODE=prefer
        export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
        export NIK_MIDDLEWARE_AUTH_JWT_SECRET="$(cat "$JWT_FILE")"
        export CLOUDPIVOT_STORAGE_MASTER_KEY="$(cat "$MASTER_FILE")"
        export X_CONFIG_SECRET_KEY="m109-drill-config-secret"
        export NIK_ADMIN_PASSWORD="$ADMIN_PASSWORD"
        exec "$BIN"
    ) >"$log" 2>&1 &
    echo $! > "$RUN_DIR/backend.pid"
    local i
    for i in $(seq 1 120); do
        if curl -fsS -m 2 "http://127.0.0.1:$PORT/health" >/dev/null 2>&1; then return 0; fi
        sleep 1
    done
    tail -20 "$log"
    return 1
}

stop_backend() {
    [[ -f "$RUN_DIR/backend.pid" ]] || return 0
    local pid; pid="$(cat "$RUN_DIR/backend.pid")"
    kill "$pid" 2>/dev/null
    for _ in $(seq 1 30); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
    kill -9 "$pid" 2>/dev/null
    rm -f "$RUN_DIR/backend.pid"
    for _ in $(seq 1 20); do curl -fsS -m 1 "http://127.0.0.1:$PORT/health" >/dev/null 2>&1 || break; sleep 1; done
}

cleanup() {
    stop_backend
    psql_admin "DROP DATABASE IF EXISTS $DB_UP" >/dev/null 2>&1
    psql_admin "DROP DATABASE IF EXISTS $DB_E2E" >/dev/null 2>&1
}
trap cleanup EXIT

# ============================================================================
echo "== A) 克隆库升级（真引擎：模板库 → $DB_UP）=="
psql_admin "DROP DATABASE IF EXISTS $DB_UP" >/dev/null 2>&1
psql_admin "DROP DATABASE IF EXISTS $DB_E2E" >/dev/null 2>&1
if ! psql_admin "CREATE DATABASE $DB_UP TEMPLATE $TEMPLATE" >/dev/null 2>&1; then
    echo "  克隆模板库失败（模板可能被别的会话占用），5 秒后重试一次"
    sleep 5
    psql_admin "CREATE DATABASE $DB_UP TEMPLATE $TEMPLATE" >/dev/null || exit 2
fi
BEFORE_CNT="$(q_db "$DB_UP" "select count(*) from schema_migrations where kind='versioned'")"
BEFORE_MAX="$(q_db "$DB_UP" "select coalesce(max(version::int),0) from schema_migrations where kind='versioned'")"
echo "  升级前：versioned $BEFORE_CNT 条，最大 version=$BEFORE_MAX"

start_backend "$DB_UP" "$RUN_DIR/upgrade.log" || { echo "FATAL: 升级实例没起来"; exit 2; }
ok "克隆库真启动成功（/health 200）"
APPLIED="$(grep -oE '数据库迁移完成: 应用 [0-9]+ 个迁移' "$RUN_DIR/upgrade.log" | head -1)"
echo "  ${APPLIED}"
AFTER_CNT="$(q_db "$DB_UP" "select count(*) from schema_migrations where kind='versioned'")"
[[ "${AFTER_CNT:-0}" -ge "${BEFORE_CNT:-0}" ]] && ok "versioned 迁移条数 $BEFORE_CNT → $AFTER_CNT（不回退）" || bad "versioned 条数倒退"
[[ "$(q_db "$DB_UP" "select success from schema_migrations order by version::int desc limit 1")" == "t" ]] \
    && ok "最新一条迁移 success = true" || bad "最新迁移没成功"
# 本卡只加接口、不加迁移：head 版本必须停在 V0413（下一张卡从 V0414 续）
HEAD_VER="$(q_db "$DB_UP" "select max(version::int) from schema_migrations where kind='versioned'")"
[[ "$HEAD_VER" == "413" ]] && ok "head 迁移版本 = V0413（本卡无新迁移，下一卡从 V0414 续）" || bad "head 版本 = $HEAD_VER（期望 413）"

stop_backend
start_backend "$DB_UP" "$RUN_DIR/upgrade-restart.log" || { echo "FATAL: 重启失败"; exit 2; }
APPLIED2="$(grep -oE '数据库迁移完成: 应用 [0-9]+ 个迁移' "$RUN_DIR/upgrade-restart.log" | head -1)"
case "$APPLIED2" in
    *"应用 0 个迁移"*) ok "重启：应用 0 个迁移（引擎级幂等）" ;;
    *) bad "重启又应用了迁移：[$APPLIED2]" ;;
esac
stop_backend

# ============================================================================
echo
echo "== B) 空库首启（全量迁移；真 HTTP 的落点）=="
psql_admin "CREATE DATABASE $DB_E2E" >/dev/null || { echo "FATAL: 建空库失败"; exit 2; }
start_backend "$DB_E2E" "$RUN_DIR/e2e.log" || { echo "FATAL: 空库实例没起来"; tail -30 "$RUN_DIR/e2e.log"; exit 2; }
CNT3="$(q_db "$DB_E2E" "select count(*) from schema_migrations where kind='versioned'")"
[[ "${CNT3:-0}" -ge 332 ]] && ok "空库首启 versioned 迁移 = $CNT3（≥332）" || bad "空库首启 versioned = $CNT3"
curl -fsS -m 3 "http://127.0.0.1:$PORT/health" >/dev/null && ok "/health 200" || bad "/health 不通"

# 演练库：关掉 admin 的强制改密（只为跑通本卡接口，不涉及业务口径）
q_db "$DB_E2E" "update sys_user set must_change_password = false where username = 'admin'" >/dev/null
[[ "$(q_db "$DB_E2E" "select must_change_password from sys_user where username='admin'")" == "f" ]] \
    && ok "演练库：清掉 admin 的 must_change_password" || bad "清 must_change_password 失败"

# ============================================================================
echo
echo "== C) 真 HTTP：报表取数按币种 =="
TOKEN="$(curl -sS -m 10 -X POST "$BASE/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"admin\",\"password\":\"$ADMIN_PASSWORD\"}" \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print((d.get("data") or {}).get("access_token") or "")' 2>/dev/null)"
[[ -n "${TOKEN:-}" ]] && ok "登录成功（access_token ${#TOKEN} 字符）" || { bad "登录失败"; stop_backend; exit 2; }
AUTH=(-H "Authorization: Bearer $TOKEN")

# 内控：`erp.separation_of_duties.approve` 开着 ⇒ 制单 ≠ 审核，再造一个复核账号
q_db "$DB_E2E" "insert into sys_user (username, password, must_change_password)
                select 'm109checker', password, false from sys_user where username='admin'
                  and not exists (select 1 from sys_user u where u.username='m109checker' and u.deleted_at is null)" >/dev/null
q_db "$DB_E2E" "insert into sys_user_role (user_id, role_id)
                select u.id, r.id from sys_user u, sys_role r
                 where u.username='m109checker' and u.deleted_at is null
                   and r.role_code='SUPER_ADMIN' and r.deleted_at is null
                   and not exists (select 1 from sys_user_role ur where ur.user_id=u.id and ur.role_id=r.id)" >/dev/null
TOKEN_C="$(curl -sS -m 10 -X POST "$BASE/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"m109checker\",\"password\":\"$ADMIN_PASSWORD\"}" \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print((d.get("data") or {}).get("access_token") or "")' 2>/dev/null)"
if [[ -n "${TOKEN_C:-}" ]]; then ok "复核账号 m109checker 登录成功（持 SUPER_ADMIN）"; else bad "复核账号登录失败"; fi
AUTH_C=(-H "Authorization: Bearer $TOKEN_C")

json_field() { # json_field <file> <py-expr on d["data"]>
    python3 -c "
import json,sys
d=json.load(open('$1'))
x=d.get('data') or {}
print(x$2)
" 2>/dev/null
}
err_code() { python3 -c "import json;d=json.load(open('$1'));print(d.get('code'))" 2>/dev/null; }
err_msg()  { python3 -c "import json;d=json.load(open('$1'));print((d.get('msg') or '')[:160])" 2>/dev/null; }
num_eq() { # num_eq <label> <got> <expected>
    python3 -c "
import sys
from decimal import Decimal
got=sys.argv[1]; want=Decimal(sys.argv[2])
sys.exit(0 if got not in ('', 'None', 'NO_ROW') and Decimal(str(got))==want else 1)
" "$2" "$3" && ok "$1 = $3" || bad "$1 ≠ $3（实际 [$2]）"
}
line_amount() { # line_amount <file> <item_code> <field: amount|previous_amount|converted_amount>
    python3 -c "
import json
d=json.load(open('$1')).get('data') or {}
for side in ('assets','liabilities','equity'):
    for row in d.get(side) or []:
        if row.get('item_code')=='$2':
            print(row.get('$3'))
            raise SystemExit
print('NO_ROW')
" 2>/dev/null
}
item_amount() { # item_amount <file> <item_code> <field: current|ytd|current_base|ytd_base>
    python3 -c "
import json
d=json.load(open('$1')).get('data') or {}
for row in d.get('items') or []:
    if row.get('item_code')=='$2':
        print(row.get('$3'))
        raise SystemExit
print('NO_ROW')
" 2>/dev/null
}

# ---------- ① 夹具：期间 / 汇率（数据结构用 SQL；外币标记走 M1-03 接口） ----------
# 期间命名空间 2065-xx 归本脚本独占
# 牌价：USD/EUR/HKD→CNY 三对整体删掉再种（库自带的行若带 `effective_to IS NULL`，
# 会与 2065 年的行在排除约束下判成重叠 —— 本机 PG 18.6 实测如此）
q_db "$DB_E2E" "delete from biz_exchange_rate where from_currency in ('USD','EUR','HKD') and to_currency='CNY'" >/dev/null
q_db "$DB_E2E" "insert into biz_accounting_period (period_year, period_month, period_code, start_date, end_date, status)
    values (2065,9,'2065-09','2065-09-01','2065-09-30','OPEN'),
           (2065,10,'2065-10','2065-10-01','2065-10-31','OPEN')
    on conflict (period_code) do update set status=excluded.status, closed_at=null, closed_by=null" >/dev/null
# USD 2065-09-01..10-14 = 7.0 / 10-15..12-31 = 7.1；EUR 全期 8.0；HKD **只有 9 月**（10 月缺牌价）
# ⚠️ V0407 的「区间不重叠」排除约束：末行 `effective_to` **必须给有限末日**
# （模板/空库自带的 USD 行是 `[2026-11-15, ∞)`，写 NULL 会把区间铺到无穷 ⇒ 冲突）
seed_rate() { # seed_rate <currency> <date> <rate> <effective_to 字面量>
    q_db "$DB_E2E" "insert into biz_exchange_rate
            (from_currency, to_currency, rate_type, effective_date, effective_to, rate)
        values ('$1','CNY','SPOT','$2'::date, $4::date, '$3'::numeric)
        on conflict (from_currency, to_currency, rate_type, effective_date)
        do update set rate=excluded.rate, effective_to=excluded.effective_to" >/dev/null
}
seed_rate USD 2065-09-01 7.0 "'2065-10-14'"
seed_rate USD 2065-10-15 7.1 "'2065-12-31'"
seed_rate EUR 2065-09-01 8.0 "'2065-12-31'"
seed_rate HKD 2065-09-01 1.1 "'2065-09-30'"
ok "夹具：两个期间（2065-09/10）+ USD 两条 / EUR 一条 / HKD 只在 9 月"

ACC_CASH="$(q_db "$DB_E2E" "select id from biz_account where account_code='1001' and deleted_at is null")"
ACC_AR="$(q_db "$DB_E2E" "select id from biz_account where account_code='1122' and deleted_at is null")"
ACC_AP="$(q_db "$DB_E2E" "select id from biz_account where account_code='2202' and deleted_at is null")"
ACC_REV="$(q_db "$DB_E2E" "select id from biz_account where account_code='5001' and deleted_at is null")"
ACC_COST="$(q_db "$DB_E2E" "select id from biz_account where account_code='5401' and deleted_at is null")"
for pair in "1001:$ACC_CASH" "1122:$ACC_AR" "2202:$ACC_AP" "5001:$ACC_REV" "5401:$ACC_COST"; do
    [[ -n "${pair#*:}" ]] || { bad "标准科目 ${pair%%:*} 不在科目表里（空库应带 V0183 的 56 个标准科目）"; stop_backend; exit 2; }
done
ok "夹具：五个标准科目就位（1001 / 1122 / 2202 / 5001 / 5401 —— 报表行是真行；5602 系科目要部门辅助核算，改用 5401）"

mark_fx() { # mark_fx <account_id> <out>
    curl -sS -m 10 -o "$2" -w '%{http_code}' -X PUT "$BASE/business/erp/account/$1/foreign-currency" \
        "${AUTH[@]}" -H 'Content-Type: application/json' -d '{"is_foreign_currency":true}'
}
MARKED=0
for acc in "$ACC_CASH" "$ACC_AR" "$ACC_AP" "$ACC_REV" "$ACC_COST"; do
    st="$(mark_fx "$acc" "$RUN_DIR/mark.json")"
    [[ "$st" == "200" ]] && MARKED=$((MARKED+1)) || bad "M1-03 标记科目失败：$st（$(err_msg "$RUN_DIR/mark.json")）"
done
[[ "$MARKED" == "5" ]] && ok "M1-03 接口把 5 个科目标成「外币核算」⇒ 200（不绕过接口）" || bad "只标成 $MARKED 个"

# ---------- ② 七张凭证：建单 → 审核 → 记账 ----------
create_voucher() { # create_voucher <file> <json>
    curl -sS -m 15 -o "$1" -w '%{http_code}' -X POST "$BASE/business/erp/voucher" \
        "${AUTH[@]}" -H 'Content-Type: application/json' -d "$2"
}
audit_post() { # audit_post <voucher_id> <label>
    local vid="$1" label="$2" st
    st="$(curl -sS -m 10 -o "$RUN_DIR/audit.json" -w '%{http_code}' -X POST "$BASE/business/erp/voucher/$vid/audit" \
        "${AUTH_C[@]}" -H 'Content-Type: application/json' -d '{}')"
    [[ "$st" == "200" ]] || { bad "$label 审核返回 $st（$(err_msg "$RUN_DIR/audit.json")）"; return 1; }
    st="$(curl -sS -m 10 -o "$RUN_DIR/post.json" -w '%{http_code}' -X POST "$BASE/business/erp/voucher/$vid/post" \
        "${AUTH_C[@]}" -H 'Content-Type: application/json' -d '{}')"
    [[ "$st" == "200" ]] || { bad "$label 记账返回 $st（$(err_msg "$RUN_DIR/post.json")）"; return 1; }
    ok "$label 建单(admin) → 审核(checker) → 记账(checker) 全通"
}
post_one() { # post_one <file> <label> <json>
    local st
    st="$(create_voucher "$1" "$3")"
    [[ "$st" == "200" ]] || { bad "$2 建单失败 $st（$(err_msg "$1")）"; stop_backend; exit 2; }
    local vid; vid="$(json_field "$1" ".get('id')")"
    audit_post "$vid" "$2"
}

post_one "$RUN_DIR/v1.json" "V1（USD 200 @7.0，上期）" "{
  \"voucher_date\":\"2065-09-20\",\"summary\":\"M109 上期外币\",\"currency_code\":\"USD\",
  \"entries\":[{\"account_id\":\"$ACC_CASH\",\"debit\":200},{\"account_id\":\"$ACC_AP\",\"credit\":200}]}"
post_one "$RUN_DIR/v2.json" "V2（USD 100 @7.1，本期收入）" "{
  \"voucher_date\":\"2065-10-18\",\"summary\":\"M109 本期收入\",\"currency_code\":\"USD\",
  \"entries\":[{\"account_id\":\"$ACC_AR\",\"debit\":100},{\"account_id\":\"$ACC_REV\",\"credit\":100}]}"
post_one "$RUN_DIR/v3.json" "V3（CNY 50，本位币凭证）" "{
  \"voucher_date\":\"2065-10-12\",\"summary\":\"M109 本位币调整\",
  \"entries\":[{\"account_id\":\"$ACC_AR\",\"debit\":50},{\"account_id\":\"$ACC_AP\",\"credit\":50}]}"
post_one "$RUN_DIR/v4.json" "V4（USD 30 @7.1，本期成本）" "{
  \"voucher_date\":\"2065-10-20\",\"summary\":\"M109 本期成本\",\"currency_code\":\"USD\",
  \"entries\":[{\"account_id\":\"$ACC_COST\",\"debit\":30},{\"account_id\":\"$ACC_CASH\",\"credit\":30}]}"
post_one "$RUN_DIR/v5.json" "V5（EUR 99 @8.0）" "{
  \"voucher_date\":\"2065-10-22\",\"summary\":\"M109 欧元\",\"currency_code\":\"EUR\",
  \"entries\":[{\"account_id\":\"$ACC_CASH\",\"debit\":99},{\"account_id\":\"$ACC_AP\",\"credit\":99}]}"
post_one "$RUN_DIR/v6.json" "V6（HKD 70 @1.1，只在 9 月有牌价）" "{
  \"voucher_date\":\"2065-09-25\",\"summary\":\"M109 港币\",\"currency_code\":\"HKD\",
  \"entries\":[{\"account_id\":\"$ACC_CASH\",\"debit\":70},{\"account_id\":\"$ACC_AP\",\"credit\":70}]}"
st="$(create_voucher "$RUN_DIR/v7.json" "{
  \"voucher_date\":\"2065-10-25\",\"summary\":\"M109 草稿\",\"currency_code\":\"USD\",
  \"entries\":[{\"account_id\":\"$ACC_CASH\",\"debit\":999},{\"account_id\":\"$ACC_AP\",\"credit\":999}]}")"
[[ "$st" == "200" ]] && ok "V7 草稿建单成功（故意不审核不记账 ⇒ 不许进报表）" || bad "V7 建单失败 $st"

# ---------- ③ 本位币口径 ----------
get_bs() { # get_bs <query> <out>
    curl -sS -m 20 -o "$2" -w '%{http_code}' "$BASE/business/erp/report/balance-sheet?$1" "${AUTH[@]}"
}
get_is() { # get_is <query> <out>
    curl -sS -m 20 -o "$2" -w '%{http_code}' "$BASE/business/erp/report/income-statement?$1" "${AUTH[@]}"
}

ST="$(get_bs 'period=2065-10' "$RUN_DIR/bs_base.json")"
[[ "$ST" == "200" ]] && ok "本位币资产负债表 200" || bad "本位币资产负债表返回 $ST（$(err_msg "$RUN_DIR/bs_base.json")）"
[[ "$(json_field "$RUN_DIR/bs_base.json" "['currency']['mode']")" == "BASE" ]] && ok "本位币口径 mode = BASE" || bad "mode 不对"
[[ "$(json_field "$RUN_DIR/bs_base.json" ".get('unit')")" == "元" ]] && ok "本位币口径单位 = 元" || bad "单位不对"
num_eq "本位币 1001 期末（1400 − 213 + 792 + 77 含港币 70×1.1）" "$(line_amount "$RUN_DIR/bs_base.json" MONETARY_FUNDS amount)" 2056
[[ "$(line_amount "$RUN_DIR/bs_base.json" MONETARY_FUNDS converted_amount)" == "None" ]] \
    && ok "本位币口径不出折合列（converted_amount = null）" || bad "本位币口径意外有折合值"

# ---------- ④ USD 口径 ----------
ST="$(get_bs 'period=2065-10&currency=USD' "$RUN_DIR/bs_usd.json")"
[[ "$ST" == "200" ]] && ok "USD 资产负债表 200" || bad "USD 资产负债表返回 $ST（$(err_msg "$RUN_DIR/bs_usd.json")）"
[[ "$(json_field "$RUN_DIR/bs_usd.json" "['currency']['mode']")" == "FOREIGN" ]] && ok "USD 口径 mode = FOREIGN" || bad "USD mode 不对"
[[ "$(json_field "$RUN_DIR/bs_usd.json" ".get('unit')")" == "USD" ]] && ok "USD 口径单位 = USD" || bad "USD 单位不对：$(json_field "$RUN_DIR/bs_usd.json" ".get('unit')")"
num_eq "USD 口径汇率（期间末日 7.1）" "$(json_field "$RUN_DIR/bs_usd.json" "['currency']['rate']")" 7.1
[[ "$(json_field "$RUN_DIR/bs_usd.json" "['currency']['entry_count']")" == "4" ]] \
    && ok "USD 本期分录 4 条（V2 两条 + V4 两条；草稿与本位币凭证不算）" || bad "entry_count = $(json_field "$RUN_DIR/bs_usd.json" "['currency']['entry_count']")"
num_eq "USD 1001 期末原币（200 − 30，草稿 999 / EUR 99 / HKD 70 都不进）" "$(line_amount "$RUN_DIR/bs_usd.json" MONETARY_FUNDS amount)" 170
num_eq "USD 1001 上期原币" "$(line_amount "$RUN_DIR/bs_usd.json" MONETARY_FUNDS previous_amount)" 200
num_eq "USD 1001 折合（170 × 7.1）" "$(line_amount "$RUN_DIR/bs_usd.json" MONETARY_FUNDS converted_amount)" 1207.0000
num_eq "USD 1122 期末原币（只算 USD 的 100，本位币凭证的 50 不进）" "$(line_amount "$RUN_DIR/bs_usd.json" ACCOUNTS_RECEIVABLE amount)" 100
num_eq "USD 2202 期末原币（上期 200）" "$(line_amount "$RUN_DIR/bs_usd.json" ACCOUNTS_PAYABLE amount)" 200
[[ "$(json_field "$RUN_DIR/bs_usd.json" "['check']['balanced']")" == "True" ]] \
    && ok "USD 口径勾稽平衡（外币凭证原币也平）" || bad "USD 口径不平：$(json_field "$RUN_DIR/bs_usd.json" "['check']['message']")"
num_eq "USD 资产总计" "$(json_field "$RUN_DIR/bs_usd.json" "['check']['assets_total']")" 270
num_eq "USD 负债+权益总计" "$(json_field "$RUN_DIR/bs_usd.json" "['check']['liability_equity_total']")" 270
python3 -c "
import json,sys
d=json.load(open('$RUN_DIR/bs_usd.json')).get('data') or {}
notes=' '.join(d.get('notes') or [])
sys.exit(0 if '原币口径' in notes else 1)
" && ok "USD 口径带口径附注（附注里有「原币口径」）" || bad "USD 口径没有口径附注"

# ---------- ⑤ EUR / HKD ----------
ST="$(get_bs 'period=2065-10&currency=EUR' "$RUN_DIR/bs_eur.json")"
[[ "$ST" == "200" ]] && ok "EUR 资产负债表 200" || bad "EUR 返回 $ST"
num_eq "EUR 1001 期末原币（只算 EUR 的 99）" "$(line_amount "$RUN_DIR/bs_eur.json" MONETARY_FUNDS amount)" 99
num_eq "EUR 折合（99 × 8.0）" "$(line_amount "$RUN_DIR/bs_eur.json" MONETARY_FUNDS converted_amount)" 792.0000

ST="$(get_bs 'period=2065-10&currency=HKD' "$RUN_DIR/bs_hkd.json")"
[[ "$ST" == "200" ]] && ok "HKD 资产负债表 200（缺牌价不该让报表打不开）" || bad "HKD 返回 $ST"
[[ "$(json_field "$RUN_DIR/bs_hkd.json" "['currency']['rate_missing']")" == "True" ]] && ok "HKD 缺期末牌价 ⇒ rate_missing = true" || bad "rate_missing 不对"
[[ "$(json_field "$RUN_DIR/bs_hkd.json" "['currency']['rate']")" == "None" ]] && ok "HKD rate = null（绝不回落 1）" || bad "rate 不是 null"
num_eq "HKD 1001 期末原币（9 月的 70 结转过来）" "$(line_amount "$RUN_DIR/bs_hkd.json" MONETARY_FUNDS amount)" 70
[[ "$(line_amount "$RUN_DIR/bs_hkd.json" MONETARY_FUNDS converted_amount)" == "None" ]] \
    && ok "HKD 折合列留空（绝不 70 × 1）" || bad "HKD 折合列不是 null"
python3 -c "
import json,sys
d=json.load(open('$RUN_DIR/bs_hkd.json')).get('data') or {}
notes=' '.join(d.get('notes') or [])
sys.exit(0 if '留空' in notes else 1)
" && ok "HKD 附注说明「折合列留空」" || bad "HKD 附注没说要留空"

# ---------- ⑥ 利润表 ----------
ST="$(get_is 'period=2065-10&currency=USD' "$RUN_DIR/is_usd.json")"
[[ "$ST" == "200" ]] && ok "USD 利润表 200" || bad "USD 利润表返回 $ST（$(err_msg "$RUN_DIR/is_usd.json")）"
num_eq "USD 营业收入（本期原币）" "$(item_amount "$RUN_DIR/is_usd.json" REVENUE current)" 100
num_eq "USD 营业收入（本年累计）" "$(item_amount "$RUN_DIR/is_usd.json" REVENUE ytd)" 100
num_eq "USD 营业收入折合（100 × 7.1）" "$(item_amount "$RUN_DIR/is_usd.json" REVENUE current_base)" 710.0000
num_eq "USD 主营业务成本（本期原币）" "$(item_amount "$RUN_DIR/is_usd.json" COST_OF_SALES current)" 30
num_eq "USD 净利润（100 − 30）" "$(item_amount "$RUN_DIR/is_usd.json" NET_PROFIT current)" 70
[[ "$(json_field "$RUN_DIR/is_usd.json" "['check']['balanced']")" == "True" ]] \
    && ok "USD 利润表勾稽通过（净利润 70 ≡ 未分配利润变动 70）" || bad "USD 利润表不平：$(json_field "$RUN_DIR/is_usd.json" "['check']['message']")"
num_eq "USD 未分配利润本年变动" "$(json_field "$RUN_DIR/is_usd.json" "['check']['equity_change']")" 70

ST="$(get_is 'period=2065-10' "$RUN_DIR/is_base.json")"
[[ "$ST" == "200" ]] && ok "本位币利润表 200" || bad "本位币利润表返回 $ST"
num_eq "本位币营业收入（710）" "$(item_amount "$RUN_DIR/is_base.json" REVENUE current)" 710
num_eq "本位币主营业务成本（213）" "$(item_amount "$RUN_DIR/is_base.json" COST_OF_SALES current)" 213
num_eq "本位币净利润（497）" "$(item_amount "$RUN_DIR/is_base.json" NET_PROFIT current)" 497
[[ "$(item_amount "$RUN_DIR/is_base.json" REVENUE current_base)" == "None" ]] \
    && ok "本位币利润表不出折合列" || bad "本位币利润表意外有折合值"

# ---------- ⑦ 入参口径 ----------
ST="$(get_bs 'period=2065-10&currency=CNY' "$RUN_DIR/bs_cny.json")"
[[ "$ST" == "200" && "$(json_field "$RUN_DIR/bs_cny.json" "['currency']['mode']")" == "BASE" ]] \
    && ok "传本位币 CNY ⇒ 200 + BASE（切回本位币不是错误）" || bad "传 CNY 返回 $ST"

ST="$(get_bs 'period=2065-10&currency=usd' "$RUN_DIR/bs_lower.json")"
[[ "$ST" == "200" && "$(json_field "$RUN_DIR/bs_lower.json" "['currency']['currency']")" == "USD" ]] \
    && ok "小写 usd ⇒ 规范成 USD（200）" || bad "小写币种返回 $ST"

ST="$(get_bs 'period=2065-10&currency=usd%24' "$RUN_DIR/bs_bad.json")"
[[ "$ST" == "400" && "$(err_code "$RUN_DIR/bs_bad.json")" == "10001" ]] \
    && ok "格式非法 usd\$ ⇒ 400 + 10001（不静默忽略）" || bad "非法格式返回 $ST / code $(err_code "$RUN_DIR/bs_bad.json")"

ST="$(get_bs 'period=2065-10&currency=XYZ' "$RUN_DIR/bs_unknown.json")"
[[ "$ST" == "400" && "$(err_code "$RUN_DIR/bs_unknown.json")" == "66084" ]] \
    && ok "未知币种 XYZ ⇒ 400 + 66084（指路「币种 / 汇率」）" || bad "未知币种返回 $ST / code $(err_code "$RUN_DIR/bs_unknown.json")"

ST="$(get_is 'period=2065-10&currency=XYZ' "$RUN_DIR/is_unknown.json")"
[[ "$ST" == "400" && "$(err_code "$RUN_DIR/is_unknown.json")" == "66084" ]] \
    && ok "利润表同样拦未知币种（400 + 66084）" || bad "利润表未知币种返回 $ST"

# ---------- ⑧ 与外币余额表（M1-07）对拍 ----------
ST="$(curl -sS -m 20 -o "$RUN_DIR/fxb.json" -w '%{http_code}' \
    "$BASE/business/erp/report/fx-balance?period=2065-10&currency=USD" "${AUTH[@]}")"
[[ "$ST" == "200" ]] && ok "外币余额表 200（对拍基准）" || bad "外币余额表返回 $ST"
CLOSING="$(python3 -c "
import json
d=json.load(open('$RUN_DIR/fxb.json')).get('data') or {}
for row in d.get('rows') or []:
    if row.get('account_code')=='1001' and row.get('currency_code')=='USD':
        print(row.get('closing_fc')); raise SystemExit
print('NO_ROW')
" 2>/dev/null)"
num_eq "外币余额表 (1001, USD) 期末原币（与报表口径同一把尺子）" "$CLOSING" 170

# ---------- ⑨ 导出 / 权限 ----------
ST="$(curl -sS -m 30 -o "$RUN_DIR/bs_usd.xlsx" -w '%{http_code}' \
    "$BASE/business/erp/report/balance-sheet/export?period=2065-10&currency=USD" "${AUTH[@]}")"
[[ "$ST" == "200" ]] && ok "USD 资产负债表导出 200" || bad "USD 导出返回 $ST（$(err_msg "$RUN_DIR/bs_usd.xlsx")）"
[[ "$(head -c 2 "$RUN_DIR/bs_usd.xlsx" 2>/dev/null)" == "PK" ]] && ok "导出件是真 zip（xlsx 魔数 PK）" || bad "导出件不是 xlsx"
[[ "$(stat -c %s "$RUN_DIR/bs_usd.xlsx" 2>/dev/null || echo 0)" -gt 3000 ]] && ok "导出件大小 > 3KB（不是空壳）" || bad "导出件太小"

ST="$(curl -sS -m 20 -o "$RUN_DIR/is_usd.xlsx" -w '%{http_code}' \
    "$BASE/business/erp/report/income-statement/export?period=2065-10&currency=USD" "${AUTH[@]}")"
[[ "$ST" == "200" && "$(head -c 2 "$RUN_DIR/is_usd.xlsx")" == "PK" ]] && ok "USD 利润表导出 200 + 真 xlsx" || bad "USD 利润表导出返回 $ST"

# 无角色账号 ⇒ 403（证明「能看报表」来自权限码，不是登录即可）
q_db "$DB_E2E" "insert into sys_user (username, password, must_change_password)
                select 'm109nobody', password, false from sys_user where username='admin'
                  and not exists (select 1 from sys_user u where u.username='m109nobody' and u.deleted_at is null)" >/dev/null
TOKEN_N="$(curl -sS -m 10 -X POST "$BASE/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"m109nobody\",\"password\":\"$ADMIN_PASSWORD\"}" \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print((d.get("data") or {}).get("access_token") or "")' 2>/dev/null)"
if [[ -n "${TOKEN_N:-}" ]]; then
    ST="$(curl -sS -m 10 -o "$RUN_DIR/forbidden.json" -w '%{http_code}' \
        "$BASE/business/erp/report/balance-sheet?period=2065-10&currency=USD" -H "Authorization: Bearer $TOKEN_N")"
    [[ "$ST" == "403" ]] && ok "无码账号看 USD 报表 ⇒ 403" || bad "无码账号返回 $ST（期望 403）"
else
    bad "无码账号登录失败（403 用例没跑成）"
fi

# ============================================================================
echo
echo "== 汇总：PASS=$PASS FAIL=$FAIL =="
[[ "$FAIL" -eq 0 ]] && { echo "ALL GREEN"; exit 0; } || exit 1
