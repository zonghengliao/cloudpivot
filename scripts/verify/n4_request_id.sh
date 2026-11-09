#!/usr/bin/env bash
# =============================================================================
# n4_request_id.sh — request_id / trace 贯穿验收（卡 NEAR-FIX-4 / N-4）
#
# 用途：在一台**已部署本仓库、且环境文件就绪**的机器上（默认 145）起一个临时实例，
#       用真实 token curl 打一次请求，然后从 journald 里按 ID grep 回全部日志。
#
# 用法：
#   bash scripts/verify/n4_request_id.sh                # 默认读 /etc/cloudpivot/cloudpivot-dev.env
#   ENVFILE=/path/to/x.env PORT=8036 OUT=/tmp/n4 bash scripts/verify/n4_request_id.sh
#   KEEP=1 bash scripts/verify/n4_request_id.sh         # 结束时**不**停实例（留给人工 curl）
#
# 退出码：0 = 全部断言通过 ｜ 1 = 有断言失败（明细已打印）｜ 2 = 环境/启动失败
#
# 六条断言（对应卡面验收 ①~⑤ + 安全）：
#   ① 客户端自带 X-Request-Id → 响应头原样回传同一个值
#   ② 不带头 → 响应头给出服务端生成的 UUID v7（**不是** `-`）
#   ③ 该 ID 在日志里的命中行 ≥ 3（框架访问日志 + span 收尾 + 本层收尾），且**全是同一个 ID**
#   ④ 日志里不再出现 `"request_id":"-"` 占位符
#   ⑤ operation_logs 落库的 request_id 与响应头一致（可 join 审计）
#   ⑥ 脏头（含空格）被丢弃并换成服务端 ID（防日志/头注入）
# =============================================================================
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ENVFILE="${ENVFILE:-/etc/cloudpivot/cloudpivot-dev.env}"
PORT="${PORT:-8036}"
UNIT="${UNIT:-n4-verify}"
BASE="http://127.0.0.1:${PORT}"
OUT="${OUT:-${REPO_ROOT}/docs/evidence/20261110-n4-request-id}"
BIN="${BIN:-${REPO_ROOT}/backend/target/debug/cloudpivot}"
KEEP="${KEEP:-0}"

FAIL=0
ok()   { printf '  ✓ %s\n' "$*"; }
bad()  { printf '  ✗ %s\n' "$*"; FAIL=$((FAIL + 1)); }
info() { printf '  · %s\n' "$*"; }

mkdir -p "$OUT"

# ---------------------------------------------------------------- 环境检查
[ -r "$ENVFILE" ] || { echo "FATAL: 读不到环境文件 $ENVFILE"; exit 2; }
[ -x "$BIN" ]     || { echo "FATAL: 找不到可执行文件 $BIN（先 cargo build）"; exit 2; }
command -v curl >/dev/null || { echo "FATAL: 缺 curl"; exit 2; }
command -v python3 >/dev/null || { echo "FATAL: 缺 python3"; exit 2; }

# 读环境文件（只取本脚本要用的几个键，不 export 全量，避免污染 shell）
env_get() { grep -E "^$1=" "$ENVFILE" | tail -1 | cut -d= -f2-; }
ADMIN_USER="admin"
ADMIN_PASS="$(env_get NIK_ADMIN_PASSWORD)"
DB_NAME="$(env_get NIK_DATABASE_NAME)"
DB_USER="$(env_get NIK_DATABASE_USER)"
DB_PASS="$(env_get NIK_DATABASE_PASSWORD)"
DB_HOST="$(env_get NIK_DATABASE_HOST)"
DB_PORT="$(env_get NIK_DATABASE_PORT)"
[ -n "$ADMIN_PASS" ] || { echo "FATAL: 环境文件里没有 NIK_ADMIN_PASSWORD"; exit 2; }

# 临时实例的私有环境文件：原文件去掉监听端口 / redis 库，再追加本脚本的覆盖
# （JSON 日志是本卡要断言的形态；RUST_LOG=debug 只为让 SQL 日志出现，证明 SQL 也在 span 内）
TMP_ENV="$(mktemp /tmp/n4-verify-env.XXXXXX)"
grep -vE '^(NIK_SERVER_LISTEN|NIK_REDIS_URL|RUST_LOG|NIK_LOG_FORMAT)=' "$ENVFILE" > "$TMP_ENV"
{
  echo "NIK_SERVER_LISTEN=127.0.0.1:${PORT}"
  echo "NIK_REDIS_URL=redis://127.0.0.1:6379/4"
  echo "NIK_LOG_FORMAT=json"
  echo "RUST_LOG=debug"
} >> "$TMP_ENV"
chmod 600 "$TMP_ENV"

cleanup() {
  if [ "$KEEP" != "1" ]; then
    systemctl stop "$UNIT" >/dev/null 2>&1 || true
  fi
  rm -f "$TMP_ENV"
}
trap cleanup EXIT

echo "== 启动临时实例 ${UNIT} @ ${BASE}（profile 取环境文件，日志 JSON，redis db 4）"
systemctl stop "$UNIT" >/dev/null 2>&1 || true
systemd-run --unit="$UNIT" --collect \
  --property=EnvironmentFile="$TMP_ENV" \
  --working-directory="${REPO_ROOT}/backend" \
  "$BIN" >/dev/null || { echo "FATAL: systemd-run 启动失败"; exit 2; }

READY=0
for _ in $(seq 1 60); do
  if curl -fsS --max-time 2 "${BASE}/health" >/dev/null 2>&1; then READY=1; break; fi
  sleep 1
done
if [ "$READY" != "1" ]; then
  echo "FATAL: 实例 60s 内没起来，最近日志："
  journalctl -u "$UNIT" --no-pager -n 30 -o cat || true
  exit 2
fi
ok "实例已就绪"

# ---------------------------------------------------------------- ① 自带 ID
CLIENT_ID="n4-gw-$(date +%Y%m%d%H%M%S)-$$"
echo "== ① 客户端自带 X-Request-Id: ${CLIENT_ID}"
curl -sS -D "${OUT}/01_login_headers.txt" -o "${OUT}/01_login_body.json" \
  -X POST "${BASE}/api/system/auth/login" \
  -H 'Content-Type: application/json' \
  -H "X-Request-Id: ${CLIENT_ID}" \
  -d "{\"username\":\"${ADMIN_USER}\",\"password\":\"${ADMIN_PASS}\"}"
RESP_ID="$(grep -i '^x-request-id:' "${OUT}/01_login_headers.txt" | tr -d '\r' | awk '{print $2}')"
[ "$RESP_ID" = "$CLIENT_ID" ] && ok "响应头回传同一个 ID（${RESP_ID}）" \
  || bad "响应头 ${RESP_ID:-<缺失>} ≠ 客户端传入的 ${CLIENT_ID}"

TOKEN="$(python3 - "${OUT}/01_login_body.json" <<'PY'
import json, sys
try:
    body = json.load(open(sys.argv[1]))
except Exception:
    print(""); raise SystemExit
print(((body.get("data") or {}).get("access_token")) or "")
PY
)"
if [ -n "$TOKEN" ]; then ok "登录成功，拿到真实 access_token（${#TOKEN} 字符）"
else bad "登录失败，响应：$(head -c 300 "${OUT}/01_login_body.json")"; fi

# 令牌是凭据，**不入库**：证据文件只留结构 + 长度（取值已在上面取到，后续 curl 用它）。
python3 - "${OUT}/01_login_body.json" <<'PY'
import json, sys
path = sys.argv[1]
try:
    raw = json.load(open(path, encoding="utf-8"))
except Exception:
    raise SystemExit
data = raw.get("data") or {}
masked = {
    "_note": "登录响应体：令牌已脱敏（JWT 也是凭据，原值不入库）。仅用于证明真实登录成功。",
    "code": raw.get("code"),
    "msg": raw.get("msg"),
    "data": {
        "access_token": "<REDACTED>",
        "refresh_token": "<REDACTED>",
        "access_token_chars": len(data.get("access_token") or ""),
        "other_fields": sorted(k for k in data if k not in ("access_token", "refresh_token")),
    },
}
open(path, "w", encoding="utf-8").write(json.dumps(masked, ensure_ascii=False, indent=1) + "\n")
PY

# ---------------------------------------------------------------- ② 服务端生成
echo "== ② 不带头 → 服务端生成"
curl -sS -D "${OUT}/02_headers_generated.txt" -o /dev/null \
  -H "Authorization: Bearer ${TOKEN}" \
  "${BASE}/api/system/user/page?page=1&page_size=5"
GEN_ID="$(grep -i '^x-request-id:' "${OUT}/02_headers_generated.txt" | tr -d '\r' | awk '{print $2}')"
if printf '%s' "$GEN_ID" | grep -Eq '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'; then
  ok "生成的 ID 是 UUID 形态：${GEN_ID}"
else
  bad "生成的 ID 不合规或缺失：${GEN_ID:-<缺失>}"
fi
[ "$GEN_ID" != "-" ] && ok "不是占位符 '-'" || bad "仍是占位符 '-'"

# ---------------------------------------------------------------- ⑥ 脏头
echo "== ⑥ 脏头（含空格）必须被丢弃重生成"
curl -sS -D "${OUT}/06_headers_malformed.txt" -o /dev/null \
  -H "X-Request-Id: bad id with space" \
  "${BASE}/health/deps"
BAD_ID="$(grep -i '^x-request-id:' "${OUT}/06_headers_malformed.txt" | tr -d '\r' | awk '{print $2}')"
if [ "$BAD_ID" != "bad id with space" ] && printf '%s' "$BAD_ID" | grep -Eq '^[0-9a-f]{8}-'; then
  ok "脏值被换成服务端 ID：${BAD_ID}"
else
  bad "脏值没被替换：${BAD_ID:-<缺失>}"
fi

# ---------------------------------------------------------------- ③ 日志全同 ID
echo "== ③ 按 request_id grep 日志"
sleep 1   # 给 journald 一点落盘时间
journalctl -u "$UNIT" --no-pager -o cat > "${OUT}/03_all_logs.jsonl"
grep -F "$CLIENT_ID" "${OUT}/03_all_logs.jsonl" > "${OUT}/03_grep_client_id.jsonl" || true
HITS="$(wc -l < "${OUT}/03_grep_client_id.jsonl")"
DISTINCT="$(grep -oE '"request_id":"[^"]+"' "${OUT}/03_grep_client_id.jsonl" | sort -u | wc -l)"
[ "$HITS" -ge 3 ] && ok "命中 ${HITS} 行（≥3：访问日志 + span 收尾 + 本层收尾 + SQL）" \
  || bad "只命中 ${HITS} 行，不足 3 行"
[ "$DISTINCT" -le 1 ] && ok "这些行的 request_id 只有 1 种取值" \
  || bad "出现 ${DISTINCT} 种 request_id（同一请求内 ID 漂移）"
grep -Fq '"request_id":"-"' "${OUT}/03_all_logs.jsonl" \
  && bad "日志里仍出现 \"request_id\":\"-\" 占位符" \
  || ok "日志里没有 \"request_id\":\"-\" 占位符"

# 生成那份也要能按 ID 找回（证明「服务端生成」与「客户端传入」两条路都通）
grep -Fq "$GEN_ID" "${OUT}/03_all_logs.jsonl" \
  && ok "服务端生成的 ID ${GEN_ID} 也能在日志里检索到" \
  || bad "服务端生成的 ID 在日志里找不到"

# ---------------------------------------------------------------- ⑤ 落库 join
echo "== ⑤ operation_logs 的 request_id 与响应头一致"
if command -v psql >/dev/null; then
  PGPASSWORD="$DB_PASS" psql -h "${DB_HOST:-127.0.0.1}" -p "${DB_PORT:-5432}" \
    -U "$DB_USER" -d "$DB_NAME" -A -F '|' -t \
    -c "SELECT created_at, username, operation, request_path, status_code, request_id
          FROM operation_logs WHERE request_id = '${CLIENT_ID}' ORDER BY created_at DESC;" \
    > "${OUT}/05_operation_logs.txt" 2>&1 || true
  if grep -Fq "$CLIENT_ID" "${OUT}/05_operation_logs.txt"; then
    ok "operation_logs 里查到该 request_id：$(head -1 "${OUT}/05_operation_logs.txt")"
  else
    bad "operation_logs 里没查到 ${CLIENT_ID}（内容见 ${OUT}/05_operation_logs.txt）"
  fi
else
  info "本机没有 psql，跳过落库断言（不算失败）"
fi

# ---------------------------------------------------------------- 汇总
{
  echo "# N-4 验收实测（$(date -Is) @ $(hostname)）"
  echo "实例: ${UNIT} @ ${BASE}（profile=dev, log=json, redis db 4）"
  echo "客户端自带 ID: ${CLIENT_ID} → 响应头 ${RESP_ID}"
  echo "服务端生成 ID: ${GEN_ID}"
  echo "脏头替换后 ID: ${BAD_ID}"
  echo "grep 命中行数: ${HITS}（distinct request_id: ${DISTINCT}）"
  echo "结果: $([ "$FAIL" -eq 0 ] && echo 全绿 || echo "失败 ${FAIL} 项")"
} | tee "${OUT}/00_summary.txt"

echo
[ "$FAIL" -eq 0 ] && { echo "RESULT: PASS"; exit 0; } || { echo "RESULT: FAIL(${FAIL})"; exit 1; }
