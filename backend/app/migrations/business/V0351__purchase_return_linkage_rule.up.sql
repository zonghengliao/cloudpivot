-- ============================================================
-- V0351：采购退货（PURCHASE_RETURN）**确认联动的 1 条预置规则**（TST-01 · 接线完整性）
--
-- 依据：`backend/src/business/erp/purchase_return_handler.rs::linkage_ctx`（触发点上下文 + 通知草稿）
--       + `backend/src/business/flow/linkage/{spec,engine,actions}.rs`
--       + `migrations/business/V0323__asset_linkage_rules.up.sql`（同型先例）
--
-- # 为什么需要这张迁移
--
--   P2-03（9cf6cc1「采购退货闭环」）把 `PURCHASE_RETURN` 加进 `spec::TRIGGER_SPECS`
--   并标 `wired = true`：`confirm_return` 事务内真的调
--   `linkage::engine::run_excluding(PURCHASE_RETURN_EXCLUDED_ACTIONS)`、commit 后调
--   `run_deferred`。但**迁移全集跑完后 `biz_flow_linkage_rule` 里 PURCHASE_RETURN 规则 0 条**
--   ⇒ 配置页「已接线」、代码也在跑，可「退货确认后通知制单人」这条能力实际不存在
--   （典型「看起来配好了，实际不工作」，与 V0323 的 FIX-3 问题同源）。
--
--   实测（改造前）144 生产库：
--     SELECT trigger_entity, trigger_status, action_code FROM biz_flow_linkage_rule
--      WHERE deleted_at IS NULL;
--     → FIXED_ASSET×3 / STOCK_IN×2 / TASK×1，**没有 PURCHASE_RETURN**。
--
-- # 为什么只有 1 条（不是 5 条）
--
--   `PURCHASE_RETURN` 的 5 个状态里**只有 `CONFIRMED` 真的调引擎**（唯一动账点，
--   见 `purchase_return_handler.rs` 文件头「联动（卡面 §⑤）」）：`DRAFT / SUBMITTED /
--   APPROVED / CANCELLED` 都不接 —— 提交 / 批准 / 作废不动账，需要通知就在审批流
--   自己的节点上配。给这 4 个状态种规则 = 配得上下拉但**永远等不到触发**，比不种更糟。
--
-- # 动作为什么是 SEND_NOTICE + TAKEOVER
--
--   * 这个触发点上**能用的动作只有 SEND_NOTICE**：`PURCHASE_RETURN_EXCLUDED_ACTIONS`
--     排掉了 `WRITE_INVENTORY / WRITE_INVENTORY_TXN / UPDATE_COST /
--     UPDATE_PAYABLE_BALANCE / GENERATE_VOUCHER`（五本账由
--     `purchase_return_posting::confirm_tx` 专用路径落账，通用引擎再跑一次就是重复落账 /
--     凭证生凭证）；其余动作（`CREATE_TASK` 等）仍是占位，写配置阶段就被拒。
--   * `SHADOW` 只做影子对比、不发通知 —— 这里**没有旧代码在发**（`linkage_ctx` 只把草稿
--     放进 `LinkageContext.notice`，投递完全靠引擎）⇒ 写成 SHADOW 等于规则配了也一条不发。
--     所以必须 `{"mode": "TAKEOVER"}`。
--   * 不写 `recipient` = 沿用触发点算出来的收件人（`linkage_ctx`：制单人，制单人缺失退操作人）。
--     标题 / 正文同理不覆盖：`linkage_ctx` 已拼好「采购退货「PRT-…」已确认：供应商 …，
--     退货数量 …，价税合计 …，仓库 …」。
--
-- # 幂等（生产上可能已经手工配过规则）
--
--   `ON CONFLICT (trigger_entity, trigger_status, action_code) DO NOTHING`：同一键上
--   **已经存在的行**（运维手工配的、被停用的、被软删的）一律**原样保留** ——
--   那是运维的决定，迁移不该覆盖。自检只断言「这个键有一行」，不强制 `enabled` / `TAKEOVER`。
--   不写 BEGIN/COMMIT（迁移器自己在事务里执行）。
-- ============================================================

INSERT INTO biz_flow_linkage_rule
    (id, name, trigger_entity, trigger_status, action_code, action_params,
     enabled, seq, created_at, updated_at, create_by, update_by, deleted_at)
VALUES
    (gen_random_uuid(),
     '采购退货确认 → 通知制单人',
     'PURCHASE_RETURN', 'CONFIRMED', 'SEND_NOTICE',
     '{"mode": "TAKEOVER"}'::jsonb,
     true, 10, now(), now(), NULL, NULL, NULL)
ON CONFLICT (trigger_entity, trigger_status, action_code) DO NOTHING;

-- ------------------------------------------------------------
-- 自检（迁移期断言，失败即整笔回滚）
-- ------------------------------------------------------------
DO $$
DECLARE
    n_keys    integer;
    n_alive   integer;
    n_take    integer;
    n_bad_act integer;
BEGIN
    -- ① 这个触发点上「真的会响」的状态只有 CONFIRMED，必须有一行
    SELECT count(DISTINCT trigger_status) INTO n_keys FROM biz_flow_linkage_rule
     WHERE trigger_entity = 'PURCHASE_RETURN'
       AND action_code = 'SEND_NOTICE'
       AND trigger_status = 'CONFIRMED';
    IF n_keys <> 1 THEN
        RAISE EXCEPTION 'V0351 自检失败：PURCHASE_RETURN/CONFIRMED 的 SEND_NOTICE 规则应有 1 行，实际 %', n_keys;
    END IF;

    -- ② 退货确认的五本账（库存 / 库位账 / 流水 / 应付余额 / 凭证）由
    --    purchase_return_posting::confirm_tx 专用路径落账。这个触发点上出现
    --    动账类规则就是**重复落账的种子**，必须当场红。
    SELECT count(*) INTO n_bad_act FROM biz_flow_linkage_rule
     WHERE trigger_entity = 'PURCHASE_RETURN' AND deleted_at IS NULL
       AND action_code IN ('WRITE_INVENTORY', 'WRITE_INVENTORY_TXN', 'UPDATE_COST',
                           'UPDATE_PAYABLE_BALANCE', 'GENERATE_VOUCHER', 'REVERSE_VOUCHER');
    IF n_bad_act <> 0 THEN
        RAISE EXCEPTION 'V0351 自检失败：PURCHASE_RETURN 上有 % 条动账类规则（五本账由 confirm_tx 专用路径落账，通用引擎再跑一次就是重复落账）', n_bad_act;
    END IF;

    SELECT count(*) INTO n_alive FROM biz_flow_linkage_rule
     WHERE trigger_entity = 'PURCHASE_RETURN' AND action_code = 'SEND_NOTICE'
       AND trigger_status = 'CONFIRMED' AND deleted_at IS NULL;
    SELECT count(*) INTO n_take FROM biz_flow_linkage_rule
     WHERE trigger_entity = 'PURCHASE_RETURN' AND action_code = 'SEND_NOTICE'
       AND trigger_status = 'CONFIRMED' AND deleted_at IS NULL
       AND enabled = true AND action_params ->> 'mode' = 'TAKEOVER';

    RAISE NOTICE 'V0351 完成：PURCHASE_RETURN 预置 1 条 SEND_NOTICE 联动规则（确认 CONFIRMED）；当前存活 % 条、其中 enabled+TAKEOVER % 条（已有手工配置时按运维的来，不覆盖）', n_alive, n_take;
END $$;
