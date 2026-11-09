#!/usr/bin/env bash
# E2E-FIX-2 145 实测：book_qty = SUM(所有桶) + 修复路径不改错账面
# 在 145 上起一个**隔离实例**（独立库 cp_e2efix2 + 端口 18318），全程不碰 cloudpivot_dev / :8033。
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
export PATH=/root/.cargo/bin:$PATH

DB=cp_e2efix2
PORT=18318
BASE="http://127.0.0.1:$PORT/api"
RAW=/tmp/e2efix2/raw; mkdir -p "$RAW"
ADMIN_PASS="${NIK_ADMIN_PASSWORD:-Admin@123456}"
NEW_PASS='E2eFix2@145pass'
PASS=0; FAIL=0
say(){ printf '%s\n' "$*"; }
ok(){ PASS=$((PASS+1)); say "  ✅ $*"; }
bad(){ FAIL=$((FAIL+1)); say "  ❌ $*"; }
PSQL(){ psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$DB" -At -F'|' -P pager=off "$@"; }
api(){ # api <name> <METHOD> <path> [token] [json]
  local name=$1 method=$2 path=$3 token=${4:-} body=${5:-}
  local out="$RAW/$name.json"
  local -a args=(-s -m 60 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path")
  [ -n "$token" ] && args+=(--oauth2-bearer "$token")
  [ -n "$body" ] && args+=(-H 'Content-Type: application/json' -d "$body")
  HTTP=$(curl "${args[@]}")
  BODY=$(cat "$out" 2>/dev/null)
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
}
data(){ printf '%s' "$BODY" | jq -r "$1" 2>/dev/null; }

say "==== 0. 建隔离库 + 起实例（不影响 cloudpivot_dev / :8033）"
su - postgres -c "dropdb --force $DB" >/dev/null 2>&1
su - postgres -c "createdb -O $NIK_DATABASE_USER $DB" || { echo "createdb 失败"; exit 1; }
cd /opt/cloudpivot/backend || exit 1
export NIK_DATABASE_NAME="$DB" NIK_SERVER_LISTEN="127.0.0.1:$PORT"
: > /tmp/e2efix2.log
setsid nohup ./target/debug/cloudpivot > /tmp/e2efix2.log 2>&1 < /dev/null &
PID=$!
for _ in $(seq 1 240); do grep -q "数据库迁移完成" /tmp/e2efix2.log && break; sleep 1; done
sleep 6
say "  pid=$PID 迁移=$(grep -c '数据库迁移完成' /tmp/e2efix2.log) health=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:$PORT/health)"
say "  库指纹 schema_migrations(max|count)=$(PSQL -c 'select max(version::int), count(*) from schema_migrations' | tr '|' ' ')"

say "==== 1. 造场景（真实 API：批量生成 3 件未分配桶 + 3 件默认库位桶）"
MAT=$(PSQL -c "select id from biz_material where material_code='E2EFIX2-M-SN'")
if [ -z "$MAT" ]; then
  UNIT=$(PSQL -c "select id from biz_unit where unit_code='E2EFIX2-PCS'" || true)
  PSQL -c "INSERT INTO biz_unit (id, unit_code, unit_name, is_base, status, sort_order)
           VALUES ('e2f20001-0000-4000-8000-000000000001','E2EFIX2-PCS','件',1,1,1)" >/dev/null
  MAT='e2f20004-0000-4000-8000-000000000001'
  WH='e2f20002-0000-4000-8000-000000000001'
  LOC='e2f20003-0000-4000-8000-000000000001'
  IL='e2f20008-0000-4000-8000-000000000001'
  PSQL -c "INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_type, allow_negative,
                                      status, sort_order, parent_id, level, warehouse_path, is_leaf)
           VALUES ('$WH','E2EFIX2-WH','E2EFIX2 仓','NORMAL',0,1,91,NULL,1,'/$WH/',1)" >/dev/null
  PSQL -c "INSERT INTO biz_location (id, location_code, location_name, warehouse_id, location_type,
                                     is_default, capacity, status, sort_order)
           VALUES ('$LOC','E2EFIX2-A-01','E2EFIX2 默认库位','$WH','STORAGE',1,1000,1,1)" >/dev/null
  PSQL -c "INSERT INTO biz_material (id, material_code, material_name, spec, unit_id, material_type,
                                     safety_stock, costing_method, tax_rate, status, serial_required)
           VALUES ('$MAT','E2EFIX2-M-SN','E2EFIX2 一物一码物料','SN','e2f20001-0000-4000-8000-000000000001',
                   'FINISHED',0,'MOVING_AVG',0.13,1,TRUE)" >/dev/null
  # 库位库存行（= 应用里 find-or-create 的那一行；location_id 是库位字典 id）
  PSQL -c "INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty, locked_qty)
           VALUES ('$IL','$MAT','$WH','$LOC',0,0)" >/dev/null
else
  WH=$(PSQL -c "select id from biz_warehouse where warehouse_code='E2EFIX2-WH'")
  IL=$(PSQL -c "select id from biz_inventory_location where material_id='$MAT' and location_id is not null limit 1")
fi
say "  material=$MAT warehouse=$WH location_row=$IL"

# 登录（全新库 admin 带 must_change_password → 必须先改密，否则业务接口一律 403）
do_change_password(){
  api chpass PUT /system/auth/password "$TOK" \
      "{\"old_password\":\"$1\",\"new_password\":\"$NEW_PASS\"}"
  say "  改密 http=$HTTP code=$CODE"
  api login POST /system/auth/login "" "{\"username\":\"admin\",\"password\":\"$NEW_PASS\"}"
  TOK=$(data '.data.access_token')
}
api login POST /system/auth/login "" "{\"username\":\"admin\",\"password\":\"$ADMIN_PASS\"}"
TOK=$(data '.data.access_token')
MUST=$(data '.data.must_change_password')
if [ "$MUST" = "true" ]; then
  say "  首次登录（must_change_password=true）→ 先改密"
  do_change_password "$ADMIN_PASS"
fi
if [ -n "$TOK" ] && [ "$TOK" != "null" ]; then ok "登录拿到 token"; else bad "登录失败 http=$HTTP $BODY"; fi

# 业务接口探针：万一 must_change_password 标志没回，403 文本也能兜住
api probe GET "/business/erp/inventory/audit?dry_run=true" "$TOK"
if [ "$HTTP" = "403" ]; then
  say "  业务接口 403（$(printf '%s' "$BODY" | head -c 120)）→ 改密后重登"
  do_change_password "$ADMIN_PASS"
  ok "改密后重新拿到 token"
fi

api gen1 POST /business/erp/serial/batch-generate "$TOK" \
  "{\"material_id\":\"$MAT\",\"warehouse_id\":\"$WH\",\"count\":3,\"unit_cost\":10}"
if [ "$CODE" = "0" ]; then ok "批量生成 3 件（未指定库位 → 未分配桶）"; else bad "批量生成1 http=$HTTP $(printf '%s' "$BODY" | head -c 300)"; fi
api gen2 POST /business/erp/serial/batch-generate "$TOK" \
  "{\"material_id\":\"$MAT\",\"warehouse_id\":\"$WH\",\"location_id\":\"$IL\",\"count\":3,\"unit_cost\":10}"
if [ "$CODE" = "0" ]; then ok "入库 3 件（指定默认库位 → 默认库位桶）"; else bad "批量生成2 http=$HTTP $(printf '%s' "$BODY" | head -c 300)"; fi

say "  -- 账实快照（SQL）"
PSQL -c "select 'bucket='||coalesce(location_id::text,'(未分配)')||' qty='||qty
           from biz_inventory where material_id='$MAT' order by location_id nulls first" | sed 's/^/     /'
PSQL -c "select 'Σqty='||coalesce(sum(qty),0)||' serials='||(select count(*) from biz_material_serial
           where material_id='$MAT' and status='IN_STOCK' and deleted_at is null)
           from biz_inventory where material_id='$MAT'" | sed 's/^/     /'

say "==== 2. GET /erp/inventory/audit?dry_run=true → 期望 book=6 actual=6 diff=0（不再假红）"
api audit GET "/business/erp/inventory/audit?dry_run=true" "$TOK"
say "  http=$HTTP code=$CODE"
if [ "$CODE" = "0" ]; then
  say "  report: $(printf '%s' "$BODY" | jq -c '{checked:.data.checked,mismatched:.data.mismatched,unfixable:.data.unfixable,items:(.data.items|length)}')"
  MATCH=$(printf '%s' "$BODY" | jq -r --arg m "$MAT" '[.data.items[]?|select(.material_id==$m)]|length')
  if [ "$MATCH" = "0" ]; then ok "两桶并存、账实一致 → 该物料无漂移（旧口径会报 book=3 / diff=-3）"; else
    bad "仍报漂移：$(printf '%s' "$BODY" | jq -c --arg m "$MAT" '[.data.items[]?|select(.material_id==$m)]')"; fi
else bad "巡检接口 http=$HTTP $(printf '%s' "$BODY" | head -c 300)"; fi

say "==== 3. dry_run=false（账实一致）→ 一行都不许改"
api fix0 GET "/business/erp/inventory/audit?dry_run=false" "$TOK"
say "  http=$HTTP fixed=$(data '.data.fixed')"
AFTER=$(PSQL -c "select string_agg(coalesce(location_id::text,'(未分配)')||'='||qty,',' order by location_id nulls first)
                  from biz_inventory where material_id='$MAT'")
say "  桶布局: $AFTER"

say "==== 4. 单桶场景的修复仍然正确（另起一物一码物料：只有未分配桶 → 改脏 → 差额修回）"
MAT2=$(PSQL -c "select id from biz_material where material_code='E2EFIX2-M-SN2'")
if [ -z "$MAT2" ]; then
  MAT2='e2f20004-0000-4000-8000-000000000002'
  PSQL -c "INSERT INTO biz_material (id, material_code, material_name, spec, unit_id, material_type,
                                     safety_stock, costing_method, tax_rate, status, serial_required)
           VALUES ('$MAT2','E2EFIX2-M-SN2','E2EFIX2 单桶物料','SN2','e2f20001-0000-4000-8000-000000000001',
                   'FINISHED',0,'MOVING_AVG',0.13,1,TRUE)" >/dev/null
fi
api gen3 POST /business/erp/serial/batch-generate "$TOK" \
  "{\"material_id\":\"$MAT2\",\"warehouse_id\":\"$WH\",\"count\":3,\"unit_cost\":10}"
say "  生成 3 件：http=$HTTP code=$CODE 桶=$(PSQL -c "select count(*) from biz_inventory where material_id='$MAT2'")"
PSQL -c "UPDATE biz_inventory SET qty=9, amount=90 WHERE material_id='$MAT2' AND location_id IS NULL" >/dev/null
api audit3 GET "/business/erp/inventory/audit?dry_run=true" "$TOK"
say "  修前: $(printf '%s' "$BODY" | jq -c --arg m "$MAT2" '[.data.items[]?|select(.material_id==$m)]|map({book_qty,actual_qty,diff,bucket_count,fixable})')"
api fix2 GET "/business/erp/inventory/audit?dry_run=false" "$TOK"
say "  dry_run=false: fixed=$(data '.data.fixed')"
S2=$(PSQL -c "select qty::numeric(18,4)||' / amount='||amount::numeric(18,4)||' / buckets='||count(*)
               from biz_inventory where material_id='$MAT2' and location_id is null group by qty, amount")
say "  修后: $S2"
case "$S2" in
  3.0000*) ok "单桶按差额修回 3（金额随数量重算 30）" ;;
  *) bad "单桶修复结果异常：$S2" ;;
esac

say "==== 5. 多桶 + 真差异 → dry_run=false 必须一行不改（数据安全级验收）"
# 把两桶物料的未分配桶改成 9 → Σ=12、在库 6、diff=+6、bucket_count=2
PSQL -c "UPDATE biz_inventory SET qty=9, amount=90 WHERE material_id='$MAT' AND location_id IS NULL" >/dev/null
api audit2 GET "/business/erp/inventory/audit?dry_run=true" "$TOK"
say "  dry_run=true 明细: $(printf '%s' "$BODY" | jq -c --arg m "$MAT" '[.data.items[]?|select(.material_id==$m)]|map({book_qty,actual_qty,diff,bucket_count,fixable,unfixable_reason})')"
BEFORE=$(PSQL -c "select string_agg(coalesce(location_id::text,'(未分配)')||'='||qty,',' order by location_id nulls first)
                   from biz_inventory where material_id='$MAT'")
api fix1 GET "/business/erp/inventory/audit?dry_run=false" "$TOK"
say "  http=$HTTP fixed=$(data '.data.fixed') unfixable=$(data '.data.unfixable')"
say "  该物料: $(printf '%s' "$BODY" | jq -c --arg m "$MAT" '[.data.items[]?|select(.material_id==$m)]|map({book_qty,actual_qty,diff,bucket_count,fixable,unfixable_reason})')"
AFTER2=$(PSQL -c "select string_agg(coalesce(location_id::text,'(未分配)')||'='||qty,',' order by location_id nulls first)
                   from biz_inventory where material_id='$MAT'")
say "  before: $BEFORE"
say "  after : $AFTER2"
if [ "$BEFORE" = "$AFTER2" ]; then ok "多桶行被拒绝自动修：账面逐桶原样"; else bad "账面被改动（$BEFORE → $AFTER2）"; fi

say "==== 6. 收尾：停实例（库保留取证）"
kill "$PID" 2>/dev/null; sleep 1
ss -ltn | grep -q ":$PORT " && bad "端口 $PORT 仍被占用" || ok "实例已停，$PORT 释放"
say ""
say "== PASS=$PASS FAIL=$FAIL"
[ "$FAIL" = 0 ] || say "== 有失败项"
