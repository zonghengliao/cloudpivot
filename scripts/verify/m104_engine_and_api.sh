#!/usr/bin/env bash
# =============================================================================
# m104_engine_and_api.sh —— V0409（M1-04 凭证外币金额）真引擎 + 真 HTTP 实测
#
# 卡：t_b95e935d。**不是** psql 手动执行 SQL，而是用编译出的后端二进制真启动，
# 走引擎自己的迁移器（schema_migrations 指纹 / 幂等 / 顺序）与真 HTTP 路由。
#
# 三段：
#   A) 克隆库升级：模板库 cloudpivot_test（停版 V0404）克隆成 cp_m104_upgrade → 真启动
#      → 应用 V0405..V0409 → schema_migrations 登记 → 重启 0 个迁移
#   B) 空库首启：空库 cp_m104_e2e → 真启动 → 全量迁移（含 V0409 自检块）
#   C) 真 HTTP：登录 admin → 外币凭证录入的十条路径（成功 / 尾差 / 未标科目 / 汇率不符 /
#      未知币种 / 缺牌价 / 本位币不变 / 红冲继承 / 选项接口 / 详情回读）
#
# 用法：bash scripts/verify/m104_engine_and_api.sh [PG_HOST]
#   默认 PG_HOST=172.18.10.145（99 上跑）；145 上跑传 127.0.0.1。
# 端口固定 18089（隔离段 18000-18999），库名含 e2e/upgrade，脚本自己 DROP。
# =============================================================================
set -uo pipefail

PG_HOST="${1:-172.18.10.145}"
export PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4
PG_USER=cloudpivot
TEMPLATE=cloudpivot_test
DB_UP=cp_m104_upgrade
DB_E2E=cp_m104_e2e
PORT=18089
BASE="http://127.0.0.1:$PORT/api"
REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
BACKEND_DIR="$REPO_ROOT/backend"
BIN="$BACKEND_DIR/target/debug/cloudpivot"
RUN_DIR="${M104_RUN_DIR:-/root/.hermes/profiles/backend-developer/cache/scratch/m104-run}"
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

# 管理员初始口令：`bootstrap::seed_super_admin` 只认 `NIK_ADMIN_PASSWORD`（仓库内无缺省口令），
# 缺失时会生成一次性随机口令并把账号标 `must_change_password = true`（除改密外全部接口 403）。
ADMIN_PASSWORD='M104@Drill#2026'

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
        export X_CONFIG_SECRET_KEY="m104-drill-config-secret"
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
echo "== A) 克隆库升级（真引擎：模板库 → cp_m104_upgrade）=="
psql_admin "DROP DATABASE IF EXISTS $DB_UP" >/dev/null 2>&1
psql_admin "DROP DATABASE IF EXISTS $DB_E2E" >/dev/null 2>&1
if ! psql_admin "CREATE DATABASE $DB_UP TEMPLATE $TEMPLATE" >/dev/null 2>&1; then
    echo "FATAL: 克隆模板库失败（模板可能被别的会话占用），5 秒后重试一次"
    sleep 5
    psql_admin "CREATE DATABASE $DB_UP TEMPLATE $TEMPLATE" >/dev/null || exit 2
fi
BEFORE_CNT="$(q_db "$DB_UP" "select count(*) from schema_migrations where kind='versioned'")"
BEFORE_MAX="$(q_db "$DB_UP" "select coalesce(max(version::int),0) from schema_migrations where kind='versioned'")"
echo "  升级前：versioned $BEFORE_CNT 条，最大 version=$BEFORE_MAX（模板停在 V$BEFORE_MAX）"

start_backend "$DB_UP" "$RUN_DIR/upgrade.log" || { echo "FATAL: 升级实例没起来"; exit 2; }
ok "克隆库真启动成功（/health 200）"
APPLIED="$(grep -oE '数据库迁移完成: 应用 [0-9]+ 个迁移' "$RUN_DIR/upgrade.log" | head -1)"
echo "  $APPLIED"
APPLIED_N="$(echo "$APPLIED" | grep -oE '[0-9]+' | head -1)"
# 模板库比仓库落后几步（V0405..V0409 一起补），所以不断言数量，断言「V0409 被应用 + 条数增量一致」。
[[ "${APPLIED_N:-0}" -ge 1 ]] && ok "应用了 $APPLIED_N 个迁移（含 V0409）" || bad "一个迁移都没应用 —— V0409 没被引擎识别"
grep -q "V0409 自检通过" "$RUN_DIR/upgrade.log" && ok "V0409 自检块在真引擎里通过（NOTICE 已打印）" || bad "没看到 V0409 自检 NOTICE"
ROW="$(q_db "$DB_UP" "select version||'/'||success||'/'||source from schema_migrations where name='voucher_entry_foreign_amounts'")"
echo "  schema_migrations：$ROW"
case "$ROW" in
    409/true/*V0409__voucher_entry_foreign_amounts.up.sql) ok "schema_migrations 登记 409/true/源文件" ;;
    *) bad "schema_migrations 登记不对：[$ROW]" ;;
esac
COL="$(q_db "$DB_UP" "select string_agg(column_name||':'||data_type||'('||numeric_precision||','||numeric_scale||')/'||is_nullable, ' | ' order by column_name)
        from information_schema.columns where table_name='biz_voucher_entry' and column_name like 'fc\_%'")"
[[ "$COL" == "fc_credit_amount:numeric(20,4)/YES | fc_debit_amount:numeric(20,4)/YES" ]] \
    && ok "升级后两列 = numeric(20,4) / 可空" || bad "升级后列结构异常：[$COL]"
[[ "$(q_db "$DB_UP" "select count(*) from pg_constraint where conrelid='public.biz_voucher_entry'::regclass and conname='ck_biz_voucher_entry_fc_side'")" == "1" ]] \
    && ok "ck_biz_voucher_entry_fc_side 在位" || bad "CHECK 不在位"
[[ "$(q_db "$DB_UP" "select count(*) from pg_indexes where indexname='idx_biz_voucher_entry_fc'")" == "1" ]] \
    && ok "idx_biz_voucher_entry_fc 在位" || bad "部分索引不在位"

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
echo "== B) 空库首启（全量迁移 + V0409 自检；真 HTTP 的落点）=="
psql_admin "CREATE DATABASE $DB_E2E" >/dev/null || { echo "FATAL: 建空库失败"; exit 2; }
start_backend "$DB_E2E" "$RUN_DIR/e2e.log" || { echo "FATAL: 空库实例没起来"; tail -30 "$RUN_DIR/e2e.log"; exit 2; }
APPLIED3="$(grep -oE '数据库迁移完成: 应用 [0-9]+ 个迁移' "$RUN_DIR/e2e.log" | head -1)"
echo "  $APPLIED3"
CNT3="$(q_db "$DB_E2E" "select count(*) from schema_migrations where kind='versioned'")"
[[ "${CNT3:-0}" -ge 329 ]] && ok "空库首启 versioned 迁移 = $CNT3（≥329：V0409 至少是第 329 条）" || bad "空库首启 versioned = $CNT3（期望 ≥329）"
grep -q "V0409 自检通过" "$RUN_DIR/e2e.log" && ok "空库首启也跑了 V0409 自检" || bad "空库首启没有 V0409 自检 NOTICE"
curl -fsS -m 3 "http://127.0.0.1:$PORT/health" >/dev/null && ok "/health 200" || bad "/health 不通"

q_db "$DB_E2E" "update sys_user set must_change_password = false where username = 'admin'" >/dev/null
[[ "$(q_db "$DB_E2E" "select must_change_password from sys_user where username='admin'")" == "f" ]] \
    && ok "演练库：清掉 admin 的 must_change_password（仅为跑通本卡接口）" || bad "清 must_change_password 失败"

# ============================================================================
echo
echo "== C) 真 HTTP：外币凭证录入 =="
TOKEN="$(curl -sS -m 10 -X POST "$BASE/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"admin\",\"password\":\"$ADMIN_PASSWORD\"}" \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print((d.get("data") or {}).get("access_token") or "")' 2>/dev/null)"
[[ -n "${TOKEN:-}" ]] && ok "登录成功（access_token ${#TOKEN} 字符）" || { bad "登录失败"; stop_backend; exit 2; }
AUTH=(-H "Authorization: Bearer ${TOKEN}")

# 内控闸「制单人不能审自己的凭证」（65019 / 403）：红冲链路必须由**另一个人**审核 + 记账。
# 演练库里克隆 admin 的口令哈希 + SUPER_ADMIN 角色造一个复核账号（只动 cp_m104_e2e）。
# ⚠️ `uk_sys_user_username` 是**部分唯一索引**（`WHERE deleted_at IS NULL`），
#    `ON CONFLICT (username)` 推断不到它（实测报「no unique or exclusion constraint matching」），
#    所以这里用 `NOT EXISTS` 判重 + 单独 UPDATE 刷新口令。
ADMIN_ROLE="$(q_db "$DB_E2E" "select id from sys_role where role_code='SUPER_ADMIN' limit 1")"
q_db "$DB_E2E" "insert into sys_user (username, password, must_change_password)
                select 'm104auditor', password, false from sys_user a
                 where a.username='admin'
                   and not exists (select 1 from sys_user u
                                    where u.username='m104auditor' and u.deleted_at is null)" >/dev/null
q_db "$DB_E2E" "update sys_user set password = (select password from sys_user where username='admin'),
                          must_change_password = false
                 where username = 'm104auditor'" >/dev/null
q_db "$DB_E2E" "insert into sys_user_role (user_id, role_id)
                select u.id, '$ADMIN_ROLE'::uuid from sys_user u
                 where u.username='m104auditor'
                   and not exists (select 1 from sys_user_role r
                                    where r.user_id = u.id and r.role_id = '$ADMIN_ROLE'::uuid)" >/dev/null
AUD_TOKEN="$(curl -sS -m 10 -X POST "$BASE/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"m104auditor\",\"password\":\"$ADMIN_PASSWORD\"}" \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print((d.get("data") or {}).get("access_token") or "")' 2>/dev/null)"
[[ -n "${AUD_TOKEN:-}" ]] && ok "复核账号 m104auditor 登录成功（内控：制单 ≠ 审核）" || { bad "复核账号登录失败"; stop_backend; exit 2; }
AUD_AUTH=(-H "Authorization: Bearer ${AUD_TOKEN}")

# ---------- 夹具：会计期间 / 科目 / 币种牌价（全部落**演练库**）----------
q_db "$DB_E2E" "insert into biz_accounting_period (period_year, period_month, period_code, start_date, end_date, status)
                values (2026, 10, '2026-10', date '2026-10-01', date '2026-10-31', 'OPEN')
                on conflict (period_code) do update set status='OPEN'" >/dev/null
q_db "$DB_E2E" "insert into biz_accounting_period (period_year, period_month, period_code, start_date, end_date, status)
                select extract(year from current_date)::smallint, extract(month from current_date)::smallint,
                       to_char(current_date,'YYYY-MM'), date_trunc('month', current_date)::date,
                       (date_trunc('month', current_date) + interval '1 month - 1 day')::date, 'OPEN'
                on conflict (period_code) do update set status='OPEN'" >/dev/null
for spec in "M104.01|M1-04 外币借方|1" "M104.02|M1-04 外币贷方|1" "M104.03|M1-04 未标外币|0"; do
    CODE="${spec%%|*}"; REST="${spec#*|}"; NAME="${REST%%|*}"; FC="${REST##*|}"
    q_db "$DB_E2E" "insert into biz_account (account_code, account_name, level, account_category, balance_direction, is_leaf, status, is_foreign_currency)
                    values ('$CODE', '$NAME', 1, 'ASSET', 'DEBIT', 1, 1, $FC)" >/dev/null
done
DEBIT_ID="$(q_db "$DB_E2E" "select id from biz_account where account_code='M104.01'")"
CREDIT_ID="$(q_db "$DB_E2E" "select id from biz_account where account_code='M104.02'")"
UNMARKED_ID="$(q_db "$DB_E2E" "select id from biz_account where account_code='M104.03'")"
BASE_CUR="$(q_db "$DB_E2E" "select code from biz_currency where is_base and status='ACTIVE' limit 1")"
[[ -n "$DEBIT_ID" && -n "$CREDIT_ID" && -n "$UNMARKED_ID" ]] && ok "演练科目已就位（2 个已标记 + 1 个未标记）" || bad "演练科目没建出来"
echo "  本位币=$BASE_CUR；借方科目=$DEBIT_ID；贷方科目=$CREDIT_ID；未标记科目=$UNMARKED_ID"
q_db "$DB_E2E" "insert into biz_currency (code, name, symbol, decimal_places, is_base, status)
                values ('EUR','欧元','€',2,false,'ACTIVE') on conflict (code) do nothing" >/dev/null
q_db "$DB_E2E" "delete from biz_exchange_rate where from_currency='EUR' and to_currency='$BASE_CUR'" >/dev/null
q_db "$DB_E2E" "insert into biz_exchange_rate (from_currency, to_currency, rate, rate_type, effective_date)
                values ('EUR','$BASE_CUR',8.5,'SPOT',date '2026-10-01')" >/dev/null
[[ "$(q_db "$DB_E2E" "select rate from biz_exchange_rate where from_currency='EUR' and to_currency='$BASE_CUR'")" == "8.50000000" ]] \
    && ok "EUR 牌价已就位（8.5，自 2026-10-01）" || bad "EUR 牌价没插进去"

# call_post <body> → 打印 HTTP 码 + body（code/msg 摘要）
call_post() {
    curl -sS -m 15 -o "$RUN_DIR/body.json" -w '%{http_code}' -X POST "$BASE/business/erp/voucher" \
        "${AUTH[@]}" -H 'Content-Type: application/json' -d "$1"
    echo -n " "
    python3 - "$RUN_DIR/body.json" <<'PY'
import json,sys
d=json.load(open(sys.argv[1]))
data=d.get("data") or {}
print("code=%s msg=%s voucher_no=%s cur=%s rate=%s debit=%s" % (
    d.get("code"), (d.get("msg") or "")[:120], data.get("voucher_no"),
    data.get("currency_code"), data.get("exchange_rate"), data.get("total_debit")))
PY
}

body_of() { cat "$RUN_DIR/body.json"; }
# 取响应体 **data** 下的字段（点号分隔；数组用下标，如 `entries.0.debit_amount`）
field() { python3 - "$RUN_DIR/body.json" "$1" <<'PY'
import json,sys
d=json.load(open(sys.argv[1]))
cur=d.get("data")
for key in sys.argv[2].split('.'):
    if cur is None: break
    if key.isdigit(): cur=cur[int(key)]
    else: cur=cur.get(key)
print(cur)
PY
}

echo
echo "-- C1) fx-options：本位币恒 1 / EUR 取到牌价 / 无牌价币种为 null --"
CODE=$(curl -sS -m 10 -o "$RUN_DIR/fx.json" -w '%{http_code}' "$BASE/business/erp/voucher/fx-options?voucher_date=2026-10-15" "${AUTH[@]}")
[[ "$CODE" == "200" ]] && ok "fx-options 200" || bad "fx-options HTTP $CODE"
python3 - "$RUN_DIR/fx.json" <<'PY'
import json,sys
d=json.load(open(sys.argv[1]))["data"]
cur={row["code"]: row for row in d["currencies"]}
print("  base=%s voucher_date=%s 币种=%s" % (d["base_currency"], d["voucher_date"], sorted(cur)))
assert cur[d["base_currency"]]["is_base"] is True, "本位币必须 is_base"
assert float(cur[d["base_currency"]]["exchange_rate"]) == 1.0, "本位币汇率必须恒 1"
assert float(cur["EUR"]["exchange_rate"]) == 8.5, "EUR 必须取到 8.5"
assert cur["HKD"]["exchange_rate"] is None, "HKD 没有牌价必须是 null（不是 1）"
print("  PASS  币种与牌价逐条正确")
PY
[[ $? -eq 0 ]] && PASS=$((PASS+1)) || FAIL=$((FAIL+1))

echo
echo "-- C2) 外币凭证：原币 100 USD? → EUR 100 × 8.5 = 850 本位币 --"
R=$(call_post "{\"voucher_date\":\"2026-10-15\",\"summary\":\"M104 外币凭证 A\",\"currency_code\":\"EUR\",
  \"entries\":[{\"account_id\":\"$DEBIT_ID\",\"debit\":\"100\"},{\"account_id\":\"$CREDIT_ID\",\"credit\":\"100\"}]}")
echo "  $R"
case "$R" in
    200*) ok "外币凭证建单 200" ;;
    *) bad "外币凭证建单失败：$R" ;;
esac
[[ "$(field currency_code)" == "EUR" ]] && ok "单头币种 = EUR" || bad "单头币种 = $(field currency_code)"
[[ "$(field exchange_rate)" == "8.50000000" ]] && ok "单头锁定汇率 8.5" || bad "单头汇率 = $(field exchange_rate)"
[[ "$(field total_debit)" == "850.0000" ]] && ok "本位币合计 = 850.0000" || bad "本位币合计 = $(field total_debit)"
[[ "$(field 'entries.0.debit_amount')" == "850.0000" ]] && ok "分录本位币金额 850.0000" || bad "分录本位币 = $(field 'entries.0.debit_amount')"
[[ "$(field 'entries.0.fc_debit_amount')" == "100.0000" ]] && ok "分录原币金额 100.0000" || bad "分录原币 = $(field 'entries.0.fc_debit_amount')"
[[ "$(field 'entries.1.fc_credit_amount')" == "100.0000" ]] && ok "贷方原币金额 100.0000" || bad "贷方原币 = $(field 'entries.1.fc_credit_amount')"
V1_ID="$(field id)"

echo
echo "-- C3) 尾差吸收：33.33 + 66.67（借）/ 100（贷）× 8.5 必须严格平衡 --"
R=$(call_post "{\"voucher_date\":\"2026-10-15\",\"summary\":\"M104 外币尾差\",\"currency_code\":\"EUR\",
  \"entries\":[{\"account_id\":\"$DEBIT_ID\",\"debit\":\"33.33\"},{\"account_id\":\"$DEBIT_ID\",\"debit\":\"66.67\"},{\"account_id\":\"$CREDIT_ID\",\"credit\":\"100\"}]}")
echo "  $R"
case "$R" in 200*) ok "尾差凭证建单 200（逐行五入后仍平衡）" ;; *) bad "尾差凭证建单失败：$R" ;; esac
[[ "$(field total_debit)" == "$(field total_credit)" ]] && ok "单头借贷合计相等（$(field total_debit)）" || bad "单头借贷不等：$(field total_debit) / $(field total_credit)"
[[ "$(field balanced)" == "True" ]] && ok "详情接口 balanced = true" || bad "balanced = $(field balanced)"
[[ "$(field 'entries.0.fc_debit_amount')" == "33.3300" ]] && ok "原币金额保持用户输入（33.3300）" || bad "原币 = $(field 'entries.0.fc_debit_amount')"

echo
echo "-- C4) 未标记「外币核算」的科目 → 400 + 66081 --"
R=$(call_post "{\"voucher_date\":\"2026-10-15\",\"summary\":\"M104 未标科目\",\"currency_code\":\"EUR\",
  \"entries\":[{\"account_id\":\"$DEBIT_ID\",\"debit\":\"10\"},{\"account_id\":\"$UNMARKED_ID\",\"credit\":\"10\"}]}")
echo "  $R"
case "$R" in 400*66081*) ok "未标记科目被拒（400 + 66081）" ;; *) bad "未标记科目没被拒：$R" ;; esac
grep -q "M104.03" "$RUN_DIR/body.json" && ok "报文点名了未标记科目（M104.03）" || bad "报文没点名科目"

echo
echo "-- C5) 汇率与当日牌价不一致 → 400 + 10001 --"
R=$(call_post "{\"voucher_date\":\"2026-10-15\",\"summary\":\"M104 汇率不符\",\"currency_code\":\"EUR\",\"exchange_rate\":\"7.2\",
  \"entries\":[{\"account_id\":\"$DEBIT_ID\",\"debit\":\"10\"},{\"account_id\":\"$CREDIT_ID\",\"credit\":\"10\"}]}")
echo "  $R"
case "$R" in 400*10001*) ok "手改汇率被拒（400 + 10001）" ;; *) bad "手改汇率没被拒：$R" ;; esac
python3 - "$RUN_DIR/body.json" <<'PY'
import json,sys
msg=(json.load(open(sys.argv[1])).get("msg") or "")
assert "8.5" in msg, "报文必须给出当日牌价：%s" % msg
print("  PASS  报文给出了当日牌价 8.5")
PY
[[ $? -eq 0 ]] && PASS=$((PASS+1)) || FAIL=$((FAIL+1))

echo
echo "-- C6) 未知币种 → 400 + 66002 --"
R=$(call_post "{\"voucher_date\":\"2026-10-15\",\"summary\":\"M104 未知币种\",\"currency_code\":\"ZZZ\",
  \"entries\":[{\"account_id\":\"$DEBIT_ID\",\"debit\":\"10\"},{\"account_id\":\"$CREDIT_ID\",\"credit\":\"10\"}]}")
echo "  $R"
case "$R" in 400*66002*) ok "未知币种被拒（400 + 66002）" ;; *) bad "未知币种没被拒：$R" ;; esac

echo
echo "-- C7) 币种码形态非法 → 400 + 10001 --"
R=$(call_post "{\"voucher_date\":\"2026-10-15\",\"summary\":\"M104 非法币种码\",\"currency_code\":\"US\",
  \"entries\":[{\"account_id\":\"$DEBIT_ID\",\"debit\":\"10\"},{\"account_id\":\"$CREDIT_ID\",\"credit\":\"10\"}]}")
echo "  $R"
case "$R" in 400*10001*) ok "非法币种码被拒（400 + 10001）" ;; *) bad "非法币种码没被拒：$R" ;; esac

echo
echo "-- C8) 无牌价的外币（HKD）→ 400，绝不按汇率 1 入账 --"
R=$(call_post "{\"voucher_date\":\"2026-10-15\",\"summary\":\"M104 无牌价\",\"currency_code\":\"HKD\",
  \"entries\":[{\"account_id\":\"$DEBIT_ID\",\"debit\":\"10\"},{\"account_id\":\"$CREDIT_ID\",\"credit\":\"10\"}]}")
echo "  $R"
case "$R" in 400*) ok "无牌价被拒（400）" ;; *) bad "无牌价没被拒：$R" ;; esac
grep -q "HKD" "$RUN_DIR/body.json" && ok "报文点名币种 HKD" || bad "报文没点名币种"

echo
echo "-- C9) 不传币种（本位币凭证）→ 行为与本卡之前逐字相同 --"
R=$(call_post "{\"voucher_date\":\"2026-10-15\",\"summary\":\"M104 本位币凭证\",
  \"entries\":[{\"account_id\":\"$DEBIT_ID\",\"debit\":\"100\"},{\"account_id\":\"$CREDIT_ID\",\"credit\":\"100\"}]}")
echo "  $R"
case "$R" in 200*) ok "本位币凭证建单 200" ;; *) bad "本位币凭证建单失败：$R" ;; esac
[[ "$(field currency_code)" == "$BASE_CUR" ]] && ok "单头币种 = 本位币 $BASE_CUR" || bad "单头币种 = $(field currency_code)"
[[ "$(field exchange_rate)" == "1.00000000" ]] && ok "单头汇率 = 1" || bad "单头汇率 = $(field exchange_rate)"
[[ "$(field 'entries.0.fc_debit_amount')" == "None" ]] && ok "本位币分录原币列为 null" || bad "本位币分录原币 = $(field 'entries.0.fc_debit_amount')"

echo
echo "-- C10) 红冲外币凭证：红字凭证继承币种/汇率，原币借贷互换 --"
CODE=$(curl -sS -m 15 -o "$RUN_DIR/audit.json" -w '%{http_code}' -X POST "$BASE/business/erp/voucher/$V1_ID/audit" \
    "${AUD_AUTH[@]}" -H 'Content-Type: application/json' -d '{"remark":"M104 复核"}')
[[ "$CODE" == "200" ]] && ok "复核人审核 200" || bad "审核 HTTP $CODE"
CODE=$(curl -sS -m 15 -o "$RUN_DIR/post.json" -w '%{http_code}' -X POST "$BASE/business/erp/voucher/$V1_ID/post" \
    "${AUD_AUTH[@]}" -H 'Content-Type: application/json' -d '{}')
[[ "$CODE" == "200" ]] && ok "复核人记账 200" || bad "记账 HTTP $CODE"
# 空库首启会种一条 PUBLISHED 的 `voucher_reverse` 审批流，但它解析不出可用审批人
# （演练库只有 admin / m104auditor，`SELF_ONLY` 被内控跳过）。本卡验的是「红冲继承币种/汇率」，
# 不是审批流本身 —— 于是把这条流程在**演练库**里停用，走 ARCH-3 DC-01 的「显式跳过审批」
# （超管 + 理由 ≥10 字），红冲才是同步生效的。
q_db "$DB_E2E" "update biz_flow_def set status='DISABLED'
                 where flow_code='voucher_reverse' or biz_type='VOUCHER_REVERSE'" >/dev/null
CODE=$(curl -sS -m 20 -o "$RUN_DIR/reverse.json" -w '%{http_code}' -X POST "$BASE/business/erp/voucher/$V1_ID/reverse" \
    "${AUD_AUTH[@]}" -H 'Content-Type: application/json' \
    -d '{"reason":"M104 红冲外币凭证","skip_approval":true,"skip_reason":"演练库未配置红冲审批流程，按显式跳过审批直接红冲（M1-04 外币验证）"}')
echo "  红冲 HTTP $CODE"
python3 - "$RUN_DIR/reverse.json" <<'PY'
import json,sys
d=json.load(open(sys.argv[1]))
data=d.get("data") or d
detail=data.get("detail") or data
assert detail.get("is_red") is True, "红冲必须产出红字凭证：%s" % d
assert detail.get("currency_code") == "EUR", "红字凭证必须继承原币币种，实际 %s" % detail.get("currency_code")
assert float(detail.get("exchange_rate")) == 8.5, "红字凭证必须继承汇率"
entries=detail["entries"]
assert float(entries[0]["fc_credit_amount"]) == 100.0, "借方原币 100 → 红字贷方原币 100（实际 %s）" % entries[0]["fc_credit_amount"]
assert float(entries[0]["fc_debit_amount"]) == 0.0, "同边原币必须为 0"
print("  PASS  红字凭证继承 EUR / 8.5，原币借贷互换")
PY
[[ $? -eq 0 ]] && PASS=$((PASS+1)) || FAIL=$((FAIL+1))

echo
echo "-- C11) 详情回读：currency/exchange_rate/原币列都在 --"
CODE=$(curl -sS -m 10 -o "$RUN_DIR/detail.json" -w '%{http_code}' "$BASE/business/erp/voucher/$V1_ID" "${AUTH[@]}")
[[ "$CODE" == "200" ]] && ok "详情 200" || bad "详情 HTTP $CODE"
python3 - "$RUN_DIR/detail.json" <<'PY'
import json,sys
d=json.load(open(sys.argv[1]))["data"]
assert d["currency_code"] == "EUR", d.get("currency_code")
assert float(d["exchange_rate"]) == 8.5
assert d["entries"][0]["fc_debit_amount"] is not None, "原币列必须下发（NULL 与「不存在」不能混）"
print("  PASS  详情字段齐全（币种 / 汇率 / 原币金额）")
PY
[[ $? -eq 0 ]] && PASS=$((PASS+1)) || FAIL=$((FAIL+1))

echo
echo "-- C12) 列表页也能按币种/汇率读回（HEADER_COLUMNS 补列回归）--"
CODE=$(curl -sS -m 10 -o "$RUN_DIR/page.json" -w '%{http_code}' "$BASE/business/erp/voucher/page?page=1&page_size=5" "${AUTH[@]}")
[[ "$CODE" == "200" ]] && ok "列表 200" || bad "列表 HTTP $CODE"
python3 - "$RUN_DIR/page.json" <<'PY'
import json,sys
rows=(json.load(open(sys.argv[1])).get("data") or {}).get("list") or []
assert rows, "列表必须至少有一张凭证"
assert all("currency_code" in row and "exchange_rate" in row for row in rows), "列表行必须带币种/汇率"
print("  PASS  列表 %d 行都带 currency_code / exchange_rate" % len(rows))
PY
[[ $? -eq 0 ]] && PASS=$((PASS+1)) || FAIL=$((FAIL+1))

echo
echo "-- C13) 库里落地形态：单头 EUR/8.5 + 分录原币 100 --"
DBROW="$(q_db "$DB_E2E" "select currency_code||'/'||exchange_rate from biz_voucher where id='$V1_ID'")"
[[ "$DBROW" == "EUR/8.50000000" ]] && ok "库层单头 = EUR/8.5" || bad "库层单头 = $DBROW"
DBFC="$(q_db "$DB_E2E" "select fc_debit_amount||'/'||debit_amount from biz_voucher_entry where voucher_id='$V1_ID' and line_no=1")"
[[ "$DBFC" == "100.0000/850.0000" ]] && ok "库层分录 = 原币 100 / 本位币 850" || bad "库层分录 = $DBFC"

echo
echo "== 结果：PASS=$PASS FAIL=$FAIL =="
[[ $FAIL -eq 0 ]] || exit 1
