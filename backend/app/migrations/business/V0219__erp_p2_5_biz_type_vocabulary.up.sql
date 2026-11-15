-- ============================================================================
-- V0219__erp_p2_5_biz_type_vocabulary.up.sql
--
-- 【P2-5】业务类型词表收敛（ARCH-3 §5 DC-21）。
--
-- 逐列 grep + 生产库 distinct 实测（`docs/architecture/P2-5-业务类型词表盘点.md`）
-- 之后，**真正需要改数据的只有一处**：
--
--   ① `biz_status_log.entity_type = 'TRANSFER'`
--      同一个「库存调拨单」在别处都叫 `STOCK_TRANSFER`：
--        · 表名 `biz_stock_transfer`
--        · `biz_inventory_txn.doc_type = 'STOCK_TRANSFER'`
--        · `biz_flow_def.biz_type = 'STOCK_TRANSFER'`（审批）
--      只有状态日志写 `TRANSFER` → 按 entity_type 分组的报表 / 巡检 / 夹具清场
--      会把调拨拆成两行。本迁移统一成 `STOCK_TRANSFER`。
--      代码侧 `state_machine::ENTITY_TRANSFER` 已改为从
--      `shared::doc_kind::DocKind::StockTransfer.as_str()` 取值，不再写字面量。
--
-- # 不改的东西（附证据，免得后来人以为漏了）
--
-- · `biz_flow_instance.biz_type`：卡面假设这里是小写 `purchase_order`，
--   实测（生产库 distinct）= `PO_APPROVE` / `STOCK_IN_POST` / `CONTRACT_APPROVE` / …，
--   与 `biz_flow_def.biz_type` **同名同值**，本来就已经统一；唯一写入口是
--   `purchase_bridge::BIZ_TYPE = "PO_APPROVE"`。**不迁移流程实例** ——
--   迁移历史审批记录是有风险的（历史查询 / 报表按 biz_type 归档），且没有必要。
-- · 11 条已发布预置流程的 biz_type：全部落在 `doc_kind::FlowBizType` 域内（迁移末尾留档）。
-- · 其余 6 根轴（审批动作 / 库存变动动作 / 流水来源 / 凭证来源 / 序列号来源 /
--   单据子类型）：**不是同一概念的不同写法**，各自独立成枚举，数据不动。
-- · `biz_inventory_txn.doc_type = 'ADJUST'`（存量 1 行，来自
--   `scripts/datafix/p14/20_reverse_inventory.up.sql` 的历史修正）：语义是
--   「库存手工调整」、没有对应单据，强行改写成别的单据类型等于篡改业务含义
--   （卡面「不改业务流程语义」）→ **保留原值**，已登记进
--   `doc_kind::TxnDocType::Adjust`，巡检不再把它当域外值。
--
-- # 为什么这里**不写**值域白名单
--
-- 值域的唯一真相是 `backend/src/business/shared/doc_kind.rs` 的 `COLUMN_DOMAINS`，
-- 由 `scripts/verify/biz_type_domain.py`（查库）与 `doc_kind_tests`（编译期/单测）守护。
-- 在迁移里再抄一份 14 列的白名单 = 造出**第 9 套词表**，正是本卡要消灭的东西。
-- 所以这里只校验「本迁移承诺的那一件事」：旧别名 `TRANSFER` 已经彻底消失。
--
-- # 约束
--
-- `biz_status_log.entity_type` 是 `VARCHAR(32)` 且**没有** CHECK 白名单（见 V0159），
-- 所以不需要「先改约束再改数据」。也不新增 CHECK：同列还要写
-- `FLOW_LINKAGE_CFG` 这类配置实体，硬白名单会让「新增一个实体」变成
-- 「必须再发一条迁移改约束」，那正是 DC-21 要治的病。
-- ============================================================================

-- ① 调拨单：entity_type TRANSFER → STOCK_TRANSFER
UPDATE biz_status_log
   SET entity_type = 'STOCK_TRANSFER'
 WHERE entity_type = 'TRANSFER';

-- ② 自检：旧别名必须彻底消失（重复执行 / 手工写脏的库也要收干净）
DO $$
DECLARE
    leftover bigint;
    fixed    bigint;
BEGIN
    SELECT count(*) INTO leftover FROM biz_status_log WHERE entity_type = 'TRANSFER';
    IF leftover <> 0 THEN
        RAISE EXCEPTION 'V0219 自检失败：仍有 % 行 biz_status_log.entity_type = ''TRANSFER''', leftover;
    END IF;
    SELECT count(*) INTO fixed FROM biz_status_log WHERE entity_type = 'STOCK_TRANSFER';
    RAISE NOTICE 'V0219：调拨状态日志已统一为 STOCK_TRANSFER（当前 % 行，旧别名 0 行）', fixed;
END $$;

-- ③ 留档：已发布预置审批流的条数（只读，不改数据 —— 给运维一个可核对数字）
DO $$
DECLARE
    published bigint;
BEGIN
    SELECT count(*) INTO published
      FROM biz_flow_def
     WHERE status = 'PUBLISHED' AND deleted_at IS NULL;
    RAISE NOTICE 'V0219：已发布预置审批流 % 条（biz_type 全部在 FlowBizType 域内，见 doc_kind.rs）', published;
END $$;
