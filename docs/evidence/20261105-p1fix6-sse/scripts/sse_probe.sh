#!/usr/bin/env bash
# P1-FIX-6 探针：验证 SSE 鉴权口径（?token= 只对白名单端点生效，其它一律 401）
#
# 用法：bash sse_probe.sh <轮次标签>
#   环境：BASE（默认 http://127.0.0.1:5140，走 vite 反代，与浏览器同一条路）
#         BE   （默认 http://127.0.0.1:18140，直连后端）
#         PW_FILE（默认 /tmp/p1fix6-iso/admin.pw）
# 输出：只打印结论与状态码，令牌一律掩码（不落日志）。
set -uo pipefail
ROUND=${1:-r1}
BASE=${BASE:-http://127.0.0.1:5140}
BE=${BE:-http://127.0.0.1:18140}
PW_FILE=${PW_FILE:-/tmp/p1fix6-iso/admin.pw}
PW=$(cat "$PW_FILE")
PASS=0; FAIL=0
ok()   { PASS=$((PASS+1)); printf '  PASS  %s\n' "$*"; }
bad()  { FAIL=$((FAIL+1)); printf '  FAIL  %s\n' "$*"; }
mask() { sed -E 's/(token=)[A-Za-z0-9._-]+/\1***/g'; }

echo "=== P1-FIX-6 SSE 鉴权探针（$ROUND，BASE=$BASE） ==="

# ---- 0. 登录拿 access token（令牌只在变量里，不打印） ----
LOGIN=$(curl -sS -m 15 -X POST "$BE/api/system/auth/login" \
  -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$PW\"}")
TOKEN=$(printf '%s' "$LOGIN" | python3 -c 'import json,sys; print((json.load(sys.stdin).get("data") or {}).get("access_token",""))')
if [[ -n "$TOKEN" ]]; then ok "登录成功（access_token 长度 ${#TOKEN}，不落日志）"; else
  bad "登录失败：$(printf '%s' "$LOGIN" | head -c 200)"; echo "FAIL=$FAIL"; exit 1; fi

# ---- 1. 白名单端点：?token= 必须 200 且是 text/event-stream ----
HDR=$(mktemp); BODY=$(mktemp)
CODE=$(curl -sS -N -m 8 -D "$HDR" -o "$BODY" -w '%{http_code}' \
  "$BE/api/business/notice/stream?token=$TOKEN" || true)
CT=$(grep -i '^content-type:' "$HDR" | tr -d '\r' | head -1)
TTFB=$(curl -sS -N -m 3 -o /dev/null -w '%{time_starttransfer}' \
  "$BE/api/business/notice/stream?token=$TOKEN" || true)
echo "  ① /api/business/notice/stream?token=*** → HTTP $CODE | $CT | TTFB=${TTFB}s"
[[ "$CODE" == "200" ]] && ok "白名单端点接受 ?token=（200，不再是 401）" || bad "白名单端点仍未放行（$CODE）"
grep -qi 'text/event-stream' "$HDR" && ok "响应是事件流（text/event-stream）" || bad "Content-Type 不是事件流"
grep -q 'keep-alive' "$BODY" && echo "  ① 心跳帧：$(grep -m1 keep-alive "$BODY")" \
  || echo "  ① 心跳帧：8s 内未见到（心跳 15s 一次，见下面 15s 那条）"
echo "  ① 首帧（建连即刻必到，浏览器 fetch 就是靠它 resolve）：$(head -c 40 "$BODY" | tr '\n' '|')"

# ---- 2. 同一令牌走 Authorization 头也必须 200（两条通道等价） ----
CODE2=$(curl -sS -N -m 3 -o /dev/null -w '%{http_code}' \
  -H "Authorization: Bearer $TOKEN" "$BE/api/business/notice/stream" || true)
echo "  ② /api/business/notice/stream（Authorization 头）→ HTTP $CODE2"
[[ "$CODE2" == "200" ]] && ok "带头认证同样 200（前端 fetch 流走的就是这条）" || bad "带头认证异常（$CODE2）"

# ---- 3. 非白名单端点：?token= 必须 401（fail-closed，不放大攻击面） ----
for p in /api/business/notice/unread-count /api/system/auth/me /api/business/notice/page; do
  C=$(curl -sS -m 10 -o /dev/null -w '%{http_code}' "$BE${p}?token=$TOKEN" || true)
  echo "  ③ ${p}?token=*** → HTTP $C"
  [[ "$C" == "401" ]] && ok "非白名单端点不采信查询令牌（$p → 401）" \
    || bad "非白名单端点被放行/异常（$p → $C）"
done

# ---- 4. 白名单端点不带任何凭证 → 401 ----
C=$(curl -sS -m 10 -o /dev/null -w '%{http_code}' "$BE/api/business/notice/stream" || true)
echo "  ④ /api/business/notice/stream（无凭证）→ HTTP $C"
[[ "$C" == "401" ]] && ok "无凭证仍 401" || bad "无凭证竟然不是 401（$C）"

# ---- 5. 环境变量覆盖（改配置不改代码的运维口径）：同端口影子实例不做，这里只自检解析 ----
echo "  ⑤ 白名单解析（启动日志实证）："
grep -m1 "查询参数令牌白名单" /tmp/p1fix6-iso/backend.log | sed -E 's/^.*INFO/INFO/' | sed 's/^/     /'

# ---- 6. 15s 心跳（证明长连接真的活着，而不是「200 完就断」） ----
HB=$(curl -sS -N -m 18 "$BE/api/business/notice/stream?token=$TOKEN" 2>/dev/null | grep -c 'keep-alive' || true)
echo "  ⑥ 18s 订阅窗口内收到 keep-alive 帧数：$HB"
[[ "$HB" -ge 1 ]] && ok "长连接存活（收到心跳）" || bad "18s 内没有心跳，连接可能被代理截断"

# ---- 7. 服务端连接日志（handler 落点） ----
echo "  ⑦ 服务端连接日志（最近 3 条「SSE 订阅请求已受理」）："
grep "SSE 订阅请求已受理" /tmp/p1fix6-iso/backend.log | tail -3 | sed 's/^/     /'
N=$(grep -c "SSE 订阅请求已受理" /tmp/p1fix6-iso/backend.log || true)
[[ "$N" -ge 1 ]] && ok "服务端有连接日志（累计 $N 条）" || bad "服务端没有连接日志"

printf '=== %s 结果：PASS=%d FAIL=%d ===\n' "$ROUND" "$PASS" "$FAIL"
[[ "$FAIL" -eq 0 ]]
