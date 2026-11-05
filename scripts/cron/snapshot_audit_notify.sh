#!/usr/bin/env bash
# ============================================================
# 快照一致性巡检 —— BLOCKER 通知钩子（144 生产侧）
#
#   由 snapshot_audit.py 在「有 BLOCKER / 巡检项执行失败」时调用：
#     SNAPSHOT_AUDIT_JSON / SNAPSHOT_AUDIT_BLOCKERS / SNAPSHOT_AUDIT_SUMMARY 由巡检注入
#   配置位置：/etc/cloudpivot/snapshot-audit.env 的 SNAPSHOT_AUDIT_NOTIFY_CMD
#
#   通知通道沿用团队既有告警链路：ssh 到 172.18.10.99 调 feishu_alert_send.py
#   （飞书 Open API v1 卡片，凭据留在 99，不在生产机上另存一份 bot secret）。
#
#   --dry-run  只打印将要发送的正文与远端命令，不发任何消息（验接线用）
# ============================================================
set -uo pipefail

DRY=0
[ "${1:-}" = "--dry-run" ] && DRY=1
[ "${SNAPSHOT_AUDIT_NOTIFY_DRYRUN:-0}" = "1" ] && DRY=1

BLOCKERS=${SNAPSHOT_AUDIT_BLOCKERS:-?}
SUMMARY=${SNAPSHOT_AUDIT_SUMMARY:-快照一致性巡检检出 ${BLOCKERS} 条 BLOCKER}
JSON=${SNAPSHOT_AUDIT_JSON:-/var/log/cloudpivot/snapshot-audit.latest.json}
HOST=$(hostname -I 2>/dev/null | awk '{print $1}'); HOST=${HOST:-$(hostname -s)}
REMOTE_USER=root
REMOTE_HOST=172.18.10.99
SENDER=/root/.hermes/profiles/backend-developer/scripts/feishu_alert_send.py

BODY=$(cat <<EOF
**${SUMMARY}**

- 主机：${HOST}（生产）
- BLOCKER：${BLOCKERS}
- 结果文件：\`${JSON}\`
- 全量日志：\`/var/log/cloudpivot/snapshot-audit.log\`
- 处置：按《快照一致性巡检与告警设计》AP-8 —— 巡检只报不修，人工看图后决定改数据还是改口径；
  确认是「历史正常」就走 DI 豁免台账（精确到行 id + 期限），**不要停 cron、不要删视图**。

抓取现场：\`sed -n '/BLOCKER/,\$p' ${JSON}\` 或直接 tail 日志。
EOF
)

if [ "$DRY" = "1" ]; then
    echo "── DRY-RUN（不会发送任何消息）──"
    echo "目标：feishu 团队告警频道（经 ${REMOTE_HOST}:${SENDER}）"
    echo "── 卡片正文 ──"
    echo "$BODY"
    echo "── 将会执行的命令 ──"
    echo "printf '%s' \"\$BODY\" | ssh ${REMOTE_USER}@${REMOTE_HOST} python3 ${SENDER} --subject '生产快照巡检 BLOCKER ${BLOCKERS}' --stdin"
    exit 0
fi

if ! printf '%s\n' "$BODY" | timeout 45 ssh -o BatchMode=yes -o ConnectTimeout=8 \
        "${REMOTE_USER}@${REMOTE_HOST}" "python3 ${SENDER} --subject '生产快照巡检 BLOCKER ${BLOCKERS}' --stdin"; then
    echo "通知投递失败（ssh/飞书）—— 告警仍已写入 /var/log/cloudpivot/snapshot-audit.log" >&2
    exit 1
fi
exit 0
