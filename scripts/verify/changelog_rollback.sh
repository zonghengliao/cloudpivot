#!/usr/bin/env bash
# ======================================================================
# C7 验证脚本：变更日志「回滚零残留」（POC-4 正式化）
#
# 依据 docs/批次C-技术架构.md §4.12（三层测试）与 §7 C7 行。
#
# 它做什么（一条命令走完 POC-4 的四个动作）：
#
#   ① 查「回滚前」该探测对象的 sys_change_log 行数        → 期望 0
#   ② begin 一个**真实**事务 → changelog::record() 写日志（传 &tx）
#   ③ 同一事务内再查一次行数                              → 期望 1
#   ④ tx.rollback() 强制回滚 → 用连接池再查              → 期望 0
#
# 输出契约（**固定格式，脚本/CI 直接按 | 切三段**）：
#
#   回滚前|事务内|回滚后        正常为  0|1|0
#
# stdout **只有那一行**（诊断一律走 stderr），这样 `bash … | grep -qx '0|1|0'`
# 可以原样用来当门禁。
#
# 用法：
#
#   bash scripts/verify/changelog_rollback.sh
#   C7_DB_URL=postgres://user:pass@host:5432/db bash scripts/verify/changelog_rollback.sh
#   bash scripts/verify/changelog_rollback.sh --verbose     # 失败时也把探针日志打出来
#
# 环境变量（优先级从高到低）：
#   C7_DB_URL          完整连接串（推荐 CI 显式传）
#   CLOUD_LOG_ENV_FILE 需要 source 的 env 文件（默认 /etc/cloudpivot/cloudpivot-test.env，
#                      里面是 ALUN_DATABASE_*；文件不存在就跳过，改用当前环境变量）
#   CARGO_BIN_DIR      cargo 所在目录（默认 ~/.cargo/bin，非登录 shell 里通常不在 PATH）
#
# 退出码：0 = 输出 0|1|0；1 = 有残留/探针失败；2 = 环境不满足（没 cargo、没 DB 连接）。
#
# 前置条件：目标库必须已应用 V0084（sys_change_log 及其月分区），
#          否则探针会因 42P01（表不存在）失败——这是**环境问题**，不是回归。
# ======================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VERBOSE=0
for arg in "$@"; do
    case "$arg" in
        --verbose | -v) VERBOSE=1 ;;
        *)
            echo "未知参数：$arg（用法见脚本头注释）" >&2
            exit 2
            ;;
    esac
done

# 1) 连接串来源：C7_DB_URL > env 文件里的 ALUN_DATABASE_* > 探针内置兜底
if [ -z "${C7_DB_URL:-}" ]; then
    ENV_FILE="${CLOUD_LOG_ENV_FILE:-/etc/cloudpivot/cloudpivot-test.env}"
    if [ -f "$ENV_FILE" ]; then
        set -a
        # shellcheck disable=SC1090
        . "$ENV_FILE" >/dev/null 2>&1 || true
        set +a
        echo "已加载数据库配置：$ENV_FILE" >&2
    else
        echo "未找到 $ENV_FILE，改用当前环境里的 ALUN_DATABASE_*" >&2
    fi
fi

# 2) cargo 不保证在 PATH 里（非登录 shell 的经典坑）
export PATH="${CARGO_BIN_DIR:-$HOME/.cargo/bin}:$PATH"
if ! command -v cargo >/dev/null 2>&1; then
    echo "FAIL：找不到 cargo（可用 CARGO_BIN_DIR 指定，例如 /root/.cargo/bin）" >&2
    exit 2
fi

LOG="$(mktemp)"
trap 'rm -f "$LOG"' EXIT

echo "构建并运行回滚探针（--features test-hooks --example c7_rollback_probe）…" >&2
cd "$ROOT/backend" || exit 2
PROBE_OUT="$(cargo run --quiet --features test-hooks --example c7_rollback_probe 2>"$LOG")"
PROBE_RC=$?

TRIPLE="$(printf '%s\n' "$PROBE_OUT" | sed -n 's/^ROLLBACK_PROBE=//p' | tail -1)"

if [ "$PROBE_RC" -eq 0 ] && [ "$TRIPLE" = "0|1|0" ]; then
    # 契约：stdout 一行，就是这三个采样点
    echo "$TRIPLE"
    echo "PASS：回滚零残留（回滚前 0 / 事务内 1 / 回滚后 0）" >&2
    exit 0
fi

# ---- 失败路径：把真实原因打全（门禁红的时候最需要的是诊断，不是一行 FAIL） ----
echo "FAIL：回滚验证未通过（cargo rc=$PROBE_RC，探针输出『${TRIPLE:-<无>}』）" >&2
echo "---- 探针日志 ----" >&2
sed 's/^/    /' "$LOG" >&2
if [ -n "$TRIPLE" ]; then
    echo "---- 断言明细 ----" >&2
    IFS='|' read -r before inside after <<<"$TRIPLE"
    echo "    回滚前=$before（期望 0）  事务内=$inside（期望 1）  回滚后=$after（期望 0）" >&2
    case "$inside:$after" in
        0:0) echo "    诊断：record 根本没写进事务 → 检查 register/diff（输入前后是否相等）与 mask 规则" >&2 ;;
        *:1) echo "    诊断：事务没回滚或日志走了第二个事务 → 检查是否有人把 &tx 换成 db()（门禁 G2/G5）" >&2 ;;
        *) echo "    诊断：非预期形态，见上方探针日志" >&2 ;;
    esac
fi
exit 1
