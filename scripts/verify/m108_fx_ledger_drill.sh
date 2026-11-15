#!/usr/bin/env bash
# =============================================================================
# m108_fx_ledger_drill.sh —— V0413（M1-08 外币明细账）真引擎 + 真 HTTP
#
# 卡：t_e2797b07。**不是** psql 手动跑 SQL，而是用编译出的后端二进制真启动，
# 走引擎自己的迁移器（schema_migrations 指纹 / 幂等 / 顺序）与真 HTTP 路由。
#
# 三段：
#   A) 克隆库升级：模板库 cloudpivot_test（停版 V0404）克隆成 cp_m108_upgrade → 真启动
#      → 应用 V0405..V0413 → 断言 V0413 自检 NOTICE / schema_migrations 登记 / 菜单页面行 /
#      FINANCE_MANAGER 授权 / 重启 0 个迁移
#   B) 空库首启：空库 cp_m108_e2e → 真启动 → 全量迁移（含 V0413 自检块）
#   C) 真 HTTP（外币明细账的全部口径）：
#      ① 外币核算标记走 M1-03 接口（不许用 SQL 绕过）
#      ② 两张 USD 外币凭证 + 一张纯本位币凭证 + 一张 EUR 凭证 + 一张草稿（HTTP 建单 → 审核 → 记账）
#      ③ 明细账数字：期初 / 本期 / 期末 / 按科目汇总 / 本位币金额 / 逐行余额（按科目累计）
#      ④ 分页：第 2 页的逐行余额必须接上第 1 页（含期初）
#      ⑤ 与外币余额表（M1-07）按科目对拍：期末原币必须相等
#      ⑥ 币种过滤 / 本位币 400 / 非法币种码 400 / 区间反了 400 / 科目 404 / 草稿不进表
#      ⑦ 只读账号（无 fx-ledger 码）⇒ 403
#
# 用法：bash scripts/verify/m108_fx_ledger_drill.sh [PG_HOST]
#   默认 PG_HOST=172.18.10.145（99 上跑）；145 上跑传 127.0.0.1。
# 端口固定 18098（隔离段 18000-18999），库名含 e2e/upgrade，脚本自己 DROP。
# =============================================================================
set -uo pipefail

PG_HOST="${1:-172.18.10.145}"
export PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4
PG_USER=cloudpivot
TEMPLATE=cloudpivot_test
DB_UP=cp_m108_upgrade
DB_E2E=cp_m108_e2e
PORT=18098
BASE="http://127.0.0.1:$PORT/api"
REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
BACKEND_DIR="$REPO_ROOT/backend"
BIN="$BACKEND_DIR/target/debug/cloudpivot"
RUN_DIR="${M108_RUN_DIR:-/root/.hermes/profiles/backend-developer/cache/scratch/m108-run}"
NODE_ID='20000000-0000-0000-0000-000000001760'
PAGE_PARENT='20000000-0000-0000-0000-000000000e02'
CODE='business:erp:report:fx-ledger'
PATH_X='/business/erp/report/fx-ledger'
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

ADMIN_PASSWORD='M108@Drill#2026'

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
        export X_CONFIG_SECRET_KEY="m108-drill-config-secret"
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
echo "== A) 克隆库升级（真引擎：模板库 → cp_m108_upgrade）=="
psql_admin "DROP DATABASE IF EXISTS $DB_UP" >/dev/null 2>&1
psql_admin "DROP DATABASE IF EXISTS $DB_E2E" >/dev/null 2>&1
if ! psql_admin "CREATE DATABASE $DB_UP TEMPLATE $TEMPLATE" >/dev/null 2>&1; then
    echo "  克隆模板库失败（模板可能被别的会话占用），5 秒后重试一次"
    sleep 5
    psql_admin "CREATE DATABASE $DB_UP TEMPLATE $TEMPLATE" >/dev/null || exit 2
fi
BEFORE_CNT="$(q_db "$DB_UP" "select count(*) from schema_migrations where kind='versioned'")"
BEFORE_MAX="$(q_db "$DB_UP" "select coalesce(max(version::int),0) from schema_migrations where kind='versioned'")"
echo "  升级前：versioned $BEFORE_CNT 条，最大 version=$BEFORE_MAX（模板停在 V$BEFORE_MAX）"

start_backend "$DB_UP" "$RUN_DIR/upgrade.log" || { echo "FATAL: 升级实例没起来"; exit 2; }
ok "克隆库真启动成功（/health 200）"
APPLIED="$(grep -oE '数据库迁移完成: 应用 [0-9]+ 个迁移' "$RUN_DIR/upgrade.log" | head -1)"
echo "  ${APPLIED}"
APPLIED_N="$(echo "$APPLIED" | grep -oE '[0-9]+' | head -1)"
[[ "${APPLIED_N:-0}" -ge 1 ]] && ok "应用了 $APPLIED_N 个迁移（含 V0413）" || bad "一个迁移都没应用 —— V0413 没被引擎识别"
grep -q "V0413 自检通过" "$RUN_DIR/upgrade.log" && ok "V0413 自检块在真引擎里通过（NOTICE 已打印）" || bad "没看到 V0413 自检 NOTICE"
ROW="$(q_db "$DB_UP" "select version||'/'||success||'/'||source from schema_migrations where name='fx_ledger_report'")"
echo "  schema_migrations：$ROW"
case "$ROW" in
    413/true/*V0413__fx_ledger_report.up.sql) ok "schema_migrations 登记 413/true/源文件" ;;
    *) bad "schema_migrations 登记不对：[$ROW]" ;;
esac
NODE="$(q_db "$DB_UP" "select menu_type||'/'||coalesce(parent_id::text,'-')||'/'||sort_order||'/'||status||'/'||title||'/'||coalesce(path,'-')
        from sys_menu where auth_code='$CODE' and deleted_at is null")"
echo "  权限节点：$NODE"
[[ "$NODE" == "2/$PAGE_PARENT/51/1/外币明细账/$PATH_X" ]] \
    && ok "菜单页面行：type=2 / 父=财务管理 / sort=51 / 启用 / title=外币明细账 / path 对" || bad "权限节点不对：[$NODE]"
GRANTS="$(q_db "$DB_UP" "select count(*) from sys_role_menu rm join sys_role r on r.id=rm.role_id and r.deleted_at is null
        where rm.menu_id='$NODE_ID'::uuid and r.role_code in ('SUPER_ADMIN','FINANCE_MANAGER')")"
[[ "${GRANTS:-0}" -ge 2 ]] && ok "授权已落地：$GRANTS 个目标角色（SUPER_ADMIN / FINANCE_MANAGER）" || bad "授权只到 ${GRANTS:-0} 行"

stop_backend
start_backend "$DB_UP" "$RUN_DIR/upgrade-restart.log" || { echo "FATAL: 重启失败"; exit 2; }
APPLIED2="$(grep -oE '数据库迁移完成: 应用 [0-9]+ 个迁移' "$RUN_DIR/upgrade-restart.log" | head -1)"
case "$APPLIED2" in
    *"应用 0 个迁移"*) ok "重启：应用 0 个迁移（引擎级幂等）" ;;
    *) bad "重启又应用了迁移：[$APPLIED2]" ;;
esac
AFTER_CNT="$(q_db "$DB_UP" "select count(*) from schema_migrations where kind='versioned'")"
[[ "$AFTER_CNT" == "$((BEFORE_CNT + ${APPLIED_N:-0}))" ]] && ok "versioned 迁移条数 $BEFORE_CNT → $AFTER_CNT（+$APPLIED_N，与日志一致）" || bad "versioned 条数异常：$BEFORE_CNT → $AFTER_CNT（日志说 +$APPLIED_N）"
stop_backend

# ============================================================================
echo
echo "== B) 空库首启（全量迁移 + V0413 自检；真 HTTP 的落点）=="
psql_admin "CREATE DATABASE $DB_E2E" >/dev/null || { echo "FATAL: 建空库失败"; exit 2; }
start_backend "$DB_E2E" "$RUN_DIR/e2e.log" || { echo "FATAL: 空库实例没起来"; tail -30 "$RUN_DIR/e2e.log"; exit 2; }
APPLIED3="$(grep -oE '数据库迁移完成: 应用 [0-9]+ 个迁移' "$RUN_DIR/e2e.log" | head -1)"
echo "  ${APPLIED3}"
CNT3="$(q_db "$DB_E2E" "select count(*) from schema_migrations where kind='versioned'")"
[[ "${CNT3:-0}" -ge 332 ]] && ok "空库首启 versioned 迁移 = $CNT3（≥332：V0413 至少是第 332 条）" || bad "空库首启 versioned = $CNT3（期望 ≥332）"
grep -q "V0413 自检通过" "$RUN_DIR/e2e.log" && ok "空库首启也跑了 V0413 自检" || bad "空库首启没有 V0413 自检 NOTICE"
curl -fsS -m 3 "http://127.0.0.1:$PORT/health" >/dev/null && ok "/health 200" || bad "/health 不通"

q_db "$DB_E2E" "update sys_user set must_change_password = false where username = 'admin'" >/dev/null
[[ "$(q_db "$DB_E2E" "select must_change_password from sys_user where username='admin'")" == "f" ]] \
    && ok "演练库：清掉 admin 的 must_change_password（仅为跑通本卡接口）" || bad "清 must_change_password 失败"

# ============================================================================
echo
echo "== C) 真 HTTP：外币明细账 =="
TOKEN="$(curl -sS -m 10 -X POST "$BASE/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"admin\",\"password\":\"$ADMIN_PASSWORD\"}" \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print((d.get("data") or {}).get("access_token") or "")' 2>/dev/null)"
[[ -n "${TOKEN:-}" ]] && ok "登录成功（access_token ${#TOKEN} 字符）" || { bad "登录失败"; stop_backend; exit 2; }
AUTH=(-H "Authorization: Bearer $TOKEN")

# 内控：`erp.separation_of_duties.approve` 开着时**制单人不能审自己的凭证**（403 + 65019）。
# 演练保持内控开着（关开关验证等于绕过真实现），所以再造一个持 SUPER_ADMIN 的复核账号：
# 制单用 admin、审核 + 记账用 checker。
q_db "$DB_E2E" "insert into sys_user (username, password, must_change_password)
                select 'm108checker', password, false from sys_user where username='admin'
                  and not exists (select 1 from sys_user u where u.username='m108checker' and u.deleted_at is null)" >/dev/null
q_db "$DB_E2E" "insert into sys_user_role (user_id, role_id)
                select u.id, r.id from sys_user u, sys_role r
                 where u.username='m108checker' and u.deleted_at is null
                   and r.role_code='SUPER_ADMIN' and r.deleted_at is null
                   and not exists (select 1 from sys_user_role ur where ur.user_id=u.id and ur.role_id=r.id)" >/dev/null
TOKEN_C="$(curl -sS -m 10 -X POST "$BASE/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"m108checker\",\"password\":\"$ADMIN_PASSWORD\"}" \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print((d.get("data") or {}).get("access_token") or "")' 2>/dev/null)"
if [[ -n "${TOKEN_C:-}" ]]; then
    ok "复核账号 m108checker 登录成功（持 SUPER_ADMIN，内控开关保持开启）"
else
    bad "复核账号登录失败（无法走「制单 ≠ 审核」这条真实路径）"
fi
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

# ---------- ① 夹具：期间 / 科目 / 汇率（主数据），外币标记走 M1-03 接口 ----------
# 期间命名空间 2063-xx 归本脚本独占（不与 2050/2051/2059/2060/2061/2026 等冲突）
q_db "$DB_E2E" "delete from biz_exchange_rate where from_currency in ('USD','EUR','HKD')" >/dev/null
q_db "$DB_E2E" "insert into biz_accounting_period (period_year, period_month, period_code, start_date, end_date, status)
    values (2063,9,'2063-09','2063-09-01','2063-09-30','OPEN'),
           (2063,10,'2063-10','2063-10-01','2063-10-31','OPEN')
    on conflict (period_code) do update set status='OPEN', closed_at=null, closed_by=null" >/dev/null
q_db "$DB_E2E" "insert into biz_account (account_code, account_name, level, account_category, balance_direction, is_leaf, is_foreign_currency)
    values ('FXL01','外币资产',1,'ASSET','DEBIT',1,0),
           ('FXL02','外币对方',1,'ASSET','DEBIT',1,0),
           ('FXL03','本位币科目',1,'ASSET','DEBIT',1,0),
           ('FXL04','欧元资产',1,'ASSET','DEBIT',1,0),
           ('FXL05','欧元对方',1,'ASSET','DEBIT',1,0)
    on conflict (account_code) do update set is_leaf=1, is_foreign_currency=0" >/dev/null
ACC1="$(q_db "$DB_E2E" "select id from biz_account where account_code='FXL01'")"
ACC2="$(q_db "$DB_E2E" "select id from biz_account where account_code='FXL02'")"
ACC3="$(q_db "$DB_E2E" "select id from biz_account where account_code='FXL03'")"
ACC4="$(q_db "$DB_E2E" "select id from biz_account where account_code='FXL04'")"
ACC5="$(q_db "$DB_E2E" "select id from biz_account where account_code='FXL05'")"
[[ -n "$ACC1" && -n "$ACC2" && -n "$ACC3" && -n "$ACC4" && -n "$ACC5" ]] && ok "夹具：5 个科目 + 两个期间（2063-09 / 2063-10）就位" || { bad "夹具科目没建起来"; stop_backend; exit 2; }

mark_fx() { # mark_fx <account_id> <true|false> <out>
    curl -sS -m 10 -o "$3" -w '%{http_code}' -X PUT "$BASE/business/erp/account/$1/foreign-currency" \
        "${AUTH[@]}" -H 'Content-Type: application/json' -d "{\"is_foreign_currency\":$2}"
}
STATUS="$(mark_fx "$ACC1" true "$RUN_DIR/mark1.json")"
[[ "$STATUS" == "200" ]] && ok "M1-03 接口标记科目 FXL01 为外币核算 ⇒ 200" || bad "标记 FXL01 返回 $STATUS（$(err_msg "$RUN_DIR/mark1.json")）"
STATUS="$(mark_fx "$ACC2" true "$RUN_DIR/mark2.json")"
[[ "$STATUS" == "200" ]] && ok "M1-03 接口标记科目 FXL02 为外币核算 ⇒ 200" || bad "标记 FXL02 返回 $STATUS"
STATUS="$(mark_fx "$ACC4" true "$RUN_DIR/mark4.json")"
[[ "$STATUS" == "200" ]] && ok "M1-03 接口标记科目 FXL04 为外币核算 ⇒ 200" || bad "标记 FXL04 返回 $STATUS"
STATUS="$(mark_fx "$ACC5" true "$RUN_DIR/mark5.json")"
[[ "$STATUS" == "200" ]] && ok "M1-03 接口标记科目 FXL05 为外币核算 ⇒ 200" || bad "标记 FXL05 返回 $STATUS"
[[ "$(q_db "$DB_E2E" "select count(*) from biz_account where account_code like 'FXL%' and is_foreign_currency=1")" == "4" ]] \
    && ok "库里 is_foreign_currency 真的落成 4 个（不是只改了返回体）" || bad "库里标记没落地"

# 汇率：USD 9/1=7.0 / 10/15=7.2 / 10/31=7.9 / 11/05=8.5（更晚，不该被采用）；EUR 只有 9/1=8.0
# ⚠️ V0407 之后汇率表带**区间不重叠**排除约束：直接 INSERT 的 `effective_to` 默认 NULL（= 无限），
# 先插早行会让后面的行全部冲突。所以每条都显式写 `effective_to`（= 同组下一条生效日 − 1，末行 NULL）。
seed_rate() { # seed_rate <currency> <date> <rate> <effective_to|NULL>
    q_db "$DB_E2E" "insert into biz_exchange_rate
            (from_currency, to_currency, rate_type, effective_date, effective_to, rate)
        values ('$1','CNY','SPOT','$2'::date, $4::date, '$3'::numeric)
        on conflict (from_currency, to_currency, rate_type, effective_date)
        do update set rate=excluded.rate, effective_to=excluded.effective_to" >/dev/null
}
seed_rate USD 2063-09-01 7.0 "'2063-10-14'"
seed_rate USD 2063-10-15 7.2 "'2063-10-30'"
seed_rate USD 2063-10-31 7.9 "'2063-11-04'"
seed_rate USD 2063-11-05 8.5 NULL
seed_rate EUR 2063-09-01 8.0 "'2063-09-30'"
ok "夹具：USD 四条牌价 + EUR 一条"

# ---------- ② 五张凭证：建单 → 审核 → 记账 ----------
create_voucher() { # create_voucher <file> <json>
    curl -sS -m 15 -o "$1" -w '%{http_code}' -X POST "$BASE/business/erp/voucher" \
        "${AUTH[@]}" -H 'Content-Type: application/json' -d "$2"
}
audit_post() { # audit_post <voucher_id> <label>   —— 用复核账号（制单 ≠ 审核）
    local vid="$1" label="$2"
    local st
    st="$(curl -sS -m 10 -o "$RUN_DIR/audit.json" -w '%{http_code}' -X POST "$BASE/business/erp/voucher/$vid/audit" \
        "${AUTH_C[@]}" -H 'Content-Type: application/json' -d '{}')"
    [[ "$st" == "200" ]] || { bad "$label 审核返回 $st（$(err_msg "$RUN_DIR/audit.json")）"; return 1; }
    st="$(curl -sS -m 10 -o "$RUN_DIR/post.json" -w '%{http_code}' -X POST "$BASE/business/erp/voucher/$vid/post" \
        "${AUTH_C[@]}" -H 'Content-Type: application/json' -d '{}')"
    [[ "$st" == "200" ]] || { bad "$label 记账返回 $st（$(err_msg "$RUN_DIR/post.json")）"; return 1; }
    ok "$label 建单(admin) → 审核(checker) → 记账(checker) 全通"
}

# V1：期初（2063-09-20）200 USD @ 7.0
STATUS="$(create_voucher "$RUN_DIR/v1.json" "{
  \"voucher_date\":\"2063-09-20\",\"summary\":\"M108 期初外币\",
  \"currency_code\":\"USD\",
  \"entries\":[{\"account_id\":\"$ACC1\",\"debit\":200},{\"account_id\":\"$ACC2\",\"credit\":200}]}")"
[[ "$STATUS" == "200" ]] || { bad "V1 建单失败 $STATUS（$(err_msg "$RUN_DIR/v1.json")）"; stop_backend; exit 2; }
ok "V1 建单成功（200 USD @ 当日牌价 7.0）"
V1="$(json_field "$RUN_DIR/v1.json" ".get('id')")"
audit_post "$V1" "V1（期初 200 USD）"

# V2：本期（2063-10-20，落在 10-15..10-30 的 7.2 区间）100 USD @ 7.2
STATUS="$(create_voucher "$RUN_DIR/v2.json" "{
  \"voucher_date\":\"2063-10-20\",\"summary\":\"M108 本期外币\",
  \"currency_code\":\"USD\",
  \"entries\":[{\"account_id\":\"$ACC1\",\"debit\":100},{\"account_id\":\"$ACC2\",\"credit\":100}]}")"
[[ "$STATUS" == "200" ]] || { bad "V2 建单失败 $STATUS（$(err_msg "$RUN_DIR/v2.json")）"; stop_backend; exit 2; }
V2="$(json_field "$RUN_DIR/v2.json" ".get('id')")"
audit_post "$V2" "V2（本期 100 USD @ 7.2）"

# V3：本期纯本位币 50（只改本位币账面，不是外币凭证 ⇒ 不许进外币明细账）
STATUS="$(create_voucher "$RUN_DIR/v3.json" "{
  \"voucher_date\":\"2063-10-22\",\"summary\":\"M108 本位币调整\",
  \"entries\":[{\"account_id\":\"$ACC2\",\"debit\":50},{\"account_id\":\"$ACC3\",\"credit\":50}]}")"
[[ "$STATUS" == "200" ]] || { bad "V3 建单失败 $STATUS（$(err_msg "$RUN_DIR/v3.json")）"; stop_backend; exit 2; }
V3="$(json_field "$RUN_DIR/v3.json" ".get('id')")"
audit_post "$V3" "V3（纯本位币 50，验证本位币凭证不进外币明细账）"

# V4：EUR 100（挂在独立的欧元科目上；用来验币种过滤）
STATUS="$(create_voucher "$RUN_DIR/v4.json" "{
  \"voucher_date\":\"2063-09-25\",\"summary\":\"M108 欧元期初\",
  \"currency_code\":\"EUR\",
  \"entries\":[{\"account_id\":\"$ACC4\",\"debit\":100},{\"account_id\":\"$ACC5\",\"credit\":100}]}")"
[[ "$STATUS" == "200" ]] || { bad "V4 建单失败 $STATUS（$(err_msg "$RUN_DIR/v4.json")）"; stop_backend; exit 2; }
V4="$(json_field "$RUN_DIR/v4.json" ".get('id')")"
audit_post "$V4" "V4（EUR 100）"

# V5：草稿 999 USD（不审核不记账 → 不许进表）
STATUS="$(create_voucher "$RUN_DIR/v5.json" "{
  \"voucher_date\":\"2063-10-25\",\"summary\":\"M108 草稿\",
  \"currency_code\":\"USD\",
  \"entries\":[{\"account_id\":\"$ACC1\",\"debit\":999},{\"account_id\":\"$ACC2\",\"credit\":999}]}")"
[[ "$STATUS" == "200" ]] && ok "V5 草稿建单成功（故意不审核不记账）" || bad "V5 建单失败 $STATUS"

# ---------- ③ 明细账主用例（currency=USD, 2063-10） ----------
get_ledger() { # get_ledger <query> <out>
    curl -sS -m 15 -o "$2" -w '%{http_code}' "$BASE/business/erp/report/fx-ledger?$1" "${AUTH[@]}"
}
acc_field() { # acc_field <file> <account_code> <field>
    python3 -c "
import json,sys
d=json.load(open('$1'))
for r in (d.get('data') or {}).get('accounts') or []:
    if r.get('account_code')=='$2':
        print(r.get('$3'))
" 2>/dev/null
}
row_field() { # row_field <file> <index> <field>
    python3 -c "
import json,sys
d=json.load(open('$1'))
rows=(d.get('data') or {}).get('rows') or []
print(rows[$2].get('$3') if len(rows)>$2 else 'NO_ROW')
" 2>/dev/null
}
num_eq() { # num_eq <label> <got> <expected>
    python3 -c "
import sys
from decimal import Decimal
got=sys.argv[1]; want=Decimal(sys.argv[2])
sys.exit(0 if got not in ('', 'None', 'NO_ROW') and Decimal(str(got))==want else 1)
" "$2" "$3" && ok "$1 = $3" || bad "$1 ≠ $3（实际 [$2]）"
}

STATUS="$(get_ledger "currency=USD&period_from=2063-10&period_to=2063-10" "$RUN_DIR/l1.json")"
echo "  明细账片段：$(head -c 300 "$RUN_DIR/l1.json")"
[[ "$STATUS" == "200" ]] && ok "GET /business/erp/report/fx-ledger?currency=USD&period_from=2063-10&period_to=2063-10 ⇒ 200" || bad "明细账返回 $STATUS（$(err_msg "$RUN_DIR/l1.json")）"
[[ "$(json_field "$RUN_DIR/l1.json" ".get('report_code')")" == "FX_LEDGER" ]] && ok "report_code = FX_LEDGER" || bad "report_code 不对"
[[ "$(json_field "$RUN_DIR/l1.json" ".get('report_name')")" == "外币明细账" ]] && ok "report_name = 外币明细账" || bad "report_name 不对"
[[ "$(json_field "$RUN_DIR/l1.json" ".get('base_currency')")" == "CNY" ]] && ok "base_currency = CNY（账套本位币）" || bad "base_currency 不对"
[[ "$(json_field "$RUN_DIR/l1.json" ".get('currency')")" == "USD" ]] && ok "currency = USD" || bad "currency 不对"
[[ "$(json_field "$RUN_DIR/l1.json" ".get('currency_name')")" == "美元" ]] && ok "currency_name = 美元" || bad "currency_name 不对"
[[ "$(json_field "$RUN_DIR/l1.json" ".get('date_from')")" == "2063-10-01" ]] && ok "date_from = 2063-10-01（yyyy-MM 取月首）" || bad "date_from 不对：$(json_field "$RUN_DIR/l1.json" ".get('date_from')")"
[[ "$(json_field "$RUN_DIR/l1.json" ".get('date_to')")" == "2063-10-31" ]] && ok "date_to = 2063-10-31（yyyy-MM 取月末）" || bad "date_to 不对"

[[ "$(json_field "$RUN_DIR/l1.json" "['accounts'].__len__()")" == "2" ]] && ok "按科目汇总 2 行（两个外币科目）" || bad "accounts 行数 = $(json_field "$RUN_DIR/l1.json" "['accounts'].__len__()")"
[[ "$(json_field "$RUN_DIR/l1.json" "['rows'].__len__()")" == "2" ]] && ok "本期明细 2 行（V2 的两条分录）" || bad "rows 行数 = $(json_field "$RUN_DIR/l1.json" "['rows'].__len__()")"
[[ "$(json_field "$RUN_DIR/l1.json" ".get('total')")" == "2" ]] && ok "total = 2" || bad "total 不对"
[[ "$(json_field "$RUN_DIR/l1.json" ".get('entry_count')")" == "2" ]] && ok "entry_count = 2" || bad "entry_count 不对"

# 资产科目：期初 200 / 本期借 100 / 期末 300 / 本位币借 720
num_eq "FXL01 期初原币" "$(acc_field "$RUN_DIR/l1.json" FXL01 opening_fc)" 200
num_eq "FXL01 本期原币借" "$(acc_field "$RUN_DIR/l1.json" FXL01 period_fc_debit)" 100
num_eq "FXL01 本期原币贷" "$(acc_field "$RUN_DIR/l1.json" FXL01 period_fc_credit)" 0
num_eq "FXL01 期末原币" "$(acc_field "$RUN_DIR/l1.json" FXL01 closing_fc)" 300
num_eq "FXL01 本位币借" "$(acc_field "$RUN_DIR/l1.json" FXL01 period_book_debit)" 720
[[ "$(acc_field "$RUN_DIR/l1.json" FXL01 closing_side)" == "DEBIT" ]] && ok "FXL01 期末方向 = DEBIT" || bad "FXL01 closing_side 不对"
[[ "$(acc_field "$RUN_DIR/l1.json" FXL01 entry_count)" == "1" ]] && ok "FXL01 本期条数 = 1" || bad "FXL01 entry_count 不对"

# 对方科目：期初 −200 / 本期贷 100 / 期末 −300 / 本位币贷**只有** 720（那笔本位币 50 不算）
num_eq "FXL02 期初原币" "$(acc_field "$RUN_DIR/l1.json" FXL02 opening_fc)" -200
num_eq "FXL02 本期原币贷" "$(acc_field "$RUN_DIR/l1.json" FXL02 period_fc_credit)" 100
num_eq "FXL02 期末原币" "$(acc_field "$RUN_DIR/l1.json" FXL02 closing_fc)" -300
num_eq "FXL02 本位币贷（不含本位币凭证的 50）" "$(acc_field "$RUN_DIR/l1.json" FXL02 period_book_credit)" 720
[[ "$(acc_field "$RUN_DIR/l1.json" FXL02 closing_side)" == "CREDIT" ]] && ok "FXL02 期末方向 = CREDIT" || bad "FXL02 closing_side 不对"

# 合计：期初 0（借贷平衡）/ 借 100 / 贷 100 / 期末 0（美元净头寸为零）
num_eq "期初原币合计" "$(json_field "$RUN_DIR/l1.json" ".get('opening_fc')")" 0
num_eq "本期原币借合计" "$(json_field "$RUN_DIR/l1.json" ".get('period_fc_debit')")" 100
num_eq "本期原币贷合计" "$(json_field "$RUN_DIR/l1.json" ".get('period_fc_credit')")" 100
num_eq "期末原币合计" "$(json_field "$RUN_DIR/l1.json" ".get('closing_fc')")" 0
[[ "$(json_field "$RUN_DIR/l1.json" ".get('closing_side')")" == "ZERO" ]] && ok "期末合计方向 = ZERO（平）" || bad "closing_side 不对"
num_eq "本位币借合计" "$(json_field "$RUN_DIR/l1.json" ".get('period_book_debit')")" 720
num_eq "本位币净额合计" "$(json_field "$RUN_DIR/l1.json" ".get('period_book_net')")" 0

# 明细行：科目 / 汇率 / 原币 / 本位币 / 逐行余额（含期初）
[[ "$(row_field "$RUN_DIR/l1.json" 0 account_code)" == "FXL01" ]] && ok "第 1 行科目 = FXL01（按科目编码排序）" || bad "第 1 行科目不对：$(row_field "$RUN_DIR/l1.json" 0 account_code)"
num_eq "第 1 行原币借" "$(row_field "$RUN_DIR/l1.json" 0 fc_debit)" 100
num_eq "第 1 行本位币借" "$(row_field "$RUN_DIR/l1.json" 0 debit_amount)" 720
num_eq "第 1 行汇率（建单锁定）" "$(row_field "$RUN_DIR/l1.json" 0 exchange_rate)" 7.2
num_eq "第 1 行逐行余额（含该科目期初 200）" "$(row_field "$RUN_DIR/l1.json" 0 running_fc_balance)" 300
[[ "$(row_field "$RUN_DIR/l1.json" 0 running_side)" == "DEBIT" ]] && ok "第 1 行余额方向 = DEBIT" || bad "第 1 行 running_side 不对"
[[ "$(row_field "$RUN_DIR/l1.json" 1 account_code)" == "FXL02" ]] && ok "第 2 行科目 = FXL02" || bad "第 2 行科目不对"
num_eq "第 2 行逐行余额（从自己的 −200 起算，不串第 1 行）" "$(row_field "$RUN_DIR/l1.json" 1 running_fc_balance)" -300
[[ "$(row_field "$RUN_DIR/l1.json" 1 running_side)" == "CREDIT" ]] && ok "第 2 行余额方向 = CREDIT" || bad "第 2 行 running_side 不对"

# 草稿不进表：若进了，FXL01 的期末会变成 1299
python3 -c "
import json,sys
from decimal import Decimal
d=json.load(open('$RUN_DIR/l1.json')).get('data') or {}
for r in d.get('accounts') or []:
    if r.get('account_code')=='FXL01':
        sys.exit(0 if Decimal(str(r['closing_fc']))==Decimal('300') else 1)
sys.exit(1)
" && ok "草稿凭证（999 USD）不进明细账" || bad "草稿凭证进了明细账"

# 干净夹具（每个科目只挂一个币种、没有外币分录落在未标记科目）⇒ warnings 必须为空
python3 -c "
import json,sys
d=json.load(open('$RUN_DIR/l1.json')).get('data') or {}
# 请求失败时 data 为空、warnings 也「为空」⇒ 空表假绿；先要求成功响应
if d.get('report_code')!='FX_LEDGER': print('ERR 响应不是明细账（请求失败）'); sys.exit(1)
sys.exit(0 if not (d.get('warnings') or []) else 1)
" && ok "干净数据没有 warnings" || bad "干净数据却报了 warnings：$(json_field "$RUN_DIR/l1.json" ".get('warnings')")"
python3 -c "
import json,sys
d=json.load(open('$RUN_DIR/l1.json')).get('data') or {}
sys.exit(0 if any('按科目分别累计' in h for h in d.get('hints') or []) else 1)
" && ok "hints 写清「逐行余额按科目分别累计」" || bad "hints 没写余额累计口径"

# ---------- ④ 分页：第 2 页的余额必须接上第 1 页 ----------
STATUS="$(get_ledger "currency=USD&page=1&page_size=2" "$RUN_DIR/p1.json")"
[[ "$STATUS" == "200" ]] && ok "全区间第 1 页（page_size=2）⇒ 200" || bad "第 1 页返回 $STATUS"
[[ "$(json_field "$RUN_DIR/p1.json" ".get('total')")" == "4" ]] && ok "全区间 total = 4（期初 2 行 + 本期 2 行）" || bad "全区间 total = $(json_field "$RUN_DIR/p1.json" ".get('total')")"
num_eq "第 1 页第 1 行余额（FXL01 期初行）" "$(row_field "$RUN_DIR/p1.json" 0 running_fc_balance)" 200
num_eq "第 1 页第 2 行余额（同科目累加 +100）" "$(row_field "$RUN_DIR/p1.json" 1 running_fc_balance)" 300
STATUS="$(get_ledger "currency=USD&page=2&page_size=2" "$RUN_DIR/p2.json")"
[[ "$STATUS" == "200" ]] && ok "全区间第 2 页 ⇒ 200" || bad "第 2 页返回 $STATUS"
num_eq "第 2 页第 1 行余额（对方科目从自己的 0 起算）" "$(row_field "$RUN_DIR/p2.json" 0 running_fc_balance)" -200
num_eq "第 2 页第 2 行余额（同科目累加 −100）" "$(row_field "$RUN_DIR/p2.json" 1 running_fc_balance)" -300
STATUS="$(get_ledger "currency=USD&page=3&page_size=2" "$RUN_DIR/p3.json")"
[[ "$STATUS" == "200" && "$(json_field "$RUN_DIR/p3.json" "['rows'].__len__()")" == "0" ]] \
    && ok "越界页返回空 list（不报错）" || bad "越界页返回 $STATUS / $(json_field "$RUN_DIR/p3.json" "['rows'].__len__()")"

# ---------- ⑤ 与外币余额表（M1-07）按科目对拍 ----------
STATUS="$(curl -sS -m 15 -o "$RUN_DIR/b1.json" -w '%{http_code}' "$BASE/business/erp/report/fx-balance?period=2063-10&currency=USD" "${AUTH[@]}")"
[[ "$STATUS" == "200" ]] && ok "外币余额表（对照）⇒ 200" || bad "外币余额表返回 $STATUS"
python3 -c "
import json,sys
from decimal import Decimal
led=json.load(open('$RUN_DIR/l1.json')).get('data') or {}
bal=json.load(open('$RUN_DIR/b1.json')).get('data') or {}
accs=led.get('accounts') or []
if len(accs)!=2: print('明细账 accounts 不是 2 行（请求可能失败）'); sys.exit(1)
balmap={(r.get('account_code'), r.get('currency_code')): r for r in bal.get('rows') or []}
for r in accs:
    key=(r.get('account_code'), 'USD')
    if key not in balmap:
        print('余额表缺科目', key); sys.exit(1)
    if Decimal(str(r['closing_fc']))!=Decimal(str(balmap[key]['closing_fc'])):
        print('期末原币不一致', key, r['closing_fc'], balmap[key]['closing_fc']); sys.exit(1)
    if Decimal(str(r['opening_fc']))!=Decimal(str(balmap[key]['opening_fc'])):
        print('期初原币不一致', key, r['opening_fc'], balmap[key]['opening_fc']); sys.exit(1)
sys.exit(0)
" && ok "与外币余额表逐科目对拍：期初 / 期末原币全部相等（同口径）" || bad "与外币余额表对不上（有一边改了 WHERE 或 COALESCE）"

# ---------- ⑥ 币种过滤 / 科目过滤 / 非法入参 ----------
STATUS="$(get_ledger "currency=EUR" "$RUN_DIR/e1.json")"
[[ "$STATUS" == "200" ]] && ok "currency=EUR ⇒ 200" || bad "EUR 返回 $STATUS"
[[ "$(json_field "$RUN_DIR/e1.json" "['accounts'].__len__()")" == "2" ]] && ok "EUR 按科目汇总 2 行" || bad "EUR accounts = $(json_field "$RUN_DIR/e1.json" "['accounts'].__len__()")"
python3 -c "
import json,sys
d=json.load(open('$RUN_DIR/e1.json')).get('data') or {}
rows=d.get('rows') or []
# 500 时 data 里没有 rows ⇒ 空列表会「假绿」，必须要求非空且逐行币种一致
if not rows: print('ERR 没有明细行（可能不是 200）'); sys.exit(1)
sys.exit(0 if all(r.get('currency_code')=='EUR' for r in rows) else 1)
" && ok "EUR 结果里没有别的币种混进来" || bad "币种过滤没生效（或请求失败：$(err_msg "$RUN_DIR/e1.json")）"

STATUS="$(get_ledger "currency=USD&period_from=2063-10&period_to=2063-10&account_code=FXL01" "$RUN_DIR/a1.json")"
[[ "$STATUS" == "200" ]] && ok "按科目 FXL01 过滤 ⇒ 200" || bad "科目过滤返回 $STATUS"
[[ "$(json_field "$RUN_DIR/a1.json" "['accounts'].__len__()")" == "1" ]] && ok "科目过滤后汇总只剩 1 行" || bad "科目过滤后 accounts = $(json_field "$RUN_DIR/a1.json" "['accounts'].__len__()")"
[[ "$(json_field "$RUN_DIR/a1.json" "['rows'].__len__()")" == "1" ]] && ok "科目过滤后明细只剩 1 行" || bad "科目过滤后 rows 不对"
num_eq "科目过滤后期末原币" "$(json_field "$RUN_DIR/a1.json" ".get('closing_fc')")" 300
[[ "$(json_field "$RUN_DIR/a1.json" "['account']['account_code']")" == "FXL01" ]] && ok "回了科目回显（account.account_code = FXL01）" || bad "科目回显不对"

STATUS="$(get_ledger "currency=cny" "$RUN_DIR/x1.json")"
[[ "$STATUS" == "400" && "$(err_code "$RUN_DIR/x1.json")" == "10001" ]] \
    && ok "currency=CNY（本位币）⇒ 400 + 10001" || bad "本位币返回 $STATUS / $(err_code "$RUN_DIR/x1.json")"
python3 -c "
import json,sys
d=json.load(open('$RUN_DIR/x1.json'))
msg=d.get('msg') or ''
sys.exit(0 if ('本位币' in msg and '总账' in msg) else 1)
" && ok "本位币 400 的报文指路去「总账」" || bad "报文没说清去哪查本位币：$(err_msg "$RUN_DIR/x1.json")"

STATUS="$(get_ledger "currency=US" "$RUN_DIR/x2.json")"
[[ "$STATUS" == "400" && "$(err_code "$RUN_DIR/x2.json")" == "10001" ]] && ok "非法币种码 US ⇒ 400 + 10001" || bad "非法币种码返回 $STATUS"
STATUS="$(get_ledger "" "$RUN_DIR/x3.json")"
[[ "$STATUS" == "400" && "$(err_code "$RUN_DIR/x3.json")" == "10001" ]] && ok "缺 currency ⇒ 400 + 10001" || bad "缺 currency 返回 $STATUS"
STATUS="$(get_ledger "currency=USD&period_from=2063-10&period_to=2063-09" "$RUN_DIR/x4.json")"
[[ "$STATUS" == "400" && "$(err_code "$RUN_DIR/x4.json")" == "10001" ]] && ok "区间反了（from > to）⇒ 400 + 10001" || bad "区间反了返回 $STATUS"
STATUS="$(get_ledger "currency=USD&period_from=2063/10" "$RUN_DIR/x5.json")"
[[ "$STATUS" == "400" ]] && ok "非法日期 2063/10 ⇒ 400" || bad "非法日期返回 $STATUS"
STATUS="$(get_ledger "currency=USD&account_code=NOSUCH" "$RUN_DIR/x6.json")"
[[ "$STATUS" == "404" ]] && ok "不存在的科目 ⇒ 404" || bad "不存在科目返回 $STATUS"
STATUS="$(get_ledger "currency=USD&account_id=not-a-uuid" "$RUN_DIR/x7.json")"
[[ "$STATUS" == "400" ]] && ok "非法 account_id ⇒ 400" || bad "非法 account_id 返回 $STATUS"

# ---------- ⑦ 只读账号（没有 fx-ledger 码）⇒ 403 ----------
q_db "$DB_E2E" "insert into sys_role (role_code, role_name, status)
                select 'M108_READER','M1-08 只读角色',1
                 where not exists (select 1 from sys_role where role_code='M108_READER' and deleted_at is null)" >/dev/null
ROL="$(q_db "$DB_E2E" "select id from sys_role where role_code='M108_READER' and deleted_at is null limit 1")"
# 授一个**别的**报表码（资产负债表），证明 403 来自「没有本表的码」而不是「没有角色」
q_db "$DB_E2E" "insert into sys_role_menu (id, role_id, menu_id)
                select gen_random_uuid(), '$ROL'::uuid, id from sys_menu
                 where auth_code='business:erp:report:balance-sheet' and deleted_at is null
                   and not exists (select 1 from sys_role_menu where role_id='$ROL'::uuid
                                    and menu_id=(select id from sys_menu where auth_code='business:erp:report:balance-sheet' and deleted_at is null limit 1))" >/dev/null
q_db "$DB_E2E" "insert into sys_user (username, password, must_change_password)
                select 'm108reader', password, false from sys_user where username='admin'
                  and not exists (select 1 from sys_user u where u.username='m108reader' and u.deleted_at is null)" >/dev/null
UID_R="$(q_db "$DB_E2E" "select id from sys_user where username='m108reader' and deleted_at is null limit 1")"
q_db "$DB_E2E" "insert into sys_user_role (user_id, role_id)
                select '$UID_R'::uuid, '$ROL'::uuid
                 where not exists (select 1 from sys_user_role where user_id='$UID_R'::uuid and role_id='$ROL'::uuid)" >/dev/null
TOKEN_R="$(curl -sS -m 10 -X POST "$BASE/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"m108reader\",\"password\":\"$ADMIN_PASSWORD\"}" \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print((d.get("data") or {}).get("access_token") or "")' 2>/dev/null)"
if [[ -n "${TOKEN_R:-}" ]]; then
    STATUS="$(curl -sS -m 10 -o /dev/null -w '%{http_code}' "$BASE/business/erp/report/fx-ledger?currency=USD" -H "Authorization: Bearer $TOKEN_R")"
    [[ "$STATUS" == "403" ]] && ok "只有别的报表码的账号 ⇒ 403（缺 business:erp:report:fx-ledger）" || bad "只读账号返回 $STATUS（期望 403）"
    STATUS="$(curl -sS -m 10 -o /dev/null -w '%{http_code}' "$BASE/business/erp/report/balance-sheet?period=2063-10" -H "Authorization: Bearer $TOKEN_R")"
    [[ "$STATUS" == "200" ]] && ok "同一个账号访问资产负债表 ⇒ 200（证明角色与登录都正常，403 来自码）" || bad "资产负债表返回 $STATUS（应 200）"
else
    bad "只读账号登录失败（无法验证 403）"
fi

STATUS="$(curl -sS -m 10 -o /dev/null -w '%{http_code}' "$BASE/business/erp/report/fx-ledger?currency=USD")"
[[ "$STATUS" == "401" ]] && ok "无 token ⇒ 401" || bad "无 token 返回 $STATUS（期望 401）"

# ---------- ⑧ 幂等：同一查询再查一次数字不变 ----------
STATUS="$(get_ledger "currency=USD&period_from=2063-10&period_to=2063-10" "$RUN_DIR/l2.json")"
if python3 -c "
import json,sys
a=json.load(open('$RUN_DIR/l1.json')).get('data') or {}
b=json.load(open('$RUN_DIR/l2.json')).get('data') or {}
for k in ('accounts','rows','total','entry_count','opening_fc','period_fc_debit','period_fc_credit','closing_fc'):
    if a.get(k)!=b.get(k): sys.exit(1)
sys.exit(0)
"; then ok "二次查询与首次逐字段相同（只读接口无副作用）"; else bad "二次查询数字变了"; fi

echo
echo "== 汇总：PASS=$PASS FAIL=$FAIL =="
[[ "$FAIL" == "0" ]] || exit 1
