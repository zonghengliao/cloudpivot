#!/usr/bin/env bash
# 连续 N 轮全量 e2e，每轮间隔 >= GAP 秒；逐轮保留日志 + 失败现场（trace/截图）。
#
# 目标实例：默认**隔离实例** http://127.0.0.1:5189（`scripts/verify/e2e_isolated_instance.sh up`）。
# 2026-10-31 11:34 prod 夹具事故后，脚本先跑 `e2e/target-guard.mjs --check "$BASE_URL"`：
# 目标不是回环隔离端口就直接退出（exit 2），不再有「默认打生产」这条路。
# 线上验收确需打 prod：E2E_ALLOW_PROD=1 E2E_PROD_REASON='…'（判据放行 + 留痕），
# 跑完脚本会强制调 scripts/e2e-postrun-check.sh。
#
# 为什么要卡间隔：生产/隔离实例上的限流是 300 req / 60s（按 IP），一轮全量 ≈ 264 条，
# 两轮挤进 60s 必然 429 → 用例拿到空数据、20s 超时红（见 e2e/README.md「限流与连跑间隔」）。
#
# 用法（在 frontend/ 下）：bash scripts/run-full-rounds.sh [轮数] [间隔秒] [标签]
set -u
ROUNDS=${1:-3}
GAP=${2:-65}
TAG=${3:-full}
HERE=$(cd "$(dirname "$0")" && pwd)
cd "$HERE/.." || exit 1
BASE_URL=${E2E_BASE_URL:-http://127.0.0.1:5189}

# 硬判据：非隔离目标直接退出（与 playwright.config.ts 同一套规则，见 e2e/target-guard.mjs）
if ! node e2e/target-guard.mjs --check "$BASE_URL"; then
  echo "✗ 目标 $BASE_URL 不是隔离实例，拒绝连跑。" >&2
  echo "  起隔离实例：bash ../scripts/verify/e2e_isolated_instance.sh up" >&2
  exit 2
fi
echo "目标：$BASE_URL（轮数 $ROUNDS / 间隔 ${GAP}s）"

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
  LEFT=$(curl -s "$BASE_URL/api/business/task/page?page=1&page_size=200&keyword=%5BE2E%5D" -H "Authorization: Bearer $TOKEN" | python3 -c 'import sys,json;d=json.load(sys.stdin);print(d["data"]["total"] if d.get("code")==0 else "ERR")' 2>/dev/null)
  echo "  [E2E] 残留条数 = $LEFT" | tee -a "$SUMMARY"
  if [ "$i" -lt "$ROUNDS" ]; then
    echo "  ... sleep ${GAP}s（生产限流 300req/60s，必须冷却）" | tee -a "$SUMMARY"
    sleep "$GAP"
  fi
done
echo "=== 汇总 ===" | tee -a "$SUMMARY"
cat "$SUMMARY"

# 后置检查：只要本次放行打过 prod（判据写了 e2e/.auth/prod-target.flag），跑完必须做
if [ -f e2e/.auth/prod-target.flag ]; then
  echo "=== 检测到 prod-target.flag：执行后置检查（检出零成本夹具 → 清理 → 32/32）===" | tee -a "$SUMMARY"
  bash ../scripts/e2e-postrun-check.sh 2>&1 | tee -a "$SUMMARY"
  POST_RC=${PIPESTATUS[0]}
  echo "postrun exit=$POST_RC" | tee -a "$SUMMARY"
  exit "$POST_RC"
fi
