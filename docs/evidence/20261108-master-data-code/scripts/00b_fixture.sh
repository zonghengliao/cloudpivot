#!/bin/bash
# CODE-09 证据库夹具准备（隔离实例 127.0.0.1:8049 / 库 cloudpivot_code09）
#
# 两件事：
#   A) **首号跳过种子编码**（真实老库场景，不改一行代码就能证明）：
#      空库首启的种子数据里已经有 `SUP-0001`（V0137）和 `CUS-0001..0003`，
#      取号器 `next_master_code_free` 的口径是「撞了就顺延」，所以首号会是
#      SUP-0002 / CUS-0004 —— 这里先实测并留证（seq.rs 文档注释点名了这条）。
#   B) 为了让卡面矩阵里「编码传空 → 自动生成 PREFIX-0001」这条断言能**逐字实测**，
#      把 6 张主数据表的种子演示行**软删**（deleted_at=now()，与业务删除同一口径，
#      唯一索引是 `WHERE deleted_at IS NULL` 的部分索引，所以编码可复用），
#      并把 biz_seq 六行 current_no 复位成 0。
#      ⚠️ 仅限本证据库的一次性夹具：不动迁移、不动代码、不碰 144/145。
#
# 用法：bash 00b_fixture.sh
set -uo pipefail

BASE=${BASE:-http://127.0.0.1:8049}
ADMIN_USER=${ADMIN_USER:-admin}
ADMIN_PASS=${ADMIN_PASS:-Admin@123456}
ADMIN_PASS2=${ADMIN_PASS2:-Admin@123456x}
export PGPASSWORD=alun
PSQL="psql -h 127.0.0.1 -U alun -d cloudpivot_code09 -tA -q"
HERE=$(cd "$(dirname "$0")" && pwd)
RAW="$HERE/../raw"
mkdir -p "$RAW"
OUT="$RAW/00b_fixture.txt"
: > "$OUT"
say() { echo "$@" | tee -a "$OUT"; }

login() {
  curl -s -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"$ADMIN_USER\",\"password\":\"$1\"}" > "$RAW/00b_login.json"
  jq -r '.data.access_token // empty' "$RAW/00b_login.json"
}

TOKEN=$(login "$ADMIN_PASS"); [ -z "$TOKEN" ] && TOKEN=$(login "$ADMIN_PASS2")
if [ -z "$TOKEN" ]; then say "FATAL: 登录失败"; cat "$RAW/00b_login.json" | tee -a "$OUT"; exit 1; fi

AUTH_KEY="Authorization"; AUTH_SCHEME="Bearer"; AUTH_VAL="$AUTH_SCHEME $TOKEN"; AUTH="$AUTH_KEY: $AUTH_VAL"

PROBE=$(curl -s -o "$RAW/00b_probe.json" -w '%{http_code}' -H "$AUTH" "$BASE/api/business/erp/master-data/code-policy")
if [ "$PROBE" = "403" ] && grep -q "首次登录必须先修改初始密码" "$RAW/00b_probe.json"; then
  HTTP=$(curl -s -o "$RAW/00b_change_password.json" -w '%{http_code}' -X PUT "$BASE/api/system/auth/password" \
    -H "$AUTH" -H 'Content-Type: application/json' \
    -d "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$ADMIN_PASS2\"}")
  say "首登改密（初始口令 → 新口令）→ HTTP $HTTP"
  [ "$HTTP" != "200" ] && { cat "$RAW/00b_change_password.json" | tee -a "$OUT"; exit 1; }
  sleep 65     # 会话状态缓存 60s（改密后旧 token 仍被缓存）
  TOKEN=$(login "$ADMIN_PASS2")
  AUTH_VAL="$AUTH_SCHEME $TOKEN"; AUTH="$AUTH_KEY: $AUTH_VAL"
fi
say "token 就绪（长度 ${#TOKEN}）"

MODE=${1:-full}
if [ "$MODE" = "reset-only" ]; then
  OUT="$RAW/00b_fixture_reset.txt"; : > "$OUT"
fi

# ---------------- A) 种子占号 → 取号自动顺延 ----------------
if [ "$MODE" != "reset-only" ]; then
say ""
say "=== A) 种子数据占号实测（不改代码，撞号即顺延）==="
say "种子里的编码（未删行）：$( ($PSQL -c "select 'SUP:'||supplier_code from biz_supplier where deleted_at is null union all select 'CUS:'||code from biz_customer where deleted_at is null union all select 'MAT:'||material_code from biz_material where deleted_at is null union all select 'WH:'||warehouse_code from biz_warehouse where deleted_at is null union all select 'AC:'||category_code from biz_asset_category where deleted_at is null union all select 'ET:'||type_code from biz_expense_type where deleted_at is null") | paste -sd' ')"
H=$(curl -s -o "$RAW/00b_seed_sup.json" -w '%{http_code}' -X POST "$BASE/api/business/erp/supplier" \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"supplier_name":"CODE09 种子占号探针","credit_days":30,"status":1}')
SEED_SUP=$(jq -r '.data.supplier_code // empty' "$RAW/00b_seed_sup.json")
H2=$(curl -s -o "$RAW/00b_seed_cus.json" -w '%{http_code}' -X POST "$BASE/api/business/erp/customer" \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"name":"CODE09 种子占号探针","status":"ACTIVE"}')
SEED_CUS=$(jq -r '.data.code // empty' "$RAW/00b_seed_cus.json")
if [ "$H" = "200" ] && [ "$SEED_SUP" = "SUP-0002" ]; then say "  PASS  种子 SUP-0001 已被占 → 首号顺延到 SUP-0002"; else say "  FAIL  供应商首号 = [$SEED_SUP] HTTP $H（期望 SUP-0002）"; fi
if [ "$H2" = "200" ] && [ "$SEED_CUS" = "CUS-0004" ]; then say "  PASS  种子 CUS-0001..0003 已被占 → 首号顺延到 CUS-0004"; else say "  FAIL  客户首号 = [$SEED_CUS] HTTP $H2（期望 CUS-0004）"; fi

fi

# ---------------- B) 夹具复位：软删种子/探针行 + 号段归零 ----------------
say ""
say "=== B) 证据库夹具复位（软删演示行 + biz_seq 归零，仅本证据库）==="
say "复位前（未删行数 / 号段）："
say "  主数据未删行：$($PSQL -c "select (select count(*) from biz_supplier where deleted_at is null)||'/'||(select count(*) from biz_customer where deleted_at is null)||'/'||(select count(*) from biz_material where deleted_at is null)||'/'||(select count(*) from biz_warehouse where deleted_at is null)||'/'||(select count(*) from biz_asset_category where deleted_at is null)||'/'||(select count(*) from biz_expense_type where deleted_at is null)")"
say "  biz_seq：$($PSQL -c "select string_agg(seq_type||'='||current_no, ' ' order by seq_type) from biz_seq where seq_type in ('SUP','CUS','MAT','WH','AC','ET')")"

$PSQL -c "begin;
 update biz_supplier      set deleted_at = now() where deleted_at is null;
 update biz_customer      set deleted_at = now() where deleted_at is null;
 update biz_material      set deleted_at = now() where deleted_at is null;
 update biz_warehouse     set deleted_at = now() where deleted_at is null;
 update biz_asset_category set deleted_at = now() where deleted_at is null;
 update biz_expense_type  set deleted_at = now() where deleted_at is null;
 update biz_unit set deleted_at = now() where deleted_at is null and unit_code like 'KG9%';
 update biz_seq set current_no = 0 where seq_type in ('SUP','CUS','MAT','WH','AC','ET');
commit;" > /dev/null

say "复位后："
say "  主数据未删行：$($PSQL -c "select (select count(*) from biz_supplier where deleted_at is null)||'/'||(select count(*) from biz_customer where deleted_at is null)||'/'||(select count(*) from biz_material where deleted_at is null)||'/'||(select count(*) from biz_warehouse where deleted_at is null)||'/'||(select count(*) from biz_asset_category where deleted_at is null)||'/'||(select count(*) from biz_expense_type where deleted_at is null)")（期望 0/0/0/0/0/0）"
say "  biz_seq：$($PSQL -c "select string_agg(seq_type||'='||current_no, ' ' order by seq_type) from biz_seq where seq_type in ('SUP','CUS','MAT','WH','AC','ET')")（期望全 0）"
say "  软删行数（审计留痕保留）：$($PSQL -c "select (select count(*) from biz_supplier)||'/'||(select count(*) from biz_customer)||'/'||(select count(*) from biz_material)||'/'||(select count(*) from biz_warehouse)||'/'||(select count(*) from biz_asset_category)||'/'||(select count(*) from biz_expense_type)")"
