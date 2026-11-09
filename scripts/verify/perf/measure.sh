#!/usr/bin/env bash
# =============================================================================
# N-8 性能基线测量器（真实 HTTP + 真实令牌，每项 1 次预热 + 3 次计时取中位）
#
# 用法（在跑着目标实例的机器上执行）：
#   bash scripts/verify/perf/measure.sh 1000    /tmp/n8_perf_1000.csv
#   bash scripts/verify/perf/measure.sh 10000   /tmp/n8_perf_10000.csv
#   bash scripts/verify/perf/measure.sh 100000  /tmp/n8_perf_100000.csv
#
# 环境变量：
#   PERF_BASE_URL   默认 http://127.0.0.1:18099   （隔离实例，见 docs/运维规程-性能基线.md）
#   PERF_ADMIN_USER 默认 admin
#   PERF_PERIOD     试算平衡/资产负债表用的会计期间（默认 2026-11）
#   PERF_ROUNDS     每项计时轮数（默认 3，取中位）
#
# 输出：CSV（scale,endpoint,http,bytes,t1,t2,t3,median_ms）—— 直接喂给文档表格。
# 令牌只在本进程内使用，不写进任何输出文件。
# =============================================================================
set -uo pipefail

SCALE="${1:?用法: measure.sh <scale标签> [输出csv]}"
OUT_CSV="${2:-/tmp/n8_perf_${SCALE}.csv}"
ROUNDS="${PERF_ROUNDS:-3}"
PERIOD="${PERF_PERIOD:-2026-11}"
BASE="${PERF_BASE_URL:-http://127.0.0.1:18099}"
ADMIN_USER="${PERF_ADMIN_USER:-admin}"

if [ -f /etc/cloudpivot/cloudpivot-dev.env ]; then
  # shellcheck disable=SC1091
  set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
fi
ADMIN_PASS="${PERF_ADMIN_PASSWORD:-${NIK_ADMIN_PASSWORD:-}}"
[ -n "$ADMIN_PASS" ] || { echo "缺少 NIK_ADMIN_PASSWORD"; exit 2; }

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

login() {
  curl -s -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
    -d "$(jq -nc --arg u "$ADMIN_USER" --arg p "$ADMIN_PASS" '{username:$u,password:$p}')"
}
LOGIN_BODY=$(login)
TOKEN=$(printf '%s' "$LOGIN_BODY" | jq -r '.data.access_token // empty')
[ -n "$TOKEN" ] || { echo "登录失败（脱敏）: $(printf '%s' "$LOGIN_BODY" | jq -c 'del(.data.access_token,.data.refresh_token)')"; exit 1; }
AUTH="Authorization: Bearer $TOKEN"

echo "scale=$SCALE base=$BASE period=$PERIOD rounds=$ROUNDS" >&2
printf 'scale,endpoint,http,bytes,t1,t2,t3,median_ms\n' > "$OUT_CSV"

# ---- 一次计时（写 tmp/head 里的 http 与 bytes） ----
time_once() {
  local path=$1
  curl -s -o "$TMP/body.json" -w '%{http_code} %{size_download} %{time_total}' \
    -H "$AUTH" "$BASE$path"
}

# 每项：预热 1 次 → 计时 ROUNDS 次 → 取中位
measure() {
  local label=$1 path=$2
  time_once "$path" >/dev/null 2>&1
  local codes="" bytes="" times="" fields code size t
  for _ in $(seq 1 "$ROUNDS"); do
    fields=$(time_once "$path")
    code=${fields%% *}
    size=$(echo "$fields" | cut -d' ' -f2)
    t=$(echo "$fields" | cut -d' ' -f3)
    codes="$codes $code"; bytes=$size
    times="$times $t"
  done
  local median
  median=$(printf '%s\n' $times | sort -g | awk '{a[NR]=$1} END {print a[int((NR+1)/2)]}')
  local ms tcsv
  ms=$(awk -v s="$median" 'BEGIN{printf "%.1f", s*1000}')
  tcsv=$(echo "$times" | awk '{for(i=1;i<=NF;i++) printf "%s%s", (i>1?",":""), $i}')
  printf '%s,%s,%s,%s,%s,%s\n' "$SCALE" "$label" "$(echo $codes | tr ' ' '/')" "$bytes" \
    "$tcsv" "$ms" >> "$OUT_CSV"
  echo "  $label  median=${ms}ms  http=$(echo $codes | tr ' ' '/')  bytes=$bytes" >&2
}

# ---- 凭证详情要真实主键：从我们造的凭证里取一张 ----
VID=$(curl -s -H "$AUTH" "$BASE/api/business/erp/voucher/page?page=1&page_size=1&period=$PERIOD" \
      | jq -r '.data.list[0].id // empty')

echo "[1/3] 列表类" >&2
measure task_page_1        "/api/business/task/page?page=1&page_size=20"
measure task_page_100      "/api/business/task/page?page=100&page_size=20"
measure task_page_1000     "/api/business/task/page?page=1000&page_size=20"
measure voucher_page_1     "/api/business/erp/voucher/page?page=1&page_size=20"
measure voucher_page_100   "/api/business/erp/voucher/page?page=100&page_size=20"
measure voucher_page_1000  "/api/business/erp/voucher/page?page=1000&page_size=20"
measure voucher_page_period "/api/business/erp/voucher/page?page=1&page_size=20&period=$PERIOD"
if [ -n "$VID" ] && [ "$VID" != "null" ]; then
  measure voucher_detail "$(printf '/api/business/erp/voucher/%s' "$VID")"
else
  echo "  [WARN] 未能取到凭证 id（period=$PERIOD 无数据？）" >&2
fi
measure inv_txn_page_1     "/api/business/erp/inventory-txn/page?page=1&page_size=20"
measure inv_txn_page_100   "/api/business/erp/inventory-txn/page?page=100&page_size=20"
measure oplog_page_1       "/api/support/log/page?page=1&page_size=20"
measure oplog_page_1000    "/api/support/log/page?page=1000&page_size=20"

echo "[2/3] 聚合类（全表）" >&2
measure trial_balance      "/api/business/erp/ledger/trial-balance?period=$PERIOD"
measure balance_sheet      "/api/business/erp/report/balance-sheet?period=$PERIOD"

echo "[3/3] 登录（argon2 开销，POST）" >&2
# 注意：登录接口按「用户名+IP」限流 10 次/60 秒（shared/constants.rs
# LOGIN_RATE_MAX_ATTEMPTS/WINDOW），所以这里只做 1 次预热 + ROUNDS 次计时，
# 且**如实记录状态码**（超限会是 429，此时 1~2ms 的时间不是 argon2 的耗时）。
if [ "${PERF_SKIP_LOGIN:-0}" = "1" ]; then
  echo "  （PERF_SKIP_LOGIN=1，跳过登录计时：数据量不影响 argon2 开销）" >&2
else
  times=""; codes=""
  for _ in $(seq 1 $((ROUNDS + 1))); do
    fields=$(curl -s -o /dev/null -w '%{http_code} %{time_total}' -X POST "$BASE/api/system/auth/login" \
        -H 'Content-Type: application/json' \
        -d "$(jq -nc --arg u "$ADMIN_USER" --arg p "$ADMIN_PASS" '{username:$u,password:$p}')")
    codes="$codes $(echo "$fields" | cut -d' ' -f1)"
    times="$times $(echo "$fields" | cut -d' ' -f2)"
  done
  # 丢掉预热（第一次）
  times=$(echo "$times" | cut -d' ' -f2-)
  codes=$(echo "$codes" | cut -d' ' -f2-)
  median=$(printf '%s\n' $times | sort -g | awk '{a[NR]=$1} END {print a[int((NR+1)/2)]}')
  ms=$(awk -v s="$median" 'BEGIN{printf "%.1f", s*1000}')
  tcsv=$(echo "$times" | awk '{for(i=1;i<=NF;i++) printf "%s%s", (i>1?",":""), $i}')
  printf '%s,login_post,%s,%s,%s,%s\n' "$SCALE" "$(echo $codes | tr ' ' '/')" "0" "$tcsv" "$ms" >> "$OUT_CSV"
  echo "  login_post  median=${ms}ms  http=$(echo $codes | tr ' ' '/')" >&2
fi

echo "== 结果写入 $OUT_CSV ==" >&2
cat "$OUT_CSV"
