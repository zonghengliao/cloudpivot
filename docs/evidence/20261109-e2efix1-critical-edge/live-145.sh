#!/usr/bin/env bash
# =============================================================================
# E2E-FIX-1 实测：主链路「必需边」保护 + 禁用二次确认 + 巡检告警 + 业务语言报错
# （卡 t_c99b63cd）
#
# 打的是一个**全新隔离库 + 隔离端口**的实例（不碰 cloudpivot_dev / :8023 / 生产），
# 覆盖卡面 5 条验收：
#   ① 禁用 STOCK_OUT DRAFT→POSTED（不传 confirm_critical）→ 400 + 点名业务后果
#   ② 传 confirm_critical=true → 通过（送审，返回 FLW-… 审批单号）
#   ③ 巡检脚本能发现「主链路边被禁用」→ rc=1 并点名边
#   ④ 恢复 enabled=true → 巡检 rc=0
#   ⑤ 用户侧 409 报文是业务语言，不暴露内部出库单号（单号只在后端日志 internal_detail）
#
# 用法：
#   bash docs/evidence/20261109-e2efix1-critical-edge/live-145.sh
#   环境变量：DB / PORT / ENVF 可覆盖
# =============================================================================
set -uo pipefail

DB="${DB:-cp_e2efix1}"
PORT="${PORT:-18319}"
ENVF="${ENVF:-/etc/cloudpivot/cloudpivot-dev.env}"
REPO="${REPO:-/opt/cloudpivot}"
BASE="http://127.0.0.1:$PORT/api"
RAW="/tmp/e2efix1/raw"; mkdir -p "$RAW"
LOG="/tmp/e2efix1.log"

# ---- 环境：145 有 /etc/cloudpivot/cloudpivot-dev.env；99 没有，用本机 alun 兜底 ----
if [ -f "$ENVF" ]; then
    set -a; . "$ENVF"; set +a
    DB_USER="$NIK_DATABASE_USER"
    SAY_ENV="env 文件 $ENVF"
else
    DB_USER="alun"
    export NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432
    export NIK_DATABASE_USER="$DB_USER" NIK_DATABASE_PASSWORD="$DB_USER"
    export NIK_MIDDLEWARE_AUTH_JWT_SECRET="e2efix1_local_jwt_secret_0123456789abcdefghijklmn"
    export CLOUDPIVOT_STORAGE_MASTER_KEY="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
    export X_CONFIG_SECRET_KEY="e2efix1_local_config_secret"
    SAY_ENV="本机兜底（无 $ENVF，DB 账号 $DB_USER）"
fi
export PGPASSWORD="${NIK_DATABASE_PASSWORD:-alun}"
export PGHOST=127.0.0.1 PGPORT=5432 PGUSER="$DB_USER"
export PATH="/root/.cargo/bin:$PATH"

ADMIN_PASS="${NIK_ADMIN_PASSWORD:-Admin@123456}"
# 全新库的 admin 口令只认 env 注入（缺省会生成随机口令并把日志里的值打码成 `***`，
# 脚本读不到），所以没注入时这里显式钉一个，脚本才能真的登录。
export NIK_ADMIN_PASSWORD="${NIK_ADMIN_PASSWORD:-E2eFix1@Admin}"
ADMIN_PASS="$NIK_ADMIN_PASSWORD"
NEW_PASS='E2eFix1@145pass'
PASS=0; FAIL=0
say(){ printf '%s\n' "$*"; }
ok(){ PASS=$((PASS+1)); say "  ✅ $*"; }
bad(){ FAIL=$((FAIL+1)); say "  ❌ $*"; }
PSQL(){ psql -q -h 127.0.0.1 -U "$DB_USER" -d "$DB" -At -F'|' -P pager=off -c "$1"; }
SU(){ su - postgres -c "$1"; }
api(){ # api <name> <METHOD> <path> [token] [json]
  local name=$1 method=$2 path=$3 token=${4:-} body=${5:-}
  local out="$RAW/$name.json"
  local -a args=(-s -m 60 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path")
  [ -n "$token" ] && args+=(--oauth2-bearer "$token")
  [ -n "$body" ] && args+=(-H 'Content-Type: application/json' -d "$body")
  HTTP=$(curl "${args[@]}")
  BODY=$(cat "$out" 2>/dev/null)
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
}
data(){ printf '%s' "$BODY" | jq -r "$1" 2>/dev/null; }
edge_sql(){ # edge_sql <from> <to> <enabled>
  PSQL "INSERT INTO biz_flow_state_edge (entity, from_status, to_status, enabled, requires_reason)
        VALUES ('STOCK_OUT', '$1', '$2', $3, false)
        ON CONFLICT (entity, from_status, to_status)
        DO UPDATE SET enabled = $3, deleted_at = NULL, updated_at = now()" >/dev/null
}

say "==== 0. 隔离库 + 隔离实例（$SAY_ENV）"
SU "dropdb --force $DB" >/dev/null 2>&1
SU "createdb -O $DB_USER $DB" || { echo "createdb 失败"; exit 1; }
cd "$REPO/backend" || exit 1
export NIK_DATABASE_NAME="$DB" NIK_SERVER_LISTEN="127.0.0.1:$PORT"
: > "$LOG"
setsid nohup ./target/debug/cloudpivot > "$LOG" 2>&1 < /dev/null &
PID=$!
for _ in $(seq 1 240); do grep -q "数据库迁移完成" "$LOG" && break; sleep 1; done
# 迁移完成 ≠ 服务可用：之后还要跑种子 / 权限层挂载，实测再慢 10-20s。健康探针轮询到 180s。
HEALTH=000
for _ in $(seq 1 90); do
  HEALTH=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health")
  [ "$HEALTH" = "200" ] && break
  sleep 2
done
say "  pid=$PID 迁移次数=$(grep -c '数据库迁移完成' "$LOG") health=$HEALTH"
[ "$HEALTH" = "200" ] || { bad "实例没起来（health=$HEALTH），日志：$(tail -5 "$LOG")"; kill "$PID" 2>/dev/null; exit 1; }
say "  库指纹 schema_migrations(max|count)=$(PSQL 'select max(version::int), count(*) from schema_migrations' | tr '|' ' ')"
say "  主链路白名单 biz_flow_critical_edge=$(PSQL 'select count(*) from biz_flow_critical_edge') 行"
[ "$(PSQL 'select count(*) from biz_flow_critical_edge')" = "25" ] \
  && ok "V0354 已随迁移种入 25 条主链路边" || bad "白名单行数不是 25"

say "==== 1. 登录（全新库 admin 带 must_change_password → 先改密）"
# 实例 health=200 之后仍可能在写种子（admin 账号是 bootstrap 阶段落的），
# 首次登录偶发 20001 → 重试到 40s，别把「服务还没就绪」误报成「口令错」。
TOK=""
for _ in $(seq 1 20); do
  api login POST /system/auth/login "" "{\"username\":\"admin\",\"password\":\"$ADMIN_PASS\"}"
  TOK=$(data '.data.access_token')
  [ -n "$TOK" ] && [ "$TOK" != "null" ] && break
  sleep 2
done
if [ -z "$TOK" ] || [ "$TOK" = "null" ]; then bad "登录失败 http=$HTTP body=$BODY"; kill "$PID" 2>/dev/null; exit 1; fi
api chpass PUT /system/auth/password "$TOK" "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$NEW_PASS\"}"
say "  改密 http=$HTTP code=$CODE msg=$(data '.msg')"
for _ in $(seq 1 10); do
  api login POST /system/auth/login "" "{\"username\":\"admin\",\"password\":\"$NEW_PASS\"}"
  TOK=$(data '.data.access_token')
  [ -n "$TOK" ] && [ "$TOK" != "null" ] && break
  sleep 1
done
[ -n "$TOK" ] && [ "$TOK" != "null" ] && ok "admin 已登录（改密后）" || { bad "二次登录失败：$BODY"; kill "$PID" 2>/dev/null; exit 1; }

say "==== 2. GET 边配置：主链路边要能自报家门（critical / critical_impact）"
api edges GET "/business/flow/state-edge?entity=STOCK_OUT" "$TOK"
HIT=$(printf '%s' "$BODY" | jq -r '.data.list[0].base_edges[] | select(.from=="DRAFT" and .to=="POSTED") | "\(.critical)|\(.critical_impact)"')
say "  DRAFT→POSTED = $HIT"
[ "${HIT%%|*}" = "true" ] && ok "DRAFT→POSTED 标了 critical=true" || bad "DRAFT→POSTED 没标 critical：$HIT"
[ "${HIT#*|}" = "销售出库无法直接过账" ] && ok "critical_impact=销售出库无法直接过账" || bad "impact 不对：$HIT"

say "==== 3. ① 禁用 DRAFT→POSTED（不传 confirm_critical）→ 期望 400 + 点名业务后果"
api dis_no_confirm PUT /business/flow/state-edge "$TOK" \
  '{"entity":"STOCK_OUT","from_status":"DRAFT","to_status":"POSTED","enabled":false,
    "requires_reason":false,"label":null,"auth_code":null,"confirm":true,"reason":"E2E-FIX-1 实测：不传确认"}'
say "  http=$HTTP code=$CODE msg=$(data '.msg')"
[ "$HTTP" = "400" ] && ok "不确认 → 400（拒绝禁用）" || bad "期望 400，实际 $HTTP"
case "$(data '.msg')" in
  *"主链路必需边"*) ok "报文点名「主链路必需边」" ;;
  *) bad "报文没点名主链路必需边" ;;
esac
case "$(data '.msg')" in
  *"销售出库无法直接过账"*) ok "报文点名业务后果「销售出库无法直接过账」" ;;
  *) bad "报文没有业务后果" ;;
esac
case "$(data '.msg')" in
  *"confirm_critical=true"*) ok "报文给出继续操作的办法（confirm_critical=true）" ;;
  *) bad "报文没告诉调用方怎么继续" ;;
esac
LEFT=$(PSQL "select count(*) from biz_flow_state_edge where entity='STOCK_OUT' and from_status='DRAFT' and to_status='POSTED' and deleted_at is null and enabled=false")
[ "$LEFT" = "0" ] && ok "被挡下的请求没留下禁用配置行" || bad "被挡下却落了禁用行：$LEFT"

say "==== 4. ② 带 confirm_critical=true → 期望 200（送审或直接生效）"
api dis_confirm PUT /business/flow/state-edge "$TOK" \
  '{"entity":"STOCK_OUT","from_status":"DRAFT","to_status":"POSTED","enabled":false,
    "requires_reason":false,"label":null,"auth_code":null,"confirm":true,"confirm_critical":true,
    "reason":"E2E-FIX-1 实测：已确认业务后果"}'
APPLIED=$(data '.data.applied'); FLW=$(data '.data.instance_no')
say "  http=$HTTP applied=$APPLIED instance_no=$FLW msg=$(data '.data.message')"
if [ "$HTTP" = "200" ] || [ "$HTTP" = "201" ]; then
  ok "确认后禁用请求被接受（http=$HTTP）"
else
  bad "确认后仍被拒：http=$HTTP msg=$(data '.msg')"
fi
if [ "$APPLIED" = "true" ] || [ "${FLW#FLW-}" != "$FLW" ]; then
  ok "落点明确：applied=$APPLIED / 审批单=$FLW（有已发布流程 → 送审）"
else
  bad "既没生效也没送审：$BODY"
fi

say "==== 5. ⑤ 造一张真出库单（DRAFT），再按 E2E-02 事故现场把 DRAFT→POSTED 关掉"
# 关哪条边有讲究：`DRAFT→PENDING_POST` 是 **mandatory**（代码层禁不掉，库里的 false 会被
# 运行时忽略 + WARN），只有 optional 的 `DRAFT→POSTED`（免审直过）能真被关掉
# —— E2E-02 演练断链的正是它。
WH=$(PSQL "select id from biz_warehouse where status = 1 order by sort_order, warehouse_code limit 1")
MAT=$(PSQL "select id from biz_material where status = 1 order by material_code limit 1")
say "  warehouse=$WH material=$MAT"
api create_so POST /business/erp/stock-out "$TOK" \
  "{\"out_type\":\"ISSUE\",\"warehouse_id\":\"$WH\",\"reason\":\"E2E-FIX-1 实测\",
    \"items\":[{\"material_id\":\"$MAT\",\"qty\":1}]}"
SO_ID=$(data '.data.id'); OUT_NO=$(data '.data.out_no')
say "  http=$HTTP out_no=$OUT_NO id=$SO_ID"
if [ -n "$OUT_NO" ] && [ "$OUT_NO" != "null" ]; then ok "出库单建单成功：$OUT_NO"; else bad "建单失败：$BODY"; fi

edge_sql DRAFT POSTED false
say "  已禁用边：$(PSQL "select entity||' '||from_status||'→'||to_status||' enabled='||enabled from biz_flow_state_edge where entity='STOCK_OUT' and from_status='DRAFT' and to_status='POSTED'")"

api submit_so PATCH "/business/erp/stock-out/$SO_ID/status" "$TOK" '{"status":"POSTED"}'
MSG=$(data '.msg')
say "  http=$HTTP code=$CODE msg=$MSG"
[ "$HTTP" = "409" ] && ok "边被禁用 → 409（拦住了）" || bad "期望 409，实际 $HTTP：$MSG"
case "$MSG" in
  *"销售出库无法直接过账"*) ok "报错是业务语言「销售出库无法直接过账」" ;;
  *) bad "报错不是业务语言：$MSG" ;;
esac
case "$MSG" in
  *"已被系统配置禁用"*"请联系系统管理员"*) ok "报错给出下一步（联系系统管理员）" ;;
  *) bad "报错没给下一步：$MSG" ;;
esac
case "$MSG" in
  *"$OUT_NO"*) bad "❌ 用户侧报文暴露了内部单号 $OUT_NO" ;;
  *) ok "用户侧报文不含内部单号（$OUT_NO 只进 internal_detail）" ;;
esac
case "$MSG" in
  *DRAFT*|*PENDING_POST*) bad "用户侧报文仍带状态机术语：$MSG" ;;
  *) ok "用户侧报文不含状态名（DRAFT / PENDING_POST）" ;;
esac
if grep -q "internal_detail" "$LOG" || grep -q "$OUT_NO" "$LOG"; then
  ok "后端日志里有内部细节（$OUT_NO / 流转 / 可选去向），运维可追"
else
  bad "后端日志里找不到内部细节，运维无从定位"
fi

say "==== 6. ③ 巡检脚本：主链路边被禁用 → rc=1 并点名"
OUT_CRIT=$(bash "$REPO/scripts/verify/flow_critical_edges.sh" "$DB" 2>&1); RC_CRIT=$?
printf '%s\n' "$OUT_CRIT" | sed 's/^/    /'
[ "$RC_CRIT" = "1" ] && ok "巡检 rc=1（发现主链路边被禁用）" || bad "巡检 rc=$RC_CRIT（期望 1）"
case "$OUT_CRIT" in
  *"STOCK_OUT DRAFT→POSTED"*) ok "巡检点名了被禁用的边 + 业务后果" ;;
  *) bad "巡检没点名具体边" ;;
esac

say "==== 7. ④ 恢复 enabled=true → 巡检 rc=0"
edge_sql DRAFT POSTED true
OUT_OK=$(bash "$REPO/scripts/verify/flow_critical_edges.sh" "$DB" 2>&1); RC_OK=$?
printf '%s\n' "$OUT_OK" | sed 's/^/    /'
[ "$RC_OK" = "0" ] && ok "恢复后巡检 rc=0（绿）" || bad "恢复后巡检 rc=$RC_OK（期望 0）"

say "==== 8. 收尾（还原现场，杀实例）"
edge_sql DRAFT POSTED true
kill "$PID" >/dev/null 2>&1
sleep 1
say "  残留：$(PSQL "select count(*) from biz_flow_state_edge where deleted_at is null and enabled=false") 条禁用边（本库为隔离库，不清理业务单据：$OUT_NO）"

say ""
say "==== 结果：PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
