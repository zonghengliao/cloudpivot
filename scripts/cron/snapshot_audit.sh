#!/usr/bin/env bash
# ============================================================
# 快照一致性巡检 —— 每日定时包装脚本（B1）
#
#   cron:  0 2 * * *  /opt/cloudpivot/scripts/cron/snapshot_audit.sh
#   手工:  bash scripts/cron/snapshot_audit.sh            # 立刻跑一轮
#          bash scripts/cron/snapshot_audit.sh --dry-run  # 只列计划，不连库
#
# 它只做三件事：① 日切轮转（保留 30 天）② 跑 snapshot_audit.py ③ 退出码原样透出。
# **不做任何数据修复**（设计文档 AP-8：巡检发现漂移绝不允许自动回写 —— 那等于篡改历史证据）。
#
# 连接与通知均走环境变量（本脚本不存口令）：
#   NIK_DATABASE_HOST/PORT/USER/NAME/PASSWORD   —— 目标库（口令建议写在 600 的 env 文件里）
#   SNAPSHOT_AUDIT_ENV                          —— 可选 env 文件，默认 /etc/cloudpivot/snapshot-audit.env
#   SNAPSHOT_AUDIT_NOTIFY_CMD                   —— 有 BLOCKER 时执行的通知钩子（见脚本尾部说明）
#   SNAPSHOT_AUDIT_KEEP_DAYS                    —— 日志保留天数，默认 30
#
# 退出码：0 无 BLOCKER；1 有 BLOCKER（或某条巡检执行失败）；2 环境/用法错误；
#         3 上一轮还在跑（flock 未拿到）
# ============================================================
set -uo pipefail

REPO=/opt/cloudpivot
PY=${PYTHON:-python3}
AUDIT="$REPO/scripts/verify/snapshot_audit.py"
LOG_DIR=${SNAPSHOT_AUDIT_LOG_DIR:-/var/log/cloudpivot}
LOG="$LOG_DIR/snapshot-audit.log"
JSON="$LOG_DIR/snapshot-audit.latest.json"
KEEP_DAYS=${SNAPSHOT_AUDIT_KEEP_DAYS:-30}

# ① 目标库配置（可选 env 文件；都没有也能跑，靠当前环境变量）
#    查找顺序：$SNAPSHOT_AUDIT_ENV → /etc/cloudpivot/snapshot-audit.env（生产）
#              → ~/.cloudpivot/snapshot-audit.env（本机开发/验证）
for _f in "${SNAPSHOT_AUDIT_ENV:-}" /etc/cloudpivot/snapshot-audit.env "$HOME/.cloudpivot/snapshot-audit.env"; do
    if [ -n "$_f" ] && [ -f "$_f" ]; then
        # shellcheck disable=SC1090
        . "$_f"
        ENV_FILE="$_f"
        break
    fi
done
ENV_FILE=${ENV_FILE:-（未找到 env 文件，使用当前环境变量）}
# source 进来的只是 shell 变量，必须显式导出，否则 python 子进程读不到（实测踩过：
# 日志显示 db=cp_lockorder_t 而实际连的是默认库 cloudpivot，报 "no password supplied"）
export NIK_DATABASE_HOST NIK_DATABASE_PORT NIK_DATABASE_USER NIK_DATABASE_NAME NIK_DATABASE_PASSWORD
export SNAPSHOT_AUDIT_NOTIFY_CMD

mkdir -p "$LOG_DIR" || { echo "无法创建日志目录 $LOG_DIR" >&2; exit 2; }

# ② 日切轮转：日志最后一次写入不是今天 → 按「那天」的日期归档，再删 30 天前的归档
today=$(date +%F)
if [ -s "$LOG" ]; then
    log_day=$(date -r "$LOG" +%F)
    if [ "$log_day" != "$today" ]; then
        mv -f "$LOG" "$LOG.$log_day"
    fi
fi
find "$LOG_DIR" -maxdepth 1 -name 'snapshot-audit.log.*' -type f -mtime "+$KEEP_DAYS" -delete 2>/dev/null

# ③ 跑巡检：人看格式追加进日志；JSON 结果文件覆盖写（B2 的告警接线读它）
{
    echo "──────────────────────────────────────────────────────────────────────────────"
    echo "[$(date '+%F %T')] 开始：db=${NIK_DATABASE_NAME:-cloudpivot}@${NIK_DATABASE_HOST:-127.0.0.1} 配置=$ENV_FILE"
} >> "$LOG"

"$PY" "$AUDIT" --install-views --out "$JSON" --log-file "$LOG" "$@" >> "$LOG" 2>&1
rc=$?

echo "[$(date '+%F %T')] 结束：exit=$rc  日志保留 ${KEEP_DAYS} 天  轮转归档：$(ls -1 "$LOG_DIR"/snapshot-audit.log.* 2>/dev/null | wc -l) 份" >> "$LOG"

if [ "$rc" -ne 0 ]; then
    echo "快照巡检返回非 0（$rc）—— 有 BLOCKER 或执行失败，详见 $LOG" >&2
fi
exit "$rc"
