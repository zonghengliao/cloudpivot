-- ============================================================
-- V0238：P2-9 单据元数据补齐（两条 P2 合并）
--
-- ① DC-19 业务单据挂不上跟进任务
--    `biz_task` 39 列实测，没有「这张任务说的是哪张单」的列 ⇒ 采购员想给
--    某张采购单挂一条「催货」任务，只能把单号抄进标题（靠文字关联，不可查、
--    不可点回）。本迁移加 `doc_type` / `doc_id` 两列 + 一条部分索引。
--
-- ② DC-23 采购单无仓库快照
--    `biz_purchase_order.warehouse_id` 是裸 uuid，仓库改名后历史单据只能显示
--    「现名」（甚至查不到，被软删后前端只能显示 uuid 前缀）。供应商侧早有
--    `supplier_name_snapshot`（A3），仓库侧补一列同款快照，**只做采购单试点**
--    （收货 / 调拨 / 盘点等单据的快照留给后续卡，一次只做一条链）。
--
-- # 为什么 doc_type 用 varchar(32) 且**不加 CHECK 值域**
--
-- 白名单（PURCHASE_ORDER / STOCK_IN / CONTRACT / PAYABLE / VOUCHER）只有一份
-- 权威实现：`business/shared/doc_kind.rs` 的 `DocKind`（DC-21 收敛后的唯一词表）
-- 负责「有哪些业务对象」，本卡只补两张小表：`task/doc_ref.rs::DOC_TARGETS` 里
-- 「哪些类型允许被任务挂」、「该类型取哪个列当单号」。加一种挂接类型 = 加一行常量，
-- 不需要迁移（这也是不写 CHECK 值域的理由之一）。
-- DC-21 的收敛方向是「词表进字典表」，那时 CHECK 也要跟着改成外键 —— 现在
-- 写死值域只会多一次迁移。**结构性约束仍然进库**：`doc_type` 与 `doc_id`
-- 必须同时有值或同时为空（`ck_biz_task_doc_pair`），这条不会随词表变化。
--
-- # 为什么 doc_id 没有外键
--
-- `doc_id` 是**弱引用**（指向 5 张不同表之一，跨表外键在 PG 里不存在）。
-- 真正的存在性校验在 handler 里做（`doc_ref::ensure_exists`：select 一次、
-- 软删一并判掉，错误文案能点名单号），与 ERP 侧 `project_id` / `contract_id`
-- 同口径（R1 单一数据源）。
--
-- # 为什么索引是 (doc_type, doc_id) 部分索引
--
-- 唯一读路径是反向查询「这张单上挂了哪些任务」（`GET /business/task/by-doc`
-- 与采购单详情底部的「关联任务」区）。`doc_type` 在前是因为同类型单据的
-- 任务聚在一起（明细页按类型取、再按 id 命中）；部分索引 `WHERE doc_id IS
-- NOT NULL` 把存量「没挂单」的任务排除在索引外（39 列的表里绝大多数任务
-- 永远不会有单据）。
--
-- # 幂等
--
-- `ADD COLUMN IF NOT EXISTS` / `CREATE INDEX IF NOT EXISTS` / `COMMENT ON`
-- 全部可重复执行；末尾 DO 块 fail-closed 自检（列在、类型对、可空、CHECK 在、
-- 索引在），装错了直接 RAISE 回滚。
-- ============================================================

-- ---------- DC-19：任务可挂业务单据 ----------
ALTER TABLE biz_task
    ADD COLUMN IF NOT EXISTS doc_type VARCHAR(32),
    ADD COLUMN IF NOT EXISTS doc_id   UUID;

COMMENT ON COLUMN biz_task.doc_type IS
    '关联单据类型（白名单 = DocKind（business/shared/doc_kind.rs）∩ task/doc_ref.rs::DOC_TARGETS：PURCHASE_ORDER / STOCK_IN / CONTRACT / PAYABLE / VOUCHER）。与 doc_id 必须成对出现，库侧 ck_biz_task_doc_pair 兜底；值域校验在 handler（词表只有一份，DC-21 收敛后进字典表）';
COMMENT ON COLUMN biz_task.doc_id IS
    '关联单据主键（弱引用，指向 doc_type 对应的表）。建单时 handler 校验「存在且未软删」，库侧无外键（软删拦不住）。反向查询走 idx_biz_task_doc';

-- 结构性不变量：要么都空（自由任务），要么都有值（挂了单）
ALTER TABLE biz_task DROP CONSTRAINT IF EXISTS ck_biz_task_doc_pair;
ALTER TABLE biz_task
    ADD CONSTRAINT ck_biz_task_doc_pair
    CHECK ((doc_type IS NULL) = (doc_id IS NULL));

CREATE INDEX IF NOT EXISTS idx_biz_task_doc
    ON biz_task (doc_type, doc_id)
    WHERE doc_id IS NOT NULL;

COMMENT ON INDEX idx_biz_task_doc IS
    'P2-9 / DC-19：按 (doc_type, doc_id) 反查「这张单上挂了哪些任务」（采购单详情「关联任务」区、任务详情「关联单据」）；部分索引排除未挂单的自由任务';

-- ---------- DC-23：采购单仓库名快照 ----------
ALTER TABLE biz_purchase_order
    ADD COLUMN IF NOT EXISTS warehouse_name_snapshot VARCHAR(128);

COMMENT ON COLUMN biz_purchase_order.warehouse_name_snapshot IS
    '收货仓库名称快照（建单/改单那一刻的 biz_warehouse.warehouse_name）。列表只读这一列（仓库改名后历史单仍显示当时的名字）；详情按下发口径回填现值。存量单为 NULL = P2-9 之前的老单，显示时由 handler 回落到关联查（不回填）';

-- ---------- 自检：列在、类型对、可空、CHECK 在、索引在 ----------
DO $$
DECLARE
    n_doc_type integer;
    n_doc_id   integer;
    n_ck       integer;
    n_index    integer;
    n_wh       integer;
BEGIN
    SELECT count(*) INTO n_doc_type
      FROM information_schema.columns
     WHERE table_schema = current_schema()
       AND table_name = 'biz_task'
       AND column_name = 'doc_type'
       AND data_type = 'character varying'
       AND is_nullable = 'YES';
    IF n_doc_type <> 1 THEN
        RAISE EXCEPTION 'V0238 自检失败：biz_task.doc_type 必须存在且为可空 varchar（实际匹配 % 列）', n_doc_type;
    END IF;

    SELECT count(*) INTO n_doc_id
      FROM information_schema.columns
     WHERE table_schema = current_schema()
       AND table_name = 'biz_task'
       AND column_name = 'doc_id'
       AND data_type = 'uuid'
       AND is_nullable = 'YES';
    IF n_doc_id <> 1 THEN
        RAISE EXCEPTION 'V0238 自检失败：biz_task.doc_id 必须存在且为可空 uuid（实际匹配 % 列）', n_doc_id;
    END IF;

    SELECT count(*) INTO n_ck
      FROM pg_constraint
     WHERE conname = 'ck_biz_task_doc_pair'
       AND conrelid = 'biz_task'::regclass;
    IF n_ck <> 1 THEN
        RAISE EXCEPTION 'V0238 自检失败：ck_biz_task_doc_pair 未建成（实际 % 个）', n_ck;
    END IF;

    SELECT count(*) INTO n_index
      FROM pg_indexes
     WHERE schemaname = current_schema()
       AND tablename = 'biz_task'
       AND indexname = 'idx_biz_task_doc';
    IF n_index <> 1 THEN
        RAISE EXCEPTION 'V0238 自检失败：idx_biz_task_doc 未建成（实际 % 个）', n_index;
    END IF;

    SELECT count(*) INTO n_wh
      FROM information_schema.columns
     WHERE table_schema = current_schema()
       AND table_name = 'biz_purchase_order'
       AND column_name = 'warehouse_name_snapshot'
       AND data_type = 'character varying'
       AND is_nullable = 'YES';
    IF n_wh <> 1 THEN
        RAISE EXCEPTION 'V0238 自检失败：biz_purchase_order.warehouse_name_snapshot 必须存在且为可空 varchar（实际匹配 % 列）', n_wh;
    END IF;

    RAISE NOTICE 'V0238：任务可挂单据（doc_type/doc_id + ck + 部分索引）+ 采购单仓库名快照已就位';
END $$;
