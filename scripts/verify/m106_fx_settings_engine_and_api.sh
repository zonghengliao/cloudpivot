#!/usr/bin/env bash
# =============================================================================
# m106_fx_settings_engine_and_api.sh —— V0411（M1-06 汇兑损益科目配置）真引擎 + 真 HTTP
#
# 卡：t_dc8892cb。**不是** psql 手动跑 SQL，而是用编译出的后端二进制真启动，
# 走引擎自己的迁移器（schema_migrations 指纹 / 幂等 / 顺序）与真 HTTP 路由。
#
# 三段：
#   A) 克隆库升级：模板库 cloudpivot_test（停版 V0404）克隆成 cp_m106_upgrade → 真启动
#      → 应用 V0405..V0411 → 断言 V0411 自检 NOTICE / schema_migrations 登记 / 参数行 /
#      权限节点 / FINANCE_MANAGER 授权 / 重启 0 个迁移
#   B) 空库首启：空库 cp_m106_e2e → 真启动 → 全量迁移（含 V0411 自检块）+ 参数种子
#   C) 真 HTTP：
#      ① GET 配置（5603 / source=CONFIG / 候选含 5603·5601）
#      ② 四条校验各拦一次（资产类 / 非末级 / 不存在 / 空值）
#      ③ PUT 5601 成功 → 读回 200 + 库里 config_value 真的变了（非「只改了返回体」）
#      ④ 同值再 PUT = 幂等（updated_at 不变）
#      ⑤ 只读账号：GET 200（复用 fx-rate:list）、PUT 403（缺 fx-settings:update）
#      ⑥ 科目被停用后：GET 200 + problem 点名、PUT 该科目 400 + 66083
#      ⑦ 恢复 5603
#
# 用法：bash scripts/verify/m106_fx_settings_engine_and_api.sh [PG_HOST]
#   默认 PG_HOST=172.18.10.145（99 上跑）；145 上跑传 127.0.0.1。
# 端口固定 18096（隔离段 18000-18999），库名含 e2e/upgrade，脚本自己 DROP。
# =============================================================================
set -uo pipefail

PG_HOST="${1:-172.18.10.145}"
export PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4
PG_USER=cloudpivot
TEMPLATE=cloudpivot_test
DB_UP=cp_m106_upgrade
DB_E2E=cp_m106_e2e
PORT=18096
BASE="http://127.0.0.1:$PORT/api"
REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
BACKEND_DIR="$REPO_ROOT/backend"
BIN="$BACKEND_DIR/target/debug/cloudpivot"
RUN_DIR="${M106_RUN_DIR:-/root/.hermes/profiles/backend-developer/cache/scratch/m106-run}"
KEY='erp.fx.gain_loss_account'
NODE_ID='20000000-0000-0000-0000-000000001740'
PAGE_ID='20000000-0000-0000-0000-000000003168'
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
ADMIN_PASSWORD='M106@Drill#2026'

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
        export X_CONFIG_SECRET_KEY="m106-drill-config-secret"
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
echo "== A) 克隆库升级（真引擎：模板库 → cp_m106_upgrade）=="
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
[[ "${APPLIED_N:-0}" -ge 1 ]] && ok "应用了 $APPLIED_N 个迁移（含 V0411）" || bad "一个迁移都没应用 —— V0411 没被引擎识别"
grep -q "V0411 自检通过" "$RUN_DIR/upgrade.log" && ok "V0411 自检块在真引擎里通过（NOTICE 已打印）" || bad "没看到 V0411 自检 NOTICE"
ROW="$(q_db "$DB_UP" "select version||'/'||success||'/'||source from schema_migrations where name='fx_gain_loss_account'")"
echo "  schema_migrations：$ROW"
case "$ROW" in
    411/true/*V0411__fx_gain_loss_account.up.sql) ok "schema_migrations 登记 411/true/源文件" ;;
    *) bad "schema_migrations 登记不对：[$ROW]" ;;
esac
CFG="$(q_db "$DB_UP" "select config_value||'/'||config_type from sys_config where config_key='$KEY' and deleted_at is null")"
[[ "$CFG" == "5603/STRING" ]] && ok "升级后参数行 = 5603/STRING" || bad "升级后参数行不对：[$CFG]"
NODE="$(q_db "$DB_UP" "select menu_type||'/'||coalesce(parent_id::text,'-')||'/'||sort_order||'/'||status||'/'||title
        from sys_menu where auth_code='business:erp:fx-settings:update' and deleted_at is null")"
echo "  权限节点：$NODE"
[[ "$NODE" == "3/$PAGE_ID/60/1/汇兑损益科目" ]] \
    && ok "菜单按钮行：type=3 / 父=汇率管理 / sort=60 / 启用 / title=汇兑损益科目" || bad "权限节点不对：[$NODE]"
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
echo "== B) 空库首启（全量迁移 + V0411 自检；真 HTTP 的落点）=="
psql_admin "CREATE DATABASE $DB_E2E" >/dev/null || { echo "FATAL: 建空库失败"; exit 2; }
start_backend "$DB_E2E" "$RUN_DIR/e2e.log" || { echo "FATAL: 空库实例没起来"; tail -30 "$RUN_DIR/e2e.log"; exit 2; }
APPLIED3="$(grep -oE '数据库迁移完成: 应用 [0-9]+ 个迁移' "$RUN_DIR/e2e.log" | head -1)"
echo "  ${APPLIED3}"
CNT3="$(q_db "$DB_E2E" "select count(*) from schema_migrations where kind='versioned'")"
[[ "${CNT3:-0}" -ge 330 ]] && ok "空库首启 versioned 迁移 = $CNT3（≥330：V0411 至少是第 330 条）" || bad "空库首启 versioned = $CNT3（期望 ≥330）"
grep -q "V0411 自检通过" "$RUN_DIR/e2e.log" && ok "空库首启也跑了 V0411 自检" || bad "空库首启没有 V0411 自检 NOTICE"
curl -fsS -m 3 "http://127.0.0.1:$PORT/health" >/dev/null && ok "/health 200" || bad "/health 不通"
CFG="$(q_db "$DB_E2E" "select config_value from sys_config where config_key='$KEY' and deleted_at is null")"
[[ "$CFG" == "5603" ]] && ok "空库首启：参数种子 = 5603" || bad "空库首启参数种子不对：[$CFG]"

q_db "$DB_E2E" "update sys_user set must_change_password = false where username = 'admin'" >/dev/null
[[ "$(q_db "$DB_E2E" "select must_change_password from sys_user where username='admin'")" == "f" ]] \
    && ok "演练库：清掉 admin 的 must_change_password（仅为跑通本卡接口）" || bad "清 must_change_password 失败"

# ============================================================================
echo
echo "== C) 真 HTTP：汇兑损益科目配置 =="
TOKEN="$(curl -sS -m 10 -X POST "$BASE/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"admin\",\"password\":\"$ADMIN_PASSWORD\"}" \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print((d.get("data") or {}).get("access_token") or "")' 2>/dev/null)"
[[ -n "${TOKEN:-}" ]] && ok "登录成功（access_token ${#TOKEN} 字符）" || { bad "登录失败"; stop_backend; exit 2; }
AUTH=(-H "Authorization: Bearer $TOKEN")

json_field() { # json_field <file> <py-expr on d["data"]>
    python3 -c "
import json,sys
d=json.load(open('$1'))
x=d.get('data') or {}
print(x$2)
" 2>/dev/null
}

# ---------- ① GET 配置 ----------
STATUS="$(curl -sS -m 10 -o "$RUN_DIR/fx_get.json" -w '%{http_code}' "$BASE/business/erp/fx-settings" "${AUTH[@]}")"
echo "  GET 响应：$(head -c 400 "$RUN_DIR/fx_get.json")"
[[ "$STATUS" == "200" ]] && ok "GET /business/erp/fx-settings ⇒ 200" || bad "GET 返回 $STATUS"
[[ "$(json_field "$RUN_DIR/fx_get.json" ".get('config_key')")" == "$KEY" ]] && ok "config_key = $KEY" || bad "config_key 不对"
[[ "$(json_field "$RUN_DIR/fx_get.json" ".get('account_code')")" == "5603" ]] && ok "account_code = 5603（迁移种子）" || bad "account_code 不对"
[[ "$(json_field "$RUN_DIR/fx_get.json" ".get('account_name')")" == "财务费用" ]] && ok "account_name = 财务费用（科目名回填）" || bad "account_name 不对"
[[ "$(json_field "$RUN_DIR/fx_get.json" ".get('source')")" == "CONFIG" ]] && ok "source = CONFIG（参数里配的）" || bad "source 不对"
[[ "$(json_field "$RUN_DIR/fx_get.json" ".get('problem')")" == "None" ]] && ok "problem = null（种子配置可用）" || bad "problem 非空：$(json_field "$RUN_DIR/fx_get.json" ".get('problem')")"
CANDS="$(json_field "$RUN_DIR/fx_get.json" "['candidates']")"
case "$CANDS" in
    *"'code': '5603'"*|*'"code": "5603"'*) ok "候选含 5603（页面能选中当前值）" ;;
    *) bad "候选里没有 5603：$(echo "$CANDS" | head -c 200)" ;;
esac
case "$CANDS" in
    *5601*) ok "候选含 5601 销售费用（末级损益类）" ;;
    *) bad "候选里没有 5601" ;;
esac
case "$CANDS" in
    *1002*|*1601*) bad "候选里混进了资产类科目（只该有损益类）" ;;
    *) ok "候选不含资产类科目（1002 / 1601 都不在）" ;;
esac

put_code() { # put_code <account_code> <outfile>
    curl -sS -m 10 -o "$2" -w '%{http_code}' -X PUT "$BASE/business/erp/fx-settings" \
        "${AUTH[@]}" -H 'Content-Type: application/json' -d "{\"account_code\":\"$1\"}"
}
err_code() { python3 -c "import json;d=json.load(open('$1'));print(d.get('code'))" 2>/dev/null; }
err_msg()  { python3 -c "import json;d=json.load(open('$1'));print((d.get('msg') or '')[:100])" 2>/dev/null; }

# ---------- ② 四条校验 ----------
STATUS="$(put_code '1002' "$RUN_DIR/p_asset.json")"
[[ "$STATUS" == "400" && "$(err_code "$RUN_DIR/p_asset.json")" == "66083" ]] \
    && ok "资产类科目 1002 ⇒ 400 + 66083（$(err_msg "$RUN_DIR/p_asset.json")…）" \
    || bad "资产类科目返回 $STATUS / code=$(err_code "$RUN_DIR/p_asset.json")（期望 400/66083）"
case "$(err_msg "$RUN_DIR/p_asset.json")" in
    *损益*) ok "报文点明「损益类」要求" ;;
    *) bad "报文没说明损益类要求：$(err_msg "$RUN_DIR/p_asset.json")" ;;
esac

STATUS="$(put_code '5602' "$RUN_DIR/p_parent.json")"
[[ "$STATUS" == "400" && "$(err_code "$RUN_DIR/p_parent.json")" == "66083" ]] \
    && ok "汇总科目 5602 ⇒ 400 + 66083（$(err_msg "$RUN_DIR/p_parent.json")…）" \
    || bad "汇总科目返回 $STATUS / code=$(err_code "$RUN_DIR/p_parent.json")"

STATUS="$(put_code 'M106.99' "$RUN_DIR/p_missing.json")"
[[ "$STATUS" == "400" && "$(err_code "$RUN_DIR/p_missing.json")" == "66083" ]] \
    && ok "不存在的科目 M106.99 ⇒ 400 + 66083" \
    || bad "不存在科目返回 $STATUS / code=$(err_code "$RUN_DIR/p_missing.json")"
case "$(err_msg "$RUN_DIR/p_missing.json")" in
    *M106.99*) ok "报文点名编码 M106.99" ;;
    *) bad "报文没点名编码：$(err_msg "$RUN_DIR/p_missing.json")" ;;
esac

STATUS="$(put_code '' "$RUN_DIR/p_empty.json")"
[[ "$STATUS" == "400" && "$(err_code "$RUN_DIR/p_empty.json")" == "10001" ]] \
    && ok "空编码 ⇒ 400 + 10001（参数形状层面）" \
    || bad "空编码返回 $STATUS / code=$(err_code "$RUN_DIR/p_empty.json")（期望 400/10001）"
[[ "$(q_db "$DB_E2E" "select config_value from sys_config where config_key='$KEY' and deleted_at is null")" == "5603" ]] \
    && ok "四次被拒后参数值仍是 5603（拒绝不落库）" || bad "被拒的请求改动了参数"

# ---------- ③ PUT 5601 成功 + 读回 ----------
STATUS="$(put_code '5601' "$RUN_DIR/p_ok.json")"
echo "  PUT 5601 响应：$(head -c 300 "$RUN_DIR/p_ok.json")"
[[ "$STATUS" == "200" ]] && ok "PUT 5601 ⇒ 200" || bad "PUT 5601 返回 $STATUS（期望 200）"
[[ "$(json_field "$RUN_DIR/p_ok.json" ".get('account_code')")" == "5601" ]] && ok "返回体 account_code = 5601" || bad "返回体 account_code 不对"
[[ "$(q_db "$DB_E2E" "select config_value from sys_config where config_key='$KEY' and deleted_at is null")" == "5601" ]] \
    && ok "库里 config_value 真的变成 5601（不是只改返回体）" || bad "库里参数没变"
STATUS="$(curl -sS -m 10 -o "$RUN_DIR/fx_get2.json" -w '%{http_code}' "$BASE/business/erp/fx-settings" "${AUTH[@]}")"
[[ "$STATUS" == "200" && "$(json_field "$RUN_DIR/fx_get2.json" ".get('account_code')")" == "5601" ]] \
    && ok "再 GET ⇒ 5601（运行时快照已刷新，不是内存默认 5603）" || bad "GET 读回不对：$(json_field "$RUN_DIR/fx_get2.json" ".get('account_code')")"

# ---------- ④ 幂等：同值再 PUT ----------
# 基准时间取「第一次 PUT 之后」：before 取在第一次 PUT 之前的话，比的其实是
# 「种子时间 vs 两次写入后」，恒不相等（本判据要证的是**第二次**没写库）。
BEFORE_AT="$(q_db "$DB_E2E" "select updated_at from sys_config where config_key='$KEY' and deleted_at is null")"
sleep 1
STATUS="$(put_code '5601' "$RUN_DIR/p_same.json")"
AFTER_AT="$(q_db "$DB_E2E" "select updated_at from sys_config where config_key='$KEY' and deleted_at is null")"
[[ "$STATUS" == "200" ]] && ok "同值再 PUT ⇒ 200（幂等）" || bad "同值再 PUT 返回 $STATUS"
[[ "$BEFORE_AT" == "$AFTER_AT" ]] && ok "同值再 PUT 不写库（updated_at 不变）" || bad "同值再 PUT 居然改了 updated_at（$BEFORE_AT → $AFTER_AT）"

# ---------- ⑤ 只读账号：GET 200 / PUT 403 ----------
q_db "$DB_E2E" "insert into sys_role (role_code, role_name, status)
                select 'M106_READER','M1-06 只读角色',1
                 where not exists (select 1 from sys_role where role_code='M106_READER' and deleted_at is null)" >/dev/null
ROL="$(q_db "$DB_E2E" "select id from sys_role where role_code='M106_READER' and deleted_at is null limit 1")"
q_db "$DB_E2E" "insert into sys_role_menu (id, role_id, menu_id)
                select gen_random_uuid(), '$ROL'::uuid, '$PAGE_ID'::uuid
                 where not exists (select 1 from sys_role_menu where role_id='$ROL'::uuid and menu_id='$PAGE_ID'::uuid)" >/dev/null
q_db "$DB_E2E" "insert into sys_user (username, password, must_change_password)
                select 'm106reader', password, false from sys_user where username='admin'
                  and not exists (select 1 from sys_user u where u.username='m106reader' and u.deleted_at is null)" >/dev/null
UID_R="$(q_db "$DB_E2E" "select id from sys_user where username='m106reader' and deleted_at is null limit 1")"
q_db "$DB_E2E" "insert into sys_user_role (user_id, role_id)
                select '$UID_R'::uuid, '$ROL'::uuid
                 where not exists (select 1 from sys_user_role where user_id='$UID_R'::uuid and role_id='$ROL'::uuid)" >/dev/null
TOKEN_R="$(curl -sS -m 10 -X POST "$BASE/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"m106reader\",\"password\":\"$ADMIN_PASSWORD\"}" \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print((d.get("data") or {}).get("access_token") or "")' 2>/dev/null)"
if [[ -n "${TOKEN_R:-}" ]]; then
    STATUS="$(curl -sS -m 10 -o /dev/null -w '%{http_code}' "$BASE/business/erp/fx-settings" -H "Authorization: Bearer $TOKEN_R")"
    [[ "$STATUS" == "200" ]] && ok "只有 fx-rate:list 的账号也能 GET（读不另开码）" || bad "只读账号 GET 返回 $STATUS（期望 200）"
    STATUS="$(curl -sS -m 10 -o /dev/null -w '%{http_code}' -X PUT "$BASE/business/erp/fx-settings" \
        -H "Authorization: Bearer $TOKEN_R" -H 'Content-Type: application/json' -d '{"account_code":"5603"}')"
    [[ "$STATUS" == "403" ]] && ok "只读账号 PUT ⇒ 403（缺 business:erp:fx-settings:update）" || bad "只读账号 PUT 返回 $STATUS（期望 403）"
else
    bad "只读账号登录失败（无法验证 GET 200 / PUT 403）"
fi

# ---------- ⑥ 科目被停用：GET 200 + problem；PUT 该科目 400 + 66083 ----------
q_db "$DB_E2E" "update biz_account set status = 0 where account_code = '5601'" >/dev/null
STATUS="$(curl -sS -m 10 -o "$RUN_DIR/fx_broken.json" -w '%{http_code}' "$BASE/business/erp/fx-settings" "${AUTH[@]}")"
[[ "$STATUS" == "200" ]] && ok "配置指向停用科目时 GET 仍 200（页面还能打开改配置）" || bad "配置坏时 GET 返回 $STATUS（期望 200）"
[[ "$(json_field "$RUN_DIR/fx_broken.json" ".get('account_id')")" == "None" ]] && ok "account_id = null（科目不可用）" || bad "account_id 不该有值"
case "$(json_field "$RUN_DIR/fx_broken.json" ".get('problem')")" in
    *5601*停用*|*停用*5601*) ok "problem 点名 5601 已停用" ;;
    *) bad "problem 没点明原因：$(json_field "$RUN_DIR/fx_broken.json" ".get('problem')")" ;;
esac
STATUS="$(put_code '5601' "$RUN_DIR/p_disabled.json")"
[[ "$STATUS" == "400" && "$(err_code "$RUN_DIR/p_disabled.json")" == "66083" ]] \
    && ok "把配置改成已停用科目 ⇒ 400 + 66083" || bad "改成停用科目返回 $STATUS / code=$(err_code "$RUN_DIR/p_disabled.json")"
q_db "$DB_E2E" "update biz_account set status = 1 where account_code = '5601'" >/dev/null

# ---------- ⑦ 恢复 5603 ----------
STATUS="$(put_code '5603' "$RUN_DIR/p_restore.json")"
[[ "$STATUS" == "200" && "$(json_field "$RUN_DIR/p_restore.json" ".get('account_code')")" == "5603" ]] \
    && ok "恢复 5603 ⇒ 200" || bad "恢复 5603 返回 $STATUS"
[[ "$(q_db "$DB_E2E" "select config_value from sys_config where config_key='$KEY' and deleted_at is null")" == "5603" ]] \
    && ok "库里参数已回到 5603" || bad "恢复后库里参数不对"

echo
echo "== 汇总：PASS=$PASS FAIL=$FAIL =="
[[ "$FAIL" == "0" ]] || exit 1
