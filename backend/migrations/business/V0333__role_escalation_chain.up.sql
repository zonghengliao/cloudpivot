-- ======================================================
-- V0333：审批降级 ② —— 角色升级链（P3-02 / ARCH-8 §1、§7.3 场景 B）
--
-- ── 为什么需要 ────────────────────────────────────────
--   `assignee::finalize` 的自审策略 SKIP 会把发起人本人从审批人里剔掉，
--   但它**只会在原地留一条 SKIPPED 待办**，不负责「换一个人来审」。
--   于是「角色里只有制单人一个人」的岗位（小公司极常见）在提交时必然
--   `approvers.is_empty()` → 引擎 422 / 65018 → 单据永远发不出去（ARCH-8 场景 B）。
--
--   本迁移只做**第 ② 层**：给角色配一条「上级角色」，SKIP 剔光后沿链上溯找
--   第一个有可用成员的角色。第 ③ 层（节点兜底人）与第 ④ 层（ALLOW 受控放开）
--   是后续卡（P3-03 / P3-05），字段与错误码都给它们留好号段。
--
-- ── 为什么是可配置列而不是硬编码 ──────────────────────
--   ARCH-8 §1、P3-01 决策 D5：Rust 里**不许**出现 `SALES_CLERK → SALES_SUPERVISOR`
--   这类字符串。升级链是**组织配置**，随公司结构变化；写死在代码里的后果是
--   「换个客户就要发版」。所以链条存在 `sys_role.escalation_role_id`，
--   本迁移只写**初始数据**（管理员随后可改），运行时只读不猜。
--
-- ── 自引用 / 环 ──────────────────────────────────────
--   DB 层只挡得住「指向自己」（CHECK）；A→B→A 这类环在应用层用 visited 集合
--   检测（`assignee::escalate`，错误码 65040）。不加外键：角色是软删表，
--   `escalation_role_id` 指向已软删角色时应用层报 65040（配置非法），
--   比 FK 直接拒绝更好定位。
--
-- ── 幂等 ─────────────────────────────────────────────
--   迁移只跑一次。预置链的 UPDATE 只在「源角色存在 + 目标角色存在」时命中，
--   找不到的角色直接跳过（新库 / 客户裁剪过的库里角色本来就不全）。
-- ======================================================

-- ---------- 1) 角色的上级角色 ----------
ALTER TABLE sys_role ADD COLUMN escalation_role_id UUID;

COMMENT ON COLUMN sys_role.escalation_role_id IS
    '上级角色（审批降级链）：本角色没有可用审批人时沿此指针上溯找下一级；NULL = 无上级';

-- 自引用：DB 层能挡一条就地挡住（环由应用层 65040 检测）
ALTER TABLE sys_role
    ADD CONSTRAINT ck_sys_role_escalation_self
    CHECK (escalation_role_id IS NULL OR escalation_role_id <> id);

-- 反查「谁把我当上级」用（改链影响面 / 排障），只在非空行上建
CREATE INDEX idx_sys_role_escalation ON sys_role (escalation_role_id)
    WHERE escalation_role_id IS NOT NULL;

-- ---------- 2) 待办上的升级留痕 ----------
ALTER TABLE biz_flow_task ADD COLUMN escalated_from_role VARCHAR(40);
ALTER TABLE biz_flow_task ADD COLUMN escalate_reason VARCHAR(100);

COMMENT ON COLUMN biz_flow_task.escalated_from_role IS
    '升级留痕：原本该审的角色编码（如 SALES_CLERK）；非空 = 这条待办是沿升级链派下来的';
COMMENT ON COLUMN biz_flow_task.escalate_reason IS
    '升级原因：SKIP_SELF_APPROVAL（自审策略剔光）/ NO_MEMBER（角色 0 成员）';

-- ---------- 3) 预置升级链（**只是初始数据，管理员可改**） ----------
--
-- 链条（ARCH-8 §1 / 卡面）：
--   销售员 → 销售主管 → 销售总监 → 总经理
--   采购员 → 采购主管/采购经理 → 采购总监 → 总经理
--   财务专员 → 财务主管/财务经理 → 财务总监 → 总经理
--   仓管   → 仓库主管 → 运营总监 → 总经理
--   部门经理 → 分管副总 → 总经理
--
-- `targets` 是**按优先级排列的候选编码**：取第一个在库里存在的角色。
-- 这样「客户库里没有『销售总监』这个角色」时链条不会断在中间，而是直接落到
-- 总经理（否则升级链形同虚设 —— 而 P3-02 的价值就在于「上溯总能找到人」）。
-- 源角色 / 目标角色任一不存在 → 这一级不写（WHERE JOIN 不命中），不报错。
UPDATE sys_role r
   SET escalation_role_id = tgt.id,
       updated_at = now()
  FROM (VALUES
        ('SALES_CLERK',        ARRAY['SALES_SUPERVISOR', 'GENERAL_MANAGER']),
        ('SALES_SUPERVISOR',   ARRAY['SALES_DIRECTOR', 'GENERAL_MANAGER']),
        ('SALES_DIRECTOR',     ARRAY['GENERAL_MANAGER']),
        ('PURCHASER',          ARRAY['PURCHASE_SUPERVISOR', 'PURCHASE_MANAGER', 'GENERAL_MANAGER']),
        ('PURCHASE_SUPERVISOR', ARRAY['PURCHASE_DIRECTOR', 'PURCHASE_MANAGER', 'GENERAL_MANAGER']),
        ('PURCHASE_MANAGER',   ARRAY['PURCHASE_DIRECTOR', 'GENERAL_MANAGER']),
        ('ACCOUNTANT',         ARRAY['FINANCE_MANAGER', 'GENERAL_MANAGER']),
        ('FINANCE_MANAGER',    ARRAY['FINANCE_DIRECTOR', 'GENERAL_MANAGER']),
        ('FINANCE_DIRECTOR',   ARRAY['GENERAL_MANAGER']),
        ('WAREHOUSE_KEEPER',   ARRAY['WAREHOUSE_MANAGER', 'GENERAL_MANAGER']),
        ('WAREHOUSE_MANAGER',  ARRAY['OPERATIONS_DIRECTOR', 'GENERAL_MANAGER']),
        ('DEPT_LEADER',        ARRAY['VICE_PRESIDENT', 'GENERAL_MANAGER'])
       ) AS chain(from_code, targets)
  JOIN LATERAL (
        SELECT s.id
          FROM unnest(chain.targets) WITH ORDINALITY AS k(code, ord)
          JOIN sys_role s ON s.role_code = k.code AND s.deleted_at IS NULL
         ORDER BY k.ord
         LIMIT 1
       ) AS tgt ON TRUE
 WHERE r.role_code = chain.from_code
   AND r.deleted_at IS NULL
   AND tgt.id <> r.id;

-- ---------- 4) 自检 ----------
DO $$
DECLARE
    v_cols   int;
    v_chains int;
    v_bad    int;
BEGIN
    SELECT count(*) INTO v_cols
      FROM information_schema.columns
     WHERE table_name = 'sys_role' AND column_name = 'escalation_role_id';
    IF v_cols <> 1 THEN
        RAISE EXCEPTION 'V0333：sys_role.escalation_role_id 未建成（v_cols=%）', v_cols;
    END IF;

    SELECT count(*) INTO v_cols
      FROM information_schema.columns
     WHERE table_name = 'biz_flow_task'
       AND column_name IN ('escalated_from_role', 'escalate_reason');
    IF v_cols <> 2 THEN
        RAISE EXCEPTION 'V0333：biz_flow_task 升级留痕列未建成（v_cols=%）', v_cols;
    END IF;

    SELECT count(*) INTO v_bad FROM sys_role WHERE escalation_role_id = id;
    IF v_bad > 0 THEN
        RAISE EXCEPTION 'V0333：预置链出现自引用（% 行）', v_bad;
    END IF;

    SELECT count(*) INTO v_chains FROM sys_role WHERE escalation_role_id IS NOT NULL;
    RAISE NOTICE 'V0333：升级链预置完成（% 个角色已配上级角色）', v_chains;
END $$;
