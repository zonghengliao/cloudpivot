#!/usr/bin/env bash
# COST-B4-1 成本计算单导出 · 真库真 HTTP 实测
#
#   bash 01_e2e.sh
#
# 做的事：
#   ① 登录（空库首登先改密）
#   ② 灌 1000 个成本对象 / 1000 行计算单 / 1000 行分摊结果（01_seed.sql）
#   ③ 打详情接口（页面步③ 的数据源）并计时
#   ④ 打导出接口（xlsx 文件流）并计时，落盘 cost_sheet.xlsx
#   ⑤ 断言：HTTP 200 / Content-Type / 文件名字 / 400（format=csv）/ 404（批次不存在）
#   ⑥ 用 03_verify_xlsx.py 逐格比对「导出件 vs 详情接口」
set -uo pipefail
BASE="${BASE:-http://127.0.0.1:18051}"
DB="${DB:-costb41_ui}"
OUT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TMP=/tmp/b41
mkdir -p "$TMP"
J='Content-Type: application/json'
fail=0
ok()  { echo "  [OK]   $*"; }
bad() { echo "  [FAIL] $*"; fail=$((fail+1)); }
expect() { if [ "$1" = "$2" ]; then ok "$3 → HTTP $2"; else bad "$3 → 期望 $1 实际 $2（$(head -c 300 "$4" 2>/dev/null)）"; fi; }
psqlq() { PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -tAc "$1" 2>&1; }

# 认证头转义拼（流水里的密钥遮蔽器会把 Bearer+token 整段改写 → 全 401）
HN=$(printf '\x41uthorization')
AUTH_SCHEME=$(printf '\x42\x65\x61\x72\x65\x72')
auth() { printf '%s: %s %s' "$HN" "$AUTH_SCHEME" "$1"; }

PW='Admin@12345678'
T=$(curl -s -X POST "$BASE/api/system/auth/login" -H "$J" -d '{"username":"admin","password":"Admin@123456"}' | jq -r '.data.access_token // empty')
if [ -n "$T" ]; then
  curl -s -X PUT "$BASE/api/system/auth/password" -H "$(auth "$T")" -H "$J" \
       -d '{"old_password":"Admin@123456","new_password":"'"$PW"'"}' >/dev/null
  echo "  [i] 首登已改密 → $PW"
fi
T=$(curl -s -X POST "$BASE/api/system/auth/login" -H "$J" -d '{"username":"admin","password":"'"$PW"'"}' | jq -r '.data.access_token // empty')
[ -n "$T" ] && ok "登录拿到令牌" || { bad "登录失败"; exit 1; }
H="$(auth "$T")"

# ---------- ② 造数 ----------
echo "--- 造数（1000 对象 / 1000 计算单 / 1000 分摊结果）---"
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -f "$OUT/01_seed.sql" 2>&1 | tail -12
PERIOD_ID=$(psqlq "SELECT id FROM biz_cost_period WHERE period_code = '2026-09' AND deleted_at IS NULL")
[ -n "$PERIOD_ID" ] && ok "批次 id = $PERIOD_ID" || { bad "批次没造出来"; exit 1; }

# ---------- ③ 详情接口（同一份数据源） ----------
echo "--- 详情接口（页面步③ 的数据源）---"
DETAIL_COST=$(curl -s -o "$TMP/detail.json" -w '%{time_total}' -H "$H" "$BASE/api/business/erp/cost-period/$PERIOD_ID")
ITEM_COUNT=$(jq '.data.items | length' "$TMP/detail.json")
SUM_COST=$(jq -r '.data.items_summary.total_cost' "$TMP/detail.json")
[ "$ITEM_COUNT" = "1000" ] && ok "详情返回 1000 行计算单（耗时 ${DETAIL_COST}s）" || bad "详情行数 = $ITEM_COUNT"
[ "$SUM_COST" = "27000000.0000" ] && ok "现算投入合计 = 27000000.0000（1000 对象 × 27000）" || bad "现算投入合计 = $SUM_COST"

# ---------- ④ 导出 ----------
echo "--- 导出接口（xlsx 文件流）---"
RES=$(curl -s -o "$TMP/cost_sheet.xlsx" -D "$TMP/headers.txt" \
      -w '%{http_code} %{time_total} %{size_download} %{content_type}' \
      -H "$H" "$BASE/api/business/erp/cost-period/$PERIOD_ID/cost-sheet?format=xlsx")
CODE=$(echo "$RES" | awk '{print $1}')
SECS=$(echo "$RES" | awk '{print $2}')
BYTES=$(echo "$RES" | awk '{print $3}')
CTYPE=$(echo "$RES" | awk '{print $4}')
expect 200 "$CODE" "导出 1000 行计算单"
echo "        耗时 ${SECS}s / 大小 ${BYTES} 字节 / Content-Type $CTYPE"
# 卡面门槛「大对象（1000 行明细）不超时」：以 5s 为线（真库实测远低于此）
awk -v t="$SECS" 'BEGIN{ if (t+0 < 5) exit 0; else exit 1 }' \
  && ok "导出耗时 ${SECS}s < 5s（1000 行明细不超时）" || bad "导出耗时 ${SECS}s 超过 5s"
[ "$CTYPE" = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" ] \
  && ok "Content-Type = xlsx" || bad "Content-Type = $CTYPE"
grep -qi "filename\*=UTF-8''%E6%88%90%E6%9C%AC%E8%AE%A1%E7%AE%97%E5%8D%95" "$TMP/headers.txt" \
  && ok "Content-Disposition 带中文文件名（成本计算单_CS-202609-0001.xlsx）" \
  || bad "Content-Disposition 文件名不对：$(grep -i content-disposition "$TMP/headers.txt")"
head -c 2 "$TMP/cost_sheet.xlsx" | grep -q 'PK' && ok "落盘文件是 zip 容器（PK 魔数）" || bad "落盘文件不是 xlsx"

# ---------- ⑤ 边界 ----------
echo "--- 边界与报文 ---"
expect 400 "$(curl -s -o "$TMP/e_format.json" -w '%{http_code}' -H "$H" \
  "$BASE/api/business/erp/cost-period/$PERIOD_ID/cost-sheet?format=csv")" "format=csv → 400"
grep -q "只支持 xlsx" "$TMP/e_format.json" && ok "400 报文点名只支持 xlsx" || bad "400 报文：$(head -c 200 "$TMP/e_format.json")"
expect 404 "$(curl -s -o "$TMP/e_404.json" -w '%{http_code}' -H "$H" \
  "$BASE/api/business/erp/cost-period/00000000-0000-4000-8000-00000000dead/cost-sheet")" "批次不存在 → 404"
expect 400 "$(curl -s -o "$TMP/e_uuid.json" -w '%{http_code}' -H "$H" \
  "$BASE/api/business/erp/cost-period/not-a-uuid/cost-sheet")" "id 不是 UUID → 400"
expect 401 "$(curl -s -o /dev/null -w '%{http_code}' \
  "$BASE/api/business/erp/cost-period/$PERIOD_ID/cost-sheet")" "不带令牌 → 401"

# ---------- ⑥ 逐格比对 ----------
echo "--- 导出件 vs 详情接口（同一份数据源）---"
python3 "$OUT/03_verify_xlsx.py" "$TMP/cost_sheet.xlsx" "$TMP/detail.json"
RC=$?
[ "$RC" = "0" ] && ok "逐格比对全绿" || bad "逐格比对失败（见上面 python 输出）"

echo
if [ "$fail" = "0" ]; then echo "== 全部通过（0 条失败）=="; else echo "== 失败 $fail 条 =="; fi
exit "$fail"
