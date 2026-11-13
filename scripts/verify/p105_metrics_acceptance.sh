#!/usr/bin/env bash
# P1-05 证据采集：在已启动的本地实例（:8023，空库首启 profile=p105）上跑一遍验收命令。
# 用法：bash scripts/verify/p105_metrics_acceptance.sh [端口] [输出目录]
set -uo pipefail

PORT="${1:-8023}"
OUT="${2:-docs/evidence/2026-11-13-p105-metrics}"
mkdir -p "$OUT"

BASE="http://127.0.0.1:${PORT}"

{
  echo "### ① curl -s http://127.0.0.1:${PORT}/metrics | head -20"
  curl -s "${BASE}/metrics" | head -20
  echo
  echo "### ② 状态码 + 体积"
  curl -s -o /tmp/p105_metrics_body.txt -w "http=%{http_code} bytes=%{size_download} content_type=%{content_type}\n" "${BASE}/metrics"
  echo
  echo "### ③ cloudpivot_* 序列数"
  grep -c '^cloudpivot_' /tmp/p105_metrics_body.txt
  echo
  echo "### ④ 每个 cloudpivot_* 指标的 HELP / TYPE 行（业务指标必须带说明）"
  grep -E '^# (HELP|TYPE) cloudpivot_' /tmp/p105_metrics_body.txt
  echo
  echo "### ⑤ 身份相关两条 gauge（先访问 /health/deps 触发采样）"
  curl -s -o /dev/null "${BASE}/health/deps"
  curl -s "${BASE}/metrics" | grep -E 'cloudpivot_(auth_cache_degraded|token_blacklist_rows)'
  echo
  echo "### ⑥ 访问控制：伪造非回环来源（X-Forwarded-For）应 403"
  curl -s -o /dev/null -w "http=%{http_code}\n" -H 'X-Forwarded-For: 203.0.113.9' "${BASE}/metrics"
  echo
  echo "### ⑦ 端点挂在根路径（不参与 [router].prefix）"
  curl -s -o /dev/null -w "/api/metrics http=%{http_code}\n" "${BASE}/api/metrics"
} > "$OUT/01_acceptance_curl.out" 2>&1

echo "written: $OUT/01_acceptance_curl.out"
