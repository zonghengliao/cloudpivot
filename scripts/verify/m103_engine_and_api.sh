#!/usr/bin/env bash
# =============================================================================
# m103_engine_and_api.sh —— V0408（M1-03 科目「是否外币核算」标记）真引擎 + 真 HTTP 实测
#
# 卡：t_52b02f6a。**不是** psql 手动执行 SQL，而是用编译出的后端二进制真启动，
# 走引擎自己的迁移器（schema_migrations 指纹 / 幂等 / 顺序）与真 HTTP 路由。
#
# 三段：
#   A) 克隆库升级：模板库 cloudpivot_test（versioned 到 V0407）克隆成 cp_m103_upgrade
#      → 真启动 → 只应用 V0408 → schema_migrations 登记 → 重启 0 个迁移
#   B) 空库首启：空库 cp_m103_e2e → 真启动 → 全量迁移（含 V0408 自检块）
#   C) 真 HTTP：登录 admin → 科目外币核算开关的四条路径 + 三条错误路径 + 列表筛选
#
# 用法：bash scripts/verify/m103_engine_and_api.sh [PG_HOST]
#   默认 PG_HOST=172.18.10.145（99 上跑）；145 上跑传 127.0.0.1。
# 端口固定 18088（隔离段 18000-18999），库名含 e2e/upgrade，脚本自己 DROP。
# =============================================================================
set -uo pipefail

PG_HOST="${1:-172.18.10.145}"
export PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4
PG_USER=cloudpivot
TEMPLATE=cloudpivot_test
DB_UP=cp_m103_upgrade
DB_E2E=cp_m103_e2e
PORT=18088
BASE="http://127.0.0.1:$PORT/api"
REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
BACKEND_DIR="$REPO_ROOT/backend"
BIN="$BACKEND_DIR/target/debug/cloudpivot"
RUN_DIR="${M103_RUN_DIR:-/root/.hermes/profiles/backend-developer/cache/scratch/m103-run}"
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
# 缺失时生成一次性随机口令并把账号标记 `must_change_password = true`（除改密外全部接口 403）。
# 演练实例显式注入 + 种完后清掉该标记（只在本脚本建的演练库里做，见 B 段）。
ADMIN_PASSWORD='M103@Drill#2026'

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
        export X_CONFIG_SECRET_KEY="m103-drill-config-secret"
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
    # 端口彻底放开再起下一个实例
    for _ in $(seq 1 20); do curl -fsS -m 1 "http://127.0.0.1:$PORT/health" >/dev/null 2>&1 || break; sleep 1; done
}

cleanup() {
    stop_backend
    psql_admin "DROP DATABASE IF EXISTS $DB_UP" >/dev/null 2>&1
    psql_admin "DROP DATABASE IF EXISTS $DB_E2E" >/dev/null 2>&1
}
trap cleanup EXIT

# ============================================================================
echo "== A) 克隆库升级（真引擎：模板库 → cp_m103_upgrade）=="
psql_admin "DROP DATABASE IF EXISTS $DB_UP" >/dev/null 2>&1
psql_admin "DROP DATABASE IF EXISTS $DB_E2E" >/dev/null 2>&1
if ! psql_admin "CREATE DATABASE $DB_UP TEMPLATE $TEMPLATE" >/dev/null 2>&1; then
    echo "FATAL: 克隆模板库失败（模板可能被别的会话占用），重试一次"
    sleep 5
    psql_admin "CREATE DATABASE $DB_UP TEMPLATE $TEMPLATE" >/dev/null || exit 2
fi
BEFORE_MAX="$(q_db "$DB_UP" "select coalesce(max(version::int),0) from schema_migrations where kind='versioned'")"
BEFORE_CNT="$(q_db "$DB_UP" "select count(*) from schema_migrations where kind='versioned'")"
echo "  升级前：versioned $BEFORE_CNT 条，最大 version=$BEFORE_MAX（模板停在 V$BEFORE_MAX，V0408 之后都是本卡要验的）"

start_backend "$DB_UP" "$RUN_DIR/upgrade.log" || { echo "FATAL: 升级实例没起来"; exit 2; }
ok "克隆库真启动成功（/health 200）"
APPLIED="$(grep -oE '数据库迁移完成: 应用 [0-9]+ 个迁移' "$RUN_DIR/upgrade.log" | head -1)"
echo "  $APPLIED"
APPLIED_N="$(echo "$APPLIED" | grep -oE '[0-9]+' | head -1)"
# 模板库比仓库落后几步（M1-01/M1-02 的 V0406/V0407 也可能没被种进模板），所以这里不断言数量，
# 断言的是「V0408 被应用 + versioned 条数增量 = 应用数」。
[[ "${APPLIED_N:-0}" -ge 1 ]] && ok "应用了 $APPLIED_N 个迁移（含 V0408）" || bad "一个迁移都没应用 —— V0408 没被引擎识别"
grep -q "V0408 自检通过" "$RUN_DIR/upgrade.log" && ok "V0408 自检块在真引擎里通过（NOTICE 已打印）" || bad "没看到 V0408 自检 NOTICE"
ROW="$(q_db "$DB_UP" "select version||'/'||success||'/'||source from schema_migrations where name='account_foreign_currency'")"
echo "  schema_migrations：$ROW"
case "$ROW" in
    408/true/*V0408__account_foreign_currency.up.sql) ok "schema_migrations 登记 408/true/源文件" ;;
    *) bad "schema_migrations 登记不对：[$ROW]" ;;
esac
COL="$(q_db "$DB_UP" "select data_type||'/'||is_nullable||'/'||coalesce(column_default,'-') from information_schema.columns where table_name='biz_account' and column_name='is_foreign_currency'")"
[[ "$COL" == "smallint/NO/0" ]] && ok "升级后列结构 = smallint/NO/0" || bad "升级后列结构异常：[$COL]"
CK="$(q_db "$DB_UP" "select count(*) from pg_constraint where conrelid='public.biz_account'::regclass and conname in ('ck_biz_account_foreign_currency','ck_biz_account_fc_leaf')")"
[[ "$CK" == "2" ]] && ok "两条 CHECK 在位" || bad "CHECK 只有 $CK 条"
MENU="$(q_db "$DB_UP" "select count(*) from sys_menu where auth_code='business:erp:account:update' and menu_type=3 and deleted_at is null")"
[[ "$MENU" == "1" ]] && ok "权限码菜单按钮行在位且唯一" || bad "权限码行计数 = $MENU"
[[ "$(q_db "$DB_UP" "select count(*) from biz_account where is_foreign_currency <> 0")" == "0" ]] && ok "存量 83 行全部回填 0" || bad "存量行有非 0 值"

# 重启：引擎级幂等
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
echo "== B) 空库首启（全量迁移 + V0408 自检；真 HTTP 的落点）=="
psql_admin "CREATE DATABASE $DB_E2E" >/dev/null || { echo "FATAL: 建空库失败"; exit 2; }
start_backend "$DB_E2E" "$RUN_DIR/e2e.log" || { echo "FATAL: 空库实例没起来"; tail -30 "$RUN_DIR/e2e.log"; exit 2; }
APPLIED3="$(grep -oE '数据库迁移完成: 应用 [0-9]+ 个迁移' "$RUN_DIR/e2e.log" | head -1)"
echo "  $APPLIED3"
CNT3="$(q_db "$DB_E2E" "select count(*) from schema_migrations where kind='versioned'")"
[[ "$CNT3" == "328" ]] && ok "空库首启 versioned 迁移 = 328（V0408 是第 328 条）" || bad "空库首启 versioned = $CNT3（期望 328）"
grep -q "V0408 自检通过" "$RUN_DIR/e2e.log" && ok "空库首启也跑了 V0408 自检" || bad "空库首启没有 V0408 自检 NOTICE"
curl -fsS -m 3 "http://127.0.0.1:$PORT/health" >/dev/null && ok "/health 200" || bad "/health 不通"

# 种超管时口令来自 NIK_ADMIN_PASSWORD，但 `must_change_password` 恒为 true（CODE-06：种子口令
# 不应长期有效），未改密前除改密/登出外全部接口 403。演练实例里直接清掉这一位（只动本脚本
# 建的 cp_m103_e2e），把注意力留给本卡的接口而不是首登改密流程。
q_db "$DB_E2E" "update sys_user set must_change_password = false where username = 'admin'" >/dev/null
[[ "$(q_db "$DB_E2E" "select must_change_password from sys_user where username='admin'")" == "f" ]] \
    && ok "演练库：清掉 admin 的 must_change_password（仅为跑通本卡接口）" || bad "清 must_change_password 失败"

# ============================================================================
echo
echo "== C) 真 HTTP：科目外币核算开关 =="
TOKEN="$(curl -sS -m 10 -X POST "$BASE/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"admin\",\"password\":\"$ADMIN_PASSWORD\"}" \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); print((d.get("data") or {}).get("access_token") or "")' 2>/dev/null)"
[[ -n "${TOKEN:-}" ]] && ok "登录成功（access_token ${#TOKEN} 字符）" || { bad "登录失败"; stop_backend; exit 2; }
AUTH=(-H "Authorization: Bearer $TOKEN")

# 造一对科目：末级（M103.01，挂在外币父科目下）+ 汇总（M103）。用专属编码避开种子科目
# （空库首启自带 1001/1002/1405/2202/2221 等种子行，占它们的编码会撞 uk_biz_account_code）。
q_db "$DB_E2E" "insert into biz_account(account_code,account_name,level,account_category,balance_direction,is_leaf) values ('M103','__m103_parent__',1,'ASSET','DEBIT',0)" >/dev/null
PARENT_ID="$(q_db "$DB_E2E" "select id from biz_account where account_code='M103'")"
q_db "$DB_E2E" "insert into biz_account(account_code,account_name,parent_id,level,account_category,balance_direction,is_leaf) values ('M103.01','__m103_leaf__','$PARENT_ID'::uuid,2,'ASSET','DEBIT',1)" >/dev/null
LEAF_ID="$(q_db "$DB_E2E" "select id from biz_account where account_code='M103.01'")"
echo "  末级科目 M103.01 = $LEAF_ID；汇总科目 M103 = $PARENT_ID"
[[ -n "$LEAF_ID" && -n "$PARENT_ID" ]] && ok "演练科目已就位（1 末级 + 1 汇总）" || bad "演练科目没建出来"
[[ "$(q_db "$DB_E2E" "select is_leaf from biz_account where id='$PARENT_ID'")" == "0" ]] && ok "M103 确实是汇总科目（is_leaf=0）" || bad "M103 不是汇总科目"

call_put() { # call_put <id> <body> → 打印 HTTP 码 + body
    local id="$1" body="$2"
    curl -sS -m 10 -o "$RUN_DIR/body.json" -w '%{http_code}' -X PUT \
        "$BASE/business/erp/account/$id/foreign-currency" \
        "${AUTH[@]}" -H 'Content-Type: application/json' -d "$body"
    echo -n " "
    python3 -c 'import json;d=json.load(open("'"$RUN_DIR"'/body.json"));print("code=%s msg=%s flag=%s"%(d.get("code"),d.get("msg"),(d.get("data") or {}).get("is_foreign_currency")))' 2>/dev/null \
        || cat "$RUN_DIR/body.json"
}

echo "-- C1) 列出科目（升级后每个科目都带 is_foreign_currency）--"
curl -sS -m 10 "$BASE/business/erp/account/page?page=1&page_size=5" "${AUTH[@]}" > "$RUN_DIR/page.json"
python3 - "$RUN_DIR/page.json" <<'PY'
import json,sys
d=json.load(open(sys.argv[1]))
rows=d["data"]["list"]
print("  行数=%d；字段 is_foreign_currency 全为 False：%s" % (
    len(rows), all(r["is_foreign_currency"] is False for r in rows)))
print("  样例：", {k: rows[0][k] for k in ("account_code","account_name","is_leaf","is_foreign_currency")})
PY

echo "-- C2) 开启末级科目的外币核算 --"
R="$(call_put "$LEAF_ID" '{"is_foreign_currency":true}')"
echo "  $R"
case "$R" in 200*flag=True*) ok "PUT 开启 → 200 + is_foreign_currency=true" ;; *) bad "开启失败：$R" ;; esac
[[ "$(q_db "$DB_E2E" "select is_foreign_currency from biz_account where id='$LEAF_ID'")" == "1" ]] && ok "库里的值 = 1" || bad "库里的值不是 1"
[[ "$(q_db "$DB_E2E" "select update_by is not null from biz_account where id='$LEAF_ID'")" == "t" ]] && ok "update_by 已落操作人" || bad "update_by 没写"

echo "-- C3) 幂等：重复开关同一个值（不写库、不刷 updated_at）--"
UPD1="$(q_db "$DB_E2E" "select updated_at from biz_account where id='$LEAF_ID'")"
sleep 1
R="$(call_put "$LEAF_ID" '{"is_foreign_currency":true}')"
echo "  $R"
case "$R" in 200*flag=True*) ok "重复开启 → 200（幂等）" ;; *) bad "幂等失败：$R" ;; esac
UPD2="$(q_db "$DB_E2E" "select updated_at from biz_account where id='$LEAF_ID'")"
[[ "$UPD1" == "$UPD2" ]] && ok "updated_at 未被刷新（$UPD2）" || bad "幂等仍刷了 updated_at：$UPD1 → $UPD2"

echo "-- C4) 非末级科目开启 → 必须 400 + 66080 --"
R="$(call_put "$PARENT_ID" '{"is_foreign_currency":true}')"
echo "  $R"
case "$R" in 400*code=66080*) ok "400 + 66080（业务规则拒绝，文案点名末级科目）" ;; *) bad "非末级没被拦住：$R" ;; esac
[[ "$(q_db "$DB_E2E" "select is_foreign_currency from biz_account where id='$PARENT_ID'")" == "0" ]] && ok "汇总科目库里仍是 0（拒绝时没写脏）" || bad "汇总科目被写脏了"

echo "-- C5) 不存在的科目 → 404 + 10002 --"
R="$(call_put "00000000-0000-0000-0000-000000000000" '{"is_foreign_currency":true}')"
echo "  $R"
case "$R" in 404*code=10002*) ok "404 + 10002" ;; *) bad "不存在科目返回不对：$R" ;; esac

echo "-- C6) 非法 UUID → 400 + 66006 --"
R="$(call_put "not-a-uuid" '{"is_foreign_currency":true}')"
echo "  $R"
case "$R" in 400*code=66006*) ok "400 + 66006" ;; *) bad "非法 UUID 返回不对：$R" ;; esac

echo "-- C7) 缺字段 → 4xx（不允许静默当 false）--"
R="$(call_put "$LEAF_ID" '{}')"
echo "  $R"
# axum 的 `Json` 提取器在反序列化失败时统一给 422 + 10001（框架口径，不是本接口特有的），
# 关键是「拒绝」而不是静默按 false 处理。
case "$R" in
    422*code=10001*) ok "缺 is_foreign_currency 被拒（422 + 10001，框架 JSON 提取器口径）" ;;
    400*code=10001*) ok "缺 is_foreign_currency 被拒（400 + 10001）" ;;
    *) bad "缺字段竟然成功：$R" ;;
esac

echo "-- C8) 列表按 is_foreign_currency 筛选 --"
curl -sS -m 10 "$BASE/business/erp/account/page?page=1&page_size=50&is_foreign_currency=true" "${AUTH[@]}" > "$RUN_DIR/page-fc.json"
python3 - "$RUN_DIR/page-fc.json" <<'PY'
import json,sys
d=json.load(open(sys.argv[1]))
rows=d["data"]["list"]
codes=[r["account_code"] for r in rows]
print("  命中 %d 条：%s（全部 flag=True：%s）" % (len(rows), codes, all(r["is_foreign_currency"] for r in rows)))
assert codes==["M103.01"], "期望只有 M103.01，实得 %s" % codes
print("  OK 只命中被标记的末级科目")
PY
[[ $? -eq 0 ]] && ok "列表筛选 is_foreign_currency=true 只回被标记科目" || bad "列表筛选结果不对"

echo "-- C9) 关闭 → 200 + false，库里回 0 --"
R="$(call_put "$LEAF_ID" '{"is_foreign_currency":false}')"
echo "  $R"
case "$R" in 200*flag=False*) ok "关闭 → 200 + false" ;; *) bad "关闭失败：$R" ;; esac
[[ "$(q_db "$DB_E2E" "select is_foreign_currency from biz_account where id='$LEAF_ID'")" == "0" ]] && ok "库里回 0" || bad "库里没回 0"

echo "-- C10) 未带令牌 → 401（路由真的挂了权限中间件）--"
CODE="$(curl -sS -m 10 -o /dev/null -w '%{http_code}' -X PUT \
    "$BASE/business/erp/account/$LEAF_ID/foreign-currency" \
    -H 'Content-Type: application/json' -d '{"is_foreign_currency":true}')"
[[ "$CODE" == "401" ]] && ok "未鉴权 → 401" || bad "未鉴权返回 $CODE（期望 401）"

echo "-- C11) 科目不是末级时先改 is_leaf=0 再开启 → 仍 66080（与 DB 约束同一条规则）--"
q_db "$DB_E2E" "update biz_account set is_leaf=0 where id='$LEAF_ID'" >/dev/null
R="$(call_put "$LEAF_ID" '{"is_foreign_currency":true}')"
echo "  $R"
case "$R" in 400*code=66080*) ok "非末级（原末级改成汇总）也 400 + 66080" ;; *) bad "改 is_leaf 后没拦住：$R" ;; esac

# ============================================================================
echo
echo "== D) 收尾 =="
stop_backend
psql_admin "DROP DATABASE IF EXISTS $DB_UP" >/dev/null 2>&1 && ok "演练库 $DB_UP 已删"
psql_admin "DROP DATABASE IF EXISTS $DB_E2E" >/dev/null 2>&1 && ok "演练库 $DB_E2E 已删"
LEFT="$(psql_admin "select count(*) from pg_database where datname like 'cp_m103%'")"
[[ "$LEFT" == "0" ]] && ok "cp_m103* 残留库计数 = 0" || bad "cp_m103* 残留 $LEFT 个"

echo
echo "== 汇总：PASS=$PASS FAIL=$FAIL =="
[ "$FAIL" -eq 0 ] || exit 1
