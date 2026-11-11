#!/usr/bin/env bash
# COST-B3-11 成本核算页的隔离实例（起 / 停）—— 只碰本机测试库，不碰生产
#
#   bash 00_up.sh up     # 建库 costb311_ui → 起 :18041 → 自动迁移（含 V0381 成本结算审批流种子）
#   bash 00_up.sh down   # 停实例 + dropdb
#
# 复用 B2-13 的同款脚本（同一份二进制/环境变量口径），只换库名与端口，便于并行。
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
exec env B213_DB="${B311_DB:-costb311_ui}" B213_PORT="${B311_PORT:-18041}" \
  bash "$ROOT/docs/evidence/2026-11-11-cost-b2-13-frontend-3pages/00_up.sh" "${1:-up}"
