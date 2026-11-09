#!/usr/bin/env bash
# E2E-08 主数据域演练公共库（145 测试机 · cloudpivot_dev · 真后端 127.0.0.1:8033）
# 被 e2e08_master_{a,b,c}.sh 与 e2e08_db_evidence.sh source。
# 状态落 /tmp/e2e08/state.sh，报文落 /tmp/e2e08/raw/，断言追加 /tmp/e2e08/asserts.log。
set -uo pipefail
ENV=/etc/cloudpivot/cloudpivot-dev.env
set -a; . "$ENV"; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
BASE="${BASE:-http://127.0.0.1:8033/api}"
WORK=${WORK:-/tmp/e2e08}; RAW=$WORK/raw; STATE=$WORK/state.sh; LOG=$WORK/asserts.log
mkdir -p "$RAW"; touch "$LOG"
[ -f "$STATE" ] && . "$STATE"
PSQLQ() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -At -F'|' -P pager=off "$@"; }
SQ()   { PSQLQ -c "$1" 2>&1 | tr '\n' ' ' | sed 's/  */ /g; s/ *$//'; }
PASS=${PASS:-0}; FAIL=${FAIL:-0}
say()  { printf '%s\n' "$*"; }
ok()   { PASS=$((PASS+1)); printf 'PASS\t%s\n' "$*" >> "$LOG"; say "  ✅ $*"; }
bad()  { FAIL=$((FAIL+1)); printf 'FAIL\t%s\n' "$*" >> "$LOG"; say "  ❌ $*"; }
check() { if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }
checkne() { if [ "$2" != "$3" ]; then ok "$1 = $2（≠ $3）"; else bad "$1 = $2（不应等于 $3）"; fi; }
contains() { case "$2" in *"$3"*) ok "$1 含「$3」";; *) bad "$1 不含「$3」（实际：$(printf '%s' "$2" | head -c 240)）";; esac; }
matches() { if printf '%s' "$2" | grep -Eq "$3"; then ok "$1 = $2（匹配 $3）"; else bad "$1 = $2（应匹配 $3）"; fi; }
summary() { say; say "== PASS=$PASS FAIL=$FAIL"; }
save() { printf '%s=%q\n' "$1" "$2" >> "$STATE"; export "$1"="$2"; }
data() { printf '%s' "$BODY" | jq -r "$1" 2>/dev/null; }
show() { printf '%s' "$BODY" | jq -c '{code,msg}' 2>/dev/null | head -c 400; }

# 取号器当前值（号段不存在则回空）
seqof() { SQ "select current_no from biz_seq where seq_type='$1' and deleted_at is null"; }

api() { # api <名> <METHOD> <path> <token> [json]
  local name=$1 method=$2 path=$3 token=$4 body=${5:-}
  local out="$RAW/$name.json"
  local -a args=(-s -m 60 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path")
  [ -n "$token" ] && args+=(--oauth2-bearer "$token")
  [ -n "$body" ] && args+=(-H 'Content-Type: application/json' -d "$body")
  HTTP=$(curl "${args[@]}")
  if [ "$HTTP" = 401 ] && [ -n "$token" ]; then
    local fresh
    fresh=$(curl -s -m 30 -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
      -d "{\"username\":\"admin\",\"password\":\"$NIK_ADMIN_PASSWORD\"}" | jq -r '.data.access_token // empty')
    if [ -n "$fresh" ]; then
      T_ADMIN=$fresh; save T_ADMIN "$fresh"
      local -a retry=(-s -m 60 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path" --oauth2-bearer "$fresh")
      [ -n "$body" ] && retry+=(-H 'Content-Type: application/json' -d "$body")
      HTTP=$(curl "${retry[@]}")
    fi
  fi
  BODY=$(cat "$out" 2>/dev/null)
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
  printf '%s' "$BODY" | jq -e . >/dev/null 2>&1 || say "  ⚠ 非 JSON($name) http=$HTTP: $(printf '%s' "$BODY" | head -c 200)"
}

# 页面列表里是否含某编码（各主数据分页接口统一取 data.list）
page_has() { # page_has <token> <page-path> <code> [jq 字段] [过滤参数名，缺省 keyword] —— 命中打印 1，否则空
  local token=$1 path=$2 want=$3 field=${4:-code} param=${5:-keyword}
  api "pg_$(echo "$path" | tr '/?=&' '____')_$want" GET "$path?page=1&page_size=50&$param=$want" "$token"
  local hit
  hit=$(printf '%s' "$BODY" | jq -r --arg w "$want" --arg f "$field" 'if ([.data.list[]? | .[$f]] | index($w)) != null then "1" else "" end' 2>/dev/null)
  printf '%s' "$hit"
}

login() { # login <user> <pass> <tag>
  local i
  for i in 1 2 3 4 5; do
    api "login_$3" POST /system/auth/login "" "{\"username\":\"$1\",\"password\":\"$2\"}"
    [ "$HTTP" = 200 ] && [ "$CODE" = 0 ] && { data '.data.access_token'; return 0; }
    sleep 2
  done
  say "FATAL: 登录失败 $1 http=$HTTP $(show)" >&2; return 1
}

# 取编基础数据（145 共享库，已存在，只读复用）
fixtures() {
  UNIT_ID=$(SQ "select id from biz_unit where unit_code='BAG' and deleted_at is null")
  [ -z "$UNIT_ID" ] && UNIT_ID=$(SQ "select id from biz_unit where deleted_at is null order by unit_code limit 1")
  ACC_ID=$(SQ "select id from biz_account where account_code='5602.02' and deleted_at is null")
  [ -z "$ACC_ID" ] && ACC_ID=$(SQ "select id from biz_account where deleted_at is null and account_code like '5602.%' order by account_code limit 1")
  CUS_ID=$(SQ "select id from biz_customer where code='CUS-0001' and deleted_at is null")
  CUS2_ID=$(SQ "select id from biz_customer where code='CUS-0002' and deleted_at is null")
  MAT_STD_ID=$(SQ "select id from biz_material where material_code='D7-M1' and deleted_at is null")
  save UNIT_ID "$UNIT_ID"; save ACC_ID "$ACC_ID"; save CUS_ID "$CUS_ID"; save CUS2_ID "$CUS2_ID"; save MAT_STD_ID "$MAT_STD_ID"
  say "  · 夹具：unit=$UNIT_ID account=$ACC_ID customer=$CUS_ID/$CUS2_ID material=$MAT_STD_ID"
}

# ---------------------------------------------------------------- 夹具清场
# 铁律：删 biz_material / biz_warehouse 之前，必须先删它们名下的 **四张账**
#   biz_material_serial → biz_inventory_txn → biz_inventory（成本账）→ biz_inventory_location（库位账）
# 顺序被硬外键钉死：`biz_inventory.location_id → biz_inventory_location.id`，反过来删 23503。
# 漏删库位账的历史事故（t_45bda7ad）：11-09 12:00–12:07 每跑一次 E2E-08 就漏一行
#   qty=3 的孤儿库位账（物料/仓库已被硬删），dev 库逐桶对账永远回不到 0。
# 另外 batch-generate 会写一张 `source_type='SERIAL_BATCH'` 的凭证 + 状态日志，
#   原来也不清 ⇒ 每跑一次 1405 借方涨 30 元而库存没有对应金额（B2 假红）+ 孤儿日志。
#   凭证用「本卡物料流水里的 doc_id（批次 id）」定位，夹具身份是从物料推出来的，不会误删真凭证。

# 把 "id1,id2" 变成 "'id1'::uuid,'id2'::uuid"；空串给一个永远不命中的哨兵
_uuid_in() {
  local ids=$1
  if [ -z "$ids" ]; then printf "'00000000-0000-0000-0000-000000000000'::uuid"; return; fi
  local out="" id
  local IFS=','
  for id in $ids; do out+="'$id'::uuid,"; done
  printf '%s' "${out%,}"
}

# 只清自己（E2E08 前缀）的演练行；不 DROP / TRUNCATE / 不动他人数据
cleanup_own() {
  local ids_file=/tmp/e2e08/last_cleanup_ids.sh
  mkdir -p /tmp/e2e08
  # ① 清场前先把「这次要删的 id」记下来：删完就查不到，收尾断言要用它逐表复核
  E2E08_MAT_IDS=$(SQ "select coalesce(string_agg(id::text, ','), '') from biz_material where material_code ilike 'E2E08%'")
  E2E08_WH_IDS=$(SQ "select coalesce(string_agg(id::text, ','), '') from biz_warehouse where warehouse_code ilike 'E2E08%'")
  E2E08_BATCH_IDS=$(SQ "select coalesce(string_agg(distinct t.doc_id::text, ','), '')
                           from biz_inventory_txn t join biz_material m on m.id = t.material_id
                          where m.material_code ilike 'E2E08%' and t.doc_type = 'SERIAL_BATCH' and t.doc_id is not null")
  printf 'E2E08_MAT_IDS=%q\nE2E08_WH_IDS=%q\nE2E08_BATCH_IDS=%q\n' \
    "$E2E08_MAT_IDS" "$E2E08_WH_IDS" "$E2E08_BATCH_IDS" > "$ids_file"

  local mat_in wh_in batch_in
  mat_in=$(_uuid_in "$E2E08_MAT_IDS")
  wh_in=$(_uuid_in "$E2E08_WH_IDS")
  batch_in=$(_uuid_in "$E2E08_BATCH_IDS")

  PSQLQ >/dev/null <<SQL
BEGIN;
-- ② 状态日志没有外键：必须**按将要删的凭证 id** 先删，否则每跑一次多一批孤儿（fixture-hygiene §1）
DELETE FROM biz_status_log
 WHERE entity_type = 'VOUCHER'
   AND entity_id IN (SELECT id FROM biz_voucher
                      WHERE source_type = 'SERIAL_BATCH'
                        AND source_id IN ($batch_in));
-- ③ 凭证分录 → 凭证（batch-generate 的「无来源入库」凭证，批次 id 来自本卡物料的流水）
DELETE FROM biz_voucher_entry
 WHERE voucher_id IN (SELECT id FROM biz_voucher
                       WHERE source_type = 'SERIAL_BATCH'
                         AND source_id IN ($batch_in));
DELETE FROM biz_voucher
 WHERE source_type = 'SERIAL_BATCH'
   AND source_id IN ($batch_in);
-- ④ 价格挂靠（引用 biz_material，先于物料）
DELETE FROM biz_price_tier WHERE material_id IN (SELECT id FROM biz_material WHERE material_code ILIKE 'E2E08%')
   OR customer_id IN (SELECT id FROM biz_customer WHERE code ILIKE 'E2E08%');
DELETE FROM biz_customer_price WHERE material_id IN (SELECT id FROM biz_material WHERE material_code ILIKE 'E2E08%')
   OR customer_id IN (SELECT id FROM biz_customer WHERE code ILIKE 'E2E08%');
-- ⑤ 序列号（location_id 指向库位账行，先解引用再删；序列号本身也先于流水）
UPDATE biz_material_serial SET location_id = NULL
 WHERE material_id IN (SELECT id FROM biz_material WHERE material_code ILIKE 'E2E08%');
DELETE FROM biz_material_serial WHERE material_id IN (SELECT id FROM biz_material WHERE material_code ILIKE 'E2E08%');
-- ⑥ 流水
DELETE FROM biz_inventory_txn WHERE material_id IN (SELECT id FROM biz_material WHERE material_code ILIKE 'E2E08%');
-- ⑦ 成本账：必须先于库位账（biz_inventory.location_id → biz_inventory_location.id）
DELETE FROM biz_inventory WHERE material_id IN (SELECT id FROM biz_material WHERE material_code ILIKE 'E2E08%');
-- ⑧ 库位账：按 (物料) / (仓库) 两个维度都删 —— 仓库维度同时收敛历史漏删行
UPDATE biz_material_serial SET location_id = NULL
 WHERE location_id IN (SELECT id FROM biz_inventory_location
                        WHERE material_id IN (SELECT id FROM biz_material WHERE material_code ILIKE 'E2E08%')
                           OR warehouse_id IN (SELECT id FROM biz_warehouse WHERE warehouse_code ILIKE 'E2E08%'));
DELETE FROM biz_inventory_location
 WHERE material_id IN (SELECT id FROM biz_material WHERE material_code ILIKE 'E2E08%')
    OR warehouse_id IN (SELECT id FROM biz_warehouse WHERE warehouse_code ILIKE 'E2E08%');
-- ⑨ 其余主数据
DELETE FROM biz_doc_attachment WHERE doc_id IN (SELECT id FROM biz_customer WHERE code ILIKE 'E2E08%');
UPDATE biz_material SET default_warehouse_id = NULL
 WHERE default_warehouse_id IN (SELECT id FROM biz_warehouse WHERE warehouse_code ILIKE 'E2E08%');
DELETE FROM biz_material WHERE material_code ILIKE 'E2E08%';
DELETE FROM biz_customer WHERE code ILIKE 'E2E08%';
DELETE FROM biz_supplier WHERE supplier_code ILIKE 'E2E08%';
DELETE FROM biz_warehouse WHERE warehouse_code ILIKE 'E2E08%';
DELETE FROM biz_asset_category WHERE category_code ILIKE 'E2E08%';
DELETE FROM biz_expense_type WHERE type_code ILIKE 'E2E08%';
DELETE FROM biz_unit WHERE unit_code ILIKE 'E2E08%';
DELETE FROM sys_user_role WHERE user_id IN (SELECT id FROM sys_user WHERE username LIKE 'e2e08%');
DELETE FROM sys_user WHERE username LIKE 'e2e08%';
COMMIT;
SQL
  say "  · 已清理本卡前缀（E2E08）的残留演练行（四张账 + 批次凭证 + 状态日志）"
}

# 数一下 "id1,id2" 里有几个 id（给下面自检标题用）
_ids_count() { local s=${1:-}; if [ -z "$s" ]; then echo 0; else printf '%s' "$s" | awk -F, '{print NF}'; fi; }

# 收尾断言：本卡 (物料,仓库) 四张账必须全 0；全库不得有孤儿库位账
# 判据是**形态判据**（主数据不存在），不写死物料编码 —— 换个夹具名/换个库也能抓到。
assert_clean() {
  local ids_file=/tmp/e2e08/last_cleanup_ids.sh
  local E2E08_MAT_IDS="" E2E08_WH_IDS="" E2E08_BATCH_IDS=""
  [ -f "$ids_file" ] && . "$ids_file"
  local mat_in wh_in batch_in
  mat_in=$(_uuid_in "${E2E08_MAT_IDS:-}")
  wh_in=$(_uuid_in "${E2E08_WH_IDS:-}")
  batch_in=$(_uuid_in "${E2E08_BATCH_IDS:-}")

  say "  · 清场自检（本次要删的 物料 $(_ids_count "${E2E08_MAT_IDS:-}") 个 / 仓库 $(_ids_count "${E2E08_WH_IDS:-}") 个 / 批次 $(_ids_count "${E2E08_BATCH_IDS:-}") 个）"
  local n

  n=$(SQ "select count(*) from biz_inventory_txn where material_id in ($mat_in)")
  check "清场后 流水（本卡物料）= 0 行" "$n" "0"
  n=$(SQ "select count(*) from biz_inventory where material_id in ($mat_in)")
  check "清场后 成本账（本卡物料）= 0 行" "$n" "0"
  n=$(SQ "select count(*) from biz_inventory_location where material_id in ($mat_in) or warehouse_id in ($wh_in)")
  check "清场后 库位账（本卡物料/仓库）= 0 行" "$n" "0"
  n=$(SQ "select count(*) from biz_material_serial where material_id in ($mat_in)")
  check "清场后 序列号（本卡物料）= 0 行" "$n" "0"
  n=$(SQ "select count(*) from biz_voucher where source_type='SERIAL_BATCH' and source_id in ($batch_in)")
  check "清场后 批次凭证（本卡批次）= 0 张" "$n" "0"
  n=$(SQ "select count(*) from biz_voucher_entry where voucher_id in (select id from biz_voucher where source_type='SERIAL_BATCH' and source_id in ($batch_in))")
  check "清场后 批次凭证分录 = 0 行" "$n" "0"
  n=$(SQ "select count(*) from biz_status_log where entity_type='VOUCHER' and entity_id in (select id from biz_voucher where source_type='SERIAL_BATCH' and source_id in ($batch_in))")
  check "清场后 凭证状态日志 = 0 行" "$n" "0"

  # 全库形态判据：库位账行必须能反查到物料与仓库（E2E-FIX-7 ⑤ / p16 10_report 口径）
  n=$(SQ "select count(*) from biz_inventory_location l
           where not exists (select 1 from biz_material m where m.id = l.material_id)")
  check "全库 孤儿库位账（material 不存在）= 0 行" "$n" "0"
  n=$(SQ "select count(*) from biz_inventory_location l
           where not exists (select 1 from biz_warehouse w where w.id = l.warehouse_id)")
  check "全库 孤儿库位账（warehouse 不存在）= 0 行" "$n" "0"
  n=$(SQ "select count(*) from biz_inventory i
           where not exists (select 1 from biz_material m where m.id = i.material_id)
              or not exists (select 1 from biz_warehouse w where w.id = i.warehouse_id)")
  check "全库 孤儿成本账 = 0 行" "$n" "0"
  n=$(SQ "select count(*) from biz_inventory_txn t
           where not exists (select 1 from biz_material m where m.id = t.material_id)")
  check "全库 孤儿流水 = 0 行" "$n" "0"

  # 「本次运行前后库位账总数不变」：只作提示（dev 库是共享的，别的卡并发写库会造成差额）
  local baseline="" now="" delta=""
  [ -s /tmp/e2e08/location_baseline.txt ] && baseline=$(tr -d '[:space:]' < /tmp/e2e08/location_baseline.txt)
  now=$(SQ "select count(*) from biz_inventory_location")
  if [ -n "$baseline" ]; then
    delta=$((now - baseline))
    if [ "$delta" = "0" ]; then
      say "  · 本次运行前后库位账总数不变（$baseline 行）"
    else
      say "  ⚠ 库位账总数 ${baseline} → ${now}（Δ$delta）；若非别的卡并发写库，说明本脚本仍有漏删"
    fi
  fi
}
