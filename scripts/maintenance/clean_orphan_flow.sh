#!/usr/bin/env bash
# ============================================================================
# T12 一次性清理：任务已删 / 已不存在，但审批单还挂在 PENDING 的**孤儿实例**
#
# 背景：任务删除是**软删**（`biz_task.deleted_at`），`biz_flow_instance` 不会跟着动
#       → 孤儿实例一直挂在「审批待办」里，点进去任务已不存在。
#       实测存量 27 条孤儿（20 APPROVED + 1 CANCELLED + 5 PENDING 已人工清掉）。
#       T12 已在 `DELETE /business/task/{id}` 的**同一事务**里从根上修掉，
#       本脚本只处理**存量**（上线后跑一次即可，之后应当恒为 0 命中）。
#
# 口径（硬红线）：
#   · **只清 PENDING** —— APPROVED / REJECTED / WITHDRAWN 是已完成的事实，保留审计；
#   · 精确到「实例 ↔ 业务行」：`biz_id = biz_task.id`（`biz_id` 为空的历史行按
#     `task_no = biz_no` 对），不做「按时间 / 按标题」这类模糊批量；
#   · 默认 **dry-run**（只打印将受影响的实例），加 `--apply` 才真改；
#   · 生产库（`cloudpivot`）额外要 `--i-know-its-prod`。
#
# 用法：
#   bash scripts/maintenance/clean_orphan_flow.sh                  # dry-run（默认）
#   bash scripts/maintenance/clean_orphan_flow.sh --apply          # 真清
#   T12_DB=cloudpivot_t12 bash scripts/maintenance/clean_orphan_flow.sh --apply
#   bash scripts/maintenance/clean_orphan_flow.sh --biz-type TASK_ACCEPT
#
# 环境变量：
#   T12_DB        库名（默认取 env 文件的 NIK_DATABASE_NAME）
#   T12_ENV_FILE  读连接信息的 env 文件，默认 /etc/cloudpivot/cloudpivot-dev.env
# ============================================================================
set -uo pipefail

APPLY=0
PROD_OK=0
BIZ_TYPE="TASK_ACCEPT"
ENV_FILE="${T12_ENV_FILE:-/etc/cloudpivot/cloudpivot-dev.env}"
COMMENT="任务已删除，审批单自动终止"

while [ $# -gt 0 ]; do
  case "$1" in
    --apply) APPLY=1 ;;
    --dry-run) APPLY=0 ;;
    --i-know-its-prod) PROD_OK=1 ;;
    --biz-type)
      shift
      BIZ_TYPE="${1:-TASK_ACCEPT}"
      ;;
    -h | --help)
      sed -n '2,30p' "$0"
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

DB_NAME="${T12_DB:-${NIK_DATABASE_NAME:-}}"
if [ -z "$DB_NAME" ]; then
  echo "找不到库名：请设 T12_DB 或让 $ENV_FILE 提供 NIK_DATABASE_NAME" >&2
  exit 2
fi
if [ "$DB_NAME" = "cloudpivot" ] && [ "$APPLY" = "1" ] && [ "$PROD_OK" = "0" ]; then
  echo "拒绝执行：$DB_NAME 是生产库。确认无误请加 --i-know-its-prod" >&2
  exit 2
fi

export PGPASSWORD="${NIK_DATABASE_PASSWORD:-}"
PSQL=(psql -h "${NIK_DATABASE_HOST:-127.0.0.1}" -p "${NIK_DATABASE_PORT:-5432}"
  -U "${NIK_DATABASE_USER:-cloudpivot}" -d "$DB_NAME" -v ON_ERROR_STOP=1
  -v biz_type="$BIZ_TYPE" -v comment="$COMMENT")

echo "库：$DB_NAME  业务类型：$BIZ_TYPE  模式：$([ "$APPLY" = "1" ] && echo 'APPLY（真改）' || echo 'DRY-RUN（只看）')"
echo

# ---------------------------------------------------------------- ① 盘点（两种模式都先看）
"${PSQL[@]}" -At -F' | ' <<'SQL'
SELECT i.instance_no,
       coalesce(i.biz_no, '-')                    AS biz_no,
       i.status,
       coalesce(to_char(t.deleted_at, 'YYYY-MM-DD HH24:MI'), '<任务行不存在>') AS biz_state
  FROM biz_flow_instance i
  LEFT JOIN biz_task t
         ON (t.id = i.biz_id) OR (i.biz_id IS NULL AND t.task_no = i.biz_no)
 WHERE i.status = 'PENDING'
   AND i.deleted_at IS NULL
   AND i.biz_type = :'biz_type'
   AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
 ORDER BY i.started_at
SQL

N_ORPHAN="$("${PSQL[@]}" -At <<'SQL'
SELECT count(*)
  FROM biz_flow_instance i
  LEFT JOIN biz_task t
         ON (t.id = i.biz_id) OR (i.biz_id IS NULL AND t.task_no = i.biz_no)
 WHERE i.status = 'PENDING'
   AND i.deleted_at IS NULL
   AND i.biz_type = :'biz_type'
   AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
SQL
)"
echo "命中 PENDING 孤儿实例：${N_ORPHAN} 条（APPROVED / REJECTED / WITHDRAWN 一律不动）"

if [ "${N_ORPHAN:-0}" = "0" ]; then
  echo "无事可做（存量已清 / 修复已生效）"
  exit 0
fi

if [ "$APPLY" != "1" ]; then
  echo
  echo "DRY-RUN：什么都没改。确认清单无误后加 --apply 真清。"
  exit 0
fi

# ---------------------------------------------------------------- ② 真清（一个事务，三条批量语句）
echo
echo "开始清理（一个事务：先待办、再节点、最后主表）……"
"${PSQL[@]}" <<'SQL'
BEGIN;

CREATE TEMP TABLE _t12_orphan ON COMMIT DROP AS
SELECT i.id
  FROM biz_flow_instance i
  LEFT JOIN biz_task t
         ON (t.id = i.biz_id) OR (i.biz_id IS NULL AND t.task_no = i.biz_no)
 WHERE i.status = 'PENDING'
   AND i.deleted_at IS NULL
   AND i.biz_type = :'biz_type'
   AND (t.id IS NULL OR t.deleted_at IS NOT NULL);

-- ① 审批待办（ck_biz_flow_task_done：非 PENDING 必须 completed_at 非空）
UPDATE biz_flow_task
   SET status = 'CANCELLED',
       comment = COALESCE(comment, :'comment'),
       completed_at = now()
 WHERE status = 'PENDING'
   AND instance_id IN (SELECT id FROM _t12_orphan);

-- ② 实例节点（与 flow::model::cancel_active_nodes 同口径）
UPDATE biz_flow_instance_node
   SET status = 'CANCELLED',
       ended_at = now(),
       duration_ms = (EXTRACT(EPOCH FROM (now() - COALESCE(activated_at, now()))) * 1000)::bigint,
       updated_at = now()
 WHERE status IN ('ACTIVE','PENDING')
   AND instance_id IN (SELECT id FROM _t12_orphan);

-- ③ 实例主表（ck_biz_flow_instance_end：status != PENDING 时 ended_at 必须非空）
UPDATE biz_flow_instance
   SET status = 'CANCELLED',
       ended_at = now(),
       result_comment = :'comment',
       revision = revision + 1,
       updated_at = now()
 WHERE status = 'PENDING'
   AND id IN (SELECT id FROM _t12_orphan);

COMMIT;
SQL

LEFT="$("${PSQL[@]}" -At <<'SQL'
SELECT count(*)
  FROM biz_flow_instance i
  LEFT JOIN biz_task t
         ON (t.id = i.biz_id) OR (i.biz_id IS NULL AND t.task_no = i.biz_no)
 WHERE i.status = 'PENDING'
   AND i.deleted_at IS NULL
   AND i.biz_type = :'biz_type'
   AND (t.id IS NULL OR t.deleted_at IS NOT NULL)
SQL
)"
echo
echo "清理后剩余 PENDING 孤儿：${LEFT} 条"
[ "${LEFT:-0}" = "0" ] || {
  echo "仍有残留，请人工复核（可能并发新增，或 biz_type 不只 $BIZ_TYPE）" >&2
  exit 1
}
echo "完成。已完成（APPROVED / REJECTED / WITHDRAWN）的审批单一条都没动。"
