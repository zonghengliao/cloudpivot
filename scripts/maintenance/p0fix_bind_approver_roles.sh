#!/usr/bin/env bash
# ============================================================================
# P0-FIX 配置修复：把真人绑到「审批岗位」角色上（角色 = 流程节点的审批人来源）
#
# 为什么需要这一步：
#   `po_approve` / `stock_in_post` 的审批节点审批人 = ROLE（DEPT_LEADER /
#   PURCHASE_MANAGER / GENERAL_MANAGER / WAREHOUSE_MANAGER，V0182 种子）。
#   设计上「客户只绑人、不改流程」，但这些角色**一个成员都没有**时，引擎按
#   `empty_assignee=FALLBACK_ADMIN` 回退成「全部超管」⇒ 审批人里必然包含制单人与发起人，
#   叠加内控（不能自审）就会把人全挡掉（生产 PO-20261031-000002 的死锁就是这么来的）。
#
# 口径（硬红线）：
#   · **只加不减**：只补缺失的 `sys_user_role` 行，不动任何已有绑定（幂等，可重复跑）；
#   · 只对**确实被流程节点引用**的角色生效（先核对 `biz_flow_node_assignee`，防绑了个没人用的角色）；
#   · 绑完打印每个审批节点的「可审批人数」，0 人的节点直接点名（那才是配置缺陷）；
#   · 默认 **dry-run**，`--apply` 才写；生产库额外要 `--i-know-its-prod`。
#
# 用法：
#   bash scripts/maintenance/p0fix_bind_approver_roles.sh --role PURCHASE_MANAGER --users henry.liao,laoliu_test
#   bash scripts/maintenance/p0fix_bind_approver_roles.sh --role PURCHASE_MANAGER --users henry.liao,laoliu_test --apply
#   bash scripts/maintenance/p0fix_bind_approver_roles.sh --audit        # 只体检：哪些审批节点 0 人
#
# 环境变量：P0FIX_DB / P0FIX_ENV_FILE（默认 /etc/cloudpivot/cloudpivot.env）
# ============================================================================
set -uo pipefail

ROLE_CODE=""
USERS=""
APPLY=0
PROD_OK=0
AUDIT=0
ENV_FILE="${P0FIX_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}"

while [ $# -gt 0 ]; do
  case "$1" in
    --role)
      shift
      ROLE_CODE="${1:-}"
      ;;
    --users)
      shift
      USERS="${1:-}"
      ;;
    --audit) AUDIT=1 ;;
    --apply) APPLY=1 ;;
    --dry-run) APPLY=0 ;;
    --i-know-its-prod) PROD_OK=1 ;;
    -h | --help)
      sed -n '2,32p' "$0"
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
psql_q() {
  psql -h "${NIK_DATABASE_HOST:-127.0.0.1}" -p "${NIK_DATABASE_PORT:-5432}" \
    -U "${NIK_DATABASE_USER:-cloudpivot}" -d "$DB_NAME" -v ON_ERROR_STOP=1 "$@"
}

echo "==================================================================="
echo "库：${DB_NAME}  模式：$([ "$APPLY" = "1" ] && echo 'APPLY（真绑）' || echo 'DRY-RUN（只看）')"
echo "==================================================================="

echo
echo "---- ① 审批岗位角色（被流程节点引用的 ROLE）----"
psql_q -P pager=off -c "
  SELECT r.role_code, r.role_name,
         (SELECT count(*) FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id AND u.deleted_at IS NULL
           WHERE ur.role_id = r.id) AS members,
         (SELECT count(DISTINCT n.node_key || '@' || d.flow_code)
            FROM biz_flow_node_assignee a
            JOIN biz_flow_node n ON n.id = a.node_id
            JOIN biz_flow_def_version v ON v.id = n.version_id
            JOIN biz_flow_def d ON d.id = v.def_id
           WHERE a.assignee_type = 'ROLE' AND a.ref_id = r.id) AS used_by_nodes
    FROM sys_role r
   WHERE r.deleted_at IS NULL
     AND EXISTS (SELECT 1 FROM biz_flow_node_assignee a WHERE a.assignee_type = 'ROLE' AND a.ref_id = r.id)
   ORDER BY r.role_code"

echo
echo "---- ② 审批节点可审批人数（角色内 0 人 = 会走 FALLBACK_ADMIN 兜底成全部超管）----"
psql_q -P pager=off -c "
  SELECT d.flow_code, n.node_key, n.node_name, n.node_type,
         string_agg(DISTINCT r.role_code, '+') FILTER (WHERE a.assignee_type = 'ROLE') AS role_codes,
         COALESCE(sum(m.members), 0) AS role_members,
         n.empty_assignee
    FROM biz_flow_node n
    JOIN biz_flow_def_version v ON v.id = n.version_id AND v.is_current
    JOIN biz_flow_def d ON d.id = v.def_id AND d.status = 'PUBLISHED'
    LEFT JOIN biz_flow_node_assignee a ON a.node_id = n.id AND a.assignee_type = 'ROLE'
    LEFT JOIN sys_role r ON r.id = a.ref_id
    LEFT JOIN LATERAL (
        SELECT count(*) AS members FROM sys_user_role ur
          JOIN sys_user u ON u.id = ur.user_id AND u.deleted_at IS NULL
         WHERE ur.role_id = r.id
    ) m ON TRUE
   WHERE n.node_type IN ('APPROVAL', 'COUNTERSIGN')
   GROUP BY d.flow_code, n.node_key, n.node_name, n.node_type, n.empty_assignee
   ORDER BY d.flow_code, n.node_key"

if [ "$AUDIT" = "1" ]; then
  echo
  echo "DRY-RUN 体检完成（--audit 不改任何数据）"
  exit 0
fi

if [ -z "$ROLE_CODE" ] || [ -z "$USERS" ]; then
  echo
  echo "需要 --role <ROLE_CODE> --users <a,b,c> 才会绑定（或用 --audit 只体检）" >&2
  exit 2
fi

# 角色必须存在
# ⚠️ psql 18 的 `-c` **不做变量插值**（`:'role'` 会原样发给服务器 → 语法错），
# 所以带变量的 SQL 一律走 stdin（heredoc 才会插值）。
ROLE_ID="$(psql_q -At -v role="$ROLE_CODE" <<'SQL'
SELECT id FROM sys_role WHERE role_code = :'role' AND deleted_at IS NULL
SQL
)"
if [ -z "$ROLE_ID" ]; then
  echo "角色不存在：$ROLE_CODE（先确认 V0182 种子角色在不在）" >&2
  exit 2
fi

echo
echo "---- ③ 绑定计划：$ROLE_CODE（$ROLE_ID）← $USERS ----"
IFS=',' read -r -a USER_LIST <<<"$USERS"
MISSING=0
for name in "${USER_LIST[@]}"; do
  row="$(psql_q -At -F'~' -v u="$name" -v rid="$ROLE_ID" <<'SQL'
    SELECT u.id, u.real_name,
           CASE WHEN EXISTS (SELECT 1 FROM sys_user_role ur WHERE ur.user_id = u.id AND ur.role_id = :'rid')
                THEN 'ALREADY' ELSE 'TO_BIND' END
      FROM sys_user u
     WHERE u.username = :'u' AND u.deleted_at IS NULL
SQL
)"
  if [ -z "$row" ]; then
    echo "  ✗ 账号不存在（跳过）：$name"
    continue
  fi
  uid="${row%%~*}"
  rest="${row#*~}"
  real="${rest%%~*}"
  state="${rest#*~}"
  echo "  · $name（$real）→ $state"
  if [ "$state" = "TO_BIND" ]; then
    MISSING=$((MISSING + 1))
    if [ "$APPLY" = "1" ]; then
      psql_q -v uid="$uid" -v rid="$ROLE_ID" <<'SQL' >/dev/null
        INSERT INTO sys_user_role (user_id, role_id)
        SELECT :'uid'::uuid, :'rid'::uuid
         WHERE NOT EXISTS (SELECT 1 FROM sys_user_role
                            WHERE user_id = :'uid'::uuid AND role_id = :'rid'::uuid)
SQL
    fi
  fi
done

if [ "$APPLY" != "1" ]; then
  echo
  echo "DRY-RUN：将新增 $MISSING 条绑定。确认后加 --apply。"
  exit 0
fi

echo
echo "---- ④ 绑定后复核（该角色的成员 + 引用它的审批节点人数）----"
psql_q -P pager=off -v rid="$ROLE_ID" <<'SQL'
  SELECT u.username, u.real_name
    FROM sys_user_role ur JOIN sys_user u ON u.id = ur.user_id AND u.deleted_at IS NULL
   WHERE ur.role_id = :'rid'::uuid ORDER BY u.username
SQL
psql_q -At -F' | ' -v rid="$ROLE_ID" <<'SQL'
  SELECT '节点 ' || d.flow_code || '.' || n.node_key || ' 可审批人数 = ' ||
         (SELECT count(*) FROM biz_flow_node_assignee a2
            JOIN sys_user_role ur2 ON ur2.role_id = a2.ref_id
            JOIN sys_user u2 ON u2.id = ur2.user_id AND u2.deleted_at IS NULL
           WHERE a2.node_id = n.id AND a2.assignee_type = 'ROLE')
    FROM biz_flow_node n
    JOIN biz_flow_def_version v ON v.id = n.version_id AND v.is_current
    JOIN biz_flow_def d ON d.id = v.def_id AND d.status = 'PUBLISHED'
   WHERE EXISTS (SELECT 1 FROM biz_flow_node_assignee a
                  WHERE a.node_id = n.id AND a.assignee_type = 'ROLE' AND a.ref_id = :'rid'::uuid)
   ORDER BY d.flow_code, n.node_key
SQL
echo
echo "完成。注意：新绑的账号若**不是**超管，还需要业务权限码 business:flow:approve 才能点同意。"
