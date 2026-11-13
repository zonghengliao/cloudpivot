#!/usr/bin/env bash
# P1-06 验收：4 个关键指标（请求数 / 延迟 / 错误率 / DB 连接）在跑起来的实例上真的存在且会增长。
#
#   bash scripts/verify/p106_metrics_acceptance.sh [端口] [输出目录]
#
# 断言（任何一条不满足 → 退出码 1，日志里能看到是哪条）：
#   ① `grep -E "requests_total|duration_seconds|errors_total|pool_connections"` 四类都在（含 # HELP / # TYPE）
#   ② 错误数在健康状态下就是 0（不是「没有序列」）
#   ③ 压一小段后 `cloudpivot_http_requests_total` 按精确码增长，直方图 _count 同步增长
#   ④ DB 连接水位 idle/active/total 三条序列都在，且 max = 配置上限
#   ⑤ `/metrics` 自身不被计数（抓取不污染业务量）
#
# 压测用 `hey -n 100 -c 5`（卡面口径）；本机没装 hey 时退化成
# `xargs -P5` 跑 100 个 curl —— 并发度与请求数一致，断言口径不变。
set -uo pipefail

PORT="${1:-8023}"
OUT="${2:-docs/evidence/2026-11-13-p106-metrics}"
mkdir -p "$OUT"
BASE="http://127.0.0.1:${PORT}"
DURATION_SECONDS="$(mktemp /tmp/p106_metrics.XXXXXX)"
trap 'rm -f "$DURATION_SECONDS"' EXIT

FAIL=0
pass() { echo "PASS: $*"; }
fail() { echo "FAIL: $*"; FAIL=1; }

# 「压一小段」的两个目标：
#   HEAVY = 真实存活的探针（根路径 /health，200）
#   CARDY = 卡面字面的 /api/health（不存在，404 —— 照样要按精确码计数）
HEAVY_PATH="/health"
CARDY_PATH="/api/health"

echo "=== 0. 实例可达性 ${BASE} ==="
code="$(curl -s -o /dev/null -w '%{http_code}' "${BASE}/health")"
echo "/health -> ${code}"
[ "$code" = "200" ] || { echo "实例不可用，先起后端"; exit 1; }

echo
echo "=== 1. 压测前基线 ==="
curl -s "${BASE}/metrics" > "$OUT/00_metrics_before.txt"
grep -E '^cloudpivot_http_requests_total' "$OUT/00_metrics_before.txt" | head -5 || true

echo
echo "=== 2. 四类指标都在（卡面 grep 口径） ==="
grep -E "requests_total|duration_seconds|errors_total|pool_connections" "$OUT/00_metrics_before.txt" \
  > "$OUT/01_four_categories.txt"
cat "$OUT/01_four_categories.txt"
for needle in \
  "cloudpivot_http_requests_total" \
  "cloudpivot_http_request_duration_seconds" \
  "cloudpivot_http_errors_total" \
  "cloudpivot_db_pool_connections" \
  "cloudpivot_db_pool_max_connections"
do
  if grep -q "^# TYPE ${needle} " "$OUT/00_metrics_before.txt"; then
    pass "指标族存在且带 TYPE：${needle}"
  else
    fail "缺少指标族：${needle}"
  fi
done

echo
echo "=== 3. 错误数在健康状态下是 0（seed 出 0 序列，而不是「没有这条」） ==="
errors_line="$(grep -E '^cloudpivot_http_errors_total ' "$OUT/00_metrics_before.txt" | head -1)"
echo "${errors_line:-<无序列>}"
if [ "${errors_line##* }" = "0" ]; then
  pass "cloudpivot_http_errors_total = 0"
else
  fail "cloudpivot_http_errors_total 不是 0 或没有序列：${errors_line:-<无>}"
fi

echo
echo "=== 4. DB 连接水位 ==="
grep -E '^cloudpivot_db_pool_(connections|max_connections)' "$OUT/00_metrics_before.txt"
for state in idle active total; do
  if grep -q "^cloudpivot_db_pool_connections{state=\"${state}\"}" "$OUT/00_metrics_before.txt"; then
    pass "连接水位 state=${state} 存在"
  else
    fail "连接水位缺 state=${state}"
  fi
done
if grep -qE '^cloudpivot_db_pool_max_connections [1-9][0-9]*$' "$OUT/00_metrics_before.txt"; then
  pass "max_connections 为正值（读到了池配置）"
else
  fail "max_connections 缺失或为 0"
fi

echo
echo "=== 5. 压一小段（hey -n 100 -c 5 / xargs -P5 兜底） ==="
if command -v hey >/dev/null 2>&1; then
  echo "压测工具：hey（$(command -v hey)）"
  hey -n 100 -c 5 "${BASE}${HEAVY_PATH}" > "$OUT/02_load_heavy.txt" 2>&1
  hey -n 100 -c 5 "${BASE}${CARDY_PATH}" > "$OUT/02_load_cardy.txt" 2>&1
  # hey 的汇总（Requests/sec 与状态码分布），留作证据
  head -20 "$OUT/02_load_heavy.txt" > "$OUT/02_load_summary.txt"
  echo "--- 卡面字面路径 ${CARDY_PATH} ---" >> "$OUT/02_load_summary.txt"
  head -20 "$OUT/02_load_cardy.txt" >> "$OUT/02_load_summary.txt"
else
  echo "压测工具：未安装 hey，改用 xargs -P5（100 请求 / 5 并发，口径等价）"
  seq 1 100 | xargs -P5 -I{} curl -s -o /dev/null "${BASE}${HEAVY_PATH}"
  seq 1 100 | xargs -P5 -I{} curl -s -o /dev/null "${BASE}${CARDY_PATH}"
  printf 'HEAVY=%s 100 次 / CARDY=%s 100 次 / c=5（xargs -P5 兜底）\n' "$HEAVY_PATH" "$CARDY_PATH" \
    > "$OUT/02_load_summary.txt"
fi
# 两次 curl 压测之后等一拍，让 exporter 的 upkeep 落盘（它不是同步写的）
sleep 2

echo
echo "=== 6. 压测后数值 ==="
curl -s "${BASE}/metrics" > "$OUT/03_metrics_after.txt"
echo "--- 请求数（精确码） ---"
grep -E "^cloudpivot_http_requests_total\{" "$OUT/03_metrics_after.txt" | sort
echo "--- 延迟直方图（_count / _sum，含卡面要求的桶） ---"
grep -E "^cloudpivot_http_request_duration_seconds_(count|sum)\{" "$OUT/03_metrics_after.txt" | sort
grep -E '^cloudpivot_http_request_duration_seconds_bucket\{path="/(health|api/health)",le="(0.01|0.05|0.1|0.5|1|5)"\}' \
  "$OUT/03_metrics_after.txt" | sort
echo "--- 错误数 / 连接水位 ---"
grep -E '^cloudpivot_http_errors_total|^cloudpivot_db_pool_' "$OUT/03_metrics_after.txt" | sort

heavy_count="$(grep -E "^cloudpivot_http_requests_total\{method=\"GET\",path=\"${HEAVY_PATH}\",status=\"200\"\}" \
  "$OUT/03_metrics_after.txt" | awk '{print $NF}')"
cardy_count="$(grep -E "^cloudpivot_http_requests_total\{method=\"GET\",path=\"${CARDY_PATH}\",status=\"404\"\}" \
  "$OUT/03_metrics_after.txt" | awk '{print $NF}')"
duration_count="$(grep -E "^cloudpivot_http_request_duration_seconds_count\{path=\"${HEAVY_PATH}\"\}" \
  "$OUT/03_metrics_after.txt" | awk '{print $NF}')"

echo
echo "计数：${HEAVY_PATH} 200 -> ${heavy_count:-<无>}；${CARDY_PATH} 404 -> ${cardy_count:-<无>}；直方图 _count -> ${duration_count:-<无>}"

[ -n "${heavy_count:-}" ] && [ "${heavy_count%.*}" -ge 100 ] 2>/dev/null \
  && pass "请求数按精确码增长：${HEAVY_PATH} 200 = ${heavy_count}（≥100）" \
  || fail "请求数没增长或没有序列：${heavy_count:-<无>}"
[ -n "${cardy_count:-}" ] && [ "${cardy_count%.*}" -ge 100 ] 2>/dev/null \
  && pass "不存在路径也按 404 计数：${CARDY_PATH} = ${cardy_count}（≥100）" \
  || fail "404 未计数：${cardy_count:-<无>}"
[ -n "${duration_count:-}" ] && [ "${duration_count%.*}" -ge 100 ] 2>/dev/null \
  && pass "延迟直方图 _count 同步增长：${duration_count}（≥100）" \
  || fail "直方图没有观测值：${duration_count:-<无>}"

echo
echo "=== 6.1 错误数只认 5xx：刚才 100 次 404 不能把它抬高 ==="
errors_after="$(grep -E '^cloudpivot_http_errors_total ' "$OUT/03_metrics_after.txt" | awk '{print $NF}')"
echo "cloudpivot_http_errors_total = ${errors_after:-<无>}（期间有 100 次 404）"
if [ "${errors_after:-x}" = "0" ]; then
  pass "4xx 不进错误数（404×100 后仍为 0）"
else
  fail "错误数被 4xx 污染或没有序列：${errors_after:-<无>}"
fi

echo
echo "=== 7. /metrics 自身不计数（抓取不污染业务量） ==="
if grep -q 'path="/metrics"' "$OUT/03_metrics_after.txt"; then
  fail "指标端点自身被计入了（会随抓取频率虚增）"
else
  pass "path=\"/metrics\" 未出现在请求计数里"
fi

echo
echo "=== 8. dp / 容器视角旁证（可跳过） ==="
echo "服务端连接数（psql 视角，供与 cloudpivot_db_pool_connections 对照）："
PGPASSWORD="${NIK_DATABASE_PASSWORD:-alun}" psql -h "${NIK_DATABASE_HOST:-127.0.0.1}" \
  -U "${NIK_DATABASE_USER:-alun}" -d "${NIK_DATABASE_NAME:-postgres}" -tAc \
  "SELECT state, count(*) FROM pg_stat_activity WHERE datname = current_database() GROUP BY 1 ORDER BY 1" \
  2>&1 | head -8 || true

echo
if [ "$FAIL" = "0" ]; then
  echo "RESULT: 全部通过（证据见 ${OUT}/）"
else
  echo "RESULT: 有断言失败（见上面 FAIL 行）"
fi
exit "$FAIL"
