-- 迁移前预测：按 V0339 的口径算「本次会被冻结成多少 ALL / 多少 ANY / 多少算不出来」
-- 输出一行：pred_all|pred_any|pred_unknown（只统计 approval_mode IS NULL 的 APPROVAL 节点）
WITH cand AS (
    SELECT n.id,
           CASE n.assignee_type
               WHEN 'USER' THEN (
                   SELECT count(DISTINCT a.ref_id) FROM biz_flow_node_assignee a
                    WHERE a.node_id = n.id AND a.assignee_type = 'USER')
               WHEN 'ROLE' THEN (
                   SELECT count(DISTINCT ur.user_id)
                     FROM biz_flow_node_assignee a
                     JOIN sys_role r ON r.id = a.ref_id AND r.deleted_at IS NULL AND r.status = 1
                     JOIN sys_user_role ur ON ur.role_id = a.ref_id
                                          AND (ur.expire_time IS NULL OR ur.expire_time > now())
                     JOIN sys_user u ON u.id = ur.user_id
                                    AND u.deleted_at IS NULL AND u.status = 1
                    WHERE a.node_id = n.id AND a.assignee_type = 'ROLE')
               WHEN 'DEPT_LEADER' THEN (
                   SELECT count(DISTINCT d.leader_id)
                     FROM biz_flow_node_assignee a
                     JOIN sys_dept d ON d.id = a.ref_id AND d.leader_id IS NOT NULL
                    WHERE a.node_id = n.id AND a.assignee_type = 'DEPT_LEADER')
               WHEN 'ADMIN' THEN (
                   SELECT count(DISTINCT u.id)
                     FROM sys_user u
                     JOIN sys_user_role ur ON ur.user_id = u.id
                     JOIN sys_role r ON r.id = ur.role_id
                    WHERE r.role_code = 'SUPER_ADMIN'
                      AND r.deleted_at IS NULL AND u.deleted_at IS NULL AND u.status = 1)
               WHEN 'INITIATOR' THEN 1
               WHEN 'INITIATOR_SUPERIOR' THEN 1
               WHEN 'TASK_OWNER' THEN 1
               WHEN 'TASK_CREATOR' THEN 1
               WHEN 'TASK_ASSIGNER' THEN 1
               ELSE NULL
           END AS candidates
      FROM biz_flow_node n
     WHERE n.node_type = 'APPROVAL' AND n.approval_mode IS NULL
)
SELECT count(*) FILTER (WHERE candidates >= 2) || '|'
    || count(*) FILTER (WHERE candidates < 2) || '|'
    || count(*) FILTER (WHERE candidates IS NULL)
  FROM cand;
