#!/usr/bin/env bash
# ============================================================
# 审批流残留体检（只读）—— 防「e2e 收尾漏清 → 夹具用户被 biz_flow_task 钉死」复发
#
# 背景（t_15be8cc3）：调拨 e2e 只删单据不删 biz_flow_instance / biz_flow_task 时，
#   审批待办会永久挂 PENDING；`biz_flow_task.assignee_id` 是硬外键，于是持 SUPER_ADMIN
#   的测试夹具用户（`tc_r_op_*` / `tc_u_op_*` …）既软删不掉也硬删不了（外键保留豁免），
#   每跑一遍就多一批「永远删不掉的软删用户」。
#
# 用法：
#   AUDIT_DSN='postgres://user:pw@127.0.0.1:5432/cloudpivot_test' \
#     bash scripts/verify/flow_residue_audit.sh            # 只体检（恒 0 退出）
#   ... bash scripts/verify/flow_residue_audit.sh --strict # 有残留时退出 1（CI 用）
#
# 环境变量：
#   AUDIT_DSN           连接串（也可用标准 PG* 变量）
#   AUDIT_FIXTURE_REGEX 夹具用户名正则（默认 '^tc_'，即所有 tc_ 前缀用例夹具）
#   AUDIT_STALE_MINUTES 实例悬挂多久算残留（默认 30 分钟；正在跑的 e2e 不算）
# ============================================================
set -uo pipefail

STRICT=0
[ "${1:-}" = "--strict" ] && STRICT=1
REGEX="${AUDIT_FIXTURE_REGEX:-^tc_}"
STALE="${AUDIT_STALE_MINUTES:-30}"

# 只读脚本，但仍要挡住拼接注入：STALE 必须是数字，REGEX 不许带单引号
case "$STALE" in
    '' | *[!0-9]*)
        echo "AUDIT_STALE_MINUTES 必须是正整数：[$STALE]" >&2
        exit 2
        ;;
esac
case "$REGEX" in
    *"'"*)
        echo "AUDIT_FIXTURE_REGEX 里不许出现单引号：[$REGEX]" >&2
        exit 2
        ;;
esac

PSQL=(psql -At -F '|' -v ON_ERROR_STOP=1)
if [ -n "${AUDIT_DSN:-}" ]; then
    PSQL+=("$AUDIT_DSN")
fi

q() { "${PSQL[@]}" -c "$1"; }

DB=$(q "SELECT current_database() || ' @ ' || current_user")
echo "== 审批流残留体检（只读）  ${DB:-<未连上>}  regex=$REGEX  stale=${STALE}m"

echo
echo "-- ① biz_flow_task 按状态"
q "SELECT status, count(*) FROM biz_flow_task GROUP BY status ORDER BY 1" | sed 's/^/   /'

echo "-- ② biz_flow_instance 按业务类型 × 状态（PENDING 或近 ${STALE} 分钟创建/更新）"
q "SELECT biz_type, status, count(*) FROM biz_flow_instance
    WHERE status = 'PENDING' OR started_at > now() - interval '${STALE} minutes'
    GROUP BY 1, 2 ORDER BY 1, 2" | sed 's/^/   /'

echo
echo "-- ③ 残留判定"
# 判据 A：PENDING 待办挂在夹具用户上（= 被钉死）
PINNED=$(q "SELECT count(*) FROM biz_flow_task t JOIN sys_user u ON u.id = t.assignee_id
             WHERE t.status = 'PENDING' AND u.username ~ '${REGEX}'")
# 判据 B：调拨单已到终态/不存在，但审批实例还 PENDING（= e2e 收尾漏清）
ORPHAN=$(q "SELECT count(*) FROM biz_flow_instance i
             WHERE i.biz_type = 'STOCK_TRANSFER' AND i.status = 'PENDING'
               AND i.started_at < now() - interval '${STALE} minutes'
               AND NOT EXISTS (SELECT 1 FROM biz_stock_transfer t
                                WHERE t.id = i.biz_id AND t.status IN ('PENDING','APPROVED','IN_TRANSIT'))")
# 判据 C：PENDING 待办挂在**已软删**的用户上（= 软删夹具用户被钉住的痕迹）
DEAD=$(q "SELECT count(*) FROM biz_flow_task t JOIN sys_user u ON u.id = t.assignee_id
           WHERE t.status = 'PENDING' AND u.deleted_at IS NOT NULL")
# 判据 D：被业务外键保留、删不掉的软删夹具用户（信息项，不单独判红）
KEPT=$(q "SELECT count(DISTINCT u.id) FROM sys_user u
           WHERE u.username ~ '${REGEX}' AND u.deleted_at IS NOT NULL
             AND EXISTS (SELECT 1 FROM biz_flow_task t WHERE t.assignee_id = u.id)")

printf '   A 夹具用户被挂 PENDING 待办        : %s 条\n' "${PINNED:-?}"
printf '   B 超 %s 分钟仍 PENDING 的调拨实例 : %s 条\n' "$STALE" "${ORPHAN:-?}"
printf '   C PENDING 待办挂在已软删用户上      : %s 条\n' "${DEAD:-?}"
printf '   D 被业务外键保留的软删夹具用户      : %s 个（信息项）\n\n' "${KEPT:-?}"

if [ "${PINNED:-0}" = "0" ] && [ "${ORPHAN:-0}" = "0" ] && [ "${DEAD:-0}" = "0" ]; then
    echo "结论：干净（A/B/C 全 0）—— 没有 e2e 残留把夹具用户钉在审批待办上。"
    exit 0
fi

echo "结论：发现残留。收尾口径见 scripts/verify/lib/flow_cleanup.sh（先终止待办/节点/实例，再删单据）。"
echo "      只按前缀收敛，别全表删；别人正在跑的 e2e（判据 B 已用 ${STALE} 分钟窗口排除）。"
[ "$STRICT" = "1" ] && exit 1
exit 0
