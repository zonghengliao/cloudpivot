#!/usr/bin/env bash
# SO-24 两轮结果一致性对比（UUID / 单号归一化后逐行 diff）
set -uo pipefail
cd /opt/cloudpivot/docs/evidence/sales-e2e/raw
norm() {
  grep -E '✅|❌|📄|^[①②③④⑤⑥⑦⑧⑨⑩]|=====|全量：|销售组：|单据汇总' "$1" \
  | sed -E 's/id=[0-9a-f-]{36}/id=<uuid>/g
            s/task=[0-9a-f-]{36}/task=<uuid>/g
            s/(SO|SH|IV|RC|OUT)-20[0-9]{6}-[0-9]{6}/\1-xxx/g
            s|/opt/cloudpivot/docs/evidence/sales-e2e/raw/|raw/|g'
}
if diff <(norm round1_console.txt) <(norm round2_console.txt) > /tmp/so24_round_diff.txt; then
  echo "DIFF-EMPTY：两轮逐行一致（UUID/单号已归一化）"
else
  echo "DIFF 非空："; cat /tmp/so24_round_diff.txt
fi
echo "--- 校验项统计："
grep -c '✅' round1_console.txt | sed 's/^/round1 ✅=/'
grep -c '✅' round2_console.txt | sed 's/^/round2 ✅=/'
grep -c '❌' round1_console.txt | sed 's/^/round1 ❌=/'
grep -c '❌' round2_console.txt | sed 's/^/round2 ❌=/'
