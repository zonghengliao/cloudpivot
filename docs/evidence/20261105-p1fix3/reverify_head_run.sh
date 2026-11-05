#!/usr/bin/env bash
# P1-FIX-3 本卡（run 707）复验：在 HEAD 上重新构建的二进制上连跑两轮真机验收 + 基线二进制对照。
# 输出目录 docs/evidence/20261105-p1fix3/reverify_head/
set -uo pipefail
export PATH=/root/.cargo/bin:/usr/local/bin:$PATH
ROOT=/opt/cloudpivot
OUT=$ROOT/docs/evidence/20261105-p1fix3/reverify_head
INST=$ROOT/docs/evidence/20261105-p1fix3/p1fix3_instance.sh
E2E=$ROOT/docs/evidence/20261105-p1fix3/p1fix3_cache_e2e.py
mkdir -p "$OUT"
cd "$ROOT" || exit 1

echo "=== 时间 $(date '+%F %T') HEAD=$(git rev-parse --short HEAD) 二进制=$(ls -l backend/target/debug/cloudpivot | awk '{print $6,$7,$8}') ==="

# 1. 清掉可能残留的旧实例（按 pid 文件，别用 pkill -f）
for pf in /root/p1fix3-instance-*.pid; do
  [ -f "$pf" ] || continue
  pid=$(cat "$pf")
  if kill -0 "$pid" 2>/dev/null; then echo "kill stale pid=$pid ($pf)"; kill -9 "$pid" 2>/dev/null; fi
  rm -f "$pf"
done
ss -lntp 2>/dev/null | grep -q ':18116 ' && { echo "!! 18116 仍被占用"; ss -lntp | grep ':18116 '; }

# 2. 起 HEAD 二进制
P1FIX3_BIN=$ROOT/backend/target/debug/cloudpivot bash "$INST" start > "$OUT/00_instance_start.txt" 2>&1
rc=$?
tail -3 "$OUT/00_instance_start.txt"
[ $rc -ne 0 ] && { echo "!! 实例启动失败"; exit 1; }

# 3. 修复后连跑两轮（同一实例不重启）
P1FIX3_EXPECT=fix python3 "$E2E" > "$OUT/after_run5_head.txt" 2>&1; echo "run5 rc=$?"
tail -3 "$OUT/after_run5_head.txt"
P1FIX3_EXPECT=fix python3 "$E2E" > "$OUT/after_run6_head.txt" 2>&1; echo "run6 rc=$?"
tail -3 "$OUT/after_run6_head.txt"

bash "$INST" stop > /dev/null 2>&1
sleep 2

# 4. 基线二进制（改动前）对照：期望 A2 = 403（bug 复现）
if [ -x /root/p1fix3-baseline-bin ]; then
  P1FIX3_BIN=/root/p1fix3-baseline-bin bash "$INST" start > "$OUT/01_instance_start_baseline.txt" 2>&1
  rc=$?
  tail -2 "$OUT/01_instance_start_baseline.txt"
  if [ $rc -eq 0 ]; then
    P1FIX3_EXPECT=baseline python3 "$E2E" > "$OUT/before_run4_baselinebin.txt" 2>&1; echo "baseline rc=$?"
    tail -3 "$OUT/before_run4_baselinebin.txt"
  fi
  # pid 文件按「二进制名」区分（TAG=basename 去非字母数字）⇒ 停基线实例必须带同一个 P1FIX3_BIN，
  # 否则 stop 找的是默认 tag 的 pid 文件，基线进程会漏在端口上（首轮踩过：pid 1891687 残留 18116）。
  P1FIX3_BIN=/root/p1fix3-baseline-bin bash "$INST" stop > /dev/null 2>&1
  sleep 2
  ss -lntp 2>/dev/null | grep -q ":${P1FIX3_PORT:-18116} " && echo "!! 端口仍被占用（基线实例没停掉）"
else
  echo "基线二进制不存在，跳过" > "$OUT/before_run4_baselinebin.txt"
fi
echo "=== DONE $(date '+%F %T') ==="
