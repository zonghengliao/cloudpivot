#!/bin/bash
# HOME-1 真机造数：6 条公告（验证「最多 5 条」截断）+ 12 条消息（验证「最多 10 条」）
# 全部走真接口，不用 SQL 塞数。
set -euo pipefail
B=http://127.0.0.1:18023
D=/root/.hermes/profiles/frontend-developer/cache/scratch/home1
# 现登一次（旧 token 已过期，避免用固化令牌）
curl -s -X POST "$B/api/system/auth/login" -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@123456"}' > "$D/login-local.json"
T=$(jq -r .data.access_token "$D/login-local.json")
echo "login code=$(jq -r .code "$D/login-local.json")"
jq -nc --arg at "$T" --arg rt "$(jq -r .data.refresh_token "$D/login-local.json")" '{at:$at,rt:$rt}' > "$D/tokens.json"
AH="Authorization: Bearer $T"

echo "== me =="
curl -s "$B/api/system/auth/me" -H "$AH" | jq -c '{code, id:.data.id, username:.data.username}'
UID_=$(curl -s "$B/api/system/auth/me" -H "$AH" | jq -r .data.id)
echo "uid=$UID_"

echo "== 造 6 条公告并发布 =="
for i in 1 2 3 4 5 6; do
  body=$(jq -nc --arg t "【HOME1】值班安排通知 第${i}条" --arg md "## 第${i}条公告正文\n\n这是首页公告区验收用的正文。" \
    '{title:$t, content_md:$md, level:"NORMAL", scope_type:"ALL"}')
  res=$(curl -s -X POST "$B/api/business/announcement" -H "$AH" -H 'Content-Type: application/json' -d "$body")
  aid=$(echo "$res" | jq -r '.data.id // empty')
  code=$(echo "$res" | jq -r .code)
  if [ -z "$aid" ]; then echo "create#$i FAIL code=$code msg=$(echo "$res" | jq -r .msg)"; continue; fi
  pres=$(curl -s -X POST "$B/api/business/announcement/$aid/publish" -H "$AH")
  echo "ann#$i id=$aid publish_code=$(echo "$pres" | jq -r .code) ann_no=$(echo "$pres" | jq -r '.data.ann_no // "-"')"
  sleep 1
done

echo "== 造 12 条消息（发给 admin 自己）=="
for i in $(seq 1 12); do
  body=$(jq -nc --arg t "【HOME1】消息第${i}条：采购订单审核提醒" --arg c "这是第${i}条消息正文，用于首页消息区验收。" --arg uid "$UID_" \
    '{notice_type:"SYSTEM", level:"INFO", title:$t, content:$c, content_format:"TEXT", target_type:"USER", target_ids:[$uid]}')
  res=$(curl -s -X POST "$B/api/business/notice" -H "$AH" -H 'Content-Type: application/json' -d "$body")
  echo "notice#$i code=$(echo "$res" | jq -r .code) no=$(echo "$res" | jq -r '.data.notice_no // "-"') msg=$(echo "$res" | jq -r '.msg // ""')"
done

echo "== 复核（接口侧）=="
curl -s "$B/api/business/announcement/visible?page=1&page_size=5" -H "$AH" | jq -c '{code, total:.data.total, rows:(.data.list|length), top:[.data.list[].title]}'
curl -s "$B/api/business/notice/mine?page=1&page_size=10" -H "$AH" | jq -c '{code, total:.data.total, rows:(.data.list|length), unread:[.data.list[]|select(.read_flag==false)]|length}'
curl -s "$B/api/business/notice/unread-count" -H "$AH" | jq -c '{code, unread:.data.unread_count}'
