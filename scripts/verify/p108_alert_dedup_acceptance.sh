#!/usr/bin/env bash
# P1-08 验收：站内告警分级 + 10 分钟去重（`POST /internal/alert` → `sys_notice` + `sys_notice_alert_dedup`）
#
#   bash scripts/verify/p108_alert_dedup_acceptance.sh <端口> <库连接串> [输出目录]
#   例：bash scripts/verify/p108_alert_dedup_acceptance.sh 18099 \
#         'postgres://alun:***@127.0.0.1:5432/cloudpivot_p108_e2e' \
#         docs/evidence/2026-11-13-p108-alert-dedup
#
# 前提（脚本会自己核对，不满足就 FAIL 并说清楚怎么起）：
#   · 被测实例用**空库首启**，并注入：
#       NIK_ADMIN_PASSWORD=Admin@123456          （消息中心要用超管登录；不注入就是随机口令）
#       NIK_ALERT_DEDUP_WINDOW_SECONDS=2         （把 10 分钟窗口压到 2 秒，否则「窗口过期」这段要真等 11 分钟）
#   · profile=dev（`config-dev.toml` 里 `[middleware.auth] enabled = true`）：
#     `/internal/alert` 是 `no_auth` 的，但「消息中心」要 JWT。
#
# 断言（任何一条不满足 → 退出码 1，日志里能看到是哪条）：
#   ① 实例活着（`/health` 200）+ 响应里的 `window_seconds` 确实是注入的小窗口
#   ② 首发：`published=true` / `deduped=false` / `merged_count=0` / `level=ERROR`（critical→ERROR）
#   ③ 窗口内连发 4 条同 `(source,title)`：全部 `deduped=true`、`merged_count` 递增到 4、**没有** notice_id
#   ④ 库里只有 **1 条** `sys_notice`；`sys_notice_alert_dedup.repeat_count = 4` 且 `notice_id` 指向那条
#   ⑤ 等窗口过期（2 秒 + 1）再发：`published=true`、`merged_count=4`；库里 **2 条**，
#      第 2 条正文含「已重复 4 次」（卡面口径：「近 10 分钟已重复 N 次」）
#   ⑥ 换标题、同 source → 立刻另落 1 条（去重键是 `(source,title)` 二元组，不是 source）
#   ⑦ `warning` → 落库级别 `WARN`（分级映射另一支）
#   ⑧ 消息中心（超管登录）：`pin_critical=true`（缺省）时 `level=ERROR` 的排第一且带 `pinned=true`；
#      `pin_critical=false` 时回到纯时间倒序（最新那条 WARN 排第一）
#
# 说明：写库全部由被测实例完成；本脚本只读库断言（psql）+ 调 API，
#       跑完删掉自己造的 `p108acc` 通知与去重窗口行。
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT" || exit 1

PORT="${1:?用法：p108_alert_dedup_acceptance.sh <端口> <库连接串> [输出目录]}"
DB_URL="${2:?缺少库连接串（postgres://user:***@host:port/db）}"
OUT="${3:-docs/evidence/2026-11-13-p108-alert-dedup}"
mkdir -p "$OUT"

BASE="http://127.0.0.1:${PORT}"
ADMIN_USER="${NIK_ADMIN_USERNAME:-admin}"
ADMIN_PASS="${NIK_ADMIN_PASSWORD:-Admin@123456}"
ADMIN_PASS_NEW="${P108_ADMIN_PASS_NEW:-Admin@123456}"
TAG="p108acc-$(date +%s)-$$"
TITLE_A="【验收】${TAG} 磁盘使用率 92%"
TITLE_B="【验收】${TAG} 导入积压"
SOURCE="p108acc"

FAIL=0
pass() { echo "PASS: $*"; }
fail() { echo "FAIL: $*"; FAIL=1; }

q() { psql "$DB_URL" -X -q -A -t -v ON_ERROR_STOP=1 -c "$1" 2>/dev/null; }

cleanup() {
  q "DELETE FROM sys_notice WHERE sender_name = '${SOURCE}'" >/dev/null || true
  q "DELETE FROM sys_notice_alert_dedup WHERE source = '${SOURCE}'" >/dev/null || true
}
trap cleanup EXIT
cleanup

# 发一条告警；输出 `<http_code>|<body>`（写临时文件，避免命令替换吞变量）
post_alert() {
  local body="$1" tmp code
  tmp="$(mktemp)"
  code="$(printf '%s' "$body" | curl -sS -m 8 -o "$tmp" -w '%{http_code}' \
    -X POST -H 'Content-Type: application/json' --data-binary @- \
    "${BASE}/internal/alert" 2>/dev/null)" || code=000
  printf '%s|%s' "${code:-000}" "$(cat "$tmp" 2>/dev/null)"
  rm -f "$tmp"
}

alert_body() {
  # $1 = title, $2 = level
  jq -cn --arg t "$1" --arg l "$2" --arg s "$SOURCE" \
    '{level:$l, title:$t, content:"P1-08 验收：去重窗口与分级", source:$s}'
}

field() { printf '%s' "$1" | jq -r ".data.$2"; }

echo "=== 0. 实例可达性 + 去重窗口 ==="
code="$(curl -s -o "$OUT/00_health.json" -w '%{http_code}' "${BASE}/health")"
echo "/health -> ${code}"
[ "$code" = "200" ] && pass "实例活着（/health 200）" || { fail "实例不可用（/health ${code}）"; exit 1; }

resp="$(post_alert "$(alert_body "$TITLE_A" critical)")"
printf '%s\n' "${resp#*|}" > "$OUT/01_first_publish.json"
echo "HTTP ${resp%%|*}  body=$(head -c 300 <<<"${resp#*|}")"
[ "${resp%%|*}" = "200" ] && pass "首发 HTTP 200" || { fail "首发 HTTP ${resp%%|*}"; exit 1; }

WINDOW="$(field "${resp#*|}" window_seconds)"
echo "window_seconds=${WINDOW}"
if [ "${WINDOW:-600}" -le 5 ] 2>/dev/null; then
  pass "去重窗口已压到 ${WINDOW}s（NIK_ALERT_DEDUP_WINDOW_SECONDS 生效）"
else
  fail "window_seconds=${WINDOW:-<空>}，请用 NIK_ALERT_DEDUP_WINDOW_SECONDS=2 起被测实例（否则本脚本要真等 11 分钟）"
fi

echo
echo "=== 1. 首发结局（发布 + critical → ERROR） ==="
published="$(field "${resp#*|}" published)"
deduped="$(field "${resp#*|}" deduped)"
merged="$(field "${resp#*|}" merged_count)"
level="$(field "${resp#*|}" level)"
notice_id="$(field "${resp#*|}" notice_id)"
echo "published=${published} deduped=${deduped} merged_count=${merged} level=${level}"
[ "$published" = "true" ] && pass "首发 published=true" || fail "首发 published=${published}"
[ "$deduped" = "false" ] && pass "首发 deduped=false" || fail "首发 deduped=${deduped}"
[ "${merged:-x}" = "0" ] && pass "首发 merged_count=0（不带合并提示）" || fail "首发 merged_count=${merged:-<空>}"
[ "$level" = "ERROR" ] && pass "critical → level=ERROR" || fail "critical → level=${level}"
[ -n "${notice_id:-}" ] && [ "$notice_id" != "null" ] && pass "首发返回 notice_id" || fail "首发没有 notice_id"

echo
echo "=== 2. 窗口内连发 4 条同 (source,title) → 全部合并 ==="
merged_seen=""
for i in 1 2 3 4; do
  resp="$(post_alert "$(alert_body "$TITLE_A" critical)")"
  printf '%s\n' "${resp#*|}" > "$OUT/02_suppress_${i}.json"
  body="${resp#*|}"
  p="$(field "$body" published)"; d="$(field "$body" deduped)"
  m="$(field "$body" merged_count)"; nid="$(field "$body" notice_id)"
  merged_seen="${merged_seen}${m} "
  if [ "$p" = "false" ] && [ "$d" = "true" ] && [ "$nid" = "null" ]; then
    pass "第 $((i + 1)) 条：published=false deduped=true merged_count=${m} notice_id=null"
  else
    fail "第 $((i + 1)) 条结局异常：published=${p} deduped=${d} merged_count=${m} notice_id=${nid}"
  fi
done
echo "merged_count 序列：${merged_seen}"
[ "$merged_seen" = "1 2 3 4 " ] && pass "合并计数 1→2→3→4 逐条递增" || fail "合并计数序列异常：${merged_seen}"

echo
echo "=== 3. 落库核对：只 1 条通知 + 窗口计数 4 ==="
rows="$(q "SELECT COUNT(*) FROM sys_notice WHERE sender_name = '${SOURCE}' AND title = '${TITLE_A}'")"
[ "${rows:-x}" = "1" ] && pass "sys_notice 恰好 1 行（5 次调用只落 1 条）" || fail "sys_notice 行数=${rows:-<空>}（应为 1）"
q "SELECT source || '|' || repeat_count || '|' || (notice_id IS NOT NULL) || '|' || title
     FROM sys_notice_alert_dedup WHERE source = '${SOURCE}' AND title = '${TITLE_A}'" > "$OUT/03_dedup_row.txt"
dedup_row="$(cat "$OUT/03_dedup_row.txt")"
echo "dedup_row=${dedup_row}"
case "$dedup_row" in
  "${SOURCE}|4|true|"*) pass "窗口行：repeat_count=4 且已回填 notice_id" ;;
  *) fail "窗口行不符合契约：${dedup_row:-<无行>}" ;;
esac
db_notice_id="$(q "SELECT notice_id FROM sys_notice_alert_dedup WHERE source = '${SOURCE}' AND title = '${TITLE_A}'")"
[ "$db_notice_id" = "${notice_id:-__none__}" ] && pass "窗口行 notice_id = 首发返回的 notice_id" \
  || fail "窗口行 notice_id=${db_notice_id:-<空>} ≠ 首发 ${notice_id:-<空>}"

echo
echo "=== 4. 等窗口过期（$((WINDOW + 1))s）再发 → 补发第 2 条并带「已重复 4 次」 ==="
sleep $((WINDOW + 1))
resp="$(post_alert "$(alert_body "$TITLE_A" critical)")"
printf '%s\n' "${resp#*|}" > "$OUT/04_republish.json"
body="${resp#*|}"
echo "HTTP ${resp%%|*}  body=$(head -c 300 <<<"$body")"
p="$(field "$body" published)"; m="$(field "$body" merged_count)"; second_id="$(field "$body" notice_id)"
[ "$p" = "true" ] && pass "窗口过期后 published=true" || fail "窗口过期后 published=${p}"
[ "${m:-x}" = "4" ] && pass "merged_count=4（带出上一窗口的合并次数）" || fail "merged_count=${m:-<空>}（应为 4）"

content="$(q "SELECT content FROM sys_notice WHERE id = '${second_id}'")"
printf '%s\n' "$content" > "$OUT/04_second_content.txt"
if grep -q "已重复 4 次" "$OUT/04_second_content.txt"; then
  pass "第 2 条正文含「已重复 4 次」"
else
  fail "第 2 条正文没有合并次数：${content:-<空>}"
fi
rows="$(q "SELECT COUNT(*) FROM sys_notice WHERE sender_name = '${SOURCE}' AND title = '${TITLE_A}'")"
[ "${rows:-x}" = "2" ] && pass "两个窗口各 1 条，共 2 条" || fail "sys_notice 行数=${rows:-<空>}（应为 2）"

echo
echo "=== 5. 换标题（同 source）→ 立刻另落一条（去重键是二元组） ==="
resp="$(post_alert "$(alert_body "$TITLE_B" warning)")"
printf '%s\n' "${resp#*|}" > "$OUT/05_other_title.json"
body="${resp#*|}"
echo "HTTP ${resp%%|*}  body=$(head -c 300 <<<"$body")"
[ "$(field "$body" published)" = "true" ] && pass "不同标题 published=true（没有被同 source 的窗口吃掉）" \
  || fail "不同标题 published=$(field "$body" published)"
[ "$(field "$body" level)" = "WARN" ] && pass "warning → level=WARN" || fail "warning → level=$(field "$body" level)"
warn_row="$(q "SELECT level FROM sys_notice WHERE title = '${TITLE_B}'")"
[ "$warn_row" = "WARN" ] && pass "落库级别 = WARN" || fail "落库级别=${warn_row:-<空>}（应为 WARN）"

echo
echo "=== 6. 消息中心：分级着色 + 严重级别置顶 ==="
login="$(curl -sS -X POST -H 'Content-Type: application/json' \
  -d "$(jq -cn --arg u "$ADMIN_USER" --arg p "$ADMIN_PASS" '{username:$u, password:$p}')" \
  "${BASE}/api/system/auth/login")"
printf '%s\n' "$login" > "$OUT/06_login.json"
token="$(printf '%s' "$login" | jq -r '.data.access_token // empty')"
if [ -z "${token:-}" ]; then
  fail "超管登录失败（$(printf '%s' "$login" | head -c 160)）—— 实例是否注入了 NIK_ADMIN_PASSWORD=${ADMIN_PASS}？"
else
  pass "超管登录成功"
  # 首登强制改密会拦掉所有业务接口：这里改成同一个口令，既解开闸门又不改变后续登录方式
  chg="$(curl -sS -X PUT -H "Authorization: Bearer ${token}" -H 'Content-Type: application/json' \
    -d "$(jq -cn --arg o "$ADMIN_PASS" --arg n "$ADMIN_PASS_NEW" '{old_password:$o, new_password:$n}')" \
    "${BASE}/api/system/auth/password")"
  printf '%s\n' "$chg" > "$OUT/06_change_password.json"
  if printf '%s' "$chg" | grep -q '"code": *0'; then
    pass "首登改密完成（$ADMIN_PASS → $ADMIN_PASS_NEW）"
    login="$(curl -sS -X POST -H 'Content-Type: application/json' \
      -d "$(jq -cn --arg u "$ADMIN_USER" --arg p "$ADMIN_PASS_NEW" '{username:$u, password:$p}')" \
      "${BASE}/api/system/auth/login")"
    token="$(printf '%s' "$login" | jq -r '.data.access_token // empty')"
  else
    echo "（改密失败：$(printf '%s' "$chg" | head -c 160) —— 直接复用当前令牌）"
  fi

  authed_get() {
    curl -sS -H "Authorization: Bearer ${token}" "${BASE}/api/business/notice/mine?page=1&page_size=10&$1"
  }

  pinned_json="$(authed_get 'pin_critical=true')"
  printf '%s\n' "$pinned_json" > "$OUT/06_mine_pinned.json"
  echo "pin_critical=true → $(printf '%s' "$pinned_json" | jq -c '[.data.list[] | {level, pinned, title}]' 2>/dev/null | head -c 400)"
  first_level="$(printf '%s' "$pinned_json" | jq -r '.data.list[0].level // empty')"
  first_pinned="$(printf '%s' "$pinned_json" | jq -r '.data.list[0].pinned // empty')"
  [ "$first_level" = "ERROR" ] && pass "置顶时第一条是 ERROR（严重告警）" || fail "置顶时第一条 level=${first_level:-<空>}"
  [ "$first_pinned" = "true" ] && pass "第一条 pinned=true（前端据此打「置顶」标记）" || fail "第一条 pinned=${first_pinned:-<空>}"

  plain_json="$(authed_get 'pin_critical=false')"
  printf '%s\n' "$plain_json" > "$OUT/06_mine_plain.json"
  echo "pin_critical=false → $(printf '%s' "$plain_json" | jq -c '[.data.list[] | {level, pinned, title}]' 2>/dev/null | head -c 400)"
  plain_first="$(printf '%s' "$plain_json" | jq -r '.data.list[0].title // empty')"
  [ "$plain_first" = "$TITLE_B" ] && pass "关掉置顶后回到纯时间倒序（最新那条 WARN 排第一）" \
    || fail "关掉置顶后第一条=${plain_first:-<空>}（应为最新的 ${TITLE_B}）"

  # 级别着色靠 level 字段：三种取值都必须在列表里能取到
  levels="$(printf '%s' "$plain_json" | jq -r '[.data.list[].level] | unique | join(",")')"
  echo "列表里出现的级别：${levels}"
  case "$levels" in
    *ERROR*) pass "列表里存在 ERROR（红色标签）" ;;
    *) fail "列表里没有 ERROR：${levels}" ;;
  esac
  case "$levels" in
    *WARN*) pass "列表里存在 WARN（黄色标签）" ;;
    *) fail "列表里没有 WARN：${levels}" ;;
  esac
fi

echo
if [ "$FAIL" -eq 0 ]; then
  echo "验收通过：全部断言 PASS（证据在 ${OUT}）"
  exit 0
fi
echo "验收失败：见上面 FAIL 行（证据在 ${OUT}）"
exit 1
