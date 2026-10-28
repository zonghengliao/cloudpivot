#!/usr/bin/env bash
# =============================================================================
# replay.sh —— 「Pro 到手」演练器：在临时 worktree 上从基线完整重放一遍
#
# 它做的事就是 README 里「半天落地」的六个步骤，但**带自动校验闸门**：
#   每一步跑完都拿结果和「我们真实历史的对应 commit」逐字节对比（git diff 为空），
#   所以「重放成功」不是靠肉眼，而是靠 git 自己比对。
#
# 用法：
#   bash migration-kit/replay.sh                       # 默认：基线 c4e8950 → /tmp/replay-test
#   bash migration-kit/replay.sh --dir /tmp/replay2 --keep
#   bash migration-kit/replay.sh --no-build            # 跳过 cargo test / typecheck
#   bash migration-kit/replay.sh --no-color-diff       # 跳过「重放产物 vs 主线 tip」色值集合对比
#   bash migration-kit/replay.sh --baseline <pro-commit> --expect-patches none \
#        --expect-rename none --expect-final none      # Pro 到手后：只跑流程，不做历史比对
#
# 退出码：0 全过；3 某闸门不匹配；4 构建/验收失败
# =============================================================================
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ---- 门禁/验收；每一步跑完立刻和「历史对应 commit」逐字节比对
# 注意：DIR 默认放 /var/tmp（真实磁盘）。**别用 /tmp** —— 容器/服务器上 /tmp 常是 tmpfs
#       （本机 3.7G），装不下 backend/target（实测 2~3G+），会在 cargo test 时
#       "No space left on device"。临时目录不等于小目录。
BASELINE="c4e8950"
DIR="/var/tmp/replay-test"
KEEP=0
BUILD=1
EXPECT_PATCHES="d5a2b61"
EXPECT_RENAME="df9f658"
# 配色品牌这一步已经**没有**「和我们历史 commit 逐字节比对」的闸门了：
#   v1 用 patch 打，末态 == 46bbe9a（莫兰迪）可比；
#   v2 改成「theme 直放 + 色值替换脚本」，产物与线上只在**注释文案**与
#   一处刻意修正的 var fallback 上有差别（详见 03-brand/brand-points.md §7），
#   所以逐字节闸门改成两个更强的替代：
#     ① 03-brand/apply.sh 自带 --check（关键变量 / 旧青色清零 / 未映射告警）
#     ② ④b 色值字面量多重集合对比（重放产物 vs 主线 tip，必须完全一致）
EXPECT_FINAL="none"
COLOR_DIFF=1

while [ $# -gt 0 ]; do
  case "$1" in
    --baseline) BASELINE="${2:?}"; shift 2 ;;
    --dir) DIR="${2:?}"; shift 2 ;;
    --keep) KEEP=1; shift ;;
    --no-build) BUILD=0; shift ;;
    --expect-patches) EXPECT_PATCHES="${2:?}"; shift 2 ;;
    --expect-rename) EXPECT_RENAME="${2:?}"; shift 2 ;;
    --expect-final) EXPECT_FINAL="${2:?}"; shift 2 ;;
    --no-color-diff) COLOR_DIFF=0; shift ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "未知参数: $1" >&2; exit 2 ;;
  esac
done

RED=$'\033[31m'; GRN=$'\033[32m'; YEL=$'\033[33m'; RST=$'\033[0m'
MAIN="$(git rev-parse --show-toplevel 2>/dev/null)" || { echo "请在仓库内执行" >&2; exit 2; }
gates_fail=0; steps_fail=0
declare -a TIMING

_now_ms() { local t="${EPOCHREALTIME:-}"; t="${t//[!0-9]/}"; if [ -z "$t" ]; then date +%s%3N; else echo $(( t / 1000 )); fi; }

step() { # step <名字> <命令...>
  local name="$1"; shift
  local t0; t0="$(_now_ms)"
  printf "\n${YEL}▶ %s${RST}\n" "$name"
  if "$@"; then
    local el=$(( $(_now_ms) - t0 ))
    TIMING+=("$(printf '%-52s %8dms' "$name" "$el")")
    printf "${GRN}✓ %s 完成（%dms）${RST}\n" "$name" "$el"
    return 0
  else
    local el=$(( $(_now_ms) - t0 ))
    TIMING+=("$(printf '%-52s %8dms  FAILED' "$name" "$el")")
    printf "${RED}✘ %s 失败${RST}\n" "$name"
    steps_fail=$((steps_fail+1))
    return 1
  fi
}

gate() { # gate <描述> <期望 commit>
  local desc="$1" want="${2:-none}"
  [ "$want" = "none" ] && { printf "  ${YEL}– 闸门跳过：%s（--expect 未提供参照 commit）${RST}\n" "$desc"; return 0; }
  if ! git -C "$DIR" rev-parse "$want" >/dev/null 2>&1; then
    printf "  ${YEL}– 闸门跳过：参照 commit %s 不在本仓库${RST}\n" "$want"; return 0
  fi
  local d; d="$(git -C "$DIR" diff --stat "$want" -- . ':!migration-kit' ':!.rename-backup' | tail -1)"
  if git -C "$DIR" diff --quiet "$want" -- . ':!migration-kit' ':!.rename-backup'; then
    printf "  ${GRN}✓ 闸门通过：%s == %s（逐字节一致）${RST}\n" "$desc" "$want"
  else
    printf "  ${RED}✘ 闸门失败：%s != %s${RST}   %s\n" "$desc" "$want" "$d"
    git -C "$DIR" diff --stat "$want" -- . ':!migration-kit' ':!.rename-backup' | head -20 | sed 's/^/      /'
    gates_fail=$((gates_fail+1))
  fi
}

printf "=====================================================================\n"
printf "云枢迁移重放演练\n  基线: %s\n  目录: %s\n  构建: %s\n" "$BASELINE" "$DIR" "$([ "$BUILD" = 1 ] && echo 是 || echo 否)"
printf "=====================================================================\n"

# ---- 0) 准备 worktree
if [ -e "$DIR" ]; then
  echo "目录已存在，先清掉：$DIR" >&2
  git -C "$MAIN" worktree remove --force "$DIR" 2>/dev/null || rm -rf "$DIR"
fi
git -C "$MAIN" worktree prune >/dev/null 2>&1
git -C "$MAIN" worktree add --detach "$DIR" "$BASELINE" >/dev/null 2>&1 || { echo "worktree add 失败（基线 $BASELINE 存在吗？）" >&2; exit 2; }
BASE_SHA="$(git -C "$DIR" rev-parse --short HEAD)"
echo "worktree 就绪 @ $BASE_SHA（detached）"

# 把准备包拷进 worktree（重放时相对路径与真实使用一致；git 里它是未跟踪目录，不参与闸门比对）
rsync -a --exclude '.rename-backup' "$HERE"/ "$DIR/migration-kit"/ 2>/dev/null || cp -a "$HERE" "$DIR/migration-kit"

cd "$DIR" || exit 2
# 注意：worktree 里 HEAD 是 detached 的重放分支，不是主线 tip。
# 最后一步「与参考树差异」必须拿主线 tip 比，所以在进 worktree 前先记下来。
MAIN_TIP="$(git -C "$MAIN" rev-parse HEAD)"

# ---- 1) 打安全/功能补丁序列
step "① 应用 02-security 补丁序列 → 云枢改动前基线" bash migration-kit/02-security/apply.sh \
  && gate "补丁序列终态" "$EXPECT_PATCHES"

# ---- 2) 改名
step "② 执行改名（rename.sh）" bash migration-kit/01-rename/rename.sh --root "$DIR" --no-backup

# ---- 2b) 改名的人工补充（文档横幅 + index.less 注释）：纯文本替换推不出来，作为独立 patch
#        顺序很关键：必须在「改名后、配色前」打 —— 配色补丁要改 index.less，
#        而这次的注释改动正好在同一处上下文。
step "②b 应用改名人工补丁（post-rename-notes.patch）" bash -c '
  cd "$1"
  if git apply --reverse --check migration-kit/01-rename/post-rename-notes.patch 2>/dev/null; then
    echo "  已应用（reverse-check 通过），跳过"; exit 0
  fi
  git apply --whitespace=nowarn migration-kit/01-rename/post-rename-notes.patch \
    || { echo "  [warn] 打不上：基线里这两个文件不同，人工按 special-cases.md 补 4 行说明"; exit 0; }
' _ "$DIR"
gate "改名声明的终态" "$EXPECT_RENAME"

# ---- 3) 改名副作用修正（patch-alun-web 判重改为代码签名）
step "③ 应用改名副作用修正（patch-alun-web 判重）" git apply --whitespace=nowarn migration-kit/01-rename/post-rename-patch-alun-web-dedupe.patch

# ---- 4) 品牌 / 配色（theme 直放 + 组件色值替换）
step "④ 应用配色 + 品牌资源（03-brand：theme 直放 + replace-colors.py）" bash migration-kit/03-brand/apply.sh --root "$DIR" \
  && gate "配色+品牌终态" "$EXPECT_FINAL"

# ---- 4b) 色值一致性：重放产物 vs 主线 tip（替代 v1 的逐字节闸门）
#        逐字节不同是**预期内**的（注释文案 + 一处刻意修正的 var fallback + 上游 CRLF），
#        但「色值字面量多重集合」必须完全一致 —— 这才是「配色没重做歪」的机器判据。
if [ "$COLOR_DIFF" = "1" ]; then
  step "④b 色值一致性对比（重放产物 vs 主线 tip 的色值集合）" bash -c '
    set -u
    KIT_DIR="$1"; MAIN_DIR="$2"; WORK_DIR="$3"
    cmp_py="$KIT_DIR/03-brand/compare-colors.py"
    if [ ! -f "$cmp_py" ]; then echo "  缺少 03-brand/compare-colors.py，跳过对比"; exit 0; fi
    python3 "$cmp_py" "$MAIN_DIR" "$WORK_DIR"
  ' _ "$HERE" "$MAIN" "$DIR"
else
  printf "\n${YEL}▶ ④b 色值一致性对比：--no-color-diff，跳过${RST}\n"
fi

# ---- 5) 模块落地（重置基线路径下应为「一致」，移植路径下应为「新增」）
step "⑤ 落地 business-monitor 模块（04-modules）" bash migration-kit/04-modules/business-monitor/copy.sh --root "$DIR"

# ---- 5b) 本地配置准备（未入库的 config-{dev,test}.toml 从 *.example.toml 生成）
#        少了这步 cargo test 会挂一条（startup_guard 的集成测试读真实配置文件）
step "⑤b 生成未入库的本地配置（prepare-config.sh）" bash migration-kit/prepare-config.sh --root "$DIR"

# ---- 5c) 前端依赖（干净 checkout 没有 node_modules；typecheck 需要）
if [ "$BUILD" = "1" ] && [ ! -d "$DIR/frontend/node_modules" ]; then
  step "⑤c 安装前端依赖（pnpm install --prefer-offline）" bash -c 'cd "$1/frontend" && pnpm install --prefer-offline' _ "$DIR"
else
  printf "\n${Y}▶ ⑤c 前端依赖：已存在或 --no-build，跳过${RST}\n"
fi

# ---- 6) 冲突清单核对
step "⑥ 生成/核对冲突清单（05-conflicts）" bash migration-kit/05-conflicts/gen-changed-files.sh --out /tmp/changed-files-probe.md

# ---- 7) 改名残留 + 静态验收
step "⑦ 改名残留校验（01-rename/verify.sh）" bash migration-kit/01-rename/verify.sh --root "$DIR" --strict
step "⑧ 全量验收（06-verify，含反馈式断言）" bash "$HERE/06-verify/verify.sh" --root "$DIR" $([ "$BUILD" = 0 ] && echo --no-build)

# ---- 8) 汇总
printf "\n=====================================================================\n重放结果\n=====================================================================\n"
for l in "${TIMING[@]}"; do printf "  %s\n" "$l"; done
printf "\n  闸门失败: %s    步骤失败: %s\n" "$gates_fail" "$steps_fail"
printf "\n  与主线 tip（%s）的差异（预期只有 docs/ERP-* 与 docs/README 的文档增量）:\n" "$(git -C "$MAIN" rev-parse --short HEAD)"
git diff --stat "$MAIN_TIP" -- . ':!migration-kit' ':!.rename-backup' 2>/dev/null | tail -10 | sed 's/^/    /'

if [ "$KEEP" = "0" ]; then
  printf "\n清理 worktree: %s\n" "$DIR"
  cd "$MAIN" && git worktree remove --force "$DIR" && echo "已清理（--keep 可保留现场）"
else
  printf "\n保留现场: %s（验证完记得 git worktree remove --force）\n" "$DIR"
fi

rc=0
[ "$gates_fail" -gt 0 ] && rc=3
[ "$steps_fail" -gt 0 ] && rc=4
exit "$rc"
