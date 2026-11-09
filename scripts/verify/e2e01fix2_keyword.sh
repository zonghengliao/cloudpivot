#!/usr/bin/env bash
# =============================================================================
# e2e01fix2_keyword.sh —— E2E01-FIX-2「采购明细列表关键字搜索」145 真机验收
#
# 在 **145**（cloudpivot-test · :8033 · 库 cloudpivot_dev）上跑：
#   scp scripts/verify/e2e01fix2_keyword.sh root@172.18.10.145:/tmp/ && \
#   ssh root@172.18.10.145 'bash /tmp/e2e01fix2_keyword.sh'
#
# 验的六条（卡面验收 ①–⑤ + SQL 对账）：
#   ① keyword=<单号>       → 只返回该单的明细（行全部属于该单，总数 = SQL 真值）
#   ② keyword=<物料编码>   → 按编码命中（行全部是该物料）
#   ③ keyword=<物料名称>   → 按名称命中（中文，行全部是该物料）
#   ④ keyword=<小写编码>   → 大小写不一致也能命中（ILIKE，不是 LIKE）
#   ⑤ keyword 空 / 全空白  → 与不传完全一致（全量），不是 0 行
#   ⑥ keyword + 另一个 order_id → 0 行（两者是 AND）
#
# 所有「期望值」都从库里现算（psql），不写死数字 —— 数据一变脚本仍然有效。
# 报文落 /tmp/e2e01fix2/raw/*.json，结论落 /tmp/e2e01fix2/summary.txt。
# 退出码：0 = 全绿，1 = 有失败项。
# =============================================================================
set -uo pipefail

ENV=/etc/cloudpivot/cloudpivot-dev.env
set -a; . "$ENV"; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"

BASE=${BASE:-http://127.0.0.1:8033/api}
WORK=${WORK:-/tmp/e2e01fix2}
RAW=$WORK/raw
mkdir -p "$RAW"
SUMMARY=$WORK/summary.txt
: > "$SUMMARY"

PASS=0; FAIL=0
say()  { printf '%s\n' "$*" | tee -a "$SUMMARY"; }
ok()   { PASS=$((PASS+1)); say "  PASS  $*"; }
bad()  { FAIL=$((FAIL+1)); say "  FAIL  $*"; }
check(){ if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }

PSQLQ() { psql -h "$NIK_DATABASE_HOST" -p "$NIK_DATABASE_PORT" -U "$NIK_DATABASE_USER" \
               -d "$NIK_DATABASE_NAME" -At -F'|' -P pager=off "$@"; }

# 与接口**同一谓词**的 SQL 真值（三列任一 ILIKE '%kw%'）。
# 用同一谓词而不是「按 id 精确 count」：接口是子串匹配，写死 id 口径会漏掉
# 「更长的单号包含本单号」这类边界，脚本与接口就会各说各话。
sql_truth_total() { # <raw keyword>
    PSQLQ -c "SELECT count(*) FROM biz_purchase_order_item i
              LEFT JOIN biz_purchase_order o ON o.id = i.order_id
              LEFT JOIN biz_material m ON m.id = i.material_id
              WHERE o.order_no ILIKE '%$1%' OR m.material_code ILIKE '%$1%'
                 OR m.material_name ILIKE '%$1%'"
}
sql_truth_distinct() { # <raw keyword> <order_id|material_id>
    PSQLQ -c "SELECT count(DISTINCT i.$2) FROM biz_purchase_order_item i
              LEFT JOIN biz_purchase_order o ON o.id = i.order_id
              LEFT JOIN biz_material m ON m.id = i.material_id
              WHERE o.order_no ILIKE '%$1%' OR m.material_code ILIKE '%$1%'
                 OR m.material_name ILIKE '%$1%'"
}

TOKEN=""
HTTP=""; BODY=""

# api <名字> <路径（含 query，调用方自己 urlencode）>
api() {
    local name=$1 path=$2 out="$RAW/$1.json"
    HTTP=$(curl -s -m 60 -o "$out" -w '%{http_code}' "$BASE$path" \
                ${TOKEN:+--oauth2-bearer "$TOKEN"})
    BODY=$(cat "$out" 2>/dev/null)
    if ! printf '%s' "$BODY" | jq -e . >/dev/null 2>&1; then
        say "  ⚠ 非 JSON（$name）http=$HTTP：$(printf '%s' "$BODY" | head -c 200)"
    fi
}
jqv()  { printf '%s' "$BODY" | jq -r "$1" 2>/dev/null; }
items(){ printf '%s' "$BODY" | jq -r '[.data.list[]?] | length' 2>/dev/null; }
total(){ printf '%s' "$BODY" | jq -r '.data.total // empty' 2>/dev/null; }
# uniq_of <jq path> → 该字段在当前页里出现过几种取值
uniq_of(){ printf '%s' "$BODY" | jq -r "[.data.list[]? | $1] | unique | length" 2>/dev/null; }
href() { python3 -c 'import sys,urllib.parse;print(urllib.parse.quote(sys.argv[1]))' "$1"; }

say "== E2E01-FIX-2 采购明细 keyword 搜索 · 145 真机（$(date '+%F %T')）"
say "   BASE=$BASE   DB=$NIK_DATABASE_NAME   HEAD=$(git -C /opt/cloudpivot log --oneline -1 2>/dev/null || echo n/a)"

# ---------- 登录 ----------
login() {
    HTTP=$(curl -s -m 30 -o "$RAW/login.json" -w '%{http_code}' -X POST "$BASE/system/auth/login" \
                -H 'Content-Type: application/json' \
                -d "{\"username\":\"admin\",\"password\":\"$NIK_ADMIN_PASSWORD\"}")
    BODY=$(cat "$RAW/login.json")
    TOKEN=$(jqv '.data.access_token')
    if [ -z "$TOKEN" ] || [ "$TOKEN" = "null" ]; then
        say "FATAL: 登录失败 http=$HTTP $(printf '%s' "$BODY" | head -c 300)"; exit 1
    fi
    say "   登录成功（admin · http=$HTTP）"
}
login

# ---------- 基准：库里总量 ----------
DB_ITEMS=$(PSQLQ -c "SELECT count(*) FROM biz_purchase_order_item")
say
say "-- ⑤ 无 keyword = 全量（与库里 $DB_ITEMS 行一致）"
api kw_none "/business/erp/purchase-order-item/page?page=1&page_size=200"
check "http(无 keyword)" "$HTTP" "200"
check "total(无 keyword)" "$(total)" "$DB_ITEMS"

api kw_blank "/business/erp/purchase-order-item/page?page=1&page_size=200&keyword="
check "total(keyword=空串)" "$(total)" "$DB_ITEMS"

api kw_spaces "/business/erp/purchase-order-item/page?page=1&page_size=200&keyword=%20%20%20"
check "total(keyword=3 空格)" "$(total)" "$DB_ITEMS"

# ---------- ① 按单号 ----------
ORDER_NO=$(PSQLQ -c "SELECT o.order_no FROM biz_purchase_order_item i \
                     JOIN biz_purchase_order o ON o.id = i.order_id \
                     GROUP BY 1 ORDER BY count(*) DESC, 1 LIMIT 1")
ORDER_ID=$(PSQLQ -c "SELECT o.id FROM biz_purchase_order o WHERE o.order_no = '$ORDER_NO'")
ORDER_CNT=$(PSQLQ -c "SELECT count(*) FROM biz_purchase_order_item WHERE order_id = '$ORDER_ID'")
EXP_TOTAL=$(sql_truth_total "$ORDER_NO")
EXP_DISTINCT=$(sql_truth_distinct "$ORDER_NO" order_id)
say
say "-- ① keyword=$ORDER_NO（该单 $ORDER_CNT 行；ILIKE 真值 $EXP_TOTAL 行 / $EXP_DISTINCT 张单）"
api kw_order_no "/business/erp/purchase-order-item/page?page=1&page_size=200&keyword=$(href "$ORDER_NO")"
check "http(单号)" "$HTTP" "200"
check "total(单号) = SQL 真值" "$(total)" "$EXP_TOTAL"
check "命中行的 order_id 种类数" "$(uniq_of '.order_id')" "$EXP_DISTINCT"
if [ "$EXP_DISTINCT" = "1" ]; then
    check "命中行的 order_id" "$(printf '%s' "$BODY" | jq -r '.data.list[0].order_id')" "$ORDER_ID"
fi

# ---------- ② 按物料编码 ----------
MAT_CODE=$(PSQLQ -c "SELECT m.material_code FROM biz_purchase_order_item i \
                     JOIN biz_material m ON m.id = i.material_id \
                     GROUP BY 1 ORDER BY count(*) DESC, 1 LIMIT 1")
MAT_ID=$(PSQLQ -c "SELECT id FROM biz_material WHERE material_code = '$MAT_CODE'")
MAT_CNT=$(PSQLQ -c "SELECT count(*) FROM biz_purchase_order_item WHERE material_id = '$MAT_ID'")
EXP_TOTAL=$(sql_truth_total "$MAT_CODE")
EXP_DISTINCT=$(sql_truth_distinct "$MAT_CODE" material_id)
say
say "-- ② keyword=$MAT_CODE（该物料 $MAT_CNT 行；ILIKE 真值 $EXP_TOTAL 行 / $EXP_DISTINCT 个物料）"
api kw_code "/business/erp/purchase-order-item/page?page=1&page_size=200&keyword=$(href "$MAT_CODE")"
CODE_TOTAL=$(total)
check "http(编码)" "$HTTP" "200"
check "total(编码) = SQL 真值" "$CODE_TOTAL" "$EXP_TOTAL"
check "命中行的 material_id 种类数" "$(uniq_of '.material_id')" "$EXP_DISTINCT"

# ---------- ③ 按物料名称 ----------
MAT_NAME=$(PSQLQ -c "SELECT m.material_name FROM biz_purchase_order_item i \
                     JOIN biz_material m ON m.id = i.material_id \
                     GROUP BY 1 ORDER BY count(*) DESC, 1 LIMIT 1")
NAME_CNT=$(PSQLQ -c "SELECT count(*) FROM biz_purchase_order_item i \
                     JOIN biz_material m ON m.id = i.material_id WHERE m.material_name = '$MAT_NAME'")
EXP_TOTAL=$(sql_truth_total "$MAT_NAME")
EXP_DISTINCT=$(sql_truth_distinct "$MAT_NAME" material_id)
say
say "-- ③ keyword=$MAT_NAME（该名称 $NAME_CNT 行；ILIKE 真值 $EXP_TOTAL 行 / $EXP_DISTINCT 个物料）"
api kw_name "/business/erp/purchase-order-item/page?page=1&page_size=200&keyword=$(href "$MAT_NAME")"
check "http(名称)" "$HTTP" "200"
check "total(名称) = SQL 真值" "$(total)" "$EXP_TOTAL"
check "命中行的 material_id 种类数" "$(uniq_of '.material_id')" "$EXP_DISTINCT"

# ---------- ④ 大小写混用 ----------
LOWER_CODE=$(printf '%s' "$MAT_CODE" | tr '[:upper:]' '[:lower:]')
say
say "-- ④ keyword=$LOWER_CODE（小写编码，ILIKE 必须同样命中）"
api kw_lower "/business/erp/purchase-order-item/page?page=1&page_size=200&keyword=$(href "$LOWER_CODE")"
check "http(小写)" "$HTTP" "200"
check "total(小写) 与 ② 一致" "$(total)" "$CODE_TOTAL"
if [ "$LOWER_CODE" != "$MAT_CODE" ]; then
    ok "用例有效：小写 != 原编码（$LOWER_CODE vs $MAT_CODE）"
else
    say "  ⚠ 用例失效：库里编码本来就是小写，这条验不出 ILIKE"
fi

# ---------- ⑥ keyword 与 order_id 是 AND ----------
say
say "-- ⑥ keyword=$ORDER_NO + order_id=（另一张单）→ 0 行"
OTHER_ID=$(PSQLQ -c "SELECT o.id FROM biz_purchase_order o \
                     WHERE o.id <> '$ORDER_ID' AND EXISTS (SELECT 1 FROM biz_purchase_order_item i WHERE i.order_id = o.id) \
                     ORDER BY o.order_no LIMIT 1")
api kw_and "/business/erp/purchase-order-item/page?page=1&page_size=200&keyword=$(href "$ORDER_NO")&order_id=$OTHER_ID"
check "http(AND)" "$HTTP" "200"
check "total(AND 冲突)" "$(total)" "0"
check "list 长度(AND 冲突)" "$(items)" "0"

# ---------- 反向：查不到的词 ----------
say
say "-- 反向：keyword=zz-no-such-material → 0 行（不是全量、不是 500）"
api kw_miss "/business/erp/purchase-order-item/page?page=1&page_size=200&keyword=zz-no-such-material-2026"
check "http(未命中)" "$HTTP" "200"
check "total(未命中)" "$(total)" "0"

# ---------- 原有筛选没被改坏 ----------
say
say "-- 回归：?order_id=<原单> 仍按原口径（总数 = 该单明细数）"
api kw_regress "/business/erp/purchase-order-item/page?page=1&page_size=200&order_id=$ORDER_ID"
check "http(order_id)" "$HTTP" "200"
check "total(order_id)" "$(total)" "$ORDER_CNT"

say
say "== PASS=$PASS FAIL=$FAIL"
say "   报文：$RAW/    汇总：$SUMMARY"
[ "$FAIL" = 0 ] || exit 1
exit 0
