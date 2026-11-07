#!/usr/bin/env bash
# ============================================================================
# FIX-4（t_763677cd）145 真机验收 · 步骤 3：全新库跑主链路（真实 token curl + DB 断言）
#
# 覆盖卡面验收：
#   ① 新库 → 期初入库 → biz_inventory_location 只有「默认库位桶」，无「未分配桶」
#   ② 调拨指定库位 A → 扣的是库位 A 的桶（不是汇总桶）
#   ③ 库位 A 库存不足 → 明确报错（不静默改扣）
#   ⑤ 全量对账：库存表 = 流水累计（(物料, 仓库) 聚合口径）0 差异
#
# 跑法（145 上，31_start_instance.sh 已把隔离实例起到 127.0.0.1:8045）：
#   bash /opt/cloudpivot/docs/evidence/20261107-fix4-inventory-bucket/32_e2e_145.sh
# ============================================================================
set -uo pipefail
EV=/opt/cloudpivot/docs/evidence/20261107-fix4-inventory-bucket
RAW=$EV/raw
mkdir -p "$RAW"
BASE=http://127.0.0.1:8045
API=$BASE/api
DB=$(cat /root/backup/.fix4_db_new)
export PGPASSWORD=$(awk -F= '/^NIK_DATABASE_PASSWORD/{print $2}' /etc/cloudpivot/cloudpivot-dev.env)
ADMIN_PASS=$(awk -F= '/^NIK_ADMIN_PASSWORD/{print $2}' /etc/cloudpivot/cloudpivot-dev.env)

PASS=0
FAIL=0
# 每次跑用不同编码（隔离库长期存活：换名字就不用删数据，历史行不影响本次断言）
TAG=${TAG:-$(date +%H%M%S)}
WHA_CODE="WH-F4A-$TAG"
WHB_CODE="WH-F4B-$TAG"
LOCDEF_CODE="F4D-$TAG"
LOC2_CODE="F4S-$TAG"
MAT_CODE="F4M-$TAG"
echo "TAG=$TAG codes=$WHA_CODE/$MAT_CODE"
ok()  { echo "PASS $*"; PASS=$((PASS + 1)); }
bad() { echo "FAIL $*"; FAIL=$((FAIL + 1)); }
chk() { # $1=实际 $2=期望 $3=说明
  if [ "$1" = "$2" ]; then ok "$3（$1）"; else bad "$3（期望 $2，实际 $1）"; fi
}
sqlq() { psql -h 127.0.0.1 -U cloudpivot -d "$DB" -X -tA -c "$1"; }
api() { # $1=method $2=path $3=token $4=body
  local method=$1 path=$2 tok=$3 body=${4:-}
  if [ -n "$body" ]; then
    HTTP=$(curl -sS -o /tmp/_f4_body.json -w '%{http_code}' -X "$method" "$API$path" \
      -H "Authorization: Bearer $tok" -H 'Content-Type: application/json' -d "$body")
  else
    HTTP=$(curl -sS -o /tmp/_f4_body.json -w '%{http_code}' -X "$method" "$API$path" \
      -H "Authorization: Bearer $tok")
  fi
  cat /tmp/_f4_body.json
}
jqv() { jq -r "$1 // empty" /tmp/_f4_body.json; }

echo "===== 0. 登录 ====="
# 全新库的首个 admin / 演示账号带 must_change_password=true（安全审计 CODE-06）：
# 直接拿 token 调业务接口会 403「首次登录必须先修改初始密码」。这里先改密再登录。
PW_FILE=/root/backup/.fix4_pw_seed
[ -f "$PW_FILE" ] || { openssl rand -hex 8 > "$PW_FILE"; chmod 600 "$PW_FILE"; }
SEED=$(cat "$PW_FILE")
ADMIN_NEW="Fx4-${SEED}Aa1"
LEAD_NEW="Lx4-${SEED}Aa1"

try_login() { # $1=user $2=pass -> token
  curl -sS -X POST "$API/system/auth/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"$1\",\"password\":\"$2\"}" | jq -r '.data.access_token // empty'
}
login_or_setup() { # $1=user $2=初始口令 $3=新口令 -> token（首次会先改密）
  local tok
  tok=$(try_login "$1" "$3")
  [ -n "$tok" ] && { echo "$tok"; return; }
  tok=$(try_login "$1" "$2")
  [ -z "$tok" ] && { echo ""; return; }
  curl -sS -o /dev/null -X PUT "$API/system/auth/password" \
    -H "Authorization: Bearer $tok" -H 'Content-Type: application/json' \
    -d "{\"old_password\":\"$2\",\"new_password\":\"$3\"}"
  try_login "$1" "$3"
}

ADMIN=$(login_or_setup admin "$ADMIN_PASS" "$ADMIN_NEW")
chk "$([ ${#ADMIN} -gt 20 ] && echo yes || echo no)" "yes" "admin 登录（首登改密后拿到 token）"
# 上一轮失败的登录尝试会把演示账号锁住（登录失败计数），测试库先解锁 + 清零
sqlq "UPDATE sys_user SET is_locked = false, login_fail_count = 0 WHERE username IN ('admin','demo_wh','demo_lead')" > /dev/null
DEMO_PASS="${NIK_DEMO_PASSWORD:-ChangeMe@2026}"
# stock_transfer v2 的 n_approve 节点指派给「仓库主管」= demo_wh（V0251 去 ADMIN 化后的口径）
LEAD=$(login_or_setup demo_wh "$DEMO_PASS" "$LEAD_NEW")
chk "$([ ${#LEAD} -gt 20 ] && echo yes || echo no)" "yes" "demo_wh 登录（仓库主管/审批人，首登改密后）"

echo "===== 1. 主数据（两个仓 / 默认库位 + 非默认库位 / 物料）====="
UNIT=$(api GET '/business/erp/unit/page?page=1&page_size=50' "$ADMIN" | jq -r '.data.list[0].id // .data.records[0].id')
echo "unit=$UNIT"
api POST '/business/erp/warehouse' "$ADMIN" \
  "{\"warehouse_code\":\"$WHA_CODE\",\"warehouse_name\":\"FIX4 调出仓\",\"warehouse_type\":\"NORMAL\",\"level\":1}" \
  > "$RAW/01_wh_a.json"
WHA=$(jqv '.data.id'); chk "$HTTP" "200" "建调出仓 $WHA_CODE"
api POST '/business/erp/warehouse' "$ADMIN" \
  "{\"warehouse_code\":\"$WHB_CODE\",\"warehouse_name\":\"FIX4 调入仓\",\"warehouse_type\":\"NORMAL\",\"level\":1}" \
  > "$RAW/01_wh_b.json"
WHB=$(jqv '.data.id'); chk "$HTTP" "200" "建调入仓 $WHB_CODE"

api POST '/business/erp/location' "$ADMIN" \
  "{\"location_code\":\"$LOCDEF_CODE\",\"location_name\":\"A 区默认位\",\"warehouse_id\":\"$WHA\",\"location_type\":\"STORAGE\",\"is_default\":true}" \
  > "$RAW/01_loc_a.json"
LOCDICT=$(jqv '.data.id'); chk "$HTTP" "200" "建默认库位 $LOCDEF_CODE"
api POST '/business/erp/location' "$ADMIN" \
  "{\"location_code\":\"$LOC2_CODE\",\"location_name\":\"A 区二号位\",\"warehouse_id\":\"$WHA\",\"location_type\":\"STORAGE\",\"is_default\":false}" \
  > "$RAW/01_loc_a2.json"
LOCDICT2=$(jqv '.data.id'); chk "$HTTP" "200" "建非默认库位 $LOC2_CODE"

api POST '/business/erp/material' "$ADMIN" \
  "{\"material_code\":\"$MAT_CODE\",\"material_name\":\"FIX4 物料甲\",\"unit_id\":\"$UNIT\",\"material_type\":\"RAW\",\"costing_method\":\"MOVING_AVG\",\"tax_rate\":0}" \
  > "$RAW/01_mat.json"
MAT=$(jqv '.data.id'); chk "$HTTP" "200" "建物料 $MAT_CODE"

echo "===== 2. 验收 ①：期初入库 → 只落「默认库位桶」 ====="
api POST '/business/erp/opening-balance/inventory' "$ADMIN" \
  "{\"period\":\"2026-06\",\"items\":[{\"material_id\":\"$MAT\",\"warehouse_id\":\"$WHA\",\"qty\":100,\"unit_cost\":10}]}" \
  > "$RAW/02_opening.json"
chk "$HTTP" "200" "库存期初保存"

BUCKETS=$(sqlq "SELECT COUNT(*) FROM biz_inventory_location WHERE material_id='$MAT' AND warehouse_id='$WHA'")
chk "$BUCKETS" "1" "① 期初后该仓该物料只有 1 个库位桶"
NULLB=$(sqlq "SELECT COUNT(*) FROM biz_inventory_location WHERE material_id='$MAT' AND warehouse_id='$WHA' AND location_id IS NULL")
chk "$NULLB" "0" "① 没有「未分配桶」（location_id IS NULL）"
LEDGER_DICT=$(sqlq "SELECT il.location_id FROM biz_inventory_location il WHERE il.material_id='$MAT' AND il.warehouse_id='$WHA'")
chk "$LEDGER_DICT" "$LOCDICT" "① 那个桶就是仓库默认库位 F4-A-DEFAULT"
LEDGER_QTY=$(sqlq "SELECT qty FROM biz_inventory_location WHERE material_id='$MAT' AND warehouse_id='$WHA'")
chk "$LEDGER_QTY" "100.0000" "① 库位账数量 100"
LEDGER_ROW=$(sqlq "SELECT id FROM biz_inventory_location WHERE material_id='$MAT' AND warehouse_id='$WHA'")
COST_LINK=$(sqlq "SELECT COUNT(*) FROM biz_inventory i JOIN biz_inventory_location il ON il.id=i.location_id WHERE i.material_id='$MAT' AND i.warehouse_id='$WHA' AND il.id='$LEDGER_ROW'")
chk "$COST_LINK" "1" "① 成本账与库位账同一个桶（同一行 id）"
COST_NULL=$(sqlq "SELECT COUNT(*) FROM biz_inventory WHERE material_id='$MAT' AND warehouse_id='$WHA' AND location_id IS NULL")
chk "$COST_NULL" "0" "① 成本账没有未分配桶行"

echo "===== 3. 验收 ②：调拨指定库位 → 扣的就是那个库位桶 ====="
# 说明：`api` 把 HTTP 码写在**当前 shell** 的 $HTTP 里，所以函数不放进 $() 调用
mk_transfer() { # $1=qty $2=tag(文件名)  → 设全局 TID 与 HTTP
  api POST '/business/erp/transfer' "$ADMIN" \
    "{\"from_warehouse_id\":\"$WHA\",\"to_warehouse_id\":\"$WHB\",\"reason\":\"FIX-4 验收调拨\",\"items\":[{\"material_id\":\"$MAT\",\"from_location_id\":\"$LEDGER_ROW\",\"qty\":$1}]}" \
    > "$RAW/$2_transfer.json"
  TID=$(jq -r '.data.id // empty' "$RAW/$2_transfer.json")
}
approve_and_ship() { # $1=transfer id → 设全局 HTTP（最后一步 = DONE）
  api PATCH "/business/erp/transfer/$1/status" "$ADMIN" '{"status":"PENDING"}' > /dev/null
  echo "  PENDING http=$HTTP"
  api GET '/business/flow/task/todo?page=1&page_size=20' "$LEAD" > "$RAW/03_todo.json"
  local task
  task=$(jq -r --arg tid "$1" '.data.list[]? | select(.biz_id==$tid) | .id' "$RAW/03_todo.json" | head -1)
  [ -z "$task" ] && task=$(jq -r '.data.list[0].id // empty' "$RAW/03_todo.json")
  api POST "/business/flow/task/$task/approve" "$LEAD" '{"comment":"FIX-4 验收：同意调拨"}' > "$RAW/03_approve.json"
  echo "  approve http=$HTTP task=$task"
  api PATCH "/business/erp/transfer/$1/status" "$ADMIN" '{"status":"IN_TRANSIT"}' > /dev/null
  echo "  IN_TRANSIT http=$HTTP"
  api PATCH "/business/erp/transfer/$1/status" "$ADMIN" '{"status":"DONE"}' > "$RAW/03_done.json"
  echo "  DONE http=$HTTP"
}

mk_transfer 30 03
chk "$HTTP" "200" "建调拨单 A→B 30 件（from_location=库位账行 $LEDGER_ROW）"
T1=$TID
approve_and_ship "$T1"
chk "$HTTP" "200" "调拨 30 件结算完成（DONE）"

QTY_A=$(sqlq "SELECT qty FROM biz_inventory_location WHERE id='$LEDGER_ROW'")
chk "$QTY_A" "70.0000" "② 指定库位桶 100 → 70（扣的就是库位 A 的桶）"
NULLB_A=$(sqlq "SELECT COUNT(*) FROM biz_inventory_location WHERE material_id='$MAT' AND warehouse_id='$WHA' AND location_id IS NULL")
chk "$NULLB_A" "0" "② 调拨没有在调出仓凭空造出汇总桶"
BUCKETS_A=$(sqlq "SELECT COUNT(*) FROM biz_inventory_location WHERE material_id='$MAT' AND warehouse_id='$WHA'")
chk "$BUCKETS_A" "1" "② 调出仓仍只有默认库位桶"
BROKEN=$(sqlq "WITH c AS (SELECT COALESCE(il.location_id::text,'-') loc, SUM(i.qty) qty FROM biz_inventory i LEFT JOIN biz_inventory_location il ON il.id=i.location_id WHERE i.material_id='$MAT' GROUP BY 1), l AS (SELECT COALESCE(location_id::text,'-') loc, SUM(qty) qty FROM biz_inventory_location WHERE material_id='$MAT' GROUP BY 1) SELECT COUNT(*) FROM c FULL JOIN l ON c.loc=l.loc WHERE COALESCE(c.qty,0)<>COALESCE(l.qty,0)")
chk "$BROKEN" "0" "② 逐桶两本账 0 差异"

echo "===== 4. 验收 ③：指定库位库存不足 → 明确报错（不静默改扣）====="
mk_transfer 200 04
chk "$HTTP" "200" "建调拨单 200 件（库位只有 70）"
T2=$TID
approve_and_ship "$T2" > "$RAW/04_flow.log"
chk "$HTTP" "409" "③ 结算被 409 拦住（不再静默改扣汇总桶）"
MSG=$(jq -r '.msg // .message // empty' "$RAW/03_done.json")
echo "$MSG" > "$RAW/04_error_msg.txt"
case "$MSG" in
  *"当前可用 70"*) ok "③ 报文点名该库位可用量 70" ;;
  *) bad "③ 报文没点名可用量：$MSG" ;;
esac
case "$MSG" in
  *"$LOCDEF_CODE"*) ok "③ 报文点名库位编码 $LOCDEF_CODE" ;;
  *) bad "③ 报文没点名库位编码：$MSG" ;;
esac
chk "$(sqlq "SELECT status FROM biz_stock_transfer WHERE id='$T2'")" "IN_TRANSIT" "③ 被拒结算后单据仍停在 IN_TRANSIT"
chk "$(sqlq "SELECT qty FROM biz_inventory_location WHERE id='$LEDGER_ROW'")" "70.0000" "③ 账一分不动（库位账仍 70）"
chk "$(sqlq "SELECT COUNT(*) FROM biz_inventory_txn WHERE doc_id='$T2'")" "0" "③ 被拒结算没写流水"

echo "===== 5. 验收 ④：迁移幂等（重跑 V0324 up.sql）====="
BEFORE_QTY=$(sqlq "SELECT COALESCE(SUM(qty),0) FROM biz_inventory")
BEFORE_AMT=$(sqlq "SELECT COALESCE(SUM(amount),0) FROM biz_inventory")
psql -h 127.0.0.1 -U cloudpivot -d "$DB" -X -v ON_ERROR_STOP=1 \
  -f /opt/cloudpivot/backend/migrations/business/V0324__inventory_default_bucket.up.sql \
  > "$RAW/05_migrate_rerun.log" 2>&1
chk "$?" "0" "⑤ V0324 up.sql 重跑无报错"
MERGED=$(grep -o '并入 [0-9]* 个' "$RAW/05_migrate_rerun.log" | head -1)
chk "$MERGED" "并入 0 个" "⑤ 重跑没有任何桶需要并入（幂等）"
chk "$(sqlq "SELECT COALESCE(SUM(qty),0) FROM biz_inventory")" "$BEFORE_QTY" "⑤ 重跑后总数量不变"
chk "$(sqlq "SELECT COALESCE(SUM(amount),0) FROM biz_inventory")" "$BEFORE_AMT" "⑤ 重跑后总金额不变"

echo "===== 6. 验收 ⑤：全量对账（库存表 = 流水累计，按 (物料, 仓库) 聚合）====="
DIFF=$(sqlq "WITH inv AS (SELECT material_id, warehouse_id, SUM(qty) qty FROM biz_inventory GROUP BY 1,2), txn AS (SELECT material_id, warehouse_id, SUM(CASE WHEN txn_type='IN' THEN qty ELSE -qty END) qty FROM biz_inventory_txn GROUP BY 1,2) SELECT COUNT(*) FROM inv FULL JOIN txn USING (material_id, warehouse_id) WHERE COALESCE(inv.qty,0) <> COALESCE(txn.qty,0)")
chk "$DIFF" "0" "⑤ 全量对账 0 差异（库存表 = 流水累计）"
BUCKET_DIFF=$(sqlq "WITH c AS (SELECT i.material_id AS m, i.warehouse_id AS w, COALESCE(il.location_id::text,'-') AS loc, SUM(i.qty) AS qty FROM biz_inventory i LEFT JOIN biz_inventory_location il ON il.id=i.location_id GROUP BY 1,2,3), l AS (SELECT il2.material_id AS m, il2.warehouse_id AS w, COALESCE(il2.location_id::text,'-') AS loc, SUM(il2.qty) AS qty FROM biz_inventory_location il2 GROUP BY 1,2,3) SELECT COUNT(*) FROM c FULL JOIN l ON c.m=l.m AND c.w=l.w AND c.loc=l.loc WHERE COALESCE(c.qty,0) <> COALESCE(l.qty,0)")
chk "$BUCKET_DIFF" "0" "⑤ 逐桶两本账 0 差异（全库）"

echo
echo "===== 汇总：PASS=$PASS FAIL=$FAIL ====="
echo "db=$DB"
if [ "$FAIL" -ne 0 ]; then exit 1; fi
exit 0
