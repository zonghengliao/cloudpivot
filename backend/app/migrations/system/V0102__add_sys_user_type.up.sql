-- ======================================================
-- V0102：底座改动 —— `sys_user.user_type`（HUMAN / ROBOT）
--
-- 依据：docs/批次C+-工作任务管理-核心设计.md §十二「底座改动（需报批，共 1 处）」
--       + §六「AI 作为执行者」；老大 2026-10-29 已批准该处改动。
-- 回滚：V0102__add_sys_user_type.down.sql（DROP COLUMN，零残留）
--
-- ── 为什么这一条要单独成迁移，而不是混进 V0100 ──────────────────
-- V0100/V0101 是**业务** DDL（biz_* 前缀，随模块回滚）。本文件改的是**底座**
-- `sys_user`：混进去会让「回滚任务管理模块」顺手删掉底座列，而底座列一旦被
-- 其它模块（如后续工作流的机器人审批人）引用，删列就是一次跨模块故障。
-- 独立成条 → 回滚边界清晰。
--
-- ── 本卡（T1）只加列，不加任何逻辑 ──────────────────────────────
-- 「机器人不能登录 Web / 机器人专用 API 流程」属 T5；本卡只保证列存在、
-- 默认值正确（存量用户全部 HUMAN），并且**不改写任何现有查询**。
-- 因此这里没有 NOT NULL 之外的行为改动，也没有更新任何存量行（DEFAULT 会
-- 让存量行在被读取时得到 'HUMAN'，PG 11+ 加带默认值的列是元数据操作，
-- 不重写表 —— 大表上也是毫秒级）。
--
-- ── 为什么用 VARCHAR + CHECK 而不是 PG 原生 ENUM ────────────────
-- 与 biz_task.status 同一理由：将来若出现 'SERVICE'（系统服务账号）之类，
-- ENUM 加值要 DDL 且不能在事务里回滚；CHECK 改一行就能扩。
-- ======================================================

ALTER TABLE sys_user
    ADD COLUMN user_type VARCHAR(16) NOT NULL DEFAULT 'HUMAN';

-- 取值约束单独 ADD CONSTRAINT（写成内联的话，回滚时得先找出系统生成的约束名）
ALTER TABLE sys_user
    ADD CONSTRAINT ck_sys_user_user_type CHECK (user_type IN ('HUMAN', 'ROBOT'));

COMMENT ON COLUMN sys_user.user_type IS '账号类型：HUMAN 人类（默认）/ ROBOT 机器人（AI 执行者，禁止登录 Web，登录闸门属 T5）';

-- 机器人的唯一用途是「被分派任务」，按类型筛人的场景（选负责人下拉里
-- 是否显示机器人）会走 (user_type) 过滤；配上未软删条件后仍很小，暂不建索引
-- （批次 B 的教训：给 sys_user 乱加索引会拖慢登录路径的写放大）。
