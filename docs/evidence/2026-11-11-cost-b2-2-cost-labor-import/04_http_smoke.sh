#!/usr/bin/env bash
# COST-B2-2 工时导入 HTTP 冒烟（真实例 127.0.0.1:18042，空库首启 + admin 真登录）
#
# 覆盖：
#   1) 登录 + 首登改密
#   2) 建在制成本对象 + 建一个已关批对象（负例用）
#   3) GET  /business/erp/cost-labor/template        模板下载（真 xlsx，PK 头 + 可反向上传）
#   4) POST /business/erp/cost-labor/import          干净文件 → 3 行入库（success=3）
#   5) GET  /business/erp/cost-labor                 列表核对（total/金额/import+DRAFT）
#   6) POST /import                                  坏文件 → success=0 + 逐行错误（行号/列名/原因）
#   7) GET  列表                                      仍是 3 行（全有或全无：没有半批）
#   8) POST /import                                  期间不符 → 逐行错误
#   9) POST /import                                  对象已关批 → 逐行错误
#  10) POST /import                                  上传模板本身 → 422「没有可导入的数据行」
#  11) POST /import                                  未鉴权 → 401
set -uo pipefail

BASE="http://127.0.0.1:18042/api"
PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); echo "PASS $1"; }
bad() { FAIL=$((FAIL + 1)); echo "FAIL $1"; }
WORK="${WORK:-/tmp/b22_smoke}"
mkdir -p "$WORK"

echo "===== 1. 登录 admin ====="
LOGIN=$(curl -s -m 10 -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@123456"}')
TOKEN=$(echo "$LOGIN" | jq -r '.data.access_token // empty')
[ -n "$TOKEN" ] && ok "登录成功（token 长度 ${#TOKEN}）" || { bad "登录失败: $LOGIN"; exit 1; }
AUTH="Authorization: Bearer $TOKEN"

echo
echo "===== 1b. 首次登录必须改初始密码 ====="
CHG=$(curl -s -m 10 -o "$WORK/chg.json" -w "%{http_code}" -X PUT "$BASE/system/auth/password" -H "$AUTH" \
  -H 'Content-Type: application/json' \
  -d '{"old_password":"Admin@123456","new_password":"Admin@1234567"}')
echo "PUT /system/auth/password => HTTP $CHG code=$(jq -r '.code' "$WORK/chg.json")"
[ "$CHG" = "200" ] && ok "初始密码改密成功" || bad "改密失败: $(cat "$WORK/chg.json")"
LOGIN=$(curl -s -m 10 -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@1234567"}')
TOKEN=$(echo "$LOGIN" | jq -r '.data.access_token // empty')
[ -n "$TOKEN" ] && ok "用新口令重新登录成功" || { bad "重新登录失败: $LOGIN"; exit 1; }
AUTH="Authorization: Bearer $TOKEN"

PRODUCT='d1370003-0000-4000-8000-000000000005'
WAREHOUSE='d1370001-0000-4000-8000-000000000002'
DEPT='20000000-0000-4000-8000-0000000f0366'

echo
echo "===== 2. 建两张成本对象（一张 OPEN、一张随后关批） ====="
mkobj() {
  curl -s -m 10 -X POST "$BASE/business/erp/cost-object" -H "$AUTH" -H 'Content-Type: application/json' \
    -d "{\"product_id\":\"$PRODUCT\",\"target_warehouse_id\":\"$WAREHOUSE\",\"plan_qty\":\"100\",\"workshop_dept_id\":\"$DEPT\",\"start_date\":\"2026-11-01\",\"remark\":\"$1\"}"
}
OBJ=$(mkobj "B2-2 导入冒烟")
OBJ_ID=$(echo "$OBJ" | jq -r '.data.id // empty')
OBJ_NO=$(echo "$OBJ" | jq -r '.data.object_no // empty')
DEPT_NAME=$(echo "$OBJ" | jq -r '.data.workshop_dept_name // empty')
[ -n "$OBJ_ID" ] && ok "建成本对象 $OBJ_NO（部门=$DEPT_NAME）" || bad "建对象失败: $OBJ"

CLOSED=$(mkobj "B2-2 关闭对象")
CLOSED_ID=$(echo "$CLOSED" | jq -r '.data.id // empty')
CLOSE=$(curl -s -m 10 -X POST "$BASE/business/erp/cost-object/$CLOSED_ID/close" -H "$AUTH" \
  -H 'Content-Type: application/json' -d '{"force_wip":true,"remark":"B2-2 冒烟：关批对象"}')
CLOSED_NO=$(echo "$CLOSE" | jq -r '.data.object_no // empty')
CLOSED_STATUS=$(echo "$CLOSE" | jq -r '.data.status // empty')
[ "$CLOSED_STATUS" = "CLOSED" ] && ok "建并关批对象 $CLOSED_NO（status=CLOSED）" || bad "关批失败: $CLOSE"

echo
echo "===== 3. GET /business/erp/cost-labor/template（下载 8 列模板） ====="
TPL_HTTP=$(curl -s -m 20 -o "$WORK/template.xlsx" -w "%{http_code}|%{content_type}" -H "$AUTH" \
  "$BASE/business/erp/cost-labor/template")
TPL_CODE=${TPL_HTTP%%|*}
TPL_TYPE=${TPL_HTTP#*|}
TPL_SIZE=$(stat -c%s "$WORK/template.xlsx")
TPL_MAGIC=$(head -c 2 "$WORK/template.xlsx")
echo "HTTP=$TPL_CODE content_type=$TPL_TYPE size=$TPL_SIZE magic=$TPL_MAGIC"
[ "$TPL_CODE" = "200" ] && ok "模板下载 HTTP 200" || bad "模板下载失败: $TPL_HTTP"
[ "$TPL_MAGIC" = "PK" ] && ok "模板是 xlsx（zip 容器 PK 头）" || bad "模板不是 xlsx"
[ "$TPL_SIZE" -gt 3000 ] && ok "模板体积 $TPL_SIZE 字节（含说明 sheet）" || bad "模板体积异常: $TPL_SIZE"
unzip -l "$WORK/template.xlsx" 2>/dev/null | sed -n '1,12p'

echo
echo "===== 3b. 模板本身回传（只有示例行）⇒ 422 没有数据行 ====="
UP=$(curl -s -m 20 -o "$WORK/tpl_upload.json" -w "%{http_code}" -X POST "$BASE/business/erp/cost-labor/import" \
  -H "$AUTH" -F "file=@$WORK/template.xlsx" -F "period_code=2026-11")
echo "HTTP $UP => $(jq -c '{code, msg}' "$WORK/tpl_upload.json")"
[ "$UP" = "422" ] && ok "只有示例行 → 422（示例行被自动跳过）" || bad "模板回传应 422，实际 $UP"
grep -q "没有可导入的数据行" "$WORK/tpl_upload.json" && ok "报错文案点明「没有可导入的数据行」" || bad "文案不对"

echo
echo "===== 4. POST /import（干净文件：3 行） ====="
python3 "$WORK/mkxlsx.py" "$WORK/clean.xlsx" clean "$OBJ_NO" "$DEPT_NAME" >/dev/null
IMP=$(curl -s -m 30 -X POST "$BASE/business/erp/cost-labor/import" -H "$AUTH" \
  -F "file=@$WORK/clean.xlsx" -F "period_code=2026-11")
echo "$IMP" | jq -c '{code, success: .data.success, failed: .data.failed, rows: .data.rows}'
[ "$(echo "$IMP" | jq -r '.code')" = "0" ] && ok "导入接口 code=0" || bad "导入失败: $IMP"
[ "$(echo "$IMP" | jq -r '.data.success')" = "3" ] && ok "success=3（三行全入库）" || bad "success 不是 3: $IMP"
[ "$(echo "$IMP" | jq -r '.data.failed')" = "0" ] && ok "failed=0" || bad "failed 不是 0"
[ "$(echo "$IMP" | jq -r '.data.rows | length')" = "0" ] && ok "成功时 rows 为空" || bad "成功时不该有错误明细"

echo
echo "===== 5. GET /cost-labor（列表核对：金额 / 来源 / 状态） ====="
LIST=$(curl -s -m 10 -H "$AUTH" "$BASE/business/erp/cost-labor?period_code=2026-11&cost_object_id=$OBJ_ID&page=1&page_size=10")
echo "$LIST" | jq -c '{total: .data.total} + {rows: [.data.list[] | {employee_name, hours, machine_hours, rate, amount, source_type, status, dept_name}]}'
[ "$(echo "$LIST" | jq -r '.data.total')" = "3" ] && ok "列表 total=3" || bad "列表 total 异常: $LIST"
[ "$(echo "$LIST" | jq -r '[.data.list[].source_type] | unique | join(",")')" = "IMPORT" ] && ok "来源全是 IMPORT" || bad "source_type 不对"
[ "$(echo "$LIST" | jq -r '[.data.list[].status] | unique | join(",")')" = "DRAFT" ] && ok "状态全是 DRAFT（等人工确认）" || bad "status 不对"
# 列表顺序是「工作日期倒序」，所以按姓名取行，不按下标（避免顺序假设）
AMT=$(echo "$LIST" | jq -r '.data.list[] | select(.employee_name=="李四") | .amount')
[ "$AMT" = "363.7500" ] && ok "李四金额 363.7500（7.5 × 48.5，服务端算）" || bad "金额不对: $AMT"
MH=$(echo "$LIST" | jq -r '.data.list[] | select(.employee_name=="王五") | .machine_hours')
[ "$MH" = "0.00" ] && ok "机器工时留空 → 0.00" || bad "机器工时默认值不对: $MH"
AMT3=$(echo "$LIST" | jq -r '.data.list[] | select(.employee_name=="赵六") | .amount')
[ "$AMT3" = "2333.3333" ] && ok "除不尽费率 2333.3333（round(70×33.333333,4)）" || bad "金额舍入不对: $AMT3"

echo
echo "===== 6. POST /import（坏文件：1 好 3 坏）⇒ 逐行错误，一条都不入库 ====="
python3 "$WORK/mkxlsx.py" "$WORK/bad.xlsx" bad "$OBJ_NO" "$DEPT_NAME" >/dev/null
BAD=$(curl -s -m 30 -X POST "$BASE/business/erp/cost-labor/import" -H "$AUTH" \
  -F "file=@$WORK/bad.xlsx" -F "period_code=2026-11")
echo "$BAD" | jq -c '{code, success: .data.success, failed: .data.failed, rows: .data.rows}'
[ "$(echo "$BAD" | jq -r '.data.success')" = "0" ] && ok "success=0（全有或全无）" || bad "有错还应 success=0: $BAD"
[ "$(echo "$BAD" | jq -r '.data.failed')" = "3" ] && ok "failed=3（三行有错）" || bad "failed 不是 3"
[ "$(echo "$BAD" | jq -r '[.data.rows[].line] | unique | join(",")')" = "3,4,5" ] && ok "行号 3/4/5（Excel 真实行号）" || bad "行号不对: $(echo "$BAD" | jq -r '[.data.rows[].line] | unique | join(",")')"
[ "$(echo "$BAD" | jq -r '.data.rows[0].column')" = "成本对象号" ] && ok "第 3 行报「成本对象号」列" || bad "第 3 行列名不对"
echo "$BAD" | jq -r '.data.rows[] | "   行\(.line) [\(.column)] \(.message)"' | sed -n '1,6p'

echo
echo "===== 7. 列表仍是 3 行（整批拒绝后没有半批数据） ====="
LIST2=$(curl -s -m 10 -H "$AUTH" "$BASE/business/erp/cost-labor?period_code=2026-11&cost_object_id=$OBJ_ID&page=1&page_size=10")
[ "$(echo "$LIST2" | jq -r '.data.total')" = "3" ] && ok "total 仍是 3（坏批一条都没进来）" || bad "出现了半批数据: $(echo "$LIST2" | jq -r '.data.total')"

echo
echo "===== 8. 期间不符（period_code=2026-10）⇒ 逐行报「日期不在期间」 ====="
P=$(curl -s -m 30 -X POST "$BASE/business/erp/cost-labor/import" -H "$AUTH" \
  -F "file=@$WORK/clean.xlsx" -F "period_code=2026-10")
[ "$(echo "$P" | jq -r '.data.failed')" = "3" ] && ok "3 行全部报期间不符" || bad "期间校验没生效: $P"
[ "$(echo "$P" | jq -r '.data.rows[0].column')" = "日期" ] && ok "报错列是「日期」" || bad "列名不对"
echo "$P" | jq -r '.data.rows[0].message'

echo
echo "===== 9. 已关批对象 ⇒ 逐行报「只有 OPEN」 ====="
python3 "$WORK/mkxlsx.py" "$WORK/closed.xlsx" clean "$CLOSED_NO" "$DEPT_NAME" >/dev/null
C=$(curl -s -m 30 -X POST "$BASE/business/erp/cost-labor/import" -H "$AUTH" \
  -F "file=@$WORK/closed.xlsx" -F "period_code=2026-11")
[ "$(echo "$C" | jq -r '.data.failed')" = "3" ] && ok "关批对象 3 行全部被拒" || bad "关批对象没拦: $C"
echo "$C" | jq -r '.data.rows[0].message'

echo
echo "===== 10. 非 xlsx 文件 ⇒ 400 格式错 ====="
printf 'id,name\n1,2\n' > "$WORK/not_xlsx.xlsx"
N=$(curl -s -m 20 -o "$WORK/notxlsx.json" -w "%{http_code}" -X POST "$BASE/business/erp/cost-labor/import" \
  -H "$AUTH" -F "file=@$WORK/not_xlsx.xlsx")
echo "HTTP $N => $(jq -c '{code, msg}' "$WORK/notxlsx.json")"
[ "$N" = "400" ] && ok "非 xlsx → 400" || bad "非 xlsx 应 400，实际 $N"

echo
echo "===== 11. 未鉴权 / 缺文件字段 ====="
U=$(curl -s -m 10 -o /dev/null -w "%{http_code}" -X POST "$BASE/business/erp/cost-labor/import" -F "file=@$WORK/clean.xlsx")
echo "未鉴权 HTTP $U"
[ "$U" = "401" ] && ok "未鉴权 → 401" || bad "未鉴权应 401，实际 $U"
M=$(curl -s -m 10 -o "$WORK/nofile.json" -w "%{http_code}" -X POST "$BASE/business/erp/cost-labor/import" -H "$AUTH" -F "period_code=2026-11")
echo "缺 file 字段 HTTP $M => $(jq -c '{code, msg}' "$WORK/nofile.json")"
[ "$M" = "400" ] && ok "缺文件字段 → 400" || bad "缺文件应 400，实际 $M"

echo
echo "==================== 汇总 ===================="
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" = "0" ] || exit 1
