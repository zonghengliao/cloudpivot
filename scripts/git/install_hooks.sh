#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════════════
# install_hooks.sh — 安装仓库自带的 git 钩子（幂等，可重复执行）
#
# 为什么需要它：钩子放在 `.githooks/`（**入库**），但让 git 去用它靠的是
#   `git config core.hooksPath .githooks` —— 这条配置在 `.git/config` 里，
#   **不入库**。所以新人克隆、CI 机器、以及 145/144 上的部署仓，默认都是
#   「钩子文件在树上、但一个都不生效」。卡 t_2746ca53 的审查结论
#   （5.7 门禁被绕过）有一部分就是这么来的：2026-11-09 有人把 rustfmt 检查写进了
#   `.git/hooks/pre-commit`，同一天 `core.hooksPath` 切成 `.githooks`，
#   那份检查从此成了死代码，而没人发现。
#
# 用法：
#   bash scripts/git/install_hooks.sh          # 安装 / 修复（幂等）
#   bash scripts/git/install_hooks.sh --check  # 只体检：未安装或不可执行则 exit 1
#
# 退出码：0 = 已就绪；1 = --check 模式下未就绪（或安装失败）；2 = 不在 git 仓
# ═══════════════════════════════════════════════════════════════════════════
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT" || exit 2

MODE=install
for arg in "$@"; do
    case "$arg" in
        --check) MODE=check ;;
        -h|--help) sed -n '2,26p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "未知参数：$arg（支持 --check / --help）" >&2; exit 2 ;;
    esac
done

if ! git rev-parse --git-dir >/dev/null 2>&1; then
    echo "✘ 不在 git 仓库里：$ROOT" >&2
    exit 2
fi

HOOKS_DIR=".githooks"
EXPECTED_HOOKS="pre-commit pre-push post-commit"

rc=0
report_ok()   { echo "  ✓ $*"; }
report_bad()  { echo "  ✘ $*" >&2; rc=1; }

echo "==> 钩子体检（$HOOKS_DIR）"

# ① 钩子目录与文件
if [ ! -d "$HOOKS_DIR" ]; then
    report_bad "$HOOKS_DIR/ 不存在（这个分支不含钩子）"
else
    for h in $EXPECTED_HOOKS; do
        if [ ! -f "$HOOKS_DIR/$h" ]; then
            report_bad "缺 $HOOKS_DIR/$h"
        elif [ ! -x "$HOOKS_DIR/$h" ]; then
            if [ "$MODE" = "install" ]; then
                chmod +x "$HOOKS_DIR/$h" && report_ok "$HOOKS_DIR/$h 已补上可执行位"
            else
                report_bad "$HOOKS_DIR/$h 没有可执行位（git 会静默跳过它）"
            fi
        else
            report_ok "$HOOKS_DIR/$h 存在且可执行"
        fi
    done
fi

# ② core.hooksPath
current="$(git config --get core.hooksPath 2>/dev/null || true)"
if [ "$current" = "$HOOKS_DIR" ]; then
    report_ok "core.hooksPath = $HOOKS_DIR"
elif [ "$MODE" = "install" ]; then
    git config core.hooksPath "$HOOKS_DIR" || { report_bad "设置 core.hooksPath 失败"; }
    now="$(git config --get core.hooksPath 2>/dev/null || true)"
    if [ "$now" = "$HOOKS_DIR" ]; then
        report_ok "core.hooksPath 已设置（原值：${current:-未设置}）→ $now"
    else
        report_bad "core.hooksPath 设置后仍为「$now」"
    fi
else
    report_bad "core.hooksPath = 「${current:-未设置}」，应为 $HOOKS_DIR（钩子文件在，但一个都不会跑）"
fi

# ③ 提醒：.git/hooks 里的同名文件现在是死代码（不会被执行）
if [ -d .git/hooks ]; then
    for h in $EXPECTED_HOOKS; do
        if [ -f ".git/hooks/$h" ]; then
            echo "  ℹ .git/hooks/$h 存在但**不会被执行**（core.hooksPath 已指向 $HOOKS_DIR）"
            echo "    那是 2026-11-09 的遗留物；判定逻辑已迁进 $HOOKS_DIR/$h，可安全删除："
            echo "    rm .git/hooks/$h"
        fi
    done
fi

echo ""
if [ "$MODE" = "check" ]; then
    if [ "$rc" -ne 0 ]; then
        echo "✘ 钩子未就绪：bash scripts/git/install_hooks.sh" >&2
        exit 1
    fi
    echo "✓ 钩子已就绪（$EXPECTED_HOOKS）"
    exit 0
fi

if [ "$rc" -ne 0 ]; then
    echo "✘ 安装未完成，见上面的 ✘ 行" >&2
    exit 1
fi

echo "✓ 钩子安装完成。生效范围："
echo "    · commit 前：密钥扫描 / 库存写入口 / 暂存 .rs 的 rustfmt"
echo "    · push   前：bash scripts/check.sh --fast（L0 静态闸 + L1 编译与类型闸，≈2–3 分钟）"
echo "    · 发版   前：bash scripts/check.sh --full（再加 L2 连库巡检 + L3 干净检出闸）"
exit 0
