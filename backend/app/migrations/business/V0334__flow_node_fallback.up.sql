-- ======================================================
-- V0334：审批降级 ③ —— 节点兜底人（P3-03 / ARCH-8 §1 场景 C）
--
-- ── 为什么需要 ────────────────────────────────────────
--   P3-02 已交付第 ② 层（角色升级链）：自审 SKIP 剔光 / 角色 0 人时沿
--   `sys_role.escalation_role_id` 上溯。但「角色只有制单人一人 + 该角色没有上级」
--   （小公司：销售员只有老板本人、老板上面没人）时升级链走完仍无人 → 422/65018，
--   单据永远发不出去（ARCH-8 场景 C）。
--
--   本迁移只做**第 ③ 层**：给节点配「兜底审批人 / 兜底角色」，在升级链之后接管。
--   第 ④ 层（ALLOW 受控放开）是 P3-05，错误码 65043-65044 给它留着。
--
-- ── 为什么二选一而不是两个都配 ────────────────────────
--   `fallback_user_id` 与 `fallback_role_id` 同时配会产生「到底听谁」的歧义
--   （谁优先？两个人的意见冲突怎么办？）。要多人就配角色 —— 角色成员本身
--   就能表达「一群人」，且与其它来源的展开语义一致。
--   DB 层用 CHECK 直接挡掉（而不是靠应用层自觉）。
--
-- ── 为什么两列都 NULL 就等于「不兜底」 ─────────────────
--   兼容存量节点：26 列老节点上没有兜底配置，行为必须与 P3-02 之后逐字一致
--   （升级链走完仍无人 → 422/65018）。新增列全部可空 + 无默认值就是为此。
--
-- ── 不配外键 ─────────────────────────────────────────
--   `sys_role` / `sys_user` 都是软删表。加 FK 会让「指向已删角色 / 已删用户」
--   变成插入报错（保存图形时炸），而这里需要的是运行期一条明确的中文错误
--   （65042「兜底人配置无效」）。与 V0333 的 `escalation_role_id` 同一取舍。
--
-- ── 待办派单规则留痕（assigned_by_rule）───────────────
--   一次加够四种值：ROLE / ESCALATED / FALLBACK / DELEGATED。
--   P3-04（委托）复用同一列，不再开第二次迁移加列。
-- ======================================================

-- ---------- 1) 节点上的兜底配置（二选一） ----------
ALTER TABLE biz_flow_node ADD COLUMN fallback_user_id UUID;
ALTER TABLE biz_flow_node ADD COLUMN fallback_role_id UUID;

COMMENT ON COLUMN biz_flow_node.fallback_user_id IS
    '兜底审批人（用户 id）：升级链走完仍无人时由他接管；与 fallback_role_id 二选一（DB CHECK 约束）';
COMMENT ON COLUMN biz_flow_node.fallback_role_id IS
    '兜底角色（角色 id）：升级链走完仍无人时展开该角色成员；要多人兜底就配它';
COMMENT ON COLUMN biz_flow_node.empty_assignee IS
    '空审批人策略 SKIP/ERROR/FALLBACK_ADMIN —— P3-02 之后「升级链 → 兜底人」都在它之前，'
    '它只在三层都不生效时才被读到';

-- 二选一：同时配两个会让「到底听谁」变成歧义（要多人就配角色）
ALTER TABLE biz_flow_node
    ADD CONSTRAINT ck_biz_flow_node_fallback_exclusive
    CHECK (fallback_user_id IS NULL OR fallback_role_id IS NULL);

-- 反查「谁被配成兜底人」用（排障：这个人为啥总收到单），只在非空行上建
CREATE INDEX idx_biz_flow_node_fallback_user ON biz_flow_node (fallback_user_id)
    WHERE fallback_user_id IS NOT NULL;
CREATE INDEX idx_biz_flow_node_fallback_role ON biz_flow_node (fallback_role_id)
    WHERE fallback_role_id IS NOT NULL;

-- ---------- 2) 待办上的派单规则留痕 ----------
-- NULL = 默认（按节点配置的来源直派，与 P3-02 之前一致）
-- ROLE      = 按角色展开派单（assignee_source='ROLE' 的普通路径）
-- ESCALATED = 沿角色升级链改派（P3-02）
-- FALLBACK  = 升级链走完仍无人 → 节点兜底人接管（P3-03）
-- DELEGATED = 委托（P3-04 预留，本迁移先建列）
ALTER TABLE biz_flow_task ADD COLUMN assigned_by_rule VARCHAR(20)
    CONSTRAINT ck_biz_flow_task_assigned_by_rule
    CHECK (assigned_by_rule IS NULL
           OR assigned_by_rule IN ('ROLE','ESCALATED','FALLBACK','DELEGATED'));

COMMENT ON COLUMN biz_flow_task.assigned_by_rule IS
    '派单规则留痕：ROLE 角色展开 / ESCALATED 升级链改派 / FALLBACK 节点兜底人 / DELEGATED 委托；NULL = 默认';

CREATE INDEX idx_biz_flow_task_by_rule ON biz_flow_task (instance_id, assigned_by_rule)
    WHERE assigned_by_rule IS NOT NULL;

-- ---------- 3) 自检 ----------
DO $$
DECLARE
    v_node_cols int;
    v_task_cols int;
    v_ck        int;
BEGIN
    SELECT count(*) INTO v_node_cols
      FROM information_schema.columns
     WHERE table_name = 'biz_flow_node'
       AND column_name IN ('fallback_user_id', 'fallback_role_id');
    IF v_node_cols <> 2 THEN
        RAISE EXCEPTION 'V0334：biz_flow_node 兜底列未建成（v_node_cols=%）', v_node_cols;
    END IF;

    SELECT count(*) INTO v_task_cols
      FROM information_schema.columns
     WHERE table_name = 'biz_flow_task' AND column_name = 'assigned_by_rule';
    IF v_task_cols <> 1 THEN
        RAISE EXCEPTION 'V0334：biz_flow_task.assigned_by_rule 未建成（v_task_cols=%）', v_task_cols;
    END IF;

    SELECT count(*) INTO v_ck
      FROM pg_constraint
     WHERE conname = 'ck_biz_flow_node_fallback_exclusive';
    IF v_ck <> 1 THEN
        RAISE EXCEPTION 'V0334：二选一 CHECK 约束未建成（v_ck=%）', v_ck;
    END IF;

    RAISE NOTICE 'V0334：节点兜底人（fallback_user_id / fallback_role_id）+ 派单规则列（assigned_by_rule）已就绪';
END $$;
