#!/usr/bin/env bash
# ============================================================
# LNK-F12 验收脚本：在途调拨超期「可查询 + 可告警」
#
# 用法（后端已起、库已迁移到含 V0171）：
#   BASE=http://127.0.0.1:18099 \
#   F12_USER=admin F12_PASS='F12Admin@123456' \
#   DB_HOST=127.0.0.1 DB_PORT=5432 DB_USER=f12role DB_NAME=cloudpivot_f12iso DB_PASS=... \
#   bash scripts/verify/f12_transfer_overdue.sh
#
# 断言口径（对应卡面四条需求）：
#   ① 判定：`sys_config` 阈值（默认 7 天）+ `shipped_at` 基准；
#   ② 可查询：`GET /business/erp/transfer/page?in_transit_overdue=true` 捞得到压 30 天的单，
#              `=false` / 阈值外 / 刚发货的对照单都捞不到；矛盾组合 → 400 而不是空列表；
#   ③ 可见性：详情接口回 `in_transit_overdue` / `overdue_days`；
#   ④ 汇总：`GET /business/erp/transfer/overdue/summary` 的 total / threshold_days 与库一致；
#   ⑤ 收尾：本脚本造的调拨单按 `F12-` 单号前缀删净，跑完残留必须为 0。
#
# 依赖：bash / curl / psql / python3（只用标准库）
# ============================================================
set -u

BASE="${BASE:-http://127.0.0.1:18099}"
F12_USER="${F12_USER:-admin}"
F12_PASS="${F12_PASS:-F12Admin@123456}"
F12_NEWPASS="${F12_NEWPASS:-F12Rotated@123456}"
DB_HOST="${DB_HOST:-127.0.0.1}"
DB_PORT="${DB_PORT:-5432}"
DB_USER="${DB_USER:-f12role}"
DB_NAME="${DB_NAME:-cloudpivot_f12iso}"
DB_PASS="${DB_PASS:-}"
PASS=0
FAIL=0

psql_q() { PGPASSWORD="$DB_PASS" psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -tAF '~' -c "$1" 2>&1 | head -1; }
ok()    { PASS=$((PASS + 1)); echo "[PASS] $1"; }
bad()   { FAIL=$((FAIL + 1)); echo "[FAIL] $1"; }
check() { if [ "$2" = "$3" ]; then ok "$1（$2）"; else bad "$1（期望 [$3] 实际 [$2]）"; fi; }
check_contains() { case "$2" in *"$3"*) ok "$1" ;; *) bad "$1（未包含 [$3] 实际 [$2]）" ;; esac; }

# ---------- 0. 登录（处理首登强制改密） ----------
login_body() {
  curl -s -m 10 -X POST "$BASE/api/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"$F12_USER\",\"password\":\"$1\"}"
}
json_get() { python3 -c 'import sys,json
d=json.load(sys.stdin)
for k in sys.argv[1].split("."):
    d = (d or {}).get(k) if isinstance(d, dict) else None
print("" if d is None else d)' "$1"; }

TOKEN=""
BODY=""
for CAND in "$F12_PASS" "$F12_NEWPASS"; do
  BODY=$(login_body "$CAND")
  TOKEN=$(printf '%s' "$BODY" | json_get data.access_token)
  [ -n "$TOKEN" ] && { F12_PASS="$CAND"; break; }
done
if [ -z "$TOKEN" ]; then
  echo "[FAIL] 登录失败：${BODY:0:200}"
  exit 1
fi
ok "登录成功（$F12_USER）"
MUST=$(printf '%s' "$BODY" | json_get data.must_change_password)
if [ "$MUST" = "True" ] || [ "$MUST" = "true" ]; then
  curl -s -m 10 -X PUT "$BASE/api/system/auth/password" \
    -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
    -d "{\"old_password\":\"$F12_PASS\",\"new_password\":\"$F12_NEWPASS\"}" > /dev/null
  BODY=$(login_body "$F12_NEWPASS")
  TOKEN=$(printf '%s' "$BODY" | json_get data.access_token)
  F12_PASS="$F12_NEWPASS"
  ok "首登强制改密完成并重登"
fi

api_get() { curl -s -m 15 -o /tmp/f12-body.json -w '%{http_code}' "$BASE$1" -H "Authorization: Bearer $TOKEN"; }
body()    { cat /tmp/f12-body.json; }

# ---------- 1. 阈值口径 ----------
CODE=$(api_get "/api/business/erp/transfer/overdue/summary")
check "summary 接口可达（200）" "$CODE" "200"
THRESHOLD=$(body | json_get data.threshold_days)
check "阈值来自 sys_config（默认 7 天）" "$THRESHOLD" "7"
CFG=$(psql_q "select config_value from sys_config where config_key='erp.transfer.in_transit_overdue_days' and deleted_at is null")
check "库里的阈值参数与接口一致" "$CFG" "$THRESHOLD"

# ---------- 2. 造一张压 30 天的在途单（直插夹具，单号带 F12- 前缀便于清理） ----------
WH_FROM=$(psql_q "select id from biz_warehouse where warehouse_type='NORMAL' and deleted_at is null order by warehouse_code limit 1")
WH_TO=$(psql_q "select id from biz_warehouse where warehouse_type='NORMAL' and deleted_at is null order by warehouse_code desc limit 1")
WH_TRANSIT=$(psql_q "select id from biz_warehouse where warehouse_type='IN_TRANSIT' and deleted_at is null limit 1")
ADMIN_ID=$(psql_q "select id from sys_user where username='admin' limit 1")
if [ -z "$WH_FROM" ] || [ -z "$WH_TO" ] || [ -z "$WH_TRANSIT" ] || [ -z "$ADMIN_ID" ]; then
  bad "夹具前置不足（仓库 / 在途仓 / admin 缺失）"
  echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
  exit 1
fi
STUCK_NO="F12-STUCK-$$"
FRESH_NO="F12-FRESH-$$"
insert_transfer() { # $1=单号 $2=发货时间偏移（SQL interval）
  psql_q "insert into biz_stock_transfer
      (id, transfer_no, from_warehouse_id, to_warehouse_id, transit_warehouse_id, status,
       transfer_date, reason, applicant_id, total_qty, total_amount,
       submitted_at, approved_at, shipped_at, created_at, updated_at)
    values (gen_random_uuid(), '$1', '$WH_FROM'::uuid, '$WH_TO'::uuid, '$WH_TRANSIT'::uuid, 'IN_TRANSIT',
       current_date, 'F12 验收夹具', '$ADMIN_ID'::uuid, 1, 0,
       now() - interval '31 days', now() - interval '31 days', now() - $2, now(), now())
    returning id"
}
STUCK_ID=$(insert_transfer "$STUCK_NO" "interval '30 days'")
FRESH_ID=$(insert_transfer "$FRESH_NO" "interval '1 hour'")
if [ -z "$STUCK_ID" ] || [ -z "$FRESH_ID" ]; then
  bad "夹具插入失败（$STUCK_ID / $FRESH_ID）"
  echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
  exit 1
fi
ok "夹具就位（超期 $STUCK_NO / 对照 $FRESH_NO）"

# ---------- 3. 可查询：列表筛选 ----------
CODE=$(api_get "/api/business/erp/transfer/page?in_transit_overdue=true&page_size=200&transfer_no=F12-")
check "列表 in_transit_overdue=true（200）" "$CODE" "200"
check_contains "超期单出现在「超期在途」筛选结果里" "$(body)" "$STUCK_NO"
case "$(body)" in
  *"$FRESH_NO"*) bad "刚发货的对照单不该出现在超期筛选里" ;;
  *) ok "刚发货的对照单不在超期筛选里" ;;
esac
FLAG=$(python3 -c 'import json,sys
d=json.load(open("/tmp/f12-body.json"))
rows=(d.get("data") or {}).get("list") or []
hit=[r for r in rows if r.get("transfer_no")=="'"$STUCK_NO"'"]
print(json.dumps(hit[0] if hit else {}, ensure_ascii=False))')
check_contains "行内 in_transit_overdue=true" "$FLAG" '"in_transit_overdue": true'
check_contains "行内带已压天数 overdue_days" "$FLAG" '"overdue_days": 30'

CODE=$(api_get "/api/business/erp/transfer/page?in_transit_overdue=false&page_size=200&transfer_no=F12-")
check "列表 in_transit_overdue=false（200）" "$CODE" "200"
check_contains "对照单出现在「排除超期」结果里" "$(body)" "$FRESH_NO"
case "$(body)" in
  *"$STUCK_NO"*) bad "超期单不该出现在「排除超期」结果里" ;;
  *) ok "超期单不在「排除超期」结果里" ;;
esac

CODE=$(api_get "/api/business/erp/transfer/page?in_transit_overdue=true&status=DONE")
check "矛盾组合 in_transit_overdue=true + status=DONE → 400" "$CODE" "400"
check_contains "400 文案点明矛盾" "$(body)" "矛盾"

# ---------- 4. 详情可见 ----------
CODE=$(api_get "/api/business/erp/transfer/$STUCK_ID")
check "详情接口（200）" "$CODE" "200"
DETAIL_FLAG=$(python3 -c 'import json,sys
d=(json.load(open("/tmp/f12-body.json")).get("data") or {})
print(str(d.get("in_transit_overdue")), d.get("overdue_days"))' 2>/dev/null | tr -d '\r')
check "详情回 in_transit_overdue / overdue_days" "$DETAIL_FLAG" "True 30"

# ---------- 5. 汇总计数 ----------
CODE=$(api_get "/api/business/erp/transfer/overdue/summary")
TOTAL=$(body | json_get data.total)
OLDEST=$(body | json_get data.oldest_shipped_at)
if [ "${TOTAL:-0}" -ge 1 ]; then ok "汇总 total ≥ 1（$TOTAL）"; else bad "汇总 total 应 ≥ 1（实际 $TOTAL）"; fi
[ -n "$OLDEST" ] && ok "汇总带最早发货时间（$OLDEST）" || bad "汇总缺 oldest_shipped_at"

# ---------- 6. 收尾：删净夹具 ----------
psql_q "delete from biz_stock_transfer where transfer_no like 'F12-%'" > /dev/null
LEFT=$(psql_q "select count(*) from biz_stock_transfer where transfer_no like 'F12-%'")
check "夹具清理（残留 0）" "$LEFT" "0"

echo "============================================"
echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
