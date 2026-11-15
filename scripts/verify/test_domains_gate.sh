#!/usr/bin/env bash
# ============================================================================
# test_domains_gate.sh —— 门禁：lib 用例与「测试分域登记表」**恰好一一对应**（P2-61）
#
# 为什么必须有这道闸：
#   `scripts/test_domain.py run --package <域>` 是「只跑这一段」的入口。新增模块/子目录后如果
#   忘了在 `backend/test-domains.tsv` 登记，那个域的用例仍然存在、但**没有任何域会跑到它** ——
#   分域跑时静默少跑一批，且看不出来。本闸把这件事变成红：
#     · 每条用例必须命中**恰好一个**域（无遗漏、无重叠）；
#     · 每个域都必须非空（前缀写错 / 模块删了就红）；
#     · 登记表里的 crate 必须在 workspace 里真实存在；
#     · 分域合计必须等于用例总数。
#
# 用法：
#   bash scripts/verify/test_domains_gate.sh                     # 真列用例（需已编译，秒级）
#   bash scripts/verify/test_domains_gate.sh --tests-from DIR     # 读预存清单 <crate>.txt，免编译
#
# 退出码：0 = 一一对应；1 = 有遗漏/重叠/空域；2 = 前置错误（登记表坏、crate 不存在）
# ============================================================================
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
exec python3 "$ROOT/scripts/test_domain.py" check "$@"
