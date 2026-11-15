#!/usr/bin/env bash
# =============================================================================
# m107_fx_balance_drill.sh —— V0412（M1-07 外币余额表）真引擎 + 真 HTTP
#
# 卡：t_c456af9e。**不是** psql 手动跑 SQL，而是用编译出的后端二进制真启动，
# 走引擎自己的迁移器（schema_migrations 指纹 / 幂等 / 顺序）与真 HTTP 路由。
#
# 三段：
#   A) 克隆库升级：模板库 cloudpivot_test（停版 V0404）克隆成 cp_m107_upgrade → 真启动
#      → 应用 V0405..V0412 → 断言 V0412 自检 NOTICE / schema_migrations 登记 / 菜单页面行 /
#      FINANCE_MANAGER 授权 / 重启 0 个迁移
#   B) 空库首启：空库 cp_m107_e2e → 真启动 → 全量迁移（含 V0412 自检块）
#   C) 真 HTTP（外币余额表的全部口径）：
#      ① 外币核算标记走 M1-03 接口（不许用 SQL 绕过）
#      ② 两张外币凭证 + 一张纯本位币凭证（HTTP 建单 → 审核 → 记账）
#      ③ 报表数字：期初 / 本期 / 期末 / 折算 / 差额（账面含本位币分录）
#      ④ 缺牌价：折算列 null、合计 null、hints 点名、报表仍 200
#      ⑤ 币种过滤 / 本位币过滤 / 非法币种码 400 / 期间非法 400 / 未建账按自然月
#      ⑥ 草稿凭证不进表
#      ⑦ 只读账号（无 fx-balance 码）⇒ 403
#
# 用法：bash scripts/verify/m107_fx_balance_drill.sh [PG_HOST]
#   默认 PG_HOST=172.18.10.145（99 上跑）；145 上跑传 127.0.0.1。
# 端口固定 18097（隔离段 18000-18999），库名含 e2e/upgrade，脚本自己 DROP。
# =============================================================================
set -uo pipefail

PG_HOST="${1:-172.18.10.145}"
export PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4
PG_USER=cloudpivot
TEMPLATE=cloudpivot_test
DB_UP=cp_m107_upgrade
DB_E2E=cp_m107_e2e
PORT=18097
BASE="http://127.0.0.1:$PORT/api"
REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
BACKEND_DIR="$REPO_ROOT/backend"
BIN="$BACKEND_DIR/target/debug/cloudpivot"
RUN_DIR="${M107_RUN_DIR:-/root/.hermes/profiles/backend-developer/cache/scratch/m107-run}"
NODE_ID='20000000-0000-0000-0000-000000001750'
PAGE_PARENT='20000000-0000-0000-0000-000000000e02'
CODE='business:erp:report:fx-balance'
PATH_X='/business/erp/report/fx-balance'
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

ADMIN_PASSWORD='M107@Drill#2026'

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
        export X_CONFIG_SECRET_KEY="m107-drill-config-secret"
        export NIK_ADMIN_PASSWORD="$ADMIN_PASSWORD"
        exec "$BIN"
    ) >"$log" 2>&1 &
    echo $! > "$RUN_DIR/backend.pid"
    local i
    for i in $(seq 1 90); do
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
echo "== A) 克隆库升级（真引擎：模板库 → cp_m107_upgrade）=="
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
[[ "${APPLIED_N:-0}" -ge 1 ]] && ok "应用了 $APPLIED_N 个迁移（含 V0412）" || bad "一个迁移都没应用 —— V0412 没被引擎识别"
grep -q "V0412 自检通过" "$RUN_DIR/upgrade.log" && ok "V0412 自检块在真引擎里通过（NOTICE 已打印）" || bad "没看到 V0412 自检 NOTICE"
ROW="$(q_db "$DB_UP" "select version||'/'||success||'/'||source from schema_migrations where name='fx_balance_report'")"
echo "  schema_migrations：$ROW"
case "$ROW" in
    412/true/*V0412__fx_balance_report.up.sql) ok "schema_migrations 登记 412/true/源文件" ;;
    *) bad "schema_migrations 登记不对：[$ROW]" ;;
esac
NODE="$(q_db "$DB_UP" "select menu_type||'/'||coalesce(parent_id::text,'-')||'/'||sort_order||'/'||status||'/'||title||'/'||coalesce(path,'-')
        from sys_menu where auth_code='$CODE' and deleted_at is null")"
echo "  权限节点：$NODE"
[[ "$NODE" == "2/$PAGE_PARENT/50/1/外币余额表/$PATH_X" ]] \
    && ok "菜单页面行：type=2 / 父=财务管理 / sort=50 / 启用 / title=外币余额表 / path 对" || bad "权限节点不对：[$NODE]"
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
echo "== B) 空库首启（全量迁移 + V0412 自检；真 HTTP 的落点）=="
psql_admin "CREATE DATABASE $DB_E2E" >/dev/null || { echo "FATAL: 建空库失败"; exit 2; }
start_backend "$DB_E2E" "$RUN_DIR/e2e.log" || { echo "FATAL: 空库实例没起来"; tail -30 "$RUN_DIR/e2e.log"; exit 2; }
APPLIED3="$(grep -oE '数据库迁移完成: 应用 [0-9]+ 个迁移' "$RUN_DIR/e2e.log" | head -1)"
echo "  ${APPLIED3}"
CNT3="$(q_db "$DB_E2E" "select count(*) from schema_migrations where kind='versioned'")"
[[ "${CNT3:-0}" -ge 331 ]] && ok "空库首启 versioned 迁移 = $CNT3（≥331：V0412 至少是第 331 条）" || bad "空库首启 versioned = $CNT3（期望 ≥331）"
grep -q "V0412 自检通过" "$RUN_DIR/e2e.log" && ok "空库首启也跑了 V0412 自检" || bad "空库首启没有 V0412 自检 NOTICE"
curl -fsS -m 3 "http://127.0.0.1:$PORT/health" >/dev/null && ok "/health 200" || bad "/health 不通"

q_db "$DB_E2E" "update sys_user set must_change_password = false where username = 'admin'" >/dev/null
[[ "$(q_db "$DB_E2E" "select must_change_password from sys_user where username='admin'")" == "f" ]] \
    && ok "演练库：清掉 admin 的 must_change_password（仅为跑通本卡接口）" || bad "清 must_change_password 失败"

# ============================================================================
echo
echo "== C) 真 HTTP：外币余额表 =="
TOKEN="$(curl -sS -m 10 -X POST "$BASE/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"admin\",\"password\":\"$ADMIN_PASSWORD\"}" \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print((d.get("data") or {}).get("access_token") or "")' 2>/dev/null)"
[[ -n "${TOKEN:-}" ]] && ok "登录成功（access_token ${#TOKEN} 字符）" || { bad "登录失败"; stop_backend; exit 2; }
AUTH=(-H "Authorization: Bearer $TOKEN")

# 内控：`erp.separation_of_duties.approve` 开着时**制单人不能审自己的凭证**（403 + 65019）。
# 演练要保持内控开着（关开关验证等于绕过真实现），所以再造一个持 SUPER_ADMIN 的复核账号：
# 制单用 admin、审核 + 记账用 checker。
q_db "$DB_E2E" "insert into sys_user (username, password, must_change_password)
                select 'm107checker', password, false from sys_user where username='admin'
                  and not exists (select 1 from sys_user u where u.username='m107checker' and u.deleted_at is null)" >/dev/null
q_db "$DB_E2E" "insert into sys_user_role (user_id, role_id)
                select u.id, r.id from sys_user u, sys_role r
                 where u.username='m107checker' and u.deleted_at is null
                   and r.role_code='SUPER_ADMIN' and r.deleted_at is null
                   and not exists (select 1 from sys_user_role ur where ur.user_id=u.id and ur.role_id=r.id)" >/dev/null
TOKEN_C="$(curl -sS -m 10 -X POST "$BASE/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"m107checker\",\"password\":\"$ADMIN_PASSWORD\"}" \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print((d.get("data") or {}).get("access_token") or "")' 2>/dev/null)"
if [[ -n "${TOKEN_C:-}" ]]; then
    ok "复核账号 m107checker 登录成功（持 SUPER_ADMIN，内控开关保持开启）"
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
err_msg()  { python3 -c "import json;d=json.load(open('$1'));print((d.get('msg') or '')[:120])" 2>/dev/null; }

# ---------- ① 夹具：期间 / 科目 / 汇率（主数据），外币标记走 M1-03 接口 ----------
# 期间命名空间 2061-xx 归本脚本独占（不与 2050/2051/2031/2026 等真实或他卡期间冲突）
q_db "$DB_E2E" "delete from biz_exchange_rate where from_currency in ('USD','EUR','HKD')" >/dev/null
q_db "$DB_E2E" "insert into biz_accounting_period (period_year, period_month, period_code, start_date, end_date, status)
    values (2061,9,'2061-09','2061-09-01','2061-09-30','OPEN'),
           (2061,10,'2061-10','2061-10-01','2061-10-31','OPEN')
    on conflict (period_code) do update set status='OPEN', closed_at=null, closed_by=null" >/dev/null
q_db "$DB_E2E" "insert into biz_account (account_code, account_name, level, account_category, balance_direction, is_leaf, is_foreign_currency)
    values ('FXB01','外币资产',1,'ASSET','DEBIT',1,0),
           ('FXB02','外币对方',1,'ASSET','DEBIT',1,0),
           ('FXB03','本位币科目',1,'ASSET','DEBIT',1,0),
           ('FXB04','欧元资产',1,'ASSET','DEBIT',1,0),
           ('FXB05','欧元对方',1,'ASSET','DEBIT',1,0)
    on conflict (account_code) do update set is_leaf=1, is_foreign_currency=0" >/dev/null
ACC1="$(q_db "$DB_E2E" "select id from biz_account where account_code='FXB01'")"
ACC2="$(q_db "$DB_E2E" "select id from biz_account where account_code='FXB02'")"
ACC3="$(q_db "$DB_E2E" "select id from biz_account where account_code='FXB03'")"
ACC4="$(q_db "$DB_E2E" "select id from biz_account where account_code='FXB04'")"
ACC5="$(q_db "$DB_E2E" "select id from biz_account where account_code='FXB05'")"
[[ -n "$ACC1" && -n "$ACC2" && -n "$ACC3" && -n "$ACC4" && -n "$ACC5" ]] && ok "夹具：5 个科目 + 两个期间（2061-09 / 2061-10）就位" || { bad "夹具科目没建起来"; stop_backend; exit 2; }

mark_fx() { # mark_fx <account_id> <true|false> <out>
    curl -sS -m 10 -o "$3" -w '%{http_code}' -X PUT "$BASE/business/erp/account/$1/foreign-currency" \
        "${AUTH[@]}" -H 'Content-Type: application/json' -d "{\"is_foreign_currency\":$2}"
}
STATUS="$(mark_fx "$ACC1" true "$RUN_DIR/mark1.json")"
[[ "$STATUS" == "200" ]] && ok "M1-03 接口标记科目 FXB01 为外币核算 ⇒ 200" || bad "标记 FXB01 返回 $STATUS（$(err_msg "$RUN_DIR/mark1.json")）"
STATUS="$(mark_fx "$ACC2" true "$RUN_DIR/mark2.json")"
[[ "$STATUS" == "200" ]] && ok "M1-03 接口标记科目 FXB02 为外币核算 ⇒ 200" || bad "标记 FXB02 返回 $STATUS"
STATUS="$(mark_fx "$ACC4" true "$RUN_DIR/mark4.json")"
[[ "$STATUS" == "200" ]] && ok "M1-03 接口标记科目 FXB04 为外币核算 ⇒ 200" || bad "标记 FXB04 返回 $STATUS"
STATUS="$(mark_fx "$ACC5" true "$RUN_DIR/mark5.json")"
[[ "$STATUS" == "200" ]] && ok "M1-03 接口标记科目 FXB05 为外币核算 ⇒ 200" || bad "标记 FXB05 返回 $STATUS"
[[ "$(q_db "$DB_E2E" "select count(*) from biz_account where account_code like 'FXB%' and is_foreign_currency=1")" == "4" ]] \
    && ok "库里 is_foreign_currency 真的落成 4 个（不是只改了返回体）" || bad "库里标记没落地"

# 汇率：USD 9/1=7.0 / 10/15=7.2 / 10/31=7.9 / 11/05=8.5（更晚，不该被采用）；EUR 只有 9/1=8.0
# ⚠️ V0407 之后汇率表带**区间不重叠**排除约束：直接 INSERT 的 `effective_to` 默认 NULL（= 无限），
# 先插早行会让后面的行全部冲突（第一次跑就撞了 ex_biz_exchange_rate_period）。所以每条都显式
# 写 `effective_to`（恒等于「同组下一条生效日 − 1」，末行 NULL），与写入口派生出来的形状一致。
seed_rate() { # seed_rate <currency> <date> <rate> <effective_to|NULL>
    q_db "$DB_E2E" "insert into biz_exchange_rate
            (from_currency, to_currency, rate_type, effective_date, effective_to, rate)
        values ('$1','CNY','SPOT','$2'::date, $4::date, '$3'::numeric)
        on conflict (from_currency, to_currency, rate_type, effective_date)
        do update set rate=excluded.rate, effective_to=excluded.effective_to" >/dev/null
}
seed_rate USD 2061-09-01 7.0 "'2061-10-14'"
seed_rate USD 2061-10-15 7.2 "'2061-10-30'"
seed_rate USD 2061-10-31 7.9 "'2061-11-04'"
seed_rate USD 2061-11-05 8.5 NULL
# EUR 的牌价**在期末之前就失效**（effective_to = 09-30）：用来验「缺牌价不折算」。
# 若这里写成 NULL，V0407 的谓词会认为它一直有效到未来 ⇒ 期末取得到 8.0，缺牌价用例就测不到了。
seed_rate EUR 2061-09-01 8.0 "'2061-09-30'"
ok "夹具：USD 四条牌价 + EUR 一条（期末缺 EUR 牌价，用来验「不折算」）"

# ---------- ② 三张凭证：建单 → 审核 → 记账 ----------
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

# V1：期初（2061-09-20）200 USD @ 7.0
STATUS="$(create_voucher "$RUN_DIR/v1.json" "{
  \"voucher_date\":\"2061-09-20\",\"summary\":\"M107 期初外币\",
  \"currency_code\":\"USD\",
  \"entries\":[{\"account_id\":\"$ACC1\",\"debit\":200},{\"account_id\":\"$ACC2\",\"credit\":200}]}")"
[[ "$STATUS" == "200" ]] || { bad "V1 建单失败 $STATUS（$(err_msg "$RUN_DIR/v1.json")）"; stop_backend; exit 2; }
ok "V1 建单成功（200 USD @ 当日牌价 7.0）"
V1="$(json_field "$RUN_DIR/v1.json" ".get('id')")"
audit_post "$V1" "V1（期初 200 USD）"

# V2：本期（2061-10-20，落在 10-15..10-30 的 7.2 区间）100 USD @ 7.2
STATUS="$(create_voucher "$RUN_DIR/v2.json" "{
  \"voucher_date\":\"2061-10-20\",\"summary\":\"M107 本期外币\",
  \"currency_code\":\"USD\",
  \"entries\":[{\"account_id\":\"$ACC1\",\"debit\":100},{\"account_id\":\"$ACC2\",\"credit\":100}]}")"
[[ "$STATUS" == "200" ]] || { bad "V2 建单失败 $STATUS（$(err_msg "$RUN_DIR/v2.json")）"; stop_backend; exit 2; }
V2="$(json_field "$RUN_DIR/v2.json" ".get('id')")"
audit_post "$V2" "V2（本期 100 USD @ 7.2）"

# V3：本期纯本位币 50（只改账面、不改原币）
STATUS="$(create_voucher "$RUN_DIR/v3.json" "{
  \"voucher_date\":\"2061-10-22\",\"summary\":\"M107 本位币调整\",
  \"entries\":[{\"account_id\":\"$ACC2\",\"debit\":50},{\"account_id\":\"$ACC3\",\"credit\":50}]}")"
[[ "$STATUS" == "200" ]] || { bad "V3 建单失败 $STATUS（$(err_msg "$RUN_DIR/v3.json")）"; stop_backend; exit 2; }
V3="$(json_field "$RUN_DIR/v3.json" ".get('id')")"
audit_post "$V3" "V3（本位币 50，账面含本位币分录的判据）"

# V4：EUR 100（挂在独立的欧元科目上；期末无 EUR 牌价 → 折算留空）
STATUS="$(create_voucher "$RUN_DIR/v4.json" "{
  \"voucher_date\":\"2061-09-25\",\"summary\":\"M107 欧元期初\",
  \"currency_code\":\"EUR\",
  \"entries\":[{\"account_id\":\"$ACC4\",\"debit\":100},{\"account_id\":\"$ACC5\",\"credit\":100}]}")"
[[ "$STATUS" == "200" ]] || { bad "V4 建单失败 $STATUS（$(err_msg "$RUN_DIR/v4.json")）"; stop_backend; exit 2; }
V4="$(json_field "$RUN_DIR/v4.json" ".get('id')")"
audit_post "$V4" "V4（EUR 100，期末缺牌价）"

# V5：草稿 999 USD（不审核不记账 → 不许进表）
STATUS="$(create_voucher "$RUN_DIR/v5.json" "{
  \"voucher_date\":\"2061-10-25\",\"summary\":\"M107 草稿\",
  \"currency_code\":\"USD\",
  \"entries\":[{\"account_id\":\"$ACC1\",\"debit\":999},{\"account_id\":\"$ACC2\",\"credit\":999}]}")"
[[ "$STATUS" == "200" ]] && ok "V5 草稿建单成功（故意不审核不记账）" || bad "V5 建单失败 $STATUS"

# ---------- ③ 报表主用例 ----------
get_report() { # get_report <query> <out>
    curl -sS -m 15 -o "$2" -w '%{http_code}' "$BASE/business/erp/report/fx-balance?$1" "${AUTH[@]}"
}
row_of() { # row_of <file> <account_code> <currency> <field>
    python3 -c "
import json,sys
d=json.load(open('$1'))
for r in (d.get('data') or {}).get('rows') or []:
    if r.get('account_code')=='$2' and r.get('currency_code')=='$3':
        print(r.get('$4'))
" 2>/dev/null
}

STATUS="$(get_report "period=2061-10" "$RUN_DIR/r1.json")"
echo "  报表片段：$(head -c 300 "$RUN_DIR/r1.json")"
[[ "$STATUS" == "200" ]] && ok "GET /business/erp/report/fx-balance?period=2061-10 ⇒ 200" || bad "报表返回 $STATUS"
[[ "$(json_field "$RUN_DIR/r1.json" ".get('report_code')")" == "FX_BALANCE" ]] && ok "report_code = FX_BALANCE" || bad "report_code 不对"
[[ "$(json_field "$RUN_DIR/r1.json" ".get('base_currency')")" == "CNY" ]] && ok "base_currency = CNY（账套本位币）" || bad "base_currency 不对"
[[ "$(json_field "$RUN_DIR/r1.json" ".get('period_start')")" == "2061-10-01" ]] && ok "period_start = 2061-10-01" || bad "period_start 不对：$(json_field "$RUN_DIR/r1.json" ".get('period_start')")"
[[ "$(json_field "$RUN_DIR/r1.json" ".get('period_end')")" == "2061-10-31" ]] && ok "period_end = 2061-10-31" || bad "period_end 不对"
[[ "$(json_field "$RUN_DIR/r1.json" ".get('period_status')")" == "OPEN" ]] && ok "period_status = OPEN（期间未结账）" || bad "period_status 不对"
ROWN="$(json_field "$RUN_DIR/r1.json" "['rows'].__len__()")"
[[ "$ROWN" == "4" ]] && ok "明细 4 行（2 科目 × USD/EUR）" || bad "明细行数 $ROWN（期望 4）"

# USD 资产：期初 200 / 本期借 100 / 期末 300；账面 1400+720=2120；7.9 ⇒ 2370；差 250
# 判据用 Decimal 数值比较（JSON 里金额是字符串，`200` 与 `200.0000` 必须都算通过）
check_num() { # check_num <label> <account> <currency> <field> <expected>
    python3 -c "
import json,sys
from decimal import Decimal
d=json.load(open('$RUN_DIR/r1.json')).get('data') or {}
want=Decimal('$5')
for r in d.get('rows') or []:
    if r.get('account_code')=='$2' and r.get('currency_code')=='$3':
        v=r.get('$4')
        sys.exit(0 if v is not None and Decimal(str(v))==want else 1)
sys.exit(2)
" && ok "$1：$4 = $5" \
   || bad "$1：$4 ≠ $5（实际 $(row_of "$RUN_DIR/r1.json" "$2" "$3" "$4")）"
}
check_null() { # check_null <label> <account> <currency> <field>
    local got; got="$(row_of "$RUN_DIR/r1.json" "$2" "$3" "$4")"
    [[ "$got" == "None" || "$got" == "" ]] && ok "$1：$4 = null（缺牌价不折算）" || bad "$1：$4 = [$got]（期望 null）"
}
check_str() { # check_str <label> <account> <currency> <field> <expected>
    local got; got="$(row_of "$RUN_DIR/r1.json" "$2" "$3" "$4")"
    [[ "$got" == "$5" ]] && ok "$1：$4 = $5" || bad "$1：$4 = [$got]（期望 $5）"
}

check_num "USD 资产" FXB01 USD opening_fc 200
check_num "USD 资产" FXB01 USD period_fc_debit 100
check_num "USD 资产" FXB01 USD closing_fc 300
check_num "USD 资产" FXB01 USD opening_book 1400
check_num "USD 资产" FXB01 USD period_book_debit 720
check_num "USD 资产" FXB01 USD closing_book 2120
check_num "USD 资产" FXB01 USD close_rate 7.9
check_num "USD 资产" FXB01 USD converted_base 2370
check_num "USD 资产" FXB01 USD reval_diff 250
check_str "USD 资产" FXB01 USD book_basis ACCOUNT
check_str "USD 资产" FXB01 USD rate_missing False

# USD 对方：期末 -300；账面必须含那笔本位币借 50 ⇒ -2070；折算 -2370；差 -300
check_num "USD 对方" FXB02 USD closing_fc -300
check_num "USD 对方" FXB02 USD period_book_debit 50
check_num "USD 对方" FXB02 USD closing_book -2070
check_num "USD 对方" FXB02 USD converted_base -2370
check_num "USD 对方" FXB02 USD reval_diff -300

# EUR：期末有余额但没牌价 ⇒ 三列 null + rate_missing
check_num "EUR 资产" FXB04 EUR closing_fc 100
check_num "EUR 资产" FXB04 EUR closing_book 800
check_null "EUR 资产" FXB04 EUR close_rate
check_null "EUR 资产" FXB04 EUR converted_base
check_null "EUR 资产" FXB04 EUR reval_diff
check_str "EUR 资产" FXB04 EUR rate_missing True
check_num "EUR 对方" FXB05 EUR closing_fc -100
check_num "EUR 对方" FXB05 EUR closing_book -800

python3 -c "
import json,sys
from decimal import Decimal
d=json.load(open('$RUN_DIR/r1.json')).get('data') or {}
sys.exit(0 if d.get('total_closing_book') is not None and Decimal(str(d['total_closing_book']))==Decimal('50') else 1)
" && ok "期末账面合计 = 50（2120 − 2070 + 800 − 800）" || bad "total_closing_book = $(json_field "$RUN_DIR/r1.json" ".get('total_closing_book')")"
[[ "$(json_field "$RUN_DIR/r1.json" ".get('total_converted_base')")" == "None" ]] \
    && ok "折算合计 = null（EUR 缺牌价，半套折算的合计不给）" || bad "total_converted_base 应为 null"
[[ "$(json_field "$RUN_DIR/r1.json" ".get('total_reval_diff')")" == "None" ]] \
    && ok "差额合计 = null（同上）" || bad "total_reval_diff 应为 null"
[[ "$(json_field "$RUN_DIR/r1.json" ".get('missing_rates')")" == "['EUR']" ]] \
    && ok "missing_rates = ['EUR']" || bad "missing_rates = $(json_field "$RUN_DIR/r1.json" ".get('missing_rates')")"
python3 -c "
import json,sys
d=json.load(open('$RUN_DIR/r1.json')).get('data') or {}
sys.exit(0 if any('EUR' in h and '汇率 1' in h for h in d.get('hints') or []) else 1)
" && ok "hints 说明「没按汇率 1 折算」" || bad "hints 没说明缺牌价处理"

# 干净夹具（每个科目只挂一个币种、没有外币分录落在未标记科目）⇒ warnings 必须为空。
# 这条同时兜住「warnings 被写成无条件拼接」的退化（那样页面会一直红着，没人再看它）。
python3 -c "
import json,sys
d=json.load(open('$RUN_DIR/r1.json')).get('data') or {}
sys.exit(0 if not (d.get('warnings') or []) else 1)
" && ok "干净数据没有 warnings（多币种退化/未标记科目都是条件触发）" || bad "干净数据却报了 warnings：$(json_field "$RUN_DIR/r1.json" ".get('warnings')")"

# 草稿不进表（V5 的 999 若进表会变成 1299）
python3 -c "
import json,sys
from decimal import Decimal
d=json.load(open('$RUN_DIR/r1.json')).get('data') or {}
for r in d.get('rows') or []:
    if r.get('account_code')=='FXB01' and r.get('currency_code')=='USD':
        sys.exit(0 if Decimal(str(r['closing_fc']))==Decimal('300') else 1)
sys.exit(1)
" && ok "草稿凭证（999 USD）不进本表" || bad "草稿凭证进了报表"

# ---------- ④ 币种过滤 / 非法入参 / 未建账 ----------
STATUS="$(get_report "period=2061-10&currency=EUR" "$RUN_DIR/r2.json")"
[[ "$STATUS" == "200" ]] && ok "筛 EUR ⇒ 200" || bad "筛 EUR 返回 $STATUS"
[[ "$(json_field "$RUN_DIR/r2.json" "['rows'].__len__()")" == "2" ]] \
    && ok "筛 EUR ⇒ 2 行（两个科目）" || bad "筛 EUR 行数 = $(json_field "$RUN_DIR/r2.json" "['rows'].__len__()")"
python3 -c "
import json,sys
d=json.load(open('$RUN_DIR/r2.json')).get('data') or {}
sys.exit(0 if all(r['currency_code']=='EUR' for r in d.get('rows') or []) else 1)
" && ok "筛 EUR 后没有别的币种混进来" || bad "筛选没生效（混进别的币种）"

STATUS="$(get_report "period=2061-10&currency=cny" "$RUN_DIR/r3.json")"
[[ "$STATUS" == "200" ]] && ok "筛本位币 CNY ⇒ 200（不是错误）" || bad "筛 CNY 返回 $STATUS"
[[ "$(json_field "$RUN_DIR/r3.json" "['rows'].__len__()")" == "0" ]] && ok "筛 CNY ⇒ 0 行" || bad "筛 CNY 居然有行"
python3 -c "
import json,sys
d=json.load(open('$RUN_DIR/r3.json')).get('data') or {}
sys.exit(0 if any('本位币' in w for w in d.get('warnings') or []) else 1)
" && ok "筛 CNY 的 warnings 说清「本位币不构成外币余额」" || bad "筛 CNY 没解释为什么为空"

STATUS="$(get_report "period=2061-10&currency=US" "$RUN_DIR/r4.json")"
[[ "$STATUS" == "400" && "$(err_code "$RUN_DIR/r4.json")" == "10001" ]] \
    && ok "非法币种码 US ⇒ 400 + 10001（不静默忽略过滤）" || bad "非法币种码返回 $STATUS / $(err_code "$RUN_DIR/r4.json")"

STATUS="$(get_report "period=2061-13" "$RUN_DIR/r5.json")"
[[ "$STATUS" == "400" && "$(err_code "$RUN_DIR/r5.json")" == "10001" ]] \
    && ok "非法月份 2061-13 ⇒ 400 + 10001" || bad "非法月份返回 $STATUS"
STATUS="$(get_report "" "$RUN_DIR/r6.json")"
[[ "$STATUS" == "400" && "$(err_code "$RUN_DIR/r6.json")" == "10001" ]] \
    && ok "period 缺失 ⇒ 400 + 10001" || bad "缺 period 返回 $STATUS"

STATUS="$(get_report "period=2061-12" "$RUN_DIR/r7.json")"
[[ "$STATUS" == "200" ]] && ok "未建账的月份 2061-12 ⇒ 200（按自然月推算，不 400）" || bad "未建账月份返回 $STATUS"
[[ "$(json_field "$RUN_DIR/r7.json" ".get('period_status')")" == "None" ]] && ok "period_status = null（未建账）" || bad "period_status 应为 null"
[[ "$(json_field "$RUN_DIR/r7.json" ".get('period_end')")" == "2061-12-31" ]] && ok "未建账时期间末 = 2061-12-31（自然月）" || bad "自然月末不对"
python3 -c "
import json,sys
d=json.load(open('$RUN_DIR/r7.json')).get('data') or {}
sys.exit(0 if any('未建账' in h for h in d.get('hints') or []) else 1)
" && ok "hints 里写明「未建账·按自然月推算」" || bad "hints 没写期间推算"

# ---------- ⑤ 只读账号（没有 fx-balance 码）⇒ 403 ----------
q_db "$DB_E2E" "insert into sys_role (role_code, role_name, status)
                select 'M107_READER','M1-07 只读角色',1
                 where not exists (select 1 from sys_role where role_code='M107_READER' and deleted_at is null)" >/dev/null
ROL="$(q_db "$DB_E2E" "select id from sys_role where role_code='M107_READER' and deleted_at is null limit 1")"
# 授一个**别的**报表码（资产负债表），证明 403 来自「没有本表的码」而不是「没有角色」
q_db "$DB_E2E" "insert into sys_role_menu (id, role_id, menu_id)
                select gen_random_uuid(), '$ROL'::uuid, id from sys_menu
                 where auth_code='business:erp:report:balance-sheet' and deleted_at is null
                   and not exists (select 1 from sys_role_menu where role_id='$ROL'::uuid
                                    and menu_id=(select id from sys_menu where auth_code='business:erp:report:balance-sheet' and deleted_at is null limit 1))" >/dev/null
q_db "$DB_E2E" "insert into sys_user (username, password, must_change_password)
                select 'm107reader', password, false from sys_user where username='admin'
                  and not exists (select 1 from sys_user u where u.username='m107reader' and u.deleted_at is null)" >/dev/null
UID_R="$(q_db "$DB_E2E" "select id from sys_user where username='m107reader' and deleted_at is null limit 1")"
q_db "$DB_E2E" "insert into sys_user_role (user_id, role_id)
                select '$UID_R'::uuid, '$ROL'::uuid
                 where not exists (select 1 from sys_user_role where user_id='$UID_R'::uuid and role_id='$ROL'::uuid)" >/dev/null
TOKEN_R="$(curl -sS -m 10 -X POST "$BASE/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"m107reader\",\"password\":\"$ADMIN_PASSWORD\"}" \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print((d.get("data") or {}).get("access_token") or "")' 2>/dev/null)"
if [[ -n "${TOKEN_R:-}" ]]; then
    STATUS="$(curl -sS -m 10 -o /dev/null -w '%{http_code}' "$BASE/business/erp/report/fx-balance?period=2061-10" -H "Authorization: Bearer $TOKEN_R")"
    [[ "$STATUS" == "403" ]] && ok "只有别的报表码的账号 ⇒ 403（缺 business:erp:report:fx-balance）" || bad "只读账号返回 $STATUS（期望 403）"
    STATUS="$(curl -sS -m 10 -o /dev/null -w '%{http_code}' "$BASE/business/erp/report/balance-sheet?period=2061-10" -H "Authorization: Bearer $TOKEN_R")"
    [[ "$STATUS" == "200" ]] && ok "同一个账号访问资产负债表 ⇒ 200（证明角色与登录都正常，403 来自码）" || bad "资产负债表返回 $STATUS（应 200）"
else
    bad "只读账号登录失败（无法验证 403）"
fi

STATUS="$(curl -sS -m 10 -o /dev/null -w '%{http_code}' "$BASE/business/erp/report/fx-balance?period=2061-10")"
[[ "$STATUS" == "401" ]] && ok "无 token ⇒ 401" || bad "无 token 返回 $STATUS（期望 401）"

# ---------- ⑥ 幂等：同一期间再查一次数字不变 ----------
STATUS="$(get_report "period=2061-10" "$RUN_DIR/r8.json")"
if python3 -c "
import json,sys
a=json.load(open('$RUN_DIR/r1.json')).get('data') or {}
b=json.load(open('$RUN_DIR/r8.json')).get('data') or {}
for k in ('rows','currencies','total_closing_book','total_converted_base','total_reval_diff','missing_rates'):
    if a.get(k)!=b.get(k): sys.exit(1)
sys.exit(0)
"; then ok "二次查询与首次逐字段相同（只读接口无副作用）"; else bad "二次查询数字变了"; fi

echo
echo "== 汇总：PASS=$PASS FAIL=$FAIL =="
[[ "$FAIL" == "0" ]] || exit 1
