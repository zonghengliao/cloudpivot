#!/usr/bin/env bash
# =============================================================================
# m105_fx_reval_engine_and_api.sh —— V0410（M1-05 期末调汇）真引擎 + 真 HTTP 实测
#
# 卡：t_02c8407a。**不是** psql 手动跑 SQL，而是用编译出的后端二进制真启动，
# 走引擎自己的迁移器（schema_migrations 指纹 / 幂等 / 顺序）与真 HTTP 路由。
#
# 三段：
#   A) 克隆库升级：模板库 cloudpivot_test（停版 V0404）克隆成 cp_m105_upgrade → 真启动
#      → 应用 V0405..V0410 → 断言 V0410 自检 NOTICE / schema_migrations 登记 / 权限节点
#      / FINANCE_MANAGER 授权 / 重启 0 个迁移
#   B) 空库首启：空库 cp_m105_e2e → 真启动 → 全量迁移（含 V0410 自检块）
#   C) 真 HTTP：登录 admin → 期末调汇十二条路径
#      （试算 / 落账 / 凭证核对 / 幂等 / 已结账 409 / 未开账 409 / 非法 UUID 400 /
#        无权限 403 / 缺牌价 400 + 66082 / 再跑一次不再落账）
#
# 用法：bash scripts/verify/m105_fx_reval_engine_and_api.sh [PG_HOST]
#   默认 PG_HOST=172.18.10.145（99 上跑）；145 上跑传 127.0.0.1。
# 端口固定 18095（隔离段 18000-18999），库名含 e2e/upgrade，脚本自己 DROP。
# =============================================================================
set -uo pipefail

PG_HOST="${1:-172.18.10.145}"
export PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4
PG_USER=cloudpivot
TEMPLATE=cloudpivot_test
DB_UP=cp_m105_upgrade
DB_E2E=cp_m105_e2e
PORT=18095
BASE="http://127.0.0.1:$PORT/api"
REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
BACKEND_DIR="$REPO_ROOT/backend"
BIN="$BACKEND_DIR/target/debug/cloudpivot"
RUN_DIR="${M105_RUN_DIR:-/root/.hermes/profiles/backend-developer/cache/scratch/m105-run}"
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

# 管理员初始口令：`bootstrap::seed_super_admin` 只认 `NIK_ADMIN_PASSWORD`（仓库内无缺省口令）
ADMIN_PASSWORD='M105@Drill#2026'

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
        export X_CONFIG_SECRET_KEY="m105-drill-config-secret"
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
echo "== A) 克隆库升级（真引擎：模板库 → cp_m105_upgrade）=="
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
[[ "${APPLIED_N:-0}" -ge 1 ]] && ok "应用了 $APPLIED_N 个迁移（含 V0410）" || bad "一个迁移都没应用 —— V0410 没被引擎识别"
grep -q "V0410 自检通过" "$RUN_DIR/upgrade.log" && ok "V0410 自检块在真引擎里通过（NOTICE 已打印）" || bad "没看到 V0410 自检 NOTICE"
ROW="$(q_db "$DB_UP" "select version||'/'||success||'/'||source from schema_migrations where name='period_fx_revaluation'")"
echo "  schema_migrations：$ROW"
case "$ROW" in
    410/true/*V0410__period_fx_revaluation.up.sql) ok "schema_migrations 登记 410/true/源文件" ;;
    *) bad "schema_migrations 登记不对：[$ROW]" ;;
esac
NODE="$(q_db "$DB_UP" "select menu_type||'/'||coalesce(parent_id::text,'-')||'/'||sort_order||'/'||status||'/'||title
        from sys_menu where auth_code='business:erp:period:fx-reval' and deleted_at is null")"
echo "  权限节点：$NODE"
[[ "$NODE" == "3/20000000-0000-0000-0000-000000000951/6/1/期末调汇" ]] \
    && ok "菜单按钮行：type=3 / 父=会计期间 / sort=6 / 启用 / title=期末调汇" || bad "权限节点不对：[$NODE]"
GRANTS="$(q_db "$DB_UP" "select count(*) from sys_role_menu rm join sys_role r on r.id=rm.role_id and r.deleted_at is null
        where rm.menu_id='20000000-0000-0000-0000-000000001730'::uuid
          and r.role_code in ('SUPER_ADMIN','FINANCE_MANAGER')")"
[[ "${GRANTS:-0}" -ge 2 ]] && ok "授权已落地：$GRANTS 个目标角色（SUPER_ADMIN / FINANCE_MANAGER）" || bad "授权只到 ${GRANTS:-0} 行"
DUP="$(q_db "$DB_UP" "select count(*) from (select sort_order from sys_menu
        where parent_id='20000000-0000-0000-0000-000000000951'::uuid and menu_type=3 and deleted_at is null
        group by sort_order having count(*)>1) x")"
[[ "$DUP" == "0" ]] && ok "「会计期间」页同父 sort_order 无重复" || bad "同父 sort_order 有 $DUP 组重复"

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
echo "== B) 空库首启（全量迁移 + V0410 自检；真 HTTP 的落点）=="
psql_admin "CREATE DATABASE $DB_E2E" >/dev/null || { echo "FATAL: 建空库失败"; exit 2; }
start_backend "$DB_E2E" "$RUN_DIR/e2e.log" || { echo "FATAL: 空库实例没起来"; tail -30 "$RUN_DIR/e2e.log"; exit 2; }
APPLIED3="$(grep -oE '数据库迁移完成: 应用 [0-9]+ 个迁移' "$RUN_DIR/e2e.log" | head -1)"
echo "  ${APPLIED3}"
CNT3="$(q_db "$DB_E2E" "select count(*) from schema_migrations where kind='versioned'")"
[[ "${CNT3:-0}" -ge 330 ]] && ok "空库首启 versioned 迁移 = $CNT3（≥330：V0410 至少是第 330 条）" || bad "空库首启 versioned = $CNT3（期望 ≥330）"
grep -q "V0410 自检通过" "$RUN_DIR/e2e.log" && ok "空库首启也跑了 V0410 自检" || bad "空库首启没有 V0410 自检 NOTICE"
curl -fsS -m 3 "http://127.0.0.1:$PORT/health" >/dev/null && ok "/health 200" || bad "/health 不通"

q_db "$DB_E2E" "update sys_user set must_change_password = false where username = 'admin'" >/dev/null
[[ "$(q_db "$DB_E2E" "select must_change_password from sys_user where username='admin'")" == "f" ]] \
    && ok "演练库：清掉 admin 的 must_change_password（仅为跑通本卡接口）" || bad "清 must_change_password 失败"

# ============================================================================
echo
echo "== C) 真 HTTP：期末调汇 =="
TOKEN="$(curl -sS -m 10 -X POST "$BASE/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"admin\",\"password\":\"$ADMIN_PASSWORD\"}" \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print((d.get("data") or {}).get("access_token") or "")' 2>/dev/null)"
[[ -n "${TOKEN:-}" ]] && ok "登录成功（access_token ${#TOKEN} 字符）" || { bad "登录失败"; stop_backend; exit 2; }
AUTH=(-H "Authorization: Bearer ${TOKEN}")

# ---------- 夹具：期间 / 科目 / 牌价 / 一张已过账的外币凭证 ----------
q_db "$DB_E2E" "insert into biz_accounting_period (period_year, period_month, period_code, start_date, end_date, status)
                values (2026, 10, '2026-10', date '2026-10-01', date '2026-10-31', 'OPEN')
                on conflict (period_code) do update set status='OPEN', closed_at=null, closed_by=null" >/dev/null
q_db "$DB_E2E" "insert into biz_accounting_period (period_year, period_month, period_code, start_date, end_date, status)
                values (2026, 11, '2026-11', date '2026-11-01', date '2026-11-30', 'NOT_OPENED')
                on conflict (period_code) do update set status='NOT_OPENED', closed_at=null, closed_by=null" >/dev/null
for spec in "M105.01|M1-05 外币科目甲|1" "M105.02|M1-05 外币科目乙|1"; do
    CODE="${spec%%|*}"; REST="${spec#*|}"; NAME="${REST%%|*}"; FC="${REST##*|}"
    q_db "$DB_E2E" "insert into biz_account (account_code, account_name, level, account_category, balance_direction, is_leaf, status, is_foreign_currency)
                    values ('$CODE', '$NAME', 1, 'ASSET', 'DEBIT', 1, 1, $FC)" >/dev/null
done
ACC_A="$(q_db "$DB_E2E" "select id from biz_account where account_code='M105.01'")"
ACC_B="$(q_db "$DB_E2E" "select id from biz_account where account_code='M105.02'")"
BASE_CUR="$(q_db "$DB_E2E" "select code from biz_currency where is_base and status='ACTIVE' limit 1")"
PERIOD_ID="$(q_db "$DB_E2E" "select id from biz_accounting_period where period_code='2026-10'")"
PERIOD_NOT_OPENED_ID="$(q_db "$DB_E2E" "select id from biz_accounting_period where period_code='2026-11'")"
[[ -n "$ACC_A" && -n "$ACC_B" && -n "$BASE_CUR" && -n "$PERIOD_ID" && -n "$PERIOD_NOT_OPENED_ID" ]] \
    && ok "夹具就位（2 个外币科目 / 本位币 $BASE_CUR / 期间 2026-10 + 2026-11）" || { bad "夹具没建出来"; stop_backend; exit 2; }

q_db "$DB_E2E" "insert into biz_currency (code, name, symbol, decimal_places, is_base, status)
                values ('USD','美元','\$',2,false,'ACTIVE') on conflict (code) do nothing" >/dev/null
q_db "$DB_E2E" "delete from biz_exchange_rate where from_currency='USD' and to_currency='$BASE_CUR'" >/dev/null
# V0407 的区间排除约束：两行必须显式给 effective_to（含当日），否则第二行 NULL 会覆盖到无穷远
# 与第一行重叠 → ex_biz_exchange_rate_period 报冲突（引擎的写入口自己派生这一列，脚本要自己填）。
q_db "$DB_E2E" "insert into biz_exchange_rate (from_currency, to_currency, rate, rate_type, effective_date, effective_to)
                values ('USD','$BASE_CUR',7.0,'SPOT',date '2026-10-01',date '2026-10-30'),
                       ('USD','$BASE_CUR',7.5,'SPOT',date '2026-10-31',null)" >/dev/null
RATE_N="$(q_db "$DB_E2E" "select count(*) from biz_exchange_rate where from_currency='USD' and to_currency='$BASE_CUR'")"
[[ "$RATE_N" == "2" ]] && ok "牌价夹具：10-01~10-30 = 7.0、10-31 起 = 7.5" || bad "牌价夹具没落库（$RATE_N 行）"

# 已过账的外币凭证（100 USD @ 7.0 = 700 本位币；两行同币种 ⇒ 对方科目原币 -100）
q_db "$DB_E2E" "insert into biz_voucher (voucher_word, voucher_no, period, voucher_date, total_debit, total_credit,
                                        entry_count, status, source_type, source_no, currency_code, exchange_rate, remark)
                values ('记','M105-DRILL-1','2026-10',date '2026-10-10',700,700,2,'POSTED','M105','M105-DRILL-1','USD',7.0,'M1-05 演练夹具')
                on conflict do nothing" >/dev/null
V1="$(q_db "$DB_E2E" "select id from biz_voucher where voucher_no='M105-DRILL-1' and deleted_at is null limit 1")"
q_db "$DB_E2E" "insert into biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name,
                                              debit_amount, credit_amount, fc_debit_amount, fc_credit_amount)
                values ('$V1'::uuid,1,'M105 借','$ACC_A'::uuid,'M105.01','M1-05 外币科目甲',700,0,100,0),
                       ('$V1'::uuid,2,'M105 贷','$ACC_B'::uuid,'M105.02','M1-05 外币科目乙',0,700,0,100)" >/dev/null
# 再灌一笔**本位币**分录到乙科目：只改账面、不改原币 ⇒ 净差额 ≠ 0（逼出 5603 行）
q_db "$DB_E2E" "insert into biz_voucher (voucher_word, voucher_no, period, voucher_date, total_debit, total_credit,
                                        entry_count, status, source_type, source_no, currency_code, exchange_rate, remark)
                values ('记','M105-DRILL-2','2026-10',date '2026-10-20',30,30,2,'POSTED','M105','M105-DRILL-2','$BASE_CUR',1,'M1-05 本位币调整夹具')
                on conflict do nothing" >/dev/null
V2="$(q_db "$DB_E2E" "select id from biz_voucher where voucher_no='M105-DRILL-2' and deleted_at is null limit 1")"
# 纯本位币科目：空库首启不保证种了 1002 这类科目，自己建一个（不标外币）
q_db "$DB_E2E" "insert into biz_account (account_code, account_name, level, account_category, balance_direction, is_leaf, status, is_foreign_currency)
                values ('M105.CNY','M1-05 本位币科目', 1, 'ASSET', 'DEBIT', 1, 1, 0)" >/dev/null
CASH_ID="$(q_db "$DB_E2E" "select id from biz_account where account_code='M105.CNY'")"
[[ -n "$CASH_ID" ]] || { bad "本位币科目 M105.CNY 没建出来"; stop_backend; exit 2; }
q_db "$DB_E2E" "insert into biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name,
                                              debit_amount, credit_amount)
                values ('$V2'::uuid,1,'M105 本位币调整 借','$ACC_B'::uuid,'M105.02','M1-05 外币科目乙',30,0),
                       ('$V2'::uuid,2,'M105 本位币调整 贷','$CASH_ID'::uuid,'M105.CNY','M1-05 本位币科目',0,30)" >/dev/null
CNY_ROWS="$(q_db "$DB_E2E" "select count(*) from biz_voucher_entry where voucher_id='$V2'::uuid")"
[[ "$CNY_ROWS" == "2" ]] && ok "本位币调整凭证 2 行已落库（只改账面、不改原币）" || bad "本位币调整凭证分录缺失（$CNY_ROWS 行）"
[[ -n "$V1" && -n "$V2" ]] && ok "凭证夹具已过账（外币 100 USD @ 7.0 + 本位币调整 30）" || bad "凭证夹具没落库"

# ---------- ① 试算（dry_run）：算出差额但不落账 ----------
RESP="$(curl -sS -m 15 -X POST "$BASE/business/erp/period/$PERIOD_ID/fx-reval" "${AUTH[@]}" \
    -H 'Content-Type: application/json' -d '{"dry_run":true}')"
echo "  试算响应：$(echo "$RESP" | head -c 600)"
read -r CODE_NET CODE_LINES CODE_Dry CODE_VID <<<"$(echo "$RESP" | python3 -c '
import json,sys
d=json.load(sys.stdin)
x=d.get("data") or {}
print(x.get("net","-"), len(x.get("lines") or []), x.get("dry_run"), x.get("voucher_id") or "-")
' 2>/dev/null)"
[[ "$CODE_LINES" == "2" ]] && ok "试算：2 个外币科目各一行" || bad "试算行数不对：[$CODE_LINES]"
[[ "$CODE_NET" == "-30.0000" || "$CODE_NET" == "-30" ]] && ok "试算：净差额 = -30（甲 +50 / 乙 -80）" || bad "试算净差额异常：[$CODE_NET]"
[[ "$CODE_Dry" == "True" || "$CODE_Dry" == "true" ]] && ok "试算：dry_run=true" || bad "试算没标 dry_run：[$CODE_Dry]"
[[ "$CODE_VID" == "-" ]] && ok "试算：没有生成凭证" || bad "试算竟然落了凭证：[$CODE_VID]"
[[ "$(q_db "$DB_E2E" "select count(*) from biz_voucher where source_type='FX_REVAL_ACCOUNT' and deleted_at is null")" == "0" ]] \
    && ok "试算后库里没有 FX_REVAL_ACCOUNT 凭证" || bad "试算留下了凭证"

# ---------- ② 落账 ----------
RESP="$(curl -sS -m 20 -X POST "$BASE/business/erp/period/$PERIOD_ID/fx-reval" "${AUTH[@]}" \
    -H 'Content-Type: application/json' -d '{}')"
echo "  落账响应：$(echo "$RESP" | head -c 700)"
read -r POST_NET POST_LINES POST_VID POST_NO POST_SKIP <<<"$(echo "$RESP" | python3 -c '
import json,sys
d=json.load(sys.stdin)
x=d.get("data") or {}
print(x.get("net","-"), len(x.get("lines") or []), x.get("voucher_id") or "-", x.get("voucher_no") or "-", x.get("skipped"))
' 2>/dev/null)"
[[ "$POST_LINES" == "2" ]] && ok "落账：2 行科目调整" || bad "落账行数不对：[$POST_LINES]"
[[ "$POST_NO" != "-" ]] && ok "落账：生成调汇凭证 $POST_NO" || bad "落账没生成凭证"
VID="$POST_VID"
# 甲 +50 / 乙 -80 ⇒ 净损失 30 ⇒ 追加 5603 借方 30
[[ "$POST_NET" == "-30.0000" || "$POST_NET" == "-30" ]] \
    && ok "落账：净差额 = -30（净汇兑损失）" || bad "净差额异常：[$POST_NET]"
HDR="$(q_db "$DB_E2E" "select currency_code||'/'||exchange_rate||'/'||status||'/'||source_type||'/'||period
        from biz_voucher where id='$VID'::uuid")"
[[ "$HDR" == "$BASE_CUR/1.00000000/POSTED/FX_REVAL_ACCOUNT/2026-10" ]] \
    && ok "调汇凭证：本位币 / rate=1 / POSTED / FX_REVAL_ACCOUNT / 期间 2026-10（实得 $HDR）" || bad "调汇凭证单头不对：[$HDR]"
BAL="$(q_db "$DB_E2E" "select coalesce(sum(debit_amount),0)||'/'||coalesce(sum(credit_amount),0)||'/'||count(*)
        from biz_voucher_entry where voucher_id='$VID'::uuid")"
echo "  调汇凭证分录：借/贷/行数 = $BAL"
[[ "$BAL" == "80.0000/80.0000/3" ]] && ok "调汇凭证：3 行（甲 / 乙 / 5603）且借贷相等 80" || bad "调汇凭证分录异常：[$BAL]"
FCNULL="$(q_db "$DB_E2E" "select count(*) from biz_voucher_entry where voucher_id='$VID'::uuid and fc_debit_amount is not null")"
[[ "$FCNULL" == "0" ]] && ok "调汇分录不带原币（本位币凭证）" || bad "调汇分录带了原币：$FCNULL 行"
DET="$(q_db "$DB_E2E" "select string_agg(account_code||':'||debit_amount||':'||credit_amount, ' | ' order by line_no)
        from biz_voucher_entry where voucher_id='$VID'::uuid")"
echo "  分录明细：$DET"
case "$DET" in
    "M105.01:50.0000:0.0000 | M105.02:0.0000:80.0000 | 5603:30.0000:0.0000") ok "逐科目差额正确（甲借 50 / 乙贷 80 / 5603 借 30）" ;;
    *) bad "逐科目差额不对：[$DET]" ;;
esac

# ---------- ③ 幂等：再跑一次不重复落账 ----------
RESP="$(curl -sS -m 15 -X POST "$BASE/business/erp/period/$PERIOD_ID/fx-reval" "${AUTH[@]}" \
    -H 'Content-Type: application/json' -d '{}')"
SKIP="$(echo "$RESP" | python3 -c 'import json,sys; print((json.load(sys.stdin).get("data") or {}).get("skipped"))' 2>/dev/null)"
[[ "$SKIP" == "True" || "$SKIP" == "true" ]] && ok "幂等：同期间第二次 skipped=true" || bad "幂等失效：skipped=[$SKIP]"
[[ "$(q_db "$DB_E2E" "select count(*) from biz_voucher where source_type='FX_REVAL_ACCOUNT' and is_red=0 and deleted_at is null")" == "1" ]] \
    && ok "幂等：库里仍只有 1 张调汇凭证" || bad "重复落账"

# ---------- ④ 已结账期间 ⇒ 409 ----------
q_db "$DB_E2E" "update biz_accounting_period set status='CLOSED', closed_at=now() where id='$PERIOD_ID'::uuid" >/dev/null
STATUS="$(curl -sS -m 10 -o "$RUN_DIR/closed.json" -w '%{http_code}' -X POST \
    "$BASE/business/erp/period/$PERIOD_ID/fx-reval" "${AUTH[@]}" -H 'Content-Type: application/json' -d '{}')"
[[ "$STATUS" == "409" ]] && ok "已结账期间 ⇒ 409（$(python3 -c 'import json;d=json.load(open("'"$RUN_DIR"'/closed.json"));print((d.get("msg") or "")[:40])' 2>/dev/null)…）" || bad "已结账期间返回 $STATUS（期望 409）"
q_db "$DB_E2E" "update biz_accounting_period set status='OPEN', closed_at=null, closed_by=null where id='$PERIOD_ID'::uuid" >/dev/null

# ---------- ⑤ 未开账期间 ⇒ 409 ----------
STATUS="$(curl -sS -m 10 -o "$RUN_DIR/notopened.json" -w '%{http_code}' -X POST \
    "$BASE/business/erp/period/$PERIOD_NOT_OPENED_ID/fx-reval" "${AUTH[@]}" -H 'Content-Type: application/json' -d '{}')"
[[ "$STATUS" == "409" ]] && ok "未开账期间 ⇒ 409" || bad "未开账期间返回 $STATUS（期望 409）"

# ---------- ⑥ 非法 UUID ⇒ 400 ----------
STATUS="$(curl -sS -m 10 -o /dev/null -w '%{http_code}' -X POST \
    "$BASE/business/erp/period/not-a-uuid/fx-reval" "${AUTH[@]}" -H 'Content-Type: application/json' -d '{}')"
[[ "$STATUS" == "400" ]] && ok "非法 UUID ⇒ 400" || bad "非法 UUID 返回 $STATUS（期望 400）"

# ---------- ⑦ 缺牌价 ⇒ 400 + 66082（另开期间 2026-12，只用原有 10 月牌价之外的币种）----------
q_db "$DB_E2E" "insert into biz_accounting_period (period_year, period_month, period_code, start_date, end_date, status)
                values (2026, 12, '2026-12', date '2026-12-01', date '2026-12-31', 'OPEN')
                on conflict (period_code) do update set status='OPEN', closed_at=null, closed_by=null" >/dev/null
P12="$(q_db "$DB_E2E" "select id from biz_accounting_period where period_code='2026-12'")"
q_db "$DB_E2E" "delete from biz_exchange_rate where from_currency='HKD'" >/dev/null
q_db "$DB_E2E" "insert into biz_currency (code, name, symbol, decimal_places, is_base, status)
                values ('HKD','港元','HK\$',2,false,'ACTIVE') on conflict (code) do nothing" >/dev/null
q_db "$DB_E2E" "insert into biz_account (account_code, account_name, level, account_category, balance_direction, is_leaf, status, is_foreign_currency)
                values ('M105.03','M1-05 港元科目', 1, 'ASSET', 'DEBIT', 1, 1, 1)" >/dev/null
ACC_C="$(q_db "$DB_E2E" "select id from biz_account where account_code='M105.03'")"
q_db "$DB_E2E" "insert into biz_voucher (voucher_word, voucher_no, period, voucher_date, total_debit, total_credit,
                                        entry_count, status, source_type, source_no, currency_code, exchange_rate, remark)
                values ('记','M105-DRILL-HKD','2026-12',date '2026-12-05',100,100,2,'POSTED','M105','M105-DRILL-HKD','HKD',1,'M1-05 缺牌价夹具')
                on conflict do nothing" >/dev/null
VH="$(q_db "$DB_E2E" "select id from biz_voucher where voucher_no='M105-DRILL-HKD' and deleted_at is null limit 1")"
q_db "$DB_E2E" "insert into biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name,
                                              debit_amount, credit_amount, fc_debit_amount, fc_credit_amount)
                values ('$VH'::uuid,1,'M105 HKD 借','$ACC_C'::uuid,'M105.03','M1-05 港元科目',100,0,100,0),
                       ('$VH'::uuid,2,'M105 HKD 贷','$ACC_B'::uuid,'M105.02','M1-05 外币科目乙',0,100,0,100)" >/dev/null
STATUS="$(curl -sS -m 10 -o "$RUN_DIR/norate.json" -w '%{http_code}' -X POST \
    "$BASE/business/erp/period/$P12/fx-reval" "${AUTH[@]}" -H 'Content-Type: application/json' -d '{}')"
NR_CODE="$(python3 -c 'import json;d=json.load(open("'"$RUN_DIR"'/norate.json"));print(d.get("code"))' 2>/dev/null)"
NR_MSG="$(python3 -c 'import json;d=json.load(open("'"$RUN_DIR"'/norate.json"));print((d.get("msg") or "")[:90])' 2>/dev/null)"
[[ "$STATUS" == "400" && "$NR_CODE" == "66082" ]] && ok "缺期末牌价 ⇒ 400 + 66082（$NR_MSG…）" || bad "缺牌价返回 $STATUS / code=$NR_CODE（期望 400/66082）"
case "$NR_MSG" in
    *HKD*) ok "缺牌价文案点名币种 HKD" ;;
    *) bad "缺牌价文案没点名币种：[$NR_MSG]" ;;
esac

# ---------- ⑧ 无权限 ⇒ 403 ----------
q_db "$DB_E2E" "insert into sys_role (role_code, role_name, status)
                select 'M105_NOPERM','M1-05 无权限角色',1
                 where not exists (select 1 from sys_role where role_code='M105_NOPERM' and deleted_at is null)" >/dev/null
ROL="$(q_db "$DB_E2E" "select id from sys_role where role_code='M105_NOPERM' and deleted_at is null limit 1")"
q_db "$DB_E2E" "insert into sys_user (username, password, must_change_password)
                select 'm105nobody', password, false from sys_user where username='admin'
                  and not exists (select 1 from sys_user u where u.username='m105nobody' and u.deleted_at is null)" >/dev/null
q_db "$DB_E2E" "update sys_user set password=(select password from sys_user where username='admin'),
                        must_change_password=false where username='m105nobody'" >/dev/null
UID_N="$(q_db "$DB_E2E" "select id from sys_user where username='m105nobody' and deleted_at is null limit 1")"
q_db "$DB_E2E" "insert into sys_user_role (user_id, role_id)
                select '$UID_N'::uuid, '$ROL'::uuid
                 where not exists (select 1 from sys_user_role where user_id='$UID_N'::uuid and role_id='$ROL'::uuid)" >/dev/null
TOKEN_N="$(curl -sS -m 10 -X POST "$BASE/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"m105nobody\",\"password\":\"$ADMIN_PASSWORD\"}" \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print((d.get("data") or {}).get("access_token") or "")' 2>/dev/null)"
if [[ -n "${TOKEN_N:-}" ]]; then
    STATUS="$(curl -sS -m 10 -o /dev/null -w '%{http_code}' -X POST \
        "$BASE/business/erp/period/$PERIOD_ID/fx-reval" -H "Authorization: Bearer ${TOKEN_N}" \
        -H 'Content-Type: application/json' -d '{}')"
    [[ "$STATUS" == "403" ]] && ok "无 business:erp:period:fx-reval ⇒ 403" || bad "无权限返回 $STATUS（期望 403）"
else
    bad "无权限账号登录失败（无法验证 403）"
fi

# ---------- ⑨ 调汇后再看调汇凭证是否污染原币余额 ----------
FCB="$(q_db "$DB_E2E" "select coalesce(sum(coalesce(fc_debit_amount,debit_amount)-coalesce(fc_credit_amount,credit_amount)),0)::numeric(20,4)
        from biz_voucher_entry e join biz_voucher v on v.id=e.voucher_id
        where e.account_code='M105.01' and v.status='POSTED' and v.deleted_at is null and v.currency_code<>'$BASE_CUR'")"
[[ "$FCB" == "100.0000" ]] && ok "调汇凭证不污染原币余额（M105.01 仍是 100 USD）" || bad "原币余额被调汇影响：[$FCB]"

echo
echo "== 汇总：PASS=$PASS FAIL=$FAIL =="
[[ "$FAIL" == "0" ]] || exit 1
