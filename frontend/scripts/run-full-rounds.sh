#!/usr/bin/env bash
# 连续 N 轮全量 e2e（默认 baseURL = 生产），每轮间隔 >= GAP 秒；逐轮保留日志 + 失败现场（trace/截图）。
#
# 为什么要卡间隔：生产开着 [middleware.rate_limit] 300 req / 60s（按 IP），一轮全量 ≈ 264 条，
# 两轮挤进 60s 必然 429 → 用例拿到空数据、20s 超时红（见 e2e/README.md「限流与连跑间隔」）。
#
# 用法（在 frontend/ 下）：bash scripts/run-full-rounds.sh [轮数] [间隔秒] [标签]
set -u
ROUNDS=${1:-3}
GAP=${2:-65}
TAG=${3:-full}
HERE=$(cd "$(dirname "$0")" && pwd)
cd "$HERE/.." || exit 1
mkdir -p e2e-stress/traces e2e-stress/logs
SUMMARY=e2e-stress/${TAG}-summary.txt
: > "$SUMMARY"
for i in $(seq 1 "$ROUNDS"); do
  START=$(date +%H:%M:%S)
  LOG=e2e-stress/logs/${TAG}-round-${i}.log
  rm -rf test-results
  node node_modules/@playwright/test/cli.js test --reporter=list > "$LOG" 2>&1
  RC=$?
  END=$(date +%H:%M:%S)
  LINE=$(grep -E "^ +[0-9]+ (passed|failed)|[0-9]+ passed|[0-9]+ failed" "$LOG" | tail -3 | tr '\n' ' ')
  echo "round $i exit=$RC start=$START end=$END :: $LINE" | tee -a "$SUMMARY"
  if [ -d test-results ]; then
    cp -r test-results "e2e-stress/traces/${TAG}-round-${i}" 2>/dev/null
  fi
  # 数据卫生：每轮结束复核 [E2E] 残留
  TOKEN=$(cat e2e/.auth/token.txt 2>/dev/null)
  LEFT=$(curl -s "http://172.18.10.144/api/business/task/page?page=1&page_size=200&keyword=%5BE2E%5D" -H "Authorization: Bearer $TOKEN" | python3 -c 'import sys,json;d=json.load(sys.stdin);print(d["data"]["total"] if d.get("code")==0 else "ERR")' 2>/dev/null)
  echo "  [E2E] 残留条数 = $LEFT" | tee -a "$SUMMARY"
  if [ "$i" -lt "$ROUNDS" ]; then
    echo "  ... sleep ${GAP}s（生产限流 300req/60s，必须冷却）" | tee -a "$SUMMARY"
    sleep "$GAP"
  fi
done
echo "=== 汇总 ===" | tee -a "$SUMMARY"
cat "$SUMMARY"
