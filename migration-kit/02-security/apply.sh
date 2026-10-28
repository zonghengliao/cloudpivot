#!/usr/bin/env bash
# =============================================================================
# 02-security/apply.sh —— 把安全整改（含前置必要改动）的补丁序列打到当前分支
#
# 补丁序列 = c4e8950..d5a2b61（我们全部 15 个改动提交，按时间顺序），
#   为什么不是「只挑 9 个安全提交」：git 补丁是按父提交上下文生成的，
#   挑着打会因上下文缺失/文件缺位而失败（实测 d5a2b61 依赖 55d3c5d 新建的
#   dashboard/model.rs）。所以补丁序列 = 完整改动序列，安全补丁是其中 9 个。
#
# 用法：
#   bash 02-security/apply.sh --check      # 只判断能否干净应用（git apply --check）
#   bash 02-security/apply.sh              # 真打（git am 逐条）
#   bash 02-security/apply.sh --abort      # 出事回滚：git am --abort 并回到起点
#
# 前置条件：当前目录是「上游基线」分支/工作区，且工作区干净。
# 退出码：0 全部应用；2 参数/环境错误；3 某条补丁失败（已打印人工处理指引）
# =============================================================================
set -uo pipefail

MODE="apply"
KIT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PATCH_DIR="$KIT_DIR/patches"
HOOKS_OFF=""

while [ $# -gt 0 ]; do
  case "$1" in
    --check)  MODE="check"; shift ;;
    --abort)  MODE="abort"; shift ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "未知参数: $1" >&2; exit 2 ;;
  esac
done

RED=$'\033[31m'; GRN=$'\033[32m'; YEL=$'\033[33m'; RST=$'\033[0m'
die() { printf "${RED}✘ %s${RST}\n" "$*" >&2; exit 2; }

git rev-parse --git-dir >/dev/null 2>&1 || die "当前目录不是 git 仓库"
ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT" || die "进不去 $ROOT"

# 关掉仓库 hooks：post-commit 会往 origin(bare) 推，重放/演练时绝不能污染 bare
mkdir -p /tmp/.cloudpivot-no-hooks
# 提交者身份：am 必须有 committer identity，全新机器/容器里常常没配。
# 不覆盖已有配置；没有就用占位身份（Author 仍取补丁里的原作者，不受影响）。
ID_ARGS=()
if [ -z "$(git config user.email || true)" ]; then
  ID_ARGS=(-c user.name=migration-kit -c user.email=migration-kit@local)
  echo "提示：本机未配 git user.email，本次 am 使用占位提交者 migration-kit@local（Author 不变）"
fi
GIT() { git -c core.hooksPath=/tmp/.cloudpivot-no-hooks "${ID_ARGS[@]}" "$@"; }

mapfile -t PATCHES < <(ls "$PATCH_DIR"/*.patch 2>/dev/null | sort)
[ "${#PATCHES[@]}" -gt 0 ] || die "找不到补丁：$PATCH_DIR/*.patch"

if [ "$MODE" = "abort" ]; then
  git am --abort 2>/dev/null || true
  echo "已执行 git am --abort（若之前不在 am 状态则无副作用）"
  exit 0
fi

BRANCH="$(git symbolic-ref --short -q HEAD || echo 'DETACHED')"
echo "仓库: $ROOT"
echo "分支: $BRANCH   补丁数: ${#PATCHES[@]}"
# 只看「已跟踪文件的改动」：未跟踪目录（如拷进来的 migration-kit/）不阻塞 am，
# 因为补丁序列不创建这些路径；真要冲突 git 自己会拒绝。
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
  git status --porcelain --untracked-files=no | sed 's/^/  /' >&2
  die "工作区有未提交的跟踪文件改动，先 commit/stash"
fi
untracked="$(git status --porcelain --untracked-files=normal | grep '^??' || true)"
[ -n "$untracked" ] && echo "提示：有未跟踪文件（不影响 am）：$(printf '%s' "$untracked" | head -3 | tr '\n' ' ')"

if [ "$MODE" = "check" ]; then
  echo "== 逐条 check（用 3way 理论可应用性判断，不落盘）=="
  fails=0
  for p in "${PATCHES[@]}"; do
    if git apply --check --3way "$p" >/dev/null 2>&1; then
      printf "  ${GRN}✓${RST} %s\n" "$(basename "$p")"
    else
      printf "  ${YEL}?${RST} %s （对当前树不能直接 apply，序列内顺序应用后通常可过）\n" "$(basename "$p")"
      fails=$((fails+1))
    fi
  done
  echo "提示：${fails} 条对当前树不能单独 apply —— 这是正常的（补丁之间有先后依赖），真正的判定看顺序 am 的结果。"
  exit 0
fi

START_HEAD="$(git rev-parse --short HEAD)"
echo "起点 HEAD: $START_HEAD"
echo "== 逐条 git am（3way，关闭 hooks）=="
for p in "${PATCHES[@]}"; do
  name="$(basename "$p")"
  if GIT am -3 --keep-cr --no-verify "$p" >/tmp/am.out 2>&1; then
    printf "  ${GRN}✓${RST} %s → %s\n" "$name" "$(git log -1 --format=%h)"
  else
    printf "  ${RED}✘${RST} %s 应用失败\n" "$name"
    echo "---------------- 失败详情 ----------------"
    cat /tmp/am.out
    echo "---------------- 冲突文件 ----------------"
    git status --porcelain | sed 's/^/  /'
    cat <<EOF

人工处理指引（先看 02-security/design-notes.md 里对应补丁那一节）：
  1) 看冲突：  git status; git diff
  2) 手工改到「补丁想表达的终态」，然后：
       git add -A && git am --continue
     （改动太大不想 continue 就先 git am --abort，再 git apply --reject "$p" 挑 hunk 手工合并）
  3) 实在打不动：git am --abort 回到 $START_HEAD，按 design-notes.md 的
     「若 apply 失败如何手工重做」逐条手改（每个补丁都写了改哪些文件、核心逻辑）。
  已应用的补丁不会丢：git log --oneline $START_HEAD..HEAD
EOF
    exit 3
  fi
done

echo "---------------------------------------------------------------------"
echo "应用完成：$START_HEAD → $(git rev-parse --short HEAD)（$(git rev-list --count "$START_HEAD"..HEAD) 个提交）"
echo "下一步：bash ../01-rename/rename.sh --dry-run（注意顺序：补丁先打，改名后做）"
