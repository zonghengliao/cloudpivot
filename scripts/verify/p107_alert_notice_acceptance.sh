#!/usr/bin/env bash
# P1-07 验收：告警通道 `alert_check.sh` → 站内消息（`POST /internal/alert` → `sys_notice`）
#
#   bash scripts/verify/p107_alert_notice_acceptance.sh <端口> <库连接串> [输出目录]
#   例：bash scripts/verify/p107_alert_notice_acceptance.sh 18097 \
#         'postgres://alun:***@127.0.0.1:5432/cloudpivot_p107_test' \
#         docs/evidence/2026-11-13-p107-alert-notice
#
# 断言（任何一条不满足 → 退出码 1，日志里能看到是哪条）：
#   ① 实例活着（`/health` 200）
#   ② **回环**调用成功：HTTP 200 + `code=0` + `receiver_count ≥ 1`
#   ③ 落库正确：新增 1 条 `sys_notice`（`notice_type=ALERT` / `level=ERROR` /
#      `target_type=ROLE` / `sender_name=alert_check`），`sys_notice_receiver`
#      行数 = 回写的 `receiver_count`，且**超管角色下的账号**在收件人里
#   ④ **非回环**（本机 LAN IP）不带 token → HTTP 403 + `code=30001`，且**不落库**
#   ⑤ 非回环带对内网 token → 200（服务端注入 `NIK_ALERT_INTERNAL_TOKEN` 时才测）
#   ⑥ 幂等：同一 `idempotency_key` 重放 → `duplicated=true`，库行数仍为 1
#   ⑦ 非法级别 → 400 + `code=10001`
#   ⑧ 服务端能识别「超管收件人为 0」以外的正常路径（`notice_no` 前缀 `NTA`）
#
# 说明：本脚本只做**读**库断言（psql），写库全部由被测实例完成；
#       证据落盘到输出目录，跑完自动清理它自己造的 `p107acc:%` 通知。
set -uo pipefail

# 脚本在仓库根目录跑（第 7 步要调 scripts/ops/alert_check.sh）
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT" || exit 1

PORT="${1:?用法：p107_alert_notice_acceptance.sh <端口> <库连接串> [输出目录]}"
DB_URL="${2:?缺少库连接串（postgres://user:pass@host:port/db）}"
OUT="${3:-docs/evidence/2026-11-13-p107-alert-notice}"
mkdir -p "$OUT"

BASE="http://127.0.0.1:${PORT}"
LAN_IP="$(hostname -I 2>/dev/null | awk '{print $1}')"
LAN_BASE="http://${LAN_IP}:${PORT}"
TOKEN="${NIK_ALERT_INTERNAL_TOKEN:-}"
KEY_PREFIX="p107acc"
KEY="$(printf '%s:%s' "$KEY_PREFIX" "$(date +%s)-$$")"

FAIL=0
pass() { echo "PASS: $*"; }
fail() { echo "FAIL: $*"; FAIL=1; }

q() { psql "$DB_URL" -X -q -A -t -v ON_ERROR_STOP=1 -c "$1" 2>/dev/null; }

cleanup() { q "DELETE FROM sys_notice WHERE idempotency_key LIKE '${KEY_PREFIX}:%'" >/dev/null || true; }
trap cleanup EXIT

# 发一条告警；输出 `<http_code>|<body>`（写到文件，避免命令替换吞掉变量）
post_alert() {
  local target="$1" body="$2" extra_header="${3:-}"
  local tmp code
  tmp="$(mktemp)"
  if [ -n "$extra_header" ]; then
    code="$(printf '%s' "$body" | curl -sS -m 8 -o "$tmp" -w '%{http_code}' \
      -X POST -H 'Content-Type: application/json' -H "$extra_header" --data-binary @- \
      "${target}/internal/alert" 2>/dev/null)" || code=000
  else
    code="$(printf '%s' "$body" | curl -sS -m 8 -o "$tmp" -w '%{http_code}' \
      -X POST -H 'Content-Type: application/json' --data-binary @- \
      "${target}/internal/alert" 2>/dev/null)" || code=000
  fi
  printf '%s|%s' "${code:-000}" "$(cat "$tmp" 2>/dev/null)"
  rm -f "$tmp"
}

echo "=== 0. 实例可达性 ${BASE} ==="
code="$(curl -s -o "$OUT/00_health.json" -w '%{http_code}' "${BASE}/health")"
echo "/health -> ${code}"
if [ "$code" = "200" ]; then pass "实例活着（/health 200）"; else fail "实例不可用（/health ${code}）"; exit 1; fi

echo
echo "=== 1. 回环调用：critical 告警 ==="
body="$(python3 - "$KEY" <<'PY'
import json, sys
print(json.dumps({
    "level": "critical",
    "title": "【验收】磁盘使用率 99%（P1-07）",
    "content": "回环调用：巡检 → /internal/alert → 站内消息",
    "source": "alert_check",
    "idempotency_key": sys.argv[1],
}, ensure_ascii=False))
PY
)"
resp="$(post_alert "$BASE" "$body")"
printf '%s\n' "${resp#*|}" > "$OUT/01_loopback.json"
echo "HTTP ${resp%%|*}  body=$(head -c 300 <<<"${resp#*|}")"
[ "${resp%%|*}" = "200" ] && pass "回环调用 HTTP 200" || fail "回环调用 HTTP ${resp%%|*}"
grep -q '"code": *0' <<<"${resp#*|}" && pass "响应 code=0" || fail "响应 code 非 0"

notice_id="$(grep -o '"notice_id": *"[^"]*"' <<<"${resp#*|}" | head -1 | cut -d'"' -f4)"
notice_no="$(grep -o '"notice_no": *"[^"]*"' <<<"${resp#*|}" | head -1 | cut -d'"' -f4)"
receiver_count="$(grep -o '"receiver_count": *[0-9]*' <<<"${resp#*|}" | head -1 | grep -o '[0-9]*$')"
echo "notice_id=${notice_id:-<空>} notice_no=${notice_no:-<空>} receiver_count=${receiver_count:-<空>}"
[ -n "${notice_id:-}" ] && pass "返回 notice_id" || fail "未返回 notice_id"
[ "${notice_no:0:3}" = "NTA" ] && pass "notice_no 前缀 NTA（ALERT 类型）" || fail "notice_no 前缀异常：${notice_no:-<空>}"
[ "${receiver_count:-0}" -ge 1 ] 2>/dev/null && pass "receiver_count=${receiver_count} ≥ 1" \
  || fail "receiver_count=${receiver_count:-<空>} 不是 ≥1（超管角色下没人？）"

echo
echo "=== 2. 落库核对（sys_notice / sys_notice_receiver） ==="
q "SELECT notice_type || '|' || level || '|' || target_type || '|' || sender_name || '|' || title
     FROM sys_notice WHERE idempotency_key = '${KEY}'" > "$OUT/02_notice_row.txt"
cat "$OUT/02_notice_row.txt"
row="$(cat "$OUT/02_notice_row.txt")"
case "$row" in
  "ALERT|ERROR|ROLE|alert_check|"*) pass "落库字段正确：ALERT / ERROR / ROLE / alert_check" ;;
  *) fail "落库字段不符合契约：${row:-<无行>}" ;;
esac

rows="$(q "SELECT COUNT(*) FROM sys_notice WHERE idempotency_key = '${KEY}'")"
[ "$rows" = "1" ] && pass "sys_notice 恰好 1 行" || fail "sys_notice 行数=${rows:-<空>}（应为 1）"

db_receivers="$(q "SELECT COUNT(*) FROM sys_notice_receiver WHERE notice_id = '${notice_id}'")"
[ "$db_receivers" = "${receiver_count:-x}" ] && pass "收件人行数 ${db_receivers} = receiver_count" \
  || fail "收件人行数 ${db_receivers:-<空>} ≠ receiver_count ${receiver_count:-<空>}"

# 收件人必须是「超管角色下的可用账号」——直接按角色反查，不猜用户名
super_hit="$(q "SELECT COUNT(*) FROM sys_notice_receiver r
                 JOIN sys_user_role ur ON ur.user_id = r.user_id
                 JOIN sys_role ro ON ro.id = ur.role_id
                WHERE r.notice_id = '${notice_id}'
                  AND ro.role_code = 'SUPER_ADMIN' AND ro.deleted_at IS NULL")"
[ "${super_hit:-0}" -ge 1 ] 2>/dev/null && pass "收件人里有 ${super_hit} 个超管角色账号" \
  || fail "收件人里没有超管角色账号（super_hit=${super_hit:-<空>}）"

echo
echo "=== 3. 非回环调用（${LAN_IP:-<无 LAN IP>}）不带 token → 必须 403 ==="
if [ -z "$LAN_IP" ] || [ "$LAN_IP" = "127.0.0.1" ]; then
  echo "SKIP: 本机没有非回环地址，跳过准入闸反证"
else
  body_lan="$(python3 - "$KEY" <<'PY'
import json, sys
print(json.dumps({
    "level": "warning", "title": "【验收】非回环应被拒", "content": "不带 token",
    "source": "p107_acceptance", "idempotency_key": sys.argv[1] + ":lan",
}, ensure_ascii=False))
PY
)"
  resp_lan="$(post_alert "$LAN_BASE" "$body_lan")"
  printf '%s\n' "${resp_lan#*|}" > "$OUT/03_lan_without_token.json"
  echo "HTTP ${resp_lan%%|*}  body=$(head -c 300 <<<"${resp_lan#*|}")"
  [ "${resp_lan%%|*}" = "403" ] && pass "非回环无 token → HTTP 403" || fail "非回环无 token → HTTP ${resp_lan%%|*}"
  grep -q '"code": *30001' <<<"${resp_lan#*|}" && pass "错误码 30001（权限不足）" || fail "错误码不是 30001"
  leaked="$(q "SELECT COUNT(*) FROM sys_notice WHERE idempotency_key = '${KEY}:lan'")"
  [ "${leaked:-x}" = "0" ] && pass "被拒的调用没有落库" || fail "被拒的调用竟然落库了（${leaked:-<空>} 行）"

  echo
  echo "=== 4. 非回环 + 内网 token ==="
  if [ -z "$TOKEN" ]; then
    echo "SKIP: 未注入 NIK_ALERT_INTERNAL_TOKEN，跳过「带 token 放行」正证"
  else
    body_tok="$(python3 - "$KEY" <<'PY'
import json, sys
print(json.dumps({
    "level": "warning", "title": "【验收】非回环 + 内网 token", "content": "带对 token 应放行",
    "source": "p107_acceptance", "idempotency_key": sys.argv[1] + ":tok",
}, ensure_ascii=False))
PY
)"
    resp_tok="$(post_alert "$LAN_BASE" "$body_tok" "X-Internal-Token: ${TOKEN}")"
    printf '%s\n' "${resp_tok#*|}" > "$OUT/04_lan_with_token.json"
    echo "HTTP ${resp_tok%%|*}  body=$(head -c 300 <<<"${resp_tok#*|}")"
    [ "${resp_tok%%|*}" = "200" ] && pass "非回环带对 token → HTTP 200" || fail "非回环带对 token → HTTP ${resp_tok%%|*}"
    resp_bad="$(post_alert "$LAN_BASE" "$body_tok" "X-Internal-Token: wrong-token")"
    [ "${resp_bad%%|*}" = "403" ] && pass "非回环带错 token → HTTP 403" || fail "非回环带错 token → HTTP ${resp_bad%%|*}"
  fi
fi

echo
echo "=== 5. 幂等重放：同一 key 不新增行 ==="
resp_dup="$(post_alert "$BASE" "$body")"
printf '%s\n' "${resp_dup#*|}" > "$OUT/05_replay.json"
grep -q '"duplicated": *true' <<<"${resp_dup#*|}" && pass "重放判为 duplicated=true" || fail "重放未判为重复"
rows_after="$(q "SELECT COUNT(*) FROM sys_notice WHERE idempotency_key = '${KEY}'")"
[ "$rows_after" = "1" ] && pass "重放后 sys_notice 仍 1 行" || fail "重放后 sys_notice 行数=${rows_after:-<空>}"

echo
echo "=== 6. 非法级别 → 400 ==="
bad_body='{"level":"fatal","title":"非法级别","content":"应被拒绝"}'
resp_bad_level="$(post_alert "$BASE" "$bad_body")"
printf '%s\n' "${resp_bad_level#*|}" > "$OUT/06_bad_level.json"
echo "HTTP ${resp_bad_level%%|*}  body=$(head -c 300 <<<"${resp_bad_level#*|}")"
[ "${resp_bad_level%%|*}" = "400" ] && pass "非法级别 → HTTP 400" || fail "非法级别 → HTTP ${resp_bad_level%%|*}"
grep -q '"code": *10001' <<<"${resp_bad_level#*|}" && pass "错误码 10001（参数非法）" || fail "错误码不是 10001"

echo
echo "=== 7. 巡检脚本 → 站内消息（alert_check.sh 端到端） ==="
# 用一个必然触发的阈值跑脚本：磁盘阈值 1% ⇒ 一定 ALERT；只跑 disk 一项，kanban/飞书全关。
STATE_DIR_TMP="$(mktemp -d)"
ALERT_LOG_TMP="$STATE_DIR_TMP/ALERT.log"
if NIK_ALERT_BASE_URL="$BASE" \
   NIK_ALERT_DISK_THRESHOLD=1 NIK_ALERT_DISK_WARN=1 \
   NIK_ALERT_KANBAN=off NIK_ALERT_FEISHU=off \
   NIK_ALERT_STATE_DIR="$STATE_DIR_TMP/state" \
   NIK_ALERT_LOG="$ALERT_LOG_TMP" \
   NIK_ALERT_NOTICE_URL="${BASE}/internal/alert" \
   NIK_ALERT_DEDUP_SECONDS=0 \
   bash scripts/ops/alert_check.sh --only disk --force > "$OUT/07_alert_check.out" 2>&1
then
  pass "alert_check.sh 退出码 0"
else
  fail "alert_check.sh 退出码非 0"
fi
tail -6 "$OUT/07_alert_check.out"
grep -q "站内消息: 已送达超管" "$OUT/07_alert_check.out" \
  && pass "脚本日志确认「站内消息: 已送达超管」" \
  || fail "脚本日志里没有送达回执"
script_row="$(q "SELECT COUNT(*) FROM sys_notice
                  WHERE sender_name = 'alert_check' AND biz_type = 'ALERT'
                    AND content LIKE '%scripts/ops/alert_check.sh 自动产生%'
                    AND created_at > now() - interval '5 minutes'")"
[ "${script_row:-0}" -ge 1 ] 2>/dev/null && pass "脚本触发的告警已落库（${script_row} 行）" \
  || fail "脚本触发的告警没落库"
rm -rf "$STATE_DIR_TMP"

echo
if [ "$FAIL" -eq 0 ]; then
  echo "验收通过：全部断言 PASS（证据在 ${OUT}）"
  exit 0
fi
echo "验收失败：见上面 FAIL 行（证据在 ${OUT}）"
exit 1
