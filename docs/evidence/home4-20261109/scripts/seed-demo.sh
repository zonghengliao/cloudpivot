#!/usr/bin/env bash
# HOME-4 验收用演示数据：3 条任务（逾期 / 今天 / 下周）+ 2 条公告（发布即扇出消息）。
# 全部走真实接口，不用 SQL 造假。
set -u
BASE=http://127.0.0.1:18094
OUT=/var/tmp/home4/api
mkdir -p "$OUT"
TOKEN=$(cat "$OUT/admin.token")
AUTH="Authorization: Bearer $TOKEN"

TODAY=$(date +%F)
YESTERDAY=$(date -d 'yesterday' +%F)
NEXTWEEK=$(date -d '+7 day' +%F)

task() {
  curl -s -m 15 -X POST "$BASE/api/business/task" -H "$AUTH" -H 'Content-Type: application/json' \
    -d "{\"title\":\"$1\",\"priority\":\"$2\",\"due_date\":\"$3\",\"description\":\"HOME-4 验收演示数据\"}" \
    -w ' HTTP %{http_code}\n' | tail -c 160
}
echo '== 建任务 =='
task '整理本周销售周报并发出' P0 "$YESTERDAY"
task '复核三笔待付款申请' P1 "$TODAY"
task '梳理下季度采购计划' P2 "$NEXTWEEK"

echo '== 建公告 =='
ann() {
  BODY="{\"title\":\"$1\",\"content_md\":\"$2\",\"level\":\"$3\",\"scope_type\":\"ALL\"}"
  RESP=$(curl -s -m 15 -X POST "$BASE/api/business/announcement" -H "$AUTH" -H 'Content-Type: application/json' -d "$BODY")
  ID=$(printf '%s' "$RESP" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
  echo "created id=$ID"
  if [ -n "$ID" ]; then
    curl -s -m 15 -X POST "$BASE/api/business/announcement/$ID/publish" -H "$AUTH" -w ' HTTP %{http_code}\n' \
      | tail -c 200
  fi
}
ann '关于启用新首页工作台的通知' '**各位同事**：首页已调整为「工作台」形态，工作任务、公告消息、快捷入口集中在首屏。' IMPORTANT
ann '十一月份系统维护窗口安排' '维护窗口：11 月 15 日 22:00 - 23:00，期间系统只读。' NORMAL

echo '== 复核：我的任务 / 公告 / 消息条数 =='
curl -s -m 10 -H "$AUTH" "$BASE/api/business/task/my" -o "$OUT/my-tasks.json"
curl -s -m 10 -H "$AUTH" "$BASE/api/business/announcement?page=1&page_size=5" -o "$OUT/announcements.json"
curl -s -m 10 -H "$AUTH" "$BASE/api/business/notice/my?page=1&page_size=10" -o "$OUT/notices.json"
python3 - "$OUT" <<'PY'
import json,sys,os
d=sys.argv[1]
t=json.load(open(os.path.join(d,'my-tasks.json')))['data']
print('tasks counts=', t.get('counts'), 'overdue=', len(t.get('overdue',[])), 'today=', len(t.get('today',[])))
a=json.load(open(os.path.join(d,'announcements.json')))['data']
print('announcements total=', a.get('total'), [i['title'] for i in a.get('list',[])][:5])
n=json.load(open(os.path.join(d,'notices.json')))['data']
print('notices total=', n.get('total'), [i['title'] for i in n.get('list',[])][:3])
PY
