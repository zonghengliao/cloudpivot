#!/usr/bin/env bash
# =============================================================================
# SO-27 验收：销售订单关联销售合同 + 合同额度守卫（真后端 + 真 token curl）
# =============================================================================
#   bash scripts/verify/so27_instance.sh up      # 先起实例（:18227，会应用 V0302）
#   bash scripts/verify/so27_sales_order_contract_e2e.sh
#
# 验什么（卡面验收三条）：
#   ① 迁移 APPLY_OK：V0302 落 schema_migrations（applied_via=executed）+ 列/外键/索引都在
#   ② 合同金额超限被拒：累计订单额 > 合同金额 → 409 + 66019；用满（=）必须放行
#   ③ 前端能选到销售合同：页面用的那条查询（contract_type=SALES&status=ACTIVE）能查到
# 另加：挂非 SALES / 非可用状态合同 → 400 66014；合同乙方 ≠ 订单客户 → 400 66044；
#       改单换合同 / 解绑合同 / 详情带出合同号 / 列表带出 contract_id。
#
# 夹具说明（诚实声明）：合同的 DRAFT → PENDING_APPROVAL → SIGNED → ACTIVE 走
# `contract_approve` 审批流，那是 SO-26 与合同域自己的用例覆盖的路径。本卡验的是
# **订单↔合同的联动**，所以合同夹具直接用 SQL 把状态置成 ACTIVE（脚本里 `fixture_*`
# 函数，每一步都打印出来），不把审批流配置的成败耦合进本卡结论。
#
# 证据：docs/evidence/20261107-so27-sales-order-contract/raw/*（响应体逐份落盘，JWT 已脱敏）
# =============================================================================
set -uo pipefail
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$DIR/../.." && pwd)
PORT=${SO27_PORT:-18227}
BASE="http://127.0.0.1:$PORT/api"
DB=${SO27_DB:-cloudpivot_so27}
EVID="$ROOT/docs/evidence/20261107-so27-sales-order-contract"
RAW="$EVID/raw"
mkdir -p "$RAW"
export PGPASSWORD=alun
PSQLQ() { psql -h 127.0.0.1 -U alun -d "$DB" -tAF'|' "$@"; }

PASS=0; FAIL=0; FAILED=()
ok()  { PASS=$((PASS+1)); printf '  ✅ %s\n' "$*"; }
bad() { FAIL=$((FAIL+1)); FAILED+=("$*"); printf '  ❌ %s\n' "$*"; }
check() { if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }

# api <名> <METHOD> <path> [token] [json]
api() {
  local name=$1 method=$2 path=$3 token=${4:-} body=${5:-}
  local out="$RAW/$name.json"
  local -a args=(-s -m 30 -o "$out" -w '%{http_code}' -X "$method" "$BASE$path")
  # 用 --oauth2-bearer 而不是手写 Authorization 头：本仓的密钥脱敏会把
  # 「Bearer + 变量」这种字面串改写成 Bearer ***（so24_e2e.sh 就是这么被改坏的）
  [ -n "$token" ] && args+=(--oauth2-bearer "$token")
  [ -n "$body" ] && args+=(-H 'Content-Type: application/json' -d "$body")
  HTTP=$(curl "${args[@]}")
  BODY=$(cat "$out" 2>/dev/null || true)
  CODE=$(printf '%s' "$BODY" | jq -r '.code // empty' 2>/dev/null)
  # JWT 脱敏：先取出 token 再抹盘（顺序反了会把 <redacted> 当 token 用）
  if printf '%s' "$BODY" | jq -e '.data.access_token? // .data.refresh_token?' >/dev/null 2>&1; then
    jq '((.data.access_token? // "") |= (if . == "" then . else "<redacted>" end))
        | ((.data.refresh_token? // "") |= (if . == "" then . else "<redacted>" end))' \
       "$out" > "$out.tmp" 2>/dev/null && mv "$out.tmp" "$out"
  fi
}
d() { printf '%s' "$BODY" | jq -r "$1" 2>/dev/null; }

try_login() { # try_login <user> <pass> [tag] → token（失败回空，不打 PASS/FAIL）
  local code tok out="$RAW/login_${3:-try}.json"
  code=$(curl -s -m 20 -o "$out" -w '%{http_code}' -X POST "$BASE/system/auth/login" \
           -H 'Content-Type: application/json' \
           -d "{\"username\":\"$1\",\"password\":\"$2\"}")
  [ "$code" = "200" ] || { printf ''; return; }
  tok=$(jq -r '.data.access_token // empty' "$out")
  jq '((.data.access_token? // "") |= (if . == "" then . else "<redacted>" end))
      | ((.data.refresh_token? // "") |= (if . == "" then . else "<redacted>" end))' \
     "$out" > "$out.tmp" 2>/dev/null && mv "$out.tmp" "$out"
  MUST_CHANGE=$(jq -r '.data.must_change_password // false' "$out" 2>/dev/null)
  printf '%s' "$tok"
}

echo "=================================================================="
echo "SO-27 验收：库=$DB 实例=$BASE"
echo "=================================================================="

echo; echo "== 0) 实例 / 迁移记账（本卡一个表结构改动 ⇒ V0302 必须 APPLY_OK） =="
health=$(curl -s -o /dev/null -w '%{http_code}' -m 3 "http://127.0.0.1:$PORT/health")
check "/health" "$health" "200"

migs=$(PSQLQ -c "select count(*) from schema_migrations")
echo "  · 当前迁移条数=$migs（模板库 227 + 本卡 V0302 = 228；若邻居卡的已应用迁移 checksum 漂移，
   迁移器会把那一条**重放**并原地 update（applied_seq 递增），条数不会变多 —— 见下方 V0302 记账）"
if [ "$migs" -ge 228 ]; then ok "迁移条数 ≥ 228（实际 $migs）"; else bad "迁移条数 = $migs（期望 ≥ 228）"; fi

row302=$(PSQLQ -c "select version||'|'||name||'|'||kind||'|'||applied_via from schema_migrations where name='sales_order_contract' limit 1")
echo "  · V0302 记账：$row302"
case "$row302" in
  *sales_order_contract*"|versioned|executed"*) ok "V0302 kind=versioned applied_via=executed（卡面第 3 条）" ;;
  *) bad "V0302 记账不对：$row302" ;;
esac
n302=$(PSQLQ -c "select count(*) from schema_migrations where name='sales_order_contract'")
check "V0302 只记一行（幂等）" "$n302" "1"
seq302=$(PSQLQ -c "select applied_seq from schema_migrations where name='sales_order_contract'")
maxseq=$(PSQLQ -c "select max(applied_seq) from schema_migrations")
echo "  · V0302 applied_seq=$seq302（当前最大 $maxseq，说明它是本次启动最后应用的一条）"
badvia=$(PSQLQ -c "select count(*) from schema_migrations where applied_via not in ('executed','imported')")
check "applied_via 非 executed/imported 的迁移数" "$badvia" "0"

col=$(PSQLQ -c "select count(*) from information_schema.columns where table_name='biz_sales_order' and column_name='contract_id'")
check "biz_sales_order.contract_id 列存在" "$col" "1"
fk=$(PSQLQ -c "select count(*) from pg_constraint where conname='fk_biz_sales_order_contract' and contype='f'")
check "外键 fk_biz_sales_order_contract" "$fk" "1"
idx=$(PSQLQ -c "select count(*) from pg_indexes where tablename='biz_sales_order' and indexname='idx_biz_sales_order_contract'")
check "部分索引 idx_biz_sales_order_contract" "$idx" "1"
# 外键的删除动作必须是 SET NULL（不是 CASCADE：合同没了不等于订单没发生过）
fkdef=$(PSQLQ -c "select confdeltype from pg_constraint where conname='fk_biz_sales_order_contract'")
check "外键删除动作（n = SET NULL）" "$fkdef" "n"

echo; echo "== 1) admin 登录（模板库口令自 SO-25 继承，逐候选试） =="
T_ADMIN=$(try_login admin 'Admin@So25!2026' admin_so25); FIRST_PWD='Admin@So25!2026'
[ -z "$T_ADMIN" ] && { T_ADMIN=$(try_login admin 'Admin@123456' admin_def); FIRST_PWD='Admin@123456'; }
[ -z "$T_ADMIN" ] && { T_ADMIN=$(try_login admin 'Admin@So24!2026' admin_so24); FIRST_PWD='Admin@So24!2026'; }
[ -z "$T_ADMIN" ] && { echo "FATAL: admin 口令未知，无法继续"; exit 1; }
ok "admin 登录成功（口令 = $FIRST_PWD）"
if [ "${MUST_CHANGE:-false}" = "true" ]; then
  api pre_pwd_change PUT /system/auth/password "$T_ADMIN" \
      "{\"old_password\":\"$FIRST_PWD\",\"new_password\":\"Admin@So27!2026\"}"
  [ "$CODE" = "0" ] && ok "前置：首登改密 → Admin@So27!2026" || bad "前置改密失败 http=$HTTP"
  T_ADMIN=$(try_login admin 'Admin@So27!2026' admin_so27); FIRST_PWD='Admin@So27!2026'
  [ -z "$T_ADMIN" ] && { echo "FATAL: 改密后登录失败"; exit 1; }
fi

echo; echo "== 2) 夹具：两个 ACTIVE 客户 + 销售合同（SALES/ACTIVE）+ 一张 DRAFT 销售合同 + 一张采购合同 =="
STAMP=$(date +%y%m%d%H%M%S)
api cus_a POST /business/erp/customer "$T_ADMIN" \
    "{\"code\":\"SO27-CUSA-$STAMP\",\"name\":\"SO-27 合同客户甲\",\"credit_days\":30,\"remark\":\"SO-27 夹具\"}"
check "建客户甲 HTTP" "$HTTP" "200"
CUS_A=$(d '.data.id'); CUS_A_NAME=$(d '.data.name')

api cus_b POST /business/erp/customer "$T_ADMIN" \
    "{\"code\":\"SO27-CUSB-$STAMP\",\"name\":\"SO-27 合同客户乙\",\"credit_days\":30}"
check "建客户乙 HTTP" "$HTTP" "200"
CUS_B=$(d '.data.id')
[ -z "$CUS_A" ] || [ -z "$CUS_B" ] && { echo "FATAL: 客户 id 拿不到"; exit 1; }

# 销售合同（乙方 = 客户甲，合同额 1000）
api ct_sales POST /business/erp/contract "$T_ADMIN" \
    "{\"name\":\"SO-27 销售合同（额度 1000）\",\"contract_type\":\"SALES\",\"party_b_id\":\"$CUS_A\",
      \"amount\":\"1000.00\",\"sign_date\":\"2026-01-01\",\"start_date\":\"2026-01-02\",
      \"end_date\":\"2026-12-31\",\"remark\":\"SO-27 夹具\"}"
check "建 SALES 合同 HTTP" "$HTTP" "200"
CT_SALES=$(d '.data.id'); CT_NO=$(d '.data.contract_no')
check "合同类型" "$(d '.data.contract_type')" "SALES"
check "合同乙方 = 客户甲" "$(d '.data.party_b_id')" "$CUS_A"

# 夹具：把合同置 ACTIVE（见文件头「夹具说明」）
PSQLQ -c "update biz_contract set status='ACTIVE' where id='$CT_SALES'" >/dev/null
check "夹具：SALES 合同状态" "$(PSQLQ -c "select status from biz_contract where id='$CT_SALES'")" "ACTIVE"

# 一张仍是 DRAFT 的销售合同（验状态闸）
api ct_draft POST /business/erp/contract "$T_ADMIN" \
    "{\"name\":\"SO-27 销售合同（草稿）\",\"contract_type\":\"SALES\",\"party_b_id\":\"$CUS_A\",
      \"amount\":\"5000.00\",\"sign_date\":\"2026-01-01\",\"start_date\":\"2026-01-02\",\"end_date\":\"2026-12-31\"}"
check "建草稿 SALES 合同 HTTP" "$HTTP" "200"
CT_DRAFT=$(d '.data.id')

# 一张采购合同（验类型闸）：乙方用库里的启用供应商
SUP_ID=$(PSQLQ -c "select id from biz_supplier where deleted_at is null and status=1 order by created_at limit 1")
api ct_purchase POST /business/erp/contract "$T_ADMIN" \
    "{\"name\":\"SO-27 采购回归合同\",\"contract_type\":\"PURCHASE\",\"party_b_id\":\"$SUP_ID\",
      \"amount\":\"5000.00\",\"sign_date\":\"2026-01-01\",\"start_date\":\"2026-01-02\",\"end_date\":\"2026-06-30\"}"
check "建 PURCHASE 合同 HTTP" "$HTTP" "200"
CT_PURCHASE=$(d '.data.id')
[ -z "$CT_SALES" ] && { echo "FATAL: 合同 id 拿不到"; exit 1; }

MAT_ID=$(PSQLQ -c "select id from biz_material where deleted_at is null and status=1 order by created_at limit 1")
echo "  · 夹具物料=$MAT_ID 供应商=$SUP_ID"

# 订单行：数量 qty × 单价 100、税率 0 ⇒ 价税合计 = 100 × qty（算额度时一眼能对）
order_payload() { # order_payload <customer> <qty> [contract]
  local customer=$1 qty=$2 contract=${3:-}
  local ct_part=''
  [ -n "$contract" ] && ct_part=",\"contract_id\":\"$contract\""
  printf '{"customer_id":"%s"%s,"order_date":"2026-11-07","remark":"SO-27 验收","items":[{"material_id":"%s","qty":"%s","price":"100","tax_rate":"0"}]}' \
    "$customer" "$ct_part" "$MAT_ID" "$qty"
}

echo; echo "== 3) ① 建单挂合同：落库 + 详情带合同号 + 列表带 contract_id =="
api so_with_contract POST /business/erp/sales-order "$T_ADMIN" "$(order_payload "$CUS_A" 5 "$CT_SALES")"
check "POST 销售订单（挂合同，额 500/1000）HTTP" "$HTTP" "200"
SO1=$(d '.data.id')
check "订单返回的 contract_id" "$(d '.data.contract_id')" "$CT_SALES"
check "详情带出合同号（contract_no）" "$(d '.data.contract_no')" "$CT_NO"
check "价税合计" "$(d '.data.total_with_tax')" "500.0000"
check "库内 contract_id 落库" "$(PSQLQ -c "select contract_id from biz_sales_order where id='$SO1'")" "$CT_SALES"

api so_page GET "/business/erp/sales-order/page?customer_id=$CUS_A&page_size=50" "$T_ADMIN"
check "GET 分页 HTTP" "$HTTP" "200"
check "列表行的 contract_id（列表不填 contract_no，只给 id）" \
      "$(printf '%s' "$BODY" | jq -r --arg id "$SO1" '[.data.list[]? | select(.id == $id)][0].contract_id')" "$CT_SALES"
check "列表行 contract_no 恒 null（口径：只在详情补）" \
      "$(printf '%s' "$BODY" | jq -r --arg id "$SO1" '[.data.list[]? | select(.id == $id)][0].contract_no')" "null"

echo; echo "== 4) ② 额度守卫：用满放行 / 超 0.01 就 409 + 66019 =="
api so_fill POST /business/erp/sales-order "$T_ADMIN" "$(order_payload "$CUS_A" 5 "$CT_SALES")"
check "第二单 500（累计正好 1000）HTTP" "$HTTP" "200"
SO2=$(d '.data.id')
check "累计口径（库内 SUM）" \
      "$(PSQLQ -c "select sum(total_with_tax)::text from biz_sales_order where contract_id='$CT_SALES' and status<>'CANCELLED' and deleted_at is null")" \
      "1000.0000"

api so_over POST /business/erp/sales-order "$T_ADMIN" "$(order_payload "$CUS_A" 1 "$CT_SALES")"
check "第三单 100（累计 1100 > 1000）HTTP" "$HTTP" "409"
check "超限业务码（66019 ERP_CONTRACT_AMOUNT_EXCEEDED）" "$CODE" "66019"
over_msg=$(d '.msg')
case "$over_msg" in
  *"1000"*"1000"*"100"*) ok "超限报文点名合同额 / 已下单 / 本单：$over_msg" ;;
  *) bad "超限报文缺数字：$over_msg" ;;
esac
left=$(PSQLQ -c "select count(*) from biz_sales_order where customer_id='$CUS_A' and remark='SO-27 验收'")
check "被拒的建单没有落库（同客户订单数 2）" "$left" "2"

echo; echo "== 5) 合同闸：类型 / 状态 / 乙方（400 66014 / 66044）、不存在合同（404） =="
api so_purchase POST /business/erp/sales-order "$T_ADMIN" "$(order_payload "$CUS_A" 1 "$CT_PURCHASE")"
check "挂 PURCHASE 合同 → HTTP" "$HTTP" "400"
check "挂 PURCHASE 合同 → 业务码（66014）" "$CODE" "66014"
case "$(d '.msg')" in *"SALES"*) ok "文案点名只接受 SALES" ;; *) bad "文案：$(d '.msg')" ;; esac

api so_draft_ct POST /business/erp/sales-order "$T_ADMIN" "$(order_payload "$CUS_A" 1 "$CT_DRAFT")"
check "挂 DRAFT 合同 → HTTP" "$HTTP" "400"
check "挂 DRAFT 合同 → 业务码（66014）" "$CODE" "66014"
case "$(d '.msg')" in *"DRAFT"*) ok "文案点名当前状态 DRAFT" ;; *) bad "文案：$(d '.msg')" ;; esac

api so_mismatch POST /business/erp/sales-order "$T_ADMIN" "$(order_payload "$CUS_B" 1 "$CT_SALES")"
check "合同乙方 ≠ 订单客户 → HTTP" "$HTTP" "400"
check "合同乙方 ≠ 订单客户 → 业务码（66044）" "$CODE" "66044"
case "$(d '.msg')" in *"乙方"*) ok "文案点名乙方不一致" ;; *) bad "文案：$(d '.msg')" ;; esac

GHOST=$(psql -h 127.0.0.1 -U alun -d "$DB" -tAc "select gen_random_uuid()")
api so_ghost POST /business/erp/sales-order "$T_ADMIN" "$(order_payload "$CUS_A" 1 "$GHOST")"
check "合同不存在 → HTTP" "$HTTP" "404"

echo; echo "== 6) 改单：换合同额度重算（排除本单） + 空串解绑 =="
# 把第一单（500）改成 200：排除自己后 used=500（第二单），500+200=700 ≤ 1000 → 过
body=$(printf '{"customer_id":"%s","contract_id":"%s","order_date":"2026-11-07","remark":"SO-27 验收-改单","items":[{"material_id":"%s","qty":"2","price":"100","tax_rate":"0"}]}' "$CUS_A" "$CT_SALES" "$MAT_ID")
api so_update PUT "/business/erp/sales-order/$SO1" "$T_ADMIN" "$body"
check "PUT 改单（500 → 200，排除本单后 700）HTTP" "$HTTP" "200"
check "改单后价税合计" "$(d '.data.total_with_tax')" "200.0000"
check "改单后仍挂同一合同" "$(d '.data.contract_id')" "$CT_SALES"

# 再挂一次超限：改成 600（排除自己后 used=500，500+600=1100 > 1000）→ 409
body=$(printf '{"customer_id":"%s","contract_id":"%s","order_date":"2026-11-07","items":[{"material_id":"%s","qty":"6","price":"100","tax_rate":"0"}]}' "$CUS_A" "$CT_SALES" "$MAT_ID")
api so_update_over PUT "/business/erp/sales-order/$SO1" "$T_ADMIN" "$body"
check "PUT 改单超限（500 + 600 > 1000）HTTP" "$HTTP" "409"
check "PUT 改单超限业务码" "$CODE" "66019"
check "被拒的改单没有落库（金额仍是 200）" \
      "$(PSQLQ -c "select total_with_tax::text from biz_sales_order where id='$SO1'")" "200.0000"

# 解绑：contract_id 传空串 ⇒ null（合同额度不再占用）
api so_unbind PUT "/business/erp/sales-order/$SO1" "$T_ADMIN" \
    "$(printf '{"contract_id":"","order_date":"2026-11-07"}' )"
check "PUT 解绑合同（contract_id=\"\"）HTTP" "$HTTP" "200"
check "解绑后 contract_id" "$(d '.data.contract_id')" "null"
check "解绑后 contract_no" "$(d '.data.contract_no')" "null"
check "库内已解绑" "$(PSQLQ -c "select coalesce(contract_id::text,'NULL') from biz_sales_order where id='$SO1'")" "NULL"
api so_after_unbind POST /business/erp/sales-order "$T_ADMIN" "$(order_payload "$CUS_A" 5 "$CT_SALES")"
check "解绑后额度释放（再下 500 应放行）HTTP" "$HTTP" "200"
SO3=$(d '.data.id')

echo; echo "== 7) ③ 前端口径：页面那条查询能选到这张销售合同 =="
# 与 sales-order/index.vue 的 loadOptions() 逐字一致
api front_contracts GET "/business/erp/contract/page?contract_type=SALES&status=ACTIVE&page=1&page_size=200" "$T_ADMIN"
check "GET 合同分页（contract_type=SALES&status=ACTIVE）HTTP" "$HTTP" "200"
found=$(printf '%s' "$BODY" | jq -r --arg id "$CT_SALES" '[.data.list[]? | select(.id == $id)] | length')
check "下拉里能选到刚成交的销售合同" "$found" "1"
check "下拉项的合同号（label 用）" \
      "$(printf '%s' "$BODY" | jq -r --arg id "$CT_SALES" '[.data.list[]? | select(.id == $id)][0].contract_no')" "$CT_NO"
check "下拉项的乙方（选合同自动带客户用）" \
      "$(printf '%s' "$BODY" | jq -r --arg id "$CT_SALES" '[.data.list[]? | select(.id == $id)][0].party_b_id')" "$CUS_A"
# DRAFT 合同不该出现在这个下拉里
draft_found=$(printf '%s' "$BODY" | jq -r --arg id "$CT_DRAFT" '[.data.list[]? | select(.id == $id)] | length')
check "DRAFT 合同不在 ACTIVE 下拉里" "$draft_found" "0"

echo; echo "== 8) 夹具回读（库内证据） =="
PSQLQ -c "select so_no||' | '||coalesce(contract_id::text,'(未挂)')||' | '||total_with_tax::text||' | '||status
            from biz_sales_order where customer_id='$CUS_A' and deleted_at is null order by created_at" \
  | tee "$RAW/db_sales_orders.txt"
PSQLQ -c "select contract_no||' | '||contract_type||' | '||status||' | '||amount::text from biz_contract where id in ('$CT_SALES','$CT_DRAFT','$CT_PURCHASE') order by created_at" \
  | tee "$RAW/db_contracts.txt"

echo
echo "=================================================================="
echo "PASS=$PASS FAIL=$FAIL"
if [ "$FAIL" -gt 0 ]; then printf '失败项：\n'; printf '  - %s\n' "${FAILED[@]}"; echo "结果：FAIL"; exit 1; fi
echo "结果：PASS"
