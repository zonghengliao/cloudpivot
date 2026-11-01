#!/usr/bin/env bash
# ============================================================================
# P0-FIX 存量数据修复：把「因内控被拦」的 PENDING 待办标成 SKIPPED（留痕），解除审批死锁
#
# 背景（生产实测 PO-20261031-000002）：
#   采购订单审批的 `n_dept` 节点审批人是 `DEPT_LEADER`（当时 0 人绑定）→ 引擎按
#   `empty_assignee=FALLBACK_ADMIN` 回退成 4 个超管 = {admin(发起人), 老六(制单人),
#   老六测试号, Henry.Liao}，会签（`approval_mode=ALL`）要求「人齐」：
#     · Henry.Liao 已批
#     · admin   点自己的票 → 403「不能审批自己提交的」
#     · 老六    点自己的票 → 403（他是制单人）
#     · 老六测试号 没人点
#   ⇒ 那一票永远批不出来 ⇒ **流程死锁**。
#
# 代码侧（本次同一提交）已经把「生成待办时排除制单人/发起人（落 SKIPPED 留痕）」做进引擎，
# 本脚本只处理**存量**：把已经生成、但按内控永远批不出来的 PENDING 改成 SKIPPED，
# 并重算节点人数（`approver_count` 不含 SKIPPED）→ 剩下的人批完即通过。
#
# 口径（硬红线）：
#   · **只动 PENDING** —— APPROVED / REJECTED 是已发生的事实，保留审计；
#   · **不删任何待办行** —— 改成 SKIPPED + `completed_at` + `comment`（谁、为什么）；
#   · 判据按**形态**而不是按单号：`(assignee = 单据 create_by) OR (assignee = 实例发起人)`，
#     覆盖 采购订单审批 / 收货过账 / 收货作废 / 应付付款 四类内控业务（复发也能修）；
#   · 修复前**必须备份**：① 整库 `pg_dump`；② 将被修改的每一行整行快照进
#     `p0fix_self_approval_backup`（可精确回滚，见 --rollback）；
#   · 默认 **dry-run**，加 `--apply` 才真改；生产库（`cloudpivot`）额外要 `--i-know-its-prod`。
#
# 用法：
#   bash scripts/maintenance/p0fix_self_approval.sh                       # dry-run（默认，打印清单）
#   bash scripts/maintenance/p0fix_self_approval.sh --apply               # 备份 + 真改
#   bash scripts/maintenance/p0fix_self_approval.sh --verify              # 只核修复结果
#   bash scripts/maintenance/p0fix_self_approval.sh --rollback --apply    # 按快照精确回滚
#   P0FIX_DB=cloudpivot_p0fix bash scripts/maintenance/p0fix_self_approval.sh --apply
#
# 环境变量：
#   P0FIX_DB        库名（默认取 env 文件的 NIK_DATABASE_NAME）
#   P0FIX_ENV_FILE  读连接信息的 env 文件（默认 /etc/cloudpivot/cloudpivot.env）
#   P0FIX_BACKUP_DIR 整库备份目录（默认 /root/datafix-backup-p0fix-<时间戳>）
# ============================================================================
set -uo pipefail

APPLY=0
PROD_OK=0
MODE="repair"
ENV_FILE="${P0FIX_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}"
COMMENT="内控：制单人/发起人不可自审（P0-FIX 清理，改 SKIPPED 留痕）"

while [ $# -gt 0 ]; do
  case "$1" in
    --apply) APPLY=1 ;;
    --dry-run) APPLY=0 ;;
    --verify) MODE="verify" ;;
    --rollback) MODE="rollback" ;;
    --i-know-its-prod) PROD_OK=1 ;;
    -h | --help)
      sed -n '2,45p' "$0"
      exit 0
      ;;
    *)
      echo "未知参数：$1（用 --help 看用法）" >&2
      exit 2
      ;;
  esac
  shift
done

set -a
# shellcheck disable=SC1090
. "$ENV_FILE" 2>/dev/null
set +a

DB_NAME="${P0FIX_DB:-${NIK_DATABASE_NAME:-}}"
if [ -z "$DB_NAME" ]; then
  echo "找不到库名：请设 P0FIX_DB 或让 $ENV_FILE 提供 NIK_DATABASE_NAME" >&2
  exit 2
fi
if [ "$DB_NAME" = "cloudpivot" ] && [ "$APPLY" = "1" ] && [ "$PROD_OK" = "0" ]; then
  echo "拒绝执行：$DB_NAME 是生产库。确认无误请加 --i-know-its-prod" >&2
  exit 2
fi

export PGPASSWORD="${NIK_DATABASE_PASSWORD:-}"
PGHOST="${NIK_DATABASE_HOST:-127.0.0.1}"
PGPORT="${NIK_DATABASE_PORT:-5432}"
PGUSER="${NIK_DATABASE_USER:-cloudpivot}"
psql_q() { psql -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" -d "$DB_NAME" -v ON_ERROR_STOP=1 "$@"; }

# 内控判据（**唯一真源**，report / apply / verify 三处共用同一段谓词）
# 形态：PENDING 待办 + 审批人是「本单制单人」或「本实例发起人」。
read -r -d '' INNER_FROM <<'SQL'
  FROM biz_flow_task t
  JOIN biz_flow_instance i ON i.id = t.instance_id AND i.deleted_at IS NULL
  LEFT JOIN biz_purchase_order o
         ON i.biz_type = 'PO_APPROVE' AND (i.biz_id = o.id OR i.biz_no = o.order_no)
  LEFT JOIN biz_stock_in s
         ON i.biz_type IN ('STOCK_IN_POST', 'STOCK_IN_CANCEL')
        AND (i.biz_id = s.id OR i.biz_no = s.in_no)
  LEFT JOIN biz_payment p
         ON i.biz_type = 'PAYABLE_PAY' AND (i.biz_id = p.id OR i.biz_no = p.payment_no)
SQL

read -r -d '' INNER_WHERE <<'SQL'
 WHERE t.status = 'PENDING'
   AND i.status = 'PENDING'
   AND (
        (i.biz_type = 'PO_APPROVE'      AND t.assignee_id = o.create_by)
     OR (i.biz_type IN ('STOCK_IN_POST', 'STOCK_IN_CANCEL') AND t.assignee_id = s.create_by)
     OR (i.biz_type = 'PAYABLE_PAY'    AND t.assignee_id = p.create_by)
     OR t.assignee_id = i.initiator_id
   )
SQL

HIT_SQL="SELECT t.id AS task_id, COALESCE(t.assignee_name, u.username, t.assignee_id::text) AS who,
                COALESCE(t.assignee_id::text,'-') AS assignee_id,
                i.instance_no, COALESCE(i.biz_no,'-') AS biz_no, i.biz_type,
                t.node_key,
                CASE WHEN t.assignee_id = i.initiator_id THEN '发起人' ELSE '制单人' END AS why
           ${INNER_FROM}
           LEFT JOIN sys_user u ON u.id = t.assignee_id
           ${INNER_WHERE}
          ORDER BY i.instance_no, t.created_at"

echo "==================================================================="
echo "库：${DB_NAME}@${PGHOST}:${PGPORT}  模式：${MODE}$([ "$APPLY" = "1" ] && echo " / APPLY（真改）" || echo " / DRY-RUN（只看）")"
echo "==================================================================="

case "$MODE" in
  repair)
    echo
    echo "---- ① 待修复清单（内控拦住的 PENDING 待办）----"
    psql_q -P pager=off -c "$HIT_SQL"
    N_HIT=$(psql_q -At -c "SELECT count(*) ${INNER_FROM} ${INNER_WHERE}")
    echo
    echo "命中 PENDING 待办：${N_HIT} 条（APPROVED / REJECTED 一律不动，待办行不删）"

    if [ "${N_HIT:-0}" = "0" ]; then
      echo "无事可做（存量已清 / 代码修复已生效）"
      exit 0
    fi

    if [ "$APPLY" != "1" ]; then
      echo
      echo "DRY-RUN：什么都没改。确认清单无误后加 --apply 真改（会先整库备份）。"
      exit 0
    fi

    # ---------------------------------------------------------------- ② 备份
    BACKUP_DIR="${P0FIX_BACKUP_DIR:-/root/datafix-backup-p0fix-$(date +%Y%m%d-%H%M%S)}"
    mkdir -p "$BACKUP_DIR"
    echo
    echo "---- ② 整库备份 → ${BACKUP_DIR} ----"
    {
      echo "database  : ${DB_NAME} on ${PGHOST}:${PGPORT}"
      echo "taken_at  : $(date -Is)"
      echo "hit_rows  : ${N_HIT}"
      echo "script    : scripts/maintenance/p0fix_self_approval.sh"
    } >"${BACKUP_DIR}/MANIFEST.txt"
    if ! pg_dump -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" -d "$DB_NAME" \
      -f "${BACKUP_DIR}/db.dump.sql"; then
      echo "备份失败，**中止修复**（不冒险改生产）" >&2
      exit 3
    fi
    echo "备份完成：$(du -h "${BACKUP_DIR}/db.dump.sql" | cut -f1)  ${BACKUP_DIR}/db.dump.sql"

    # ---------------------------------------------------------------- ③ 修复（一个事务）
    echo
    echo "---- ③ 修复（单事务：行级快照 → 标 SKIPPED → 重算节点人数）----"
    psql_q -v comment="$COMMENT" <<'SQL'
BEGIN;

-- 行级快照（精确回滚用；幂等：同一 task 只留第一次）
CREATE TABLE IF NOT EXISTS p0fix_self_approval_backup (
    task_id      uuid PRIMARY KEY,
    snapshot     jsonb       NOT NULL,
    backed_up_at timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE p0fix_self_approval_backup IS
    'P0-FIX 内控死锁修复的行级快照（scripts/maintenance/p0fix_self_approval.sh --rollback 用）';

CREATE TEMP TABLE _p0fix_hit ON COMMIT DROP AS
SELECT t.id AS task_id, t.instance_node_id, i.id AS instance_id
  FROM biz_flow_task t
  JOIN biz_flow_instance i ON i.id = t.instance_id AND i.deleted_at IS NULL
  LEFT JOIN biz_purchase_order o
         ON i.biz_type = 'PO_APPROVE' AND (i.biz_id = o.id OR i.biz_no = o.order_no)
  LEFT JOIN biz_stock_in s
         ON i.biz_type IN ('STOCK_IN_POST', 'STOCK_IN_CANCEL')
        AND (i.biz_id = s.id OR i.biz_no = s.in_no)
  LEFT JOIN biz_payment p
         ON i.biz_type = 'PAYABLE_PAY' AND (i.biz_id = p.id OR i.biz_no = p.payment_no)
 WHERE t.status = 'PENDING'
   AND i.status = 'PENDING'
   AND (
        (i.biz_type = 'PO_APPROVE'      AND t.assignee_id = o.create_by)
     OR (i.biz_type IN ('STOCK_IN_POST', 'STOCK_IN_CANCEL') AND t.assignee_id = s.create_by)
     OR (i.biz_type = 'PAYABLE_PAY'    AND t.assignee_id = p.create_by)
     OR t.assignee_id = i.initiator_id
   );

INSERT INTO p0fix_self_approval_backup (task_id, snapshot)
SELECT h.task_id, to_jsonb(t)
  FROM _p0fix_hit h JOIN biz_flow_task t ON t.id = h.task_id
ON CONFLICT (task_id) DO NOTHING;

-- 标 SKIPPED 留痕（ck_biz_flow_task_done：非 PENDING 必须 completed_at 非空）
UPDATE biz_flow_task t
   SET status = 'SKIPPED',
       comment = :'comment',
       completed_at = now()
  FROM _p0fix_hit h
 WHERE t.id = h.task_id AND t.status = 'PENDING';

-- 重算受影响节点的人数（`approver_count` 与引擎口径一致：只数 PENDING/APPROVED/REJECTED）
UPDATE biz_flow_instance_node n
   SET approver_count = agg.total,
       approved_count = agg.approved,
       rejected_count = agg.rejected
  FROM (
        SELECT instance_node_id,
               count(*) FILTER (WHERE status IN ('PENDING','APPROVED','REJECTED')) AS total,
               count(*) FILTER (WHERE status = 'APPROVED') AS approved,
               count(*) FILTER (WHERE status = 'REJECTED') AS rejected
          FROM biz_flow_task
         WHERE instance_node_id IN (SELECT DISTINCT instance_node_id FROM _p0fix_hit)
         GROUP BY instance_node_id
       ) agg
 WHERE n.id = agg.instance_node_id;

COMMIT;
SQL
    echo "修复完成。"
    ;;
  rollback)
    echo
    echo "---- 回滚（按行级快照精确写回 status / comment / completed_at）----"
    if [ "$APPLY" != "1" ]; then
      psql_q -At -c "SELECT count(*) FROM p0fix_self_approval_backup" 2>/dev/null |
        sed 's/^/快照表行数：/'
      echo "DRY-RUN：什么都没改。确认后加 --apply 真回滚。"
      exit 0
    fi
    psql_q <<'SQL'
BEGIN;
UPDATE biz_flow_task t
   SET status = b.snapshot->>'status',
       comment = NULLIF(b.snapshot->>'comment', ''),
       completed_at = (b.snapshot->>'completed_at')::timestamptz
  FROM p0fix_self_approval_backup b
 WHERE t.id = b.task_id AND t.status = 'SKIPPED';
COMMIT;
SQL
    echo "回滚完成（快照表保留，便于复核）。"
    ;;
  verify)
    echo
    echo "---- 修复后核对：每条在跑实例还剩几张待办 / 节点状态 ----"
    psql_q -P pager=off -c "
      SELECT i.instance_no, COALESCE(i.biz_no,'-') AS biz_no, i.biz_type, i.status AS inst_status,
             n.node_key, n.status AS node_status, n.approver_count,
             count(*) FILTER (WHERE t.status = 'PENDING')  AS pending,
             count(*) FILTER (WHERE t.status = 'SKIPPED')  AS skipped,
             count(*) FILTER (WHERE t.status = 'APPROVED') AS approved,
             string_agg(DISTINCT CASE WHEN t.status = 'PENDING' THEN COALESCE(t.assignee_name, t.assignee_id::text) END, ',') AS still_pending
        FROM biz_flow_instance i
        JOIN biz_flow_instance_node n ON n.instance_id = i.id AND n.status = 'ACTIVE'
        LEFT JOIN biz_flow_task t ON t.instance_node_id = n.id AND t.round = n.round
       WHERE i.status = 'PENDING' AND i.deleted_at IS NULL
         AND i.biz_type IN ('PO_APPROVE','STOCK_IN_POST','STOCK_IN_CANCEL','PAYABLE_PAY')
       GROUP BY i.instance_no, i.biz_no, i.biz_type, i.status, n.node_key, n.status, n.approver_count
       ORDER BY i.instance_no"
    echo
    echo "---- 遗留风险：节点 ACTIVE 但已无 PENDING 待办（需要人工推进 / 引擎重算）----"
    psql_q -At -F' | ' -c "
      SELECT i.instance_no, i.biz_no, n.node_key, 'ACTIVE 但 0 张 PENDING'
        FROM biz_flow_instance i
        JOIN biz_flow_instance_node n ON n.instance_id = i.id AND n.status = 'ACTIVE'
       WHERE i.status = 'PENDING' AND i.deleted_at IS NULL
         AND i.biz_type IN ('PO_APPROVE','STOCK_IN_POST','STOCK_IN_CANCEL','PAYABLE_PAY')
         AND NOT EXISTS (SELECT 1 FROM biz_flow_task t
                          WHERE t.instance_node_id = n.id AND t.round = n.round
                            AND t.status = 'PENDING')" |
      sed 's/^/  ⚠ /'
    echo "（没有 ⚠ 行 = 每个在跑节点都还有能审批的人）"
    ;;
esac
