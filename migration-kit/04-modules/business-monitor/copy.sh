#!/usr/bin/env bash
# =============================================================================
# 04-modules/business-monitor/copy.sh —— 把「服务器状态卡」模块整目录搬进目标树
#
# 使用场景有两种：
#   A) 重置基线路径（推荐）：02 补丁序列已经包含本模块（补齐在 0007 补丁里），
#      本脚本只会核对「文件是否已在位」，输出「一致」而不改任何东西（幂等）。
#   B) 功能移植路径（Pro 只新增了独立模块、底座没动时）：把本模块拷到 Pro 上。
#
# 用法：bash 04-modules/business-monitor/copy.sh [--root <repo>] [--dry-run]
# 退出码：0 拷贝/核对完成；4 接线点缺失（需人工按 wiring.md 补）
# =============================================================================
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT=""
DRY=0
while [ $# -gt 0 ]; do
  case "$1" in
    --root) ROOT="${2:?}"; shift 2 ;;
    --root=*) ROOT="${1#*=}"; shift ;;
    --dry-run) DRY=1; shift ;;
    -h|--help) sed -n '2,14p' "$0"; exit 0 ;;
    *) echo "未知参数: $1" >&2; exit 2 ;;
  esac
done
[ -z "$ROOT" ] && ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "$ROOT" || { echo "进不去 $ROOT" >&2; exit 2; }

RED=$'\033[31m'; GRN=$'\033[32m'; YEL=$'\033[33m'; RST=$'\033[0m'
same=0; copied=0; diffn=0

# 相对业务根拷贝（排除脚本自身、wiring.md 等元文件）
while IFS= read -r src; do
  rel="${src#"$HERE"/}"
  rel="${rel#./}"
  case "$rel" in
    *.sh|*.md|wiring*|README*|overlays/*) continue ;;
  esac
  dst="$ROOT/$rel"
  if [ -f "$dst" ] && cmp -s "$src" "$dst"; then
    same=$((same+1)); continue
  fi
  if [ -f "$dst" ]; then
    printf "  ${YEL}~${RST} 内容不同，将被覆盖: %s\n" "$rel"; diffn=$((diffn+1))
  else
    printf "  ${GRN}+${RST} 新增: %s\n" "$rel"; copied=$((copied+1))
  fi
  if [ "$DRY" = "0" ]; then
    mkdir -p "$(dirname "$dst")"
    cp -p "$src" "$dst"
  fi
done < <(find "$HERE" -type f | sort)

echo "---------------------------------------------------------------------"
echo "模块文件: 一致 ${same}, 新增 ${copied}, 覆盖 ${diffn}$([ "$DRY" = 1 ] && echo '（dry-run，未落盘）')"

echo "== 接线点自检（这三处必须存在于目标树，否则模块编不过）=="
rc=0
chk() { if eval "$2" >/dev/null 2>&1; then printf "  ${GRN}✓${RST} %s\n" "$1"; else printf "  ${RED}✘${RST} %s —— 见 wiring.md\n" "$1"; rc=4; fi; }
chk "backend/src/lib.rs 声明 pub mod business;"        'grep -q "^pub mod business;" backend/src/lib.rs'
chk "backend/src/business/mod.rs 声明 pub mod monitor;" 'grep -q "^pub mod monitor;" backend/src/business/mod.rs'
chk "backend/Cargo.toml 显式 redis 依赖（INFO 采集用）"  'grep -q "^redis = " backend/Cargo.toml'
chk "frontend/src/types/components.d.ts 有 TProgress"  'grep -q "TProgress" frontend/src/types/components.d.ts'
chk "迁移 V0020 在位"                                   'test -f backend/migrations/business/V0020__add_biz_monitor_permission.up.sql'
echo "说明：dashboard 页面本身（frontend/src/views/dashboard/index.vue）不在此覆盖范围内，"
echo "      它同时承载配色/推广位等其它改动，按 05-conflicts 清单逐条核对，避免版本回退。"
exit "$rc"
