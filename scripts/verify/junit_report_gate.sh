#!/usr/bin/env bash
# ============================================================================
# junit_report_gate.sh —— 门禁：CI 报告（JUnit XML）必须「真能生成、真能自证」（P2-63）
#
# 为什么需要它：
#   JUnit XML 是 CI 上唯一能看出「哪条用例红了、为什么红、哪条变慢了」的机器可读产物。
#   它有个致命的失败形态 —— **静默降级**：报告少了几条用例、红线被吞掉、计数和内容对不上，
#   CI 界面仍然一片绿，人不会发现。所以这道闸不校验「文件存在」，而是校验**可验证性**：
#     · 解析器能把 libtest 的 ok/FAILED/ignored/失败块/汇总行逐条还原（含逐条耗时）；
#     · 计数造假、失败元素被删、用例重复、XML 截断 **四类畸变必须被抓**（反证）；
#     · 进程被杀（没有 `test result:` 行）必须落成 `<error>`，不能当 0 红；
#     · `test_domain.py` 的接线（`--junit`）不能被悄悄摘掉。
#
# 全部是纯 stdlib + 固定夹具，不编译、不连库（L0 秒级）。
#
# 用法：
#   bash scripts/verify/junit_report_gate.sh              # 跑门禁
#   bash scripts/verify/junit_report_gate.sh --selftest    # 同一件事（显式点名反证）
#   python3 scripts/junit_report.py check <xml>           # 校验一份真实产物（CI 上用）
#
# 退出码：0 = 全过；1 = 有断言失败（明细在 stdout）；2 = 前置缺失（脚本不在位）
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
GEN="$ROOT/scripts/junit_report.py"

[ -f "$GEN" ] || { echo "==> junit_report_gate 前置缺失：$GEN" >&2; exit 2; }

echo "==> junit_report_gate：解析器自检 + 4 类畸变反证 + 接线检查（源：${GEN#$ROOT/}）"
python3 "$GEN" selftest
rc=$?
if [ "$rc" -ne 0 ]; then
    echo "==> junit_report_gate 未通过（exit=$rc）" >&2
    exit 1
fi
echo "==> junit_report_gate 通过"
exit 0
