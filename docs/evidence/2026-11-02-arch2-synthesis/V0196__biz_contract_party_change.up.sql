-- ============================================================
-- V0196：合同乙方变更台账 biz_contract_party_change（ARCH-2 汇总定稿 · 第四处关注点）
--
-- 口径来源：docs/architecture/合同与勾稽关系设计.md §7.7 / §5.4
-- 设计来源：docs/architecture/snapshot-drift/V0195__biz_contract_party_change.up.sql（巡检卡草案，本文件去掉 BEGIN/COMMIT）
--
-- 为什么必须有这张表：DI-01 要把「合同换了乙方（合法历史快照）」与「合同被事后直接改库（疑似篡改）」
-- 分开。区分依据只能是「变更有没有台账」；不能用 contract.updated_at —— 改一条备注也会推高它，
-- 会把「合法变更」误判成「快照没跟随」，同时把「真篡改」掩盖成「有更新」。
--
-- 只记「乙方变更」这一种事实；金额/日期修改不进本表（它们不改变快照真相）。
-- 写入点唯一：合同变更审批通过的那个事务。
-- ============================================================

CREATE TABLE IF NOT EXISTS biz_contract_party_change (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    contract_id      UUID        NOT NULL,        -- 弱引用 biz_contract.id（本仓零外键基线）
    old_party_b_id   UUID,                        -- 变更前乙方（首次指定可为空）
    new_party_b_id   UUID        NOT NULL,        -- 变更后乙方
    changed_at       TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 生效时刻（审批通过时刻）
    instance_id      UUID,                        -- 变更审批实例（弱引用 biz_flow_instance.id）
    operator_id      UUID,                        -- 操作人（弱引用 sys_user.id）
    reason           TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT ck_bcpc_no_self CHECK (old_party_b_id IS NULL OR old_party_b_id <> new_party_b_id)
);

COMMENT ON TABLE biz_contract_party_change IS
  '合同乙方变更台账：DI-01 唯一认可的「合法变更」证据；写入点只有合同变更审批通过的事务';
COMMENT ON COLUMN biz_contract_party_change.changed_at IS
  '变更生效时刻：DI-01 用它和采购单 created_at 比较，判定「建单早于变更=合法历史快照」还是「建单晚于变更=快照未跟随」';

CREATE INDEX IF NOT EXISTS idx_bcpc_contract ON biz_contract_party_change (contract_id, changed_at DESC);
