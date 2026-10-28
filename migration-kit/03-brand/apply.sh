#!/usr/bin/env bash
# =============================================================================
# 03-brand/apply.sh —— 应用「莫兰迪配色」+「新 logo/favicon」
#
# 为什么用 patch 而不是直接 cp：配色改动不止 theme.less 一个文件，
#   还牵动 BasicLayout.vue / index.less / dashboard / login / change-password 的
#   语义色与阴影值（04792dc，共 6 文件）。patch 忠实还原全部改动；
#   theme.less、logo-512.png、favicon-64.png 同时以独立文件形式放在本目录，
#   供「只想要配色变量」或 patch 打不上时手工比对。
#
# 用法：bash 03-brand/apply.sh [--dry-run]
# 退出码 0 成功；3 有补丁应用失败
# =============================================================================
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DRY=0
[ "${1:-}" = "--dry-run" ] && DRY=1
[ "${1:-}" = "-h" ] && { sed -n '2,16p' "$0"; exit 0; }

ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || { echo "不是 git 仓库" >&2; exit 2; }
cd "$ROOT" || exit 2

RED=$'\033[31m'; GRN=$'\033[32m'; RST=$'\033[0m'
rc=0
for p in "$HERE"/01-theme-morandi.patch "$HERE"/02-brand-assets.patch; do
  n="$(basename "$p")"
  if git apply --reverse --check "$p" >/dev/null 2>&1; then
    printf "  ${GRN}✓${RST} %s 已应用（reverse-check 通过，跳过）\n" "$n"; continue
  fi
  if git apply --check --binary "$p" >/dev/null 2>&1; then
    if [ "$DRY" = "1" ]; then
      printf "  ${GRN}✓${RST} %s 可干净应用（dry-run，未落盘）\n" "$n"; continue
    fi
    if git apply --binary "$p"; then
      printf "  ${GRN}✓${RST} %s 已应用\n" "$n"
    else
      printf "  ${RED}✘${RST} %s 应用失败\n" "$n"; rc=3
    fi
  else
    printf "  ${RED}✘${RST} %s 打不上（上下文不匹配）—— 手工重做指引见 03-brand/brand-points.md\n" "$n"
    rc=3
  fi
done

if [ "$DRY" = "0" ] && [ -f frontend/public/logo.png ]; then
  echo "-- 校验："
  file frontend/public/logo.png frontend/public/favicon.png 2>/dev/null | sed 's/^/   /'
  grep -m1 'primary-color\|@primary' frontend/src/style/theme.less 2>/dev/null | sed 's/^/   主色: /' || true
fi
[ "$DRY" = "1" ] && echo "（--dry-run：未落盘）"
exit "$rc"
