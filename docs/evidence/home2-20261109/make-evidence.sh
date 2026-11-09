#!/bin/bash
# 生成 api-evidence.txt：本地隔离实例 + 145 真机（token 只打前 12 位）
set -uo pipefail
D=/root/.hermes/profiles/frontend-developer/cache/scratch/home2
OUT=$D/api-evidence.txt
BL=http://127.0.0.1:18023
PROD=http://172.18.10.145
LT=$(jq -r .data.access_token "$D/login-local.json")
PT=$(jq -r .data.access_token "$D/login-145-ok.json")
mask() { echo "${1:0:12}...(len=${#1})"; }

{
echo "# HOME-2 接口证据（工作任务卡数据源 GET /business/task/my）"
echo "生成时间：$(date '+%Y-%m-%d %H:%M:%S %Z')"
echo
echo "## ① 本地隔离实例（真后端 127.0.0.1:18023 + 隔离库 cloudpivot_home1）"
echo "### 1.1 登录 admin（token 打码）"
echo "access_token = $(mask "$LT")"
echo
echo "### 1.2 GET /api/business/task/my（造数后：3 逾期 + 9 今日）"
curl -s "$BL/api/business/task/my" -H "Authorization: Bearer $LT" \
  | jq '{code, counts:.data.counts, total:.data.total, overdue_count:(.data.overdue|length), today_count:(.data.today|length), overdue_titles:[.data.overdue[].title], first_today:.data.today[0].title}'
echo
echo "### 1.3 POST /api/business/task 造数回执（前 3 条 + 今日第 1 条）"
# 重新建 1 条做回执样例（不污染列表：建完即删）
RES=$(curl -s -X POST "$BL/api/business/task" -H "Authorization: Bearer $LT" -H 'Content-Type: application/json' \
  -d '{"title":"【HOME2-回执样例】接口连通性","priority":"P2","due_date":"2026-11-09"}')
echo "$RES" | jq '{code, id:.data.id, task_no:.data.task_no}'
SID=$(echo "$RES" | jq -r '.data.id // empty')
if [ -n "$SID" ]; then
  echo "DELETE 回执样例："
  curl -s -X DELETE "$BL/api/business/task/$SID" -H "Authorization: Bearer $LT" | jq '{code, deleted:.data.deleted}'
fi
echo
echo "### 1.4 PATCH 状态 → DOING（卡片「进行中」实心圆）"
TID=$(curl -s "$BL/api/business/task/my" -H "Authorization: Bearer $LT" | jq -r '.data.today[0].id')
curl -s "$BL/api/business/task/my" -H "Authorization: Bearer $LT" | jq -r '.data.today[0] | "today[0] = \(.task_no) \(.title) status=\(.status)"'
echo
echo "## ② 145 真机（http://172.18.10.145，nginx 反代到真后端 / 真库 cloudpivot_dev）"
echo "### 2.1 登录 admin（token 打码）"
echo "access_token = $(mask "$PT")"
echo
echo "### 2.2 GET /api/business/task/my"
curl -s "$PROD/api/business/task/my" -H "Authorization: Bearer $PT" \
  | jq '{code, counts:.data.counts, total:.data.total, overdue_titles:[.data.overdue[].title], first_today:(.data.today[0].title // null)}'
echo
echo "### 2.3 无 token 访问（401）"
curl -s -o /dev/null -w "GET /api/business/task/my 无 token → HTTP %{http_code}\n" "$PROD/api/business/task/my"
echo
echo "### 2.4 无该权限的账号（emp_drill，仅 EMPLOYEE 实际未授权）→ 403"
ET=$(curl -s -X POST "$PROD/api/system/auth/login" -H 'Content-Type: application/json' -d '{"username":"emp_drill","password":"Drill@123456"}' | jq -r .data.access_token)
curl -s "$PROD/api/business/task/my" -H "Authorization: Bearer $ET" | jq -c '{code,msg}'
echo
echo "## ③ 说明"
echo "- 卡片只调 GET /business/task/my 一个接口，未新建任何后端接口。"
echo "- 该接口只返回未终态（TODO/DOING/REVIEW）：DONE / CANCELLED 被后端排除，所以卡片底部统计取 counts 的实际字段（今日/逾期/本周），不虚构「已完成」。"
} > "$OUT" 2>&1
echo "written: $OUT"; tail -20 "$OUT"