-- ============================================================
-- V0159：单据状态变更日志 biz_status_log（批次 2 · P10 · 铁律 ③④）
--
-- 依据：
--   backend/src/business/shared/state_machine.rs  （唯一状态变更入口）
--
-- 迁移号：V0157（P5 stock_transfer_flow_seed）/ V0158（P7 付款 + 凭证红冲）已被占用，
--         本卡取下一个空号 V0159（卡面硬红线 3）。
--
-- # 为什么要这张表
--
--   开工实测：biz_stock_in / biz_purchase_order / biz_stock_transfer / biz_payable /
--   biz_voucher 五张单据表里都只有 created_by / update_by / updated_at，
--   **没有任何历史轨迹**。出事时「谁在什么时候把单子从 PENDING 改成 APPROVED」
--   查不到 —— 这是审计的第一查项，也是「状态机可被绕过」的旁证：
--   `stockin_model::sync_purchase_status` 用一句 SQL 直接把采购单推成
--   RECEIVED / PARTIAL_RECEIVED，完全没走 `can_transition()`。
--
-- # 只增不改（硬约束）
--
--   本表**故意不含** deleted_at / update_by / updated_at：
--     * 没有软删列 ⇒ 没有「删日志」这条路径；
--     * 没有 updated_at ⇒ 代码里不存在 UPDATE 语义（写入只有 INSERT）；
--     * 更正一条错误日志的唯一办法是**再写一条**说明性记录，历史不可改写。
--   日志与业务表在**同一个事务**里写（state_machine::transition 吃调用方的事务），
--   所以「业务改了但日志没写」「日志写了但业务没改」两种半截状态都不可能出现。
--
-- # 外键
--
--   entity_id 不加外键：五张单据表分属不同表，且日志必须比业务行**活得久**
--   （业务行软删 / 归档后日志依然是证据链）。同理 entity_no 冗余存一份，
--   避免「单据被硬删后按 id 查日志查不到」。
--
-- # 幂等
--
--   CREATE TABLE IF NOT EXISTS + CREATE INDEX IF NOT EXISTS，重复执行不报错。
-- ============================================================

CREATE TABLE IF NOT EXISTS biz_status_log (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    -- 单据类型：STOCK_IN / PURCHASE_ORDER / TRANSFER / PAYABLE / VOUCHER
    -- （与 shared/state_machine.rs 的 Entity::as_str 严格一一对应）
    entity_type   varchar(32) NOT NULL,
    entity_id     uuid NOT NULL,
    entity_no     varchar(64) NOT NULL,
    -- 首次创建为 NULL（建单日志），其余为变更前的状态
    from_status   varchar(24),
    to_status     varchar(24) NOT NULL,
    operator_id   uuid,
    operator_name varchar(64),
    reason        text,
    biz_ref_type  varchar(32),
    biz_ref_no    varchar(64),
    created_at    timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE biz_status_log IS
    '单据状态变更日志：只增不改（无 deleted_at / 无 UPDATE 路径）。与业务变更同事务写入，由 shared/state_machine.rs 唯一入口写。';

-- 按单据查轨迹（详情页「状态历史」）
CREATE INDEX IF NOT EXISTS idx_status_log_entity
    ON biz_status_log (entity_type, entity_id, created_at DESC);
-- 按单号查轨迹（单据号是人对得上的唯一标识，软删/硬删后仍可查）
CREATE INDEX IF NOT EXISTS idx_status_log_no
    ON biz_status_log (entity_no, created_at DESC);
-- 按类型 + 时间做运营审计（「今天所有收货单的状态变更」）
CREATE INDEX IF NOT EXISTS idx_status_log_type_time
    ON biz_status_log (entity_type, created_at DESC);

-- 自检：表与三个索引必须都在（迁移在空库首次启动时跑，不能用「有数据」当判据）
DO $$
DECLARE
    v_idx int;
BEGIN
    IF NOT EXISTS (SELECT 1
                     FROM information_schema.tables
                    WHERE table_schema = 'public' AND table_name = 'biz_status_log') THEN
        RAISE EXCEPTION 'V0159 自检失败：biz_status_log 未建成';
    END IF;

    SELECT count(*) INTO v_idx
      FROM pg_indexes
     WHERE schemaname = 'public'
       AND tablename = 'biz_status_log'
       AND indexname IN ('idx_status_log_entity', 'idx_status_log_no', 'idx_status_log_type_time');
    IF v_idx <> 3 THEN
        RAISE EXCEPTION 'V0159 自检失败：状态日志索引应有 3 个，实际 %', v_idx;
    END IF;

    -- 「只增不改」的列集合自检：任何一列软删 / 更新时间列都是设计漂移
    IF EXISTS (SELECT 1
                 FROM information_schema.columns
                WHERE table_schema = 'public' AND table_name = 'biz_status_log'
                  AND column_name IN ('deleted_at', 'update_by', 'updated_at')) THEN
        RAISE EXCEPTION 'V0159 自检失败：biz_status_log 不该有 deleted_at / update_by / updated_at 列';
    END IF;
END $$;
