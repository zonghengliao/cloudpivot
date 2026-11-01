#!/usr/bin/env bash
# =============================================================================
# P-FLOW-2 · NOTIFY_NODE 真机验收
# =============================================================================
#
# 做什么
# ------
#   1. 起一个**隔离实例**（自建库 + 18092 端口，不碰 prod / dev）
#   2. 跑 `pflow2_notify.py`：建图 → 发布（R19 放行）→ 发起 → 断言实例走完
#   3. psql 复核：`sys_notice` 里有 FLOW_NOTIFY 行、接收人 = 发起人、节点 APPROVED
#   4. 停实例（trap，异常也停）
#
# 用法
# ----
#   bash scripts/verify/pflow2_notify.sh            # 全自动（自己起实例）
#   BASE_URL=http://host:port bash ...              # 已有实例则跳过启动
#
# 前置
# ----
#   · 本机 PG 可达（默认 127.0.0.1:5432），应用账号见 `PFLOW2_DB_USER`
#   · `backend/target/debug/cloudpivot` 已编译（脚本会先 `cargo build`）
#
# 端口/库名硬约束：库名以 `pflow2` 结尾、端口 18092（避开 prod 8023 与 1808x 的其它卡）
# =============================================================================
set -uo pipefail

REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
BACKEND_DIR="$REPO_ROOT/backend"
DB=${PFLOW2_DB:-cloudpivot_test_pflow2}
PORT=${PFLOW2_PORT:-18092}
BASE_URL=${BASE_URL:-http://127.0.0.1:$PORT}
RUN_DIR=${PFLOW2_RUN_DIR:-/tmp/pflow2}
DB_USER=${PFLOW2_DB_USER:-f12role}
DB_PASS=${PFLOW2_DB_PASS:-}
MASTER_KEY=${PFLOW2_STORAGE_KEY:-$(openssl rand -hex 32)}
ADMIN_PASSWORD=${PFLOW2_ADMIN_PASSWORD:-Admin@123456}

mkdir -p "$RUN_DIR"
FAILED=0
say() { printf '%s\n' "$*"; }
fail() { FAILED=$((FAILED + 1)); printf '[FAIL] %s\n' "$*"; }
pass() { printf '[PASS] %s\n' "$*"; }

psql_q() {
  PGPASSWORD="$DB_PASS" psql -h 127.0.0.1 -U "$DB_USER" -d "$DB" -Atc "$1" 2>&1
}

PID=""
cleanup() {
  if [ -n "$PID" ] && kill -0 "$PID" 2>/dev/null; then
    kill "$PID" 2>/dev/null
    sleep 1
  fi
}
trap cleanup EXIT

# ---------- 0. 已有实例？ ----------
if curl -sf "$BASE_URL/health" >/dev/null 2>&1; then
  say "复用已在跑的实例：$BASE_URL"
else
  say "== 起隔离实例（库 $DB / 端口 $PORT）=="
  export PATH=/root/.cargo/bin:$PATH
  ( cd "$BACKEND_DIR" && cargo build -q ) || { say "cargo build 失败"; exit 1; }

  # 库不存在就从已迁移的模板库复制一份（模板库由 P-FLOW-1B 卡建立，迁移已到最新）
  if ! su - postgres -c "psql -Atc \"select 1 from pg_database where datname='$DB'\"" | grep -q 1; then
    su - postgres -c "createdb -O ${PFLOW2_DB_OWNER:-alun} -T cloudpivot_test_pflow1b $DB" \
      || { say "建库失败"; exit 1; }
  fi

  # ⚠️ CWD 必须是 backend/：`config/` 是相对路径解析的，从仓根起会退化成默认配置
  # （database.enabled=false），表现是日志「数据库未初始化，跳过种子数据写入」后 panic。
  cd "$BACKEND_DIR" || exit 1
  NIK_PROFILE=test \
  NIK_SERVER_LISTEN="127.0.0.1:$PORT" \
  NIK_DATABASE_HOST=127.0.0.1 \
  NIK_DATABASE_PORT=5432 \
  NIK_DATABASE_NAME="$DB" \
  NIK_DATABASE_USER="$DB_USER" \
  NIK_DATABASE_PASSWORD="$DB_PASS" \
  NIK_MIDDLEWARE_AUTH_JWT_SECRET="pflow2-verify-jwt-secret-0123456789" \
  NIK_ADMIN_PASSWORD="$ADMIN_PASSWORD" \
  CLOUDPIVOT_STORAGE_MASTER_KEY="$MASTER_KEY" \
    setsid nohup "$BACKEND_DIR/target/debug/cloudpivot" >"$RUN_DIR/server.log" 2>&1 < /dev/null &
  PID=$!

  for _ in $(seq 1 60); do
    if curl -sf "$BASE_URL/health" >/dev/null 2>&1; then break; fi
    sleep 1
  done
  if ! curl -sf "$BASE_URL/health" >/dev/null 2>&1; then
    say "实例没起来，日志尾部："
    tail -30 "$RUN_DIR/server.log"
    exit 1
  fi
  pass "隔离实例已就绪：$BASE_URL"
fi

# ---------- 1. HTTP 断言 ----------
say ""
say "== HTTP 全链路（pflow2_notify.py）=="
BASE_URL="$BASE_URL" PFLOW2_ADMIN_PASSWORD="$ADMIN_PASSWORD" \
  python3 "$REPO_ROOT/scripts/verify/pflow2_notify.py" --base "$BASE_URL" --password "$ADMIN_PASSWORD"
[ $? -eq 0 ] && pass "HTTP 断言全绿" || fail "HTTP 断言有失败项（见上）"

# ---------- 2. 库侧复核（卡面 §四 ③④） ----------
say ""
say "== 库侧复核 =="
INSTANCE=$(psql_q "SELECT id FROM biz_flow_instance WHERE biz_no = 'PO-PFLOW2-E2E' ORDER BY started_at DESC LIMIT 1")
if [ -z "$INSTANCE" ] || [ "${INSTANCE#*ERR}" != "$INSTANCE" ]; then
  fail "查不到验收实例（$INSTANCE）"
else
  pass "验收实例：$INSTANCE"

  NOTICES=$(psql_q "SELECT count(*) FROM sys_notice WHERE biz_id = '$INSTANCE' AND biz_type = 'FLOW_NOTIFY'")
  [ "${NOTICES:-0}" = "1" ] && pass "sys_notice 有 1 条 FLOW_NOTIFY 通知" || fail "FLOW_NOTIFY 通知条数=$NOTICES（应为 1）"

  RECEIVERS=$(psql_q "SELECT count(*) FROM sys_notice_receiver r JOIN sys_notice n ON n.id = r.notice_id
                      WHERE n.biz_id = '$INSTANCE' AND n.biz_type = 'FLOW_NOTIFY'")
  [ "${RECEIVERS:-0}" -ge 1 ] && pass "通知已扇出给 $RECEIVERS 人" || fail "通知没有接收人"

  NODE=$(psql_q "SELECT string_agg(DISTINCT status, ',') FROM biz_flow_instance_node
                 WHERE instance_id = '$INSTANCE' AND node_key = 'n_notify'")
  [ "$NODE" = "APPROVED" ] && pass "通知节点状态 = APPROVED（非阻塞）" || fail "通知节点状态=$NODE（应为 APPROVED）"

  STATUS=$(psql_q "SELECT status FROM biz_flow_instance WHERE id = '$INSTANCE'")
  [ "$STATUS" = "APPROVED" ] && pass "实例走完（$STATUS）" || fail "实例状态=$STATUS"

  say ""
  say "通知原文（审计留痕）："
  psql_q "SELECT COALESCE(template_code,'') || ' | ' || title || ' | ' || receiver_count
            FROM sys_notice WHERE biz_id = '$INSTANCE' AND biz_type = 'FLOW_NOTIFY'"
fi

say ""
if [ "$FAILED" -eq 0 ]; then
  say "RESULT: 全部通过"
  exit 0
fi
say "RESULT: $FAILED 项失败"
exit 1
