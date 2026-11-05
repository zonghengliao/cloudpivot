#!/usr/bin/env bash
# main_drift_check.sh —— 本地 main 与 origin/main 的关系体检（只读，不动历史）
#
# 为什么有这个脚本（2026-11-05，卡 t_72c90ae4）：
#   共享工作树 /opt/cloudpivot 上多人（多 worker）并行提交，而 144 bare main 曾被
#   「分支 tip rebase 重放」的方式推进 ⇒ 两条历史互相 ahead/behind，`git push` 报
#   non-fast-forward。此时**任何一方按直觉 force push / rebase 都会抹掉对方提交**。
#   所以先体检、先开卡定口径，再动手。
#
# 判据（退出码）：
#   0  线性（同步 / 仅 ahead / 仅 behind）—— 仅 ahead 时可正常 push；仅 behind 时可 ff
#   3  分叉（既 ahead 又 behind）—— **禁止** push -f / rebase，先按下面的指引定口径
#   2  环境问题（不在 git 仓 / 取不到 ref）
#
# 用法：
#   scripts/git/main_drift_check.sh              # 用已有 remote-tracking ref（不联网）
#   scripts/git/main_drift_check.sh --fetch      # 先 git fetch origin（联网）
#   scripts/git/main_drift_check.sh --branch dev # 换分支
set -u

BRANCH=main
FETCH=0
while [ $# -gt 0 ]; do
  case "$1" in
    --fetch) FETCH=1 ;;
    --branch) shift; BRANCH="${1:-main}" ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "未知参数: $1" >&2; exit 2 ;;
  esac
  shift
done

ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || { echo "不在 git 仓库内" >&2; exit 2; }
cd "$ROOT" || exit 2

if [ "$FETCH" = 1 ]; then
  git fetch origin --prune >/dev/null 2>&1 || { echo "git fetch origin 失败（网络/权限）" >&2; exit 2; }
fi

git rev-parse --verify -q "refs/heads/$BRANCH" >/dev/null || { echo "本地没有分支 $BRANCH" >&2; exit 2; }
git rev-parse --verify -q "refs/remotes/origin/$BRANCH" >/dev/null || {
  echo "没有 origin/$BRANCH（先 scripts/git/main_drift_check.sh --fetch）" >&2; exit 2; }

AHEAD=$(git rev-list --count "origin/$BRANCH..$BRANCH")
BEHIND=$(git rev-list --count "$BRANCH..origin/$BRANCH")
HEAD_SHA=$(git rev-parse --short "$BRANCH")
ORIG_SHA=$(git rev-parse --short "origin/$BRANCH")

echo "分支        : $BRANCH"
echo "本地        : $HEAD_SHA  $(git log -1 --format='%ci %s' "$BRANCH" | cut -c1-70)"
echo "origin      : $ORIG_SHA  $(git log -1 --format='%ci %s' "origin/$BRANCH" | cut -c1-70)"
echo "ahead/behind: $AHEAD / $BEHIND"

if [ "$AHEAD" -eq 0 ] && [ "$BEHIND" -eq 0 ]; then
  echo "结论        : 同步 ✅"
  exit 0
fi

if [ "$AHEAD" -gt 0 ] && [ "$BEHIND" -gt 0 ]; then
  echo "结论        : **分叉** ❌ 两条历史都有对方没有的提交"
  echo
  echo "禁止操作：git push -f / git push --force-with-lease / git rebase origin/$BRANCH"
  echo "（谁先强推谁就把对方的提交抹掉，且 144 部署直接从 bare 拉）"
  echo
  echo "应当先看两边独有提交，再定口径（union merge 优先，不丢任何一方）："
  echo "  git log --left-right --cherry-mark --oneline origin/$BRANCH...$BRANCH | grep '^[<=>]'"
  echo "  git diff --stat $BRANCH origin/$BRANCH | tail -3"
  echo "口径与流程见 docs/evidence/20261105-repo-union-merge/README.md"
  exit 3
fi

if [ "$AHEAD" -gt 0 ]; then
  echo "结论        : 本地领先 $AHEAD 条，可正常 push（无需要 -f）"
else
  echo "结论        : 本地落后 $BEHIND 条，可快进：git merge --ff-only origin/$BRANCH"
fi
exit 0
