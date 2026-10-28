#!/usr/bin/env bash
# =============================================================================
# 03-brand/apply.sh —— 一键落地「雾蓝流金」最终配色 + 品牌资源（不再打 patch）
#
# 为什么从「打 patch」改成「theme 直放 + 色值替换脚本」：
#   Pro 版很可能改过组件代码，硬贴 patch 十有八九冲突；而
#     ① theme.less 是**整文件覆盖**（没有上下文可言，永远打得上）
#     ② 组件里的硬编码色值用脚本按「色值」替换（不依赖行号/上下文）
#   两者都不怕 Pro 改代码，只有少数「变量化」片段替换需要锚点，锚点不在就告警跳过。
#
# 四步：
#   1) 覆盖 frontend/src/style/theme.less  ← theme-final.less（逐字节）
#   2) 换 frontend/public/{logo,favicon}.png ← logo-512.png / favicon-64.png
#   3) 跑 replace-colors.py                 ← 组件内硬编码色值 + 变量化片段
#   4) 校验                                  ← 关键变量 / 尺寸 / 上游青色清零 / 未映射告警
#
# 用法：
#   bash 03-brand/apply.sh                  # 在仓库根跑（自动 git rev-parse）
#   bash 03-brand/apply.sh --root /path/to/repo
#   bash 03-brand/apply.sh --dry-run        # 只打印要做什么，不落盘
#   bash 03-brand/apply.sh --check          # 只做校验（CI / 验收用）
#
# 退出码：0 成功；1 校验不通过；2 用法错误
# =============================================================================
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

ROOT=""
DRY=0
CHECK_ONLY=0
while [ $# -gt 0 ]; do
  case "$1" in
    --root) ROOT="${2:?}"; shift 2 ;;
    --root=*) ROOT="${1#*=}"; shift ;;
    --dry-run) DRY=1; shift ;;
    --check) CHECK_ONLY=1; shift ;;
    -h|--help) sed -n '2,26p' "$0"; exit 0 ;;
    *) echo "未知参数: $1" >&2; exit 2 ;;
  esac
done

if [ -z "$ROOT" ]; then
  ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || { echo "不是 git 仓库，请用 --root" >&2; exit 2; }
fi
cd "$ROOT" || { echo "进不去 $ROOT" >&2; exit 2; }

PY="${PYTHON:-python3}"
RED=$'\033[31m'; GRN=$'\033[32m'; YEL=$'\033[33m'; RST=$'\033[0m'
[ -t 1 ] || { RED=; GRN=; YEL=; RST=; }
rc=0
ok()   { printf "  ${GRN}✓${RST} %s\n" "$*"; }
bad()  { printf "  ${RED}✘${RST} %s\n" "$*"; rc=1; }
warn() { printf "  ${YEL}!${RST} %s\n" "$*"; }

if [ "$CHECK_ONLY" = "1" ]; then
  echo "== 03-brand 校验（--check）=="
  "$PY" "$HERE/replace-colors.py" --root "$ROOT" --check || rc=1
  echo "-- 品牌资源尺寸 --"
  for pair in "frontend/public/logo.png:512" "frontend/public/favicon.png:64"; do
    f="${pair%%:*}"; want="${pair##*:}"
    if [ -f "$f" ]; then
      got="$(file -b "$f" | grep -oE '[0-9]+ x [0-9]+' | head -1)"
      [ "$got" = "$want x $want" ] && ok "$f $got" || bad "$f 期望 ${want}x${want}，实际 $got"
    else
      bad "$f 不存在"
    fi
  done
  exit "$rc"
fi

# ---------- 1) theme.less 直放 ----------
echo "== 1) theme.less 直放 =="
if [ ! -f "$HERE/theme-final.less" ]; then
  bad "缺少 theme-final.less"
else
  TGT="frontend/src/style/theme.less"
  mkdir -p "$(dirname "$TGT")" 2>/dev/null || true
  if [ -f "$TGT" ] && cmp -s "$HERE/theme-final.less" "$TGT"; then
    ok "theme.less 已是最终版（逐字节一致，跳过）"
  elif [ "$DRY" = "1" ]; then
    ok "theme.less 将被覆盖（dry-run，未落盘）$(wc -l < "$HERE/theme-final.less") 行"
  else
    BK="${BRAND_BACKUP_DIR:-/var/tmp/cloudpivot-brand-backup}/$(date +%Y%m%d-%H%M%S)"
    if [ -f "$TGT" ]; then
      mkdir -p "$BK" && cp -a "$TGT" "$BK/theme.less" && warn "原 theme.less 已备份到 $BK/theme.less"
    fi
    cp "$HERE/theme-final.less" "$TGT" && ok "theme.less 已覆盖为最终版（$(wc -l < "$TGT") 行）"
  fi
fi

# ---------- 2) logo / favicon ----------
echo "== 2) 品牌资源（logo / favicon）=="
[ -f "$HERE/logo-512.png" ] || warn "缺少 logo-512.png（跳过，先用现有 logo）"
[ -f "$HERE/favicon-64.png" ] || warn "缺少 favicon-64.png（跳过，先用现有 favicon）"
if [ "$DRY" = "0" ]; then
  for pair in "logo-512.png:frontend/public/logo.png" "favicon-64.png:frontend/public/favicon.png"; do
    src="$HERE/${pair%%:*}"; dst="${pair##*:}"
    [ -f "$src" ] || continue
    mkdir -p "$(dirname "$dst")" 2>/dev/null || true
    if [ -f "$dst" ] && cmp -s "$src" "$dst"; then
      ok "$dst 已是最新（逐字节一致，跳过）"
    else
      cp "$src" "$dst" && ok "$dst 已替换（$(file -b "$dst" | grep -oE '[0-9]+ x [0-9]+' | head -1)）"
    fi
  done
else
  ok "logo/favicon 将被替换（dry-run，未落盘）"
fi

# ---------- 3) 组件色值替换 ----------
echo "== 3) 组件内硬编码色值替换（replace-colors.py）=="
if [ "$DRY" = "1" ]; then
  "$PY" "$HERE/replace-colors.py" --root "$ROOT" --dry-run || rc=1
else
  "$PY" "$HERE/replace-colors.py" --root "$ROOT" || rc=1
fi

# ---------- 4) 校验 ----------
if [ "$DRY" = "0" ]; then
  echo "== 4) 校验 =="
  "$PY" "$HERE/replace-colors.py" --root "$ROOT" --check || rc=1
  for pair in "frontend/public/logo.png:512" "frontend/public/favicon.png:64"; do
    f="${pair%%:*}"; want="${pair##*:}"
    if [ -f "$f" ]; then
      got="$(file -b "$f" | grep -oE '[0-9]+ x [0-9]+' | head -1)"
      [ "$got" = "$want x $want" ] && ok "$f $got" || bad "$f 期望 ${want}x${want}，实际 $got"
    else
      bad "$f 不存在"
    fi
  done
  # 横幅渐变必须是「深蓝 → 晨雾蓝 → hover」三档，且不是历史五色渐变
  if grep -q -- "--app-gradient-brand" frontend/src/style/theme.less; then
    g="$(grep -m1 -- "--app-gradient-brand" frontend/src/style/theme.less)"
    n="$(grep -o "#[0-9a-fA-F]\{6\}" <<<"$g" | wc -l)"
    [ "$n" = "3" ] && ok "--app-gradient-brand 三档（深蓝→晨雾蓝→hover），未回退成五色渐变" \
      || bad "--app-gradient-brand 不是三档（$n 档）：$g"
  else
    bad "theme.less 缺少 --app-gradient-brand（横幅会退化成纯色）"
  fi
  if [ "$rc" = "0" ]; then
    echo "== 03-brand 落地完成：配色 / 资源 / 校验全部通过 =="
  else
    echo "== 03-brand 有校验项失败，见上面的 ✘ =="
  fi
fi
exit "$rc"
