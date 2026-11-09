#!/usr/bin/env bash
# =============================================================================
# N-8 · 一次跑完 5 个 k6 脚本，按接口给出「达标 / 不达标 + 退出码」
#
# 用法（在目标机器上，脚本目录随意）：
#   bash scripts/verify/perf/run_perf_suite.sh [输出目录]
#
# 环境变量：
#   K6_BASE_URL        默认 http://127.0.0.1:18099（隔离的性能实例）
#   K6_PERIOD          默认 2026-11
#   K6_ADMIN_PASSWORD  不设则从 /etc/cloudpivot/cloudpivot-dev.env 的 NIK_ADMIN_PASSWORD 取
#   K6_VUS / K6_DURATION 规模覆盖（默认各脚本自己的值）
#
# 退出码：0 = 5 个脚本全部达标；非 0 = 有脚本 threshold 未过（k6 自己返回 99）
#         或 launcher 失败（127 k6 不存在 / 1 脚本崩溃）。
# =============================================================================
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="${1:-/tmp/n8_k6_$(date +%Y%m%d-%H%M%S)}"
mkdir -p "$OUT"

if [ -f /etc/cloudpivot/cloudpivot-dev.env ]; then
  # shellcheck disable=SC1091
  set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
fi
export K6_BASE_URL="${K6_BASE_URL:-http://127.0.0.1:18099}"
export K6_PERIOD="${K6_PERIOD:-2026-11}"
export K6_ADMIN_PASSWORD="${K6_ADMIN_PASSWORD:-${NIK_ADMIN_PASSWORD:-}}"

command -v k6 >/dev/null 2>&1 || { echo "缺少 k6（安装见 docs/运维规程-性能基线.md §3.2）"; exit 127; }
[ -n "$K6_ADMIN_PASSWORD" ] || { echo "缺少 K6_ADMIN_PASSWORD / NIK_ADMIN_PASSWORD"; exit 2; }

echo "K6_BASE_URL=$K6_BASE_URL  PERIOD=$K6_PERIOD  输出=$OUT"
printf '%-16s %-8s %-10s %s\n' 脚本 退出码 结论 摘要文件

FAILED=0
LIST="k6_task_list k6_voucher_list k6_trial_balance k6_report_bs k6_login"
for name in $LIST; do
  k6 run --summary-export "$OUT/$name.json" "$HERE/$name.js" > "$OUT/$name.txt" 2>&1
  code=$?
  if [ "$code" -eq 0 ]; then
    verdict="达标"
  else
    verdict="不达标"
    FAILED=$((FAILED + 1))
  fi
  printf '%-16s %-8s %-10s %s\n' "$name" "$code" "$verdict" "$OUT/$name.json"
done

echo
echo "== 各脚本的实测值 / threshold（从 summary json 取；退出码为准）=="
for name in $LIST; do
  [ -f "$OUT/$name.json" ] || continue
  echo "$name:"
  jq -r '.metrics | to_entries[]
    | select(.key | test("^http_req_duration\\{|^http_req_failed\\{|^checks$|^iterations$|^http_reqs$"))
    | "  \(.key): med=\(.value.med // "-") p95=\(.value["p(95)"] // "-") p99=\(.value["p(99)"] // "-") rate=\(.value.rate // "-") count=\(.value.count // "-")"' \
    "$OUT/$name.json" 2>/dev/null || true
  # threshold 明细：k6 把每条 threshold 的结论放在 metrics[*].thresholds 里
  # （该布尔值的语义在不同 k6 版本里不一致 —— 结论以**退出码**与日志里的 ✗ 为准）
  jq -r '.metrics | to_entries[]
    | select(.value.thresholds != null)
    | .key as $k | .value.thresholds | to_entries[]
    | "  阈值 \($k): \(.key)"' "$OUT/$name.json" 2>/dev/null || true
  grep -E "✗" "$OUT/$name.txt" >/dev/null 2>&1 && echo "  日志里有 ✗（有阈值未过）"
done

echo
if [ "$FAILED" -eq 0 ]; then
  echo "RESULT: PASS —— 5/5 脚本全部达标（日志在 $OUT）"
  exit 0
else
  echo "RESULT: FAIL —— $FAILED 个脚本未达标（日志在 $OUT）"
  exit 1
fi
