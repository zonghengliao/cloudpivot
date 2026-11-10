#!/usr/bin/env bash
# COST-B2-1 工时台账 HTTP 冒烟（真实例 127.0.0.1:18041，空库首启 + admin 真登录）
#
# 覆盖：登录 → 建成本对象 → 建工时（DRAFT）→ 列表 → 改工时（金额重算）→
#       确认 → 重复确认（幂等，不翻倍）→ 删草稿 → 负数用例（金额不符 66058 /
#       对象非 OPEN 66057 / 跨月 400 / 未鉴权 401 / 未知路径 404）
set -uo pipefail

BASE="http://127.0.0.1:18041/api"
PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); echo "PASS $1"; }
bad() { FAIL=$((FAIL + 1)); echo "FAIL $1"; }

echo "===== 1. 登录 admin ====="
LOGIN=$(curl -s -m 10 -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@123456"}')
TOKEN=$(echo "$LOGIN" | jq -r '.data.access_token // empty')
if [ -n "$TOKEN" ]; then ok "登录成功（token 长度 ${#TOKEN}）"; else bad "登录失败: $LOGIN"; exit 1; fi
AUTH="Authorization: Bearer $TOKEN"
echo
echo "===== 1b. 首次登录必须先改初始密码（否则除改密外全部 403） ====="
CHG=$(curl -s -m 10 -o /tmp/b21_chg.json -w "%{http_code}" -X PUT "$BASE/system/auth/password" -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"old_password":"Admin@123456","new_password":"Admin@1234567"}')
echo "PUT /system/auth/password => HTTP $CHG code=$(jq -r '.code' /tmp/b21_chg.json)"
[ "$CHG" = "200" ] && ok "初始密码改密成功" || bad "改密失败: $(cat /tmp/b21_chg.json)"
LOGIN=$(curl -s -m 10 -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"Admin@1234567"}')
TOKEN=$(echo "$LOGIN" | jq -r '.data.access_token // empty')
[ -n "$TOKEN" ] && ok "用新口令重新登录成功" || { bad "重新登录失败: $LOGIN"; exit 1; }
AUTH="Authorization: Bearer $TOKEN"

# 首行改名（避免 jq 的非空 ASCII 键报错）
ROWKEY='first_row'
PRODUCT='d1370003-0000-4000-8000-000000000005'
WAREHOUSE='d1370001-0000-4000-8000-000000000002'
DEPT='20000000-0000-4000-8000-0000000f0366'

echo
echo "===== 2. 建一张在制成本对象（工时行的父行） ====="
OBJ=$(curl -s -m 10 -X POST "$BASE/business/erp/cost-object" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"product_id\":\"$PRODUCT\",\"target_warehouse_id\":\"$WAREHOUSE\",\"plan_qty\":\"100\",\"workshop_dept_id\":\"$DEPT\",\"start_date\":\"2026-11-01\",\"remark\":\"B2-1 HTTP 冒烟\"}")
OBJ_ID=$(echo "$OBJ" | jq -r '.data.id // empty')
OBJ_NO=$(echo "$OBJ" | jq -r '.data.object_no // empty')
[ -n "$OBJ_ID" ] && ok "建成本对象 $OBJ_NO（id=$OBJ_ID）" || bad "建对象失败: $OBJ"

echo
echo "===== 3. POST /business/erp/cost-labor（建工时，客户端不传金额） ====="
CREATE=$(curl -s -m 10 -X POST "$BASE/business/erp/cost-labor" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"period_code\":\"2026-11\",\"work_date\":\"2026-11-06\",\"cost_object_id\":\"$OBJ_ID\",\"employee_name\":\"张三\",\"dept_id\":\"$DEPT\",\"hours\":\"8\",\"machine_hours\":\"2\",\"rate\":\"50\",\"remark\":\"B2-1 HTTP 冒烟 第 1 行\"}")
echo "$CREATE" | jq -c '.data | {id, status, source_type, hours, machine_hours, rate, amount, cost_object_no, product_name, dept_name, work_date}'
LABOR_ID=$(echo "$CREATE" | jq -r '.data.id // empty')
[ "$(echo "$CREATE" | jq -r '.code')" = "0" ] && [ -n "$LABOR_ID" ] && ok "建工时成功（金额由服务端算）" || bad "建工时失败: $CREATE"

echo
echo "===== 4. GET /business/erp/cost-labor（列表：期间 + 对象筛选） ====="
LIST=$(curl -s -m 10 -H "$AUTH" "$BASE/business/erp/cost-labor?period_code=2026-11&cost_object_id=$OBJ_ID&page=1&page_size=10")
echo "$LIST" | jq -c --arg k "$ROWKEY" '{code, total: .data.total, page: .data.page, page_size: .data.page_size} + {($k): (.data.list[0] | {employee_name, status, amount, dept_name})}'
[ "$(echo "$LIST" | jq -r '.data.total')" = "1" ] && ok "列表命中 1 条（total/page/page_size 形状与仓库一致）" || bad "列表异常: $LIST"

echo
echo "===== 5. PUT /business/erp/cost-labor/{id}（只改工时 ⇒ 金额重算） ====="
UPD=$(curl -s -m 10 -X PUT "$BASE/business/erp/cost-labor/$LABOR_ID" -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"hours":"9.5"}')
echo "$UPD" | jq -c '.data | {hours, rate, amount, status}'
[ "$(echo "$UPD" | jq -r '.data.amount')" = "475.0000" ] && ok "金额按新工时重算（9.5 × 50 = 475.0000）" || bad "金额未重算: $UPD"

echo
echo "===== 6. POST /{id}/confirm（确认） ====="
C1=$(curl -s -m 10 -X POST "$BASE/business/erp/cost-labor/$LABOR_ID/confirm" -H "$AUTH")
echo "$C1" | jq -c '.data | {status, amount, updated_at}'
[ "$(echo "$C1" | jq -r '.data.status')" = "CONFIRMED" ] && ok "确认成功 DRAFT → CONFIRMED" || bad "确认失败: $C1"

echo
echo "===== 7. 重复确认（幂等：金额不翻倍、updated_at 不动） ====="
C2=$(curl -s -m 10 -X POST "$BASE/business/erp/cost-labor/$LABOR_ID/confirm" -H "$AUTH")
echo "$C2" | jq -c '.data | {status, amount, updated_at}'
A1=$(echo "$C1" | jq -r '.data.amount')
A2=$(echo "$C2" | jq -r '.data.amount')
U1=$(echo "$C1" | jq -r '.data.updated_at')
U2=$(echo "$C2" | jq -r '.data.updated_at')
[ "$A1" = "$A2" ] && ok "重复确认金额不变（$A1 === $A2，未翻倍）" || bad "金额变了：$A1 → $A2"
[ "$U1" = "$U2" ] && ok "重复确认不写库（updated_at 未变）" || bad "第二轮确认动了 updated_at"
ROWS=$(curl -s -m 10 -H "$AUTH" "$BASE/business/erp/cost-labor?cost_object_id=$OBJ_ID&page=1&page_size=50" | jq -r '.data.total')
[ "$ROWS" = "1" ] && ok "库里仍只有 1 行（行数 $ROWS）" || bad "行数异常：$ROWS"

echo
echo "===== 8. 已确认的行改 / 删 ⇒ 409 ====="
UPD2=$(curl -s -m 10 -o /tmp/b21_upd2.json -w "%{http_code}" -X PUT "$BASE/business/erp/cost-labor/$LABOR_ID" -H "$AUTH" -H 'Content-Type: application/json' -d '{"hours":"10"}')
echo "PUT  => HTTP $UPD2 $(jq -r '.code' /tmp/b21_upd2.json) $(jq -r '.msg' /tmp/b21_upd2.json | head -1)"
[ "$UPD2" = "409" ] && ok "已确认不许改（409）" || bad "已确认却允许改：HTTP $UPD2"
DEL2=$(curl -s -m 10 -o /tmp/b21_del2.json -w "%{http_code}" -X DELETE "$BASE/business/erp/cost-labor/$LABOR_ID" -H "$AUTH")
echo "DEL  => HTTP $DEL2 $(jq -r '.code' /tmp/b21_del2.json)"
[ "$DEL2" = "409" ] && ok "已确认不许删（409）" || bad "已确认却允许删：HTTP $DEL2"

echo
echo "===== 9. 金额核对值不符 ⇒ 400 + 66058「工时×费率与金额不符」 ====="
AMT=$(curl -s -m 10 -o /tmp/b21_amt.json -w "%{http_code}" -X POST "$BASE/business/erp/cost-labor" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"period_code\":\"2026-11\",\"work_date\":\"2026-11-07\",\"cost_object_id\":\"$OBJ_ID\",\"employee_name\":\"李四\",\"dept_id\":\"$DEPT\",\"hours\":\"8\",\"rate\":\"50\",\"amount\":\"401\"}")
echo "POST => HTTP $AMT code=$(jq -r '.code' /tmp/b21_amt.json) msg=$(jq -r '.msg' /tmp/b21_amt.json | head -1)"
[ "$AMT" = "400" ] && [ "$(jq -r '.code' /tmp/b21_amt.json)" = "66058" ] && ok "金额不符 → 400 + 66058" || bad "金额不符未按 66058 拦：HTTP $AMT"

echo
echo "===== 10. 跨月工时（日期不在所选期间）⇒ 400 ====="
PER=$(curl -s -m 10 -o /tmp/b21_per.json -w "%{http_code}" -X POST "$BASE/business/erp/cost-labor" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"period_code\":\"2026-11\",\"work_date\":\"2026-12-01\",\"cost_object_id\":\"$OBJ_ID\",\"employee_name\":\"王五\",\"dept_id\":\"$DEPT\",\"hours\":\"4\",\"rate\":\"50\"}")
echo "POST => HTTP $PER code=$(jq -r '.code' /tmp/b21_per.json) msg=$(jq -r '.msg' /tmp/b21_per.json | head -1)"
[ "$PER" = "400" ] && ok "跨月 → 400" || bad "跨月未拦：HTTP $PER"

echo
echo "===== 11. 草稿行删掉（软删） ====="
DRAFT=$(curl -s -m 10 -X POST "$BASE/business/erp/cost-labor" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"period_code\":\"2026-11\",\"work_date\":\"2026-11-08\",\"cost_object_id\":\"$OBJ_ID\",\"employee_name\":\"赵六\",\"dept_id\":\"$DEPT\",\"hours\":\"5\",\"rate\":\"50\",\"remark\":\"B2-1 待删\"}")
DRAFT_ID=$(echo "$DRAFT" | jq -r '.data.id // empty')
DEL=$(curl -s -m 10 -o /tmp/b21_del.json -w "%{http_code}" -X DELETE "$BASE/business/erp/cost-labor/$DRAFT_ID" -H "$AUTH")
echo "DELETE => HTTP $DEL code=$(jq -r '.code' /tmp/b21_del.json)"
LIVE=$(curl -s -m 10 -H "$AUTH" "$BASE/business/erp/cost-labor?cost_object_id=$OBJ_ID&page=1&page_size=50" | jq -r '.data.total')
[ "$DEL" = "200" ] && [ "$LIVE" = "1" ] && ok "草稿软删成功（列表回到 1 行）" || bad "软删异常：HTTP $DEL total=$LIVE"

echo
echo "===== 12. 对象非 OPEN ⇒ 409 + 66057 ====="
CLOSE=$(curl -s -m 10 -o /tmp/b21_close.json -w "%{http_code}" -X POST "$BASE/business/erp/cost-object/$OBJ_ID/close" -H "$AUTH" -H 'Content-Type: application/json' -d '{"force_wip":true}')
echo "关批 => HTTP $CLOSE code=$(jq -r '.code' /tmp/b21_close.json)"
ONCLOSED=$(curl -s -m 10 -o /tmp/b21_cl.json -w "%{http_code}" -X POST "$BASE/business/erp/cost-labor" -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"period_code\":\"2026-11\",\"work_date\":\"2026-11-09\",\"cost_object_id\":\"$OBJ_ID\",\"employee_name\":\"孙七\",\"dept_id\":\"$DEPT\",\"hours\":\"3\",\"rate\":\"50\"}")
echo "POST(已关批对象) => HTTP $ONCLOSED code=$(jq -r '.code' /tmp/b21_cl.json) msg=$(jq -r '.msg' /tmp/b21_cl.json | head -1)"
[ "$ONCLOSED" = "409" ] && [ "$(jq -r '.code' /tmp/b21_cl.json)" = "66057" ] && ok "已关批对象录工时 → 409 + 66057" || bad "未拦：HTTP $ONCLOSED"

echo
echo "===== 13. 未鉴权 / 未知路径（证明路由真挂在 router 上） ====="
N401=$(curl -s -m 10 -o /dev/null -w "%{http_code}" "$BASE/business/erp/cost-labor?page=1")
N404=$(curl -s -m 10 -o /dev/null -w "%{http_code}" -H "$AUTH" "$BASE/business/erp/cost-labor-nope")
echo "未鉴权 => HTTP $N401 ；未知路径 => HTTP $N404"
[ "$N401" = "401" ] && ok "未鉴权 401（路由存在且鉴权生效）" || bad "未鉴权返回 $N401"
[ "$N404" = "404" ] && ok "未知路径 404（没有把 SPA fallback 当接口）" || bad "未知路径返回 $N404"

echo
echo "===== 14. 清场：删掉冒烟数据（工时 / 成本对象） ====="
export PGPASSWORD=alun
psql -h 127.0.0.1 -U alun -d b21_http -tAc \
  "DELETE FROM biz_cost_labor WHERE cost_object_id = '$OBJ_ID';
   DELETE FROM biz_cost_object WHERE id = '$OBJ_ID';
   SELECT (SELECT count(*) FROM biz_cost_labor WHERE cost_object_id = '$OBJ_ID')
        + (SELECT count(*) FROM biz_cost_object WHERE id = '$OBJ_ID');" | tail -1 | sed 's/^/残留行数=/'

echo
echo "===== 汇总：PASS=$PASS FAIL=$FAIL ====="
[ "$FAIL" = "0" ]
