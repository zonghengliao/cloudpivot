-- ============================================================================
-- V0339__approval_sign_mode_freeze（P3-06 存量冻结：APPROVAL 节点的签名模式）
-- ============================================================================
--
-- 背景
--   引擎历史口径：`approval_mode` 为空 → `unwrap_or("ALL")` → 全员签。也就是说
--   「没填模式」实际上比字面意思严得多：一个审批节点派给 3 个人，要 3 个人都点同意。
--   P3-06 把默认改成「不填 = 或签（ANY）」，并让 `APPROVAL` 节点也能显式选模式。
--
-- 为什么不能一刀切改成 ANY
--   直接把存量全改 ANY = 把**正在跑的全员签流程**静默降级成或签（审批强度偷偷下降）。
--   决策 D2 因此是**按数据冻结**：
--     · 该节点解析出的候选人数 ≥ 2 → 写 `ALL`（保持当前行为）
--     · 候选人数 < 2 或算不出来      → 写 `ANY`（ALL/ANY 在 0/1 人时判定等价）
--
-- 本迁移只冻结**草稿版本（DRAFT）**的行 —— 为什么
--   `APPROVAL` 节点分两处存：编辑期的 `biz_flow_node` 行 与 发布版的
--   `biz_flow_def_version.graph_snapshot`。实例读的是后者，而 `graph_hash` 是
--   「从行重建的 canonical JSON」的 sha256。对已发布版本只改行会带来两个后果：
--     ① 运行实例的签法**并不会**被冻结（它读快照）→ D2 想保的行为没保住；
--     ② 行 hash 与发布时写死的 hash 对不上 → `flow_graph_selfcheck`（每天 03:30）
--        会把这些版本逐条报成「行被直接修改」并写审计（天天误报）。
--   已发布 / 归档版本因此交给启动期的 `flow::sign_freeze`（`bootstrap`）处理：
--   它用**发布期同一套** `snapshot_json()` / `graph_hash()` 把 快照 + hash + 行 +
--   运行中实例节点四者同改，口径与本文件第一段逐字同义（候选人数 ≥2 → ALL，其余 ANY）。
--
-- 候选人数怎么算（与 `graph::freeze_sign_mode` 的纯函数同口径）
--   逐 `assignee_type` 取「派得出几个人」：
--     USER                 → `biz_flow_node_assignee` 里 USER 行的去重人数
--     ROLE                 → 各角色「启用成员数」之和（角色未删未停 + 授权未过期 + 用户未删未停）
--     DEPT_LEADER          → 引用部门的 `leader_id` 去重人数（没设负责人算 0）
--     ADMIN                → `SUPER_ADMIN` 角色的启用用户数
--     INITIATOR / INITIATOR_SUPERIOR / TASK_OWNER / TASK_CREATOR / TASK_ASSIGNER → 1（单值来源）
--     FORM_VAR / EXPRESSION / 没配来源 → NULL（算不出来）
--
-- 幂等
--   UPDATE 带 `WHERE approval_mode IS NULL`，重放时 `UPDATE 0`。
-- ============================================================================

-- ---------- 1) 按候选人数把**草稿版本**里 APPROVAL 的 NULL 冻结成 ALL / ANY ----------
WITH cand AS (
    SELECT n.id,
           CASE n.assignee_type
               WHEN 'USER' THEN (
                   SELECT count(DISTINCT a.ref_id)
                     FROM biz_flow_node_assignee a
                    WHERE a.node_id = n.id AND a.assignee_type = 'USER'
               )
               WHEN 'ROLE' THEN (
                   SELECT count(DISTINCT ur.user_id)
                     FROM biz_flow_node_assignee a
                     JOIN sys_role r ON r.id = a.ref_id
                                    AND r.deleted_at IS NULL AND r.status = 1
                     JOIN sys_user_role ur ON ur.role_id = a.ref_id
                                          AND (ur.expire_time IS NULL OR ur.expire_time > now())
                     JOIN sys_user u ON u.id = ur.user_id
                                    AND u.deleted_at IS NULL AND u.status = 1
                    WHERE a.node_id = n.id AND a.assignee_type = 'ROLE'
               )
               WHEN 'DEPT_LEADER' THEN (
                   SELECT count(DISTINCT d.leader_id)
                     FROM biz_flow_node_assignee a
                     JOIN sys_dept d ON d.id = a.ref_id AND d.leader_id IS NOT NULL
                    WHERE a.node_id = n.id AND a.assignee_type = 'DEPT_LEADER'
               )
               WHEN 'ADMIN' THEN (
                   SELECT count(DISTINCT u.id)
                     FROM sys_user u
                     JOIN sys_user_role ur ON ur.user_id = u.id
                     JOIN sys_role r ON r.id = ur.role_id
                    WHERE r.role_code = 'SUPER_ADMIN'
                      AND r.deleted_at IS NULL AND u.deleted_at IS NULL AND u.status = 1
               )
               WHEN 'INITIATOR' THEN 1
               WHEN 'INITIATOR_SUPERIOR' THEN 1
               WHEN 'TASK_OWNER' THEN 1
               WHEN 'TASK_CREATOR' THEN 1
               WHEN 'TASK_ASSIGNER' THEN 1
               ELSE NULL
           END AS candidates
      FROM biz_flow_node n
      JOIN biz_flow_def_version v ON v.id = n.version_id
     WHERE n.node_type = 'APPROVAL'
       AND n.approval_mode IS NULL
       AND v.version_status = 'DRAFT'
)
UPDATE biz_flow_node n
   SET approval_mode = CASE WHEN c.candidates >= 2 THEN 'ALL' ELSE 'ANY' END
  FROM cand c
 WHERE n.id = c.id;

-- ---------- 2) 自检 + 统计（数字进日志/证据，便于人工核对 D2 的落地面） ----------
DO $$
DECLARE
    draft_left_null INTEGER;
    draft_all INTEGER;
    draft_any INTEGER;
    published_pending INTEGER;
    bad_ratio INTEGER;
BEGIN
    SELECT count(*) INTO draft_left_null
      FROM biz_flow_node n
      JOIN biz_flow_def_version v ON v.id = n.version_id
     WHERE n.node_type = 'APPROVAL' AND n.approval_mode IS NULL AND v.version_status = 'DRAFT';
    IF draft_left_null > 0 THEN
        RAISE EXCEPTION 'V0339 自检失败：草稿里还有 % 个 APPROVAL 节点 approval_mode 仍为 NULL', draft_left_null;
    END IF;

    SELECT count(*) INTO bad_ratio
      FROM biz_flow_node
     WHERE approval_mode = 'RATIO' AND approve_ratio IS NULL;
    IF bad_ratio > 0 THEN
        RAISE EXCEPTION 'V0339 自检失败：% 个 RATIO 节点没有 approve_ratio（ck_biz_flow_node_ratio 该拦住）', bad_ratio;
    END IF;

    SELECT count(*) FILTER (WHERE n.approval_mode = 'ALL'),
           count(*) FILTER (WHERE n.approval_mode = 'ANY')
      INTO draft_all, draft_any
      FROM biz_flow_node n
      JOIN biz_flow_def_version v ON v.id = n.version_id
     WHERE n.node_type = 'APPROVAL' AND v.version_status = 'DRAFT';

    SELECT count(*) INTO published_pending
      FROM biz_flow_node n
      JOIN biz_flow_def_version v ON v.id = n.version_id
     WHERE n.node_type = 'APPROVAL' AND n.approval_mode IS NULL
       AND v.version_status IN ('PUBLISHED', 'ARCHIVED');

    RAISE NOTICE 'V0339 草稿冻结：ALL=% / ANY=%', draft_all, draft_any;
    RAISE NOTICE 'V0339 已发布/归档待启动期冻结器处理：% 个 APPROVAL 节点（flow::sign_freeze）', published_pending;
END $$;
