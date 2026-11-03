#!/usr/bin/env bash
# 清理 NP 卡造的所有夹具：站内信（[NP] 前缀 + 公告扇出的 NTC）、任务、公告、两个临时账号
set -u
B=http://127.0.0.1:8023
S=/root/.hermes/profiles/frontend-developer/cache/scratch/np
TOK=$(cat $S/token_admin2.txt)
A="Authorization: Bearer $TOK"
J='Content-Type: application/json'

echo "=== 1) 删 [NP] 预览/测试站内信（含公告扇出的那条）"
curl -s -H "$A" "$B/api/business/notice/page?page=1&page_size=100&keyword=%5BNP%5D" \
  | jq -r '.data.list[] | [.id, .title] | @tsv' > $S/notices.tsv
wc -l < $S/notices.tsv
while IFS=$'\t' read -r id title; do
  code=$(curl -s -o /dev/null -w '%{http_code}' -H "$A" -X DELETE "$B/api/business/notice/$id")
  echo "del notice $code  $title"
done < $S/notices.tsv
# 公告扇出的站内信标题是公告标题，用公告标题再扫一次
curl -s -H "$A" "$B/api/business/notice/page?page=1&page_size=100&keyword=%E5%85%AC%E5%91%8A%E9%AA%8C%E6%94%B6%E5%A4%B9%E5%85%B7" \
  | jq -r '.data.list[] | [.id, .title] | @tsv' > $S/notices2.tsv
while IFS=$'\t' read -r id title; do
  [ -z "${id:-}" ] && continue
  code=$(curl -s -o /dev/null -w '%{http_code}' -H "$A" -X DELETE "$B/api/business/notice/$id")
  echo "del notice(fanout) $code  $title"
done < $S/notices2.tsv

echo "=== 2) 删公告夹具"
AID=$(cat $S/ann_id.txt)
echo "ann delete -> $(curl -s -H "$A" -X DELETE "$B/api/business/announcement/$AID" | head -c 160)"

echo "=== 3) 删任务夹具"
TID=$(jq -r '.data.id' $S/task.json)
echo "task delete -> $(curl -s -H "$A" -X DELETE "$B/api/business/task/$TID" | head -c 160)"

echo "=== 4) 删临时账号 np_fresh / np_admin2"
for u in np_fresh np_admin2; do
  uid=$(curl -s -H "$A" "$B/api/system/user/page?page=1&page_size=5&keyword=$u" | jq -r --arg u "$u" '.data.list[] | select(.username==$u) | .id' | head -1)
  if [ -n "${uid:-}" ]; then
    echo "del user $u ($uid) -> $(curl -s -o /dev/null -w '%{http_code}' -H "$A" -X DELETE "$B/api/system/user/$uid")"
  else
    echo "user $u not found"
  fi
done

echo "=== 5) 复核残留"
curl -s -H "$A" "$B/api/business/notice/page?page=1&page_size=20&keyword=%5BNP%5D" | jq -c '{leftover_notice: .data.total}'
curl -s -H "$A" "$B/api/business/task/page?page=1&page_size=20&keyword=%5BNP%5D" | jq -c '{leftover_task: .data.total}'
curl -s -H "$A" "$B/api/business/announcement/page?page=1&page_size=20&keyword=%5BNP%5D" | jq -c '{leftover_ann: .data.total}'
for u in np_fresh np_admin2; do
  printf 'user %s -> ' "$u"
  curl -s -H "$A" "$B/api/system/user/page?page=1&page_size=5&keyword=$u" | jq -c --arg u "$u" '[.data.list[] | select(.username==$u)] | length'
done
