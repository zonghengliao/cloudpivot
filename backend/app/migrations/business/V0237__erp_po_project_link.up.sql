-- ============================================================
-- V0237：P2-8 采购单挂项目（ARCH-3 §5 DC-16 第一步）
--
-- # 为什么只加一列、只加在采购单上
--
-- 审计结论是「30 张 ERP 表 0 张有 project_id ⇒ 项目成本核算这条业务价值没接上」。
-- 一次给 30 张表都加 project_id 是反模式：每张单据的「成本归集时点」语义都不同
-- （采购单按订单金额？收货单按入库金额？应付按已确认应付？），铺开了以后
-- 口径没人拍板、巡检也写不出来。所以本迁移**只做第一条链的头**：
-- 采购单下单时可选挂一个项目，后续收货 / 应付 / 付款按单据链路派生，不各自重挂。
--
-- 待挂清单（本卡只记录，不动结构）：`biz_stock_in` / `biz_payable` / `biz_payment`。
-- 口径论证与「谁该挂、谁不该挂」见 `docs/architecture/P2-8-项目挂ERP口径.md`。
--
-- # 为什么可空、为什么没有外键
--
-- ① 可空：存量单（生产 144 现有 18 张）没有项目是**历史事实**，强制补值等于
--    让运维去猜「这单属于哪个项目」—— 那种数据比 NULL 更坏（R1 单一数据源，
--    不做假回填）。新单也不强制：不是所有采购都归项目（行政、备件）。
-- ② 无外键：与仓库其余 ERP 单据完全一致（`supplier_id` / `warehouse_id` /
--    `contract_id` 全是裸 uuid）。原因有两条且都成立：
--    · 软删语义：`biz_project` 走 `deleted_at` 软删，外键拦不住「已软删」，
--      真正的校验在 handler 里 select 一次（错文案能给中文，外键只能给 PG 原文）；
--    · 历史数据：项目真要物理清理时，外键会把单据一起卡死或级联删账。
--
-- # 为什么索引是 (project_id, order_date DESC) 而不是只 (project_id)
--
-- 唯一的读路径是「按项目筛采购单，按下单日期倒序」：列表默认排序就是
-- `ORDER BY order_date DESC, order_no DESC`（`PURCHASE_ORDER_ORDER`）。
-- 复合索引让「筛 + 排」都走索引，与 V0198 的 `idx_biz_po_contract
-- (contract_id, order_date DESC)` 同一口径。部分索引 `WHERE project_id IS NOT NULL`
-- 把存量的 18 张空值行排除在索引外（不占空间，也不影响全表扫）。
--
-- # 幂等
--
-- `ADD COLUMN IF NOT EXISTS` + `CREATE INDEX IF NOT EXISTS` + `COMMENT ON`，
-- 可重复执行；末尾 DO 块做 fail-closed 自检（列/索引/可空性），装错了直接报错回滚。
-- ============================================================

ALTER TABLE biz_purchase_order
    ADD COLUMN IF NOT EXISTS project_id UUID;   -- 所属项目（弱引用 biz_project.id，可空）

COMMENT ON COLUMN biz_purchase_order.project_id IS
    '所属项目（弱引用 biz_project.id，可空）。建单/改单时 handler 校验「存在且未软删」，库侧不建外键（软删语义 + 历史数据）。存量单为 NULL = 未归集项目，不做回填；成本归集口径见 docs/architecture/P2-8-项目挂ERP口径.md';

CREATE INDEX IF NOT EXISTS idx_biz_po_project
    ON biz_purchase_order (project_id, order_date DESC)
    WHERE project_id IS NOT NULL;

COMMENT ON INDEX idx_biz_po_project IS
    'P2-8：按项目筛采购单（列表 ORDER BY order_date DESC），部分索引排除未归集项目的存量单';

-- ---------- 自检：列在、类型对、可空、索引在 ----------
DO $$
DECLARE
    n_col   integer;
    n_null  integer;
    n_index integer;
BEGIN
    SELECT count(*) INTO n_col
      FROM information_schema.columns
     WHERE table_schema = current_schema()
       AND table_name = 'biz_purchase_order'
       AND column_name = 'project_id'
       AND data_type = 'uuid'
       AND is_nullable = 'YES';
    IF n_col <> 1 THEN
        RAISE EXCEPTION 'V0237 自检失败：biz_purchase_order.project_id 必须存在且为可空 uuid（实际匹配 % 列）', n_col;
    END IF;

    -- 明确断言「可空」：将来谁把它改成 NOT NULL，存量 18 张单会一起炸
    SELECT count(*) INTO n_null
      FROM information_schema.columns
     WHERE table_schema = current_schema()
       AND table_name = 'biz_purchase_order'
       AND column_name = 'project_id'
       AND is_nullable = 'NO';
    IF n_null <> 0 THEN
        RAISE EXCEPTION 'V0237 自检失败：project_id 不可是 NOT NULL（存量单未归集项目）';
    END IF;

    SELECT count(*) INTO n_index
      FROM pg_indexes
     WHERE schemaname = current_schema()
       AND tablename = 'biz_purchase_order'
       AND indexname = 'idx_biz_po_project';
    IF n_index <> 1 THEN
        RAISE EXCEPTION 'V0237 自检失败：idx_biz_po_project 未建成（实际 % 个）', n_index;
    END IF;

    RAISE NOTICE 'V0237：采购单已挂项目（project_id 可空 + idx_biz_po_project 部分索引）';
END $$;
