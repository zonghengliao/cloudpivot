-- P9 / G2：PENDING 调拨没有审批实例（提交了但没人能批 → 卡死）
--
-- 单据：TK-20261031-000002 / TK-20261031-000004（P5 之前提交，approval_instance_id IS NULL）
--
-- 为什么选「作废」（PENDING → CANCELLED）而不是「退回草稿」（PENDING → DRAFT）：
--   1. `PENDING → CANCELLED` 是 transfer_model::LEGAL_EDGES 里**已有**的合法边，数据与状态机自洽；
--      `PENDING → DRAFT` 不在合法边里，要落地就得改 transfer_*.rs（本卡硬红线 6 禁止碰，P7/P8 在用），
--      而用 SQL 直接写一个状态机不认的状态，等于给下一张卡埋雷。
--   2. 语义上也对得上：这两张单从来没有审批实例，**谁也无法批准它**，它不是「用户想撤回的草稿」，
--      而是一张提交失败的单 → 作废 + 重新建单（新流程提交即生成实例，P5 已打通）。
--   3. 作废不动库存：PENDING 的单从未结算（没写过 biz_inventory），撤掉对账务零影响。
--
-- 幂等：第二遍 WHERE status='PENDING' 命中 0 行。
UPDATE biz_stock_transfer
   SET status = 'CANCELLED',
       cancelled_at = now(),
       cancel_reason = 'P9 数据修复：P5 之前提交且未生成审批实例的卡死单（无人可批），作废后请重新建单',
       updated_at = now()
 WHERE transfer_no IN ('TK-20261031-000002', 'TK-20261031-000004')
   AND status = 'PENDING'
   AND deleted_at IS NULL;
