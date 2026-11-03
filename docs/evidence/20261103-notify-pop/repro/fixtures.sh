#!/usr/bin/env bash
# 造两类夹具（工作任务 1 条 TODO + 公告 1 条已发布），让登录弹窗 4 类都有数
set -u
B=http://127.0.0.1:8023
S=/root/.hermes/profiles/frontend-developer/cache/scratch/np
TOK=$(cat $S/token.txt)
A="Authorization: Bearer $TOK"
J='Content-Type: application/json'
UID_=$(cat $S/uid.txt)

echo "--- task create"
curl -s -H "$A" -H "$J" -X POST "$B/api/business/task" \
  -d "{\"title\":\"[NP] 登录弹窗验证据\",\"description\":\"验收夹具，验完删\",\"priority\":\"P3\",\"owner_id\":\"$UID_\"}" \
  | tee $S/task.json | head -c 300
echo
echo "--- announcement create"
curl -s -H "$A" -H "$J" -X POST "$B/api/business/announcement" \
  -d '{"title":"[NP] 公告验收夹具","content_md":"登录弹窗验收夹具，验完删。","level":"NORMAL","scope_type":"ALL"}' \
  | tee $S/ann.json | head -c 300
echo
AID=$(jq -r '.data.id' $S/ann.json)
echo "ann_id=$AID"
echo "$AID" > $S/ann_id.txt
echo "--- announcement publish"
curl -s -H "$A" -H "$J" -X POST "$B/api/business/announcement/$AID/publish" | head -c 300
echo
echo "--- counts now"
for p in "/api/business/notice/unread-count" "/api/business/announcement/mine/unread-count" "/api/business/task/page?status=TODO&page_size=1&owner_id=$UID_" "/api/business/flow/task/todo?page=1&page_size=1"; do
  printf '%s -> ' "$p"
  curl -s -H "$A" "$B$p" | jq -c '.data | {unread_count, announcement_unread, total} | with_entries(select(.value != null))'
done
