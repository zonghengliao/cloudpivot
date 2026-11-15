-- ======================================================
-- V0302：销售订单关联销售合同（SO-27，ARCH-7 §15.1.2 ③ / §15.1.3）
--
--   ① biz_sales_order.contract_id uuid（可空）—— 一张订单最多挂一张销售合同；
--   ② 外键 fk_biz_sales_order_contract → biz_contract(id)，ON DELETE SET NULL；
--   ③ 部分索引 idx_biz_sales_order_contract —— 「这张合同下有哪些订单」的回查路径
--      （额度守卫每次现算 SUM，见 handler 的 guard_contract_amount）。
--
-- ── 号段（开工实测复核）──────────────────────────────────
-- 写入本文件时 backend/migrations/business/ 最大号 = V0301
--   （V0301 是 SO-23 衍生卡 t_1dadde81 用掉的流定义修复），本卡顺延取 V0302。
--   卡面写的「V0268」已被 V0268__sales_* 系列占用（V0264–V0297 全是销售链），
--   **不能复用号段**（已应用迁移不可改、指纹校验会炸）。
--
-- ── 为什么这里**建**硬外键，而同链其余单据不建（与 ADR-06 的差异，单列说明）──
--   ADR-06 / ARCH-2 DEC-02 的「单据链零外键」基线是为了**多态引用**（合同乙方可指向
--   供应商或客户，PG 无法用一个列表达），以及避免单据之间互相锁死。
--   `biz_sales_order.contract_id` 不属于这两种情形：
--     · 它是**单态**引用（只可能指向 biz_contract），FK 能真实表达约束；
--     · 合同走软删（deleted_at），硬外键不影响软删；万一有物理清场脚本，
--       ON DELETE SET NULL 把订单的关联打个空，订单本体与金额快照一行不动
--       （禁止用 CASCADE：合同没了不等于订单没发生过）。
--   卡面第 1 条逐字要求 FK，本文件照做；若架构侧要回到「零外键」基线，
--   撤掉 fk 只需 `ALTER TABLE biz_sales_order DROP CONSTRAINT fk_biz_sales_order_contract`
--   （应用层校验与索引不依赖它，删了不影响业务）。
--
-- ── 存量（不回填）──────────────────────────────────────
--   历史订单一律 contract_id = NULL：它们真的没挂过合同，回填等于编造审计事实。
--   可空 ⇒ 不受影响的旧写入路径（建单不带 contract_id）保持原样。
--
-- ── 写法纪律（照抄 V0198 头注释）────────────────────────
--   ① 文件内不写 BEGIN; … COMMIT;（迁移器自己在事务里跑）；
--   ② 存在性守卫（列 / 约束 / 索引）⇒ up 重复执行 no-op；
--   ③ 不建 CONCURRENTLY（事务内不允许）；销售订单量级 <100 万，普通索引即可。
-- ======================================================

-- ---------- ① 加列（只改元数据，PG 11+ 不重写堆表） ----------
ALTER TABLE biz_sales_order
    ADD COLUMN IF NOT EXISTS contract_id uuid;

COMMENT ON COLUMN biz_sales_order.contract_id IS
    '销售合同（biz_contract.id，contract_type 必须是 SALES）。可空 = 未挂合同（历史单与散单）；'
    '建单 / 改单时由 handler 校验存在性、类型、状态与乙方=订单客户，并对合同额度做守卫（409 / 66019）';

-- ---------- ② 加外键（存在性守卫） ----------
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_biz_sales_order_contract') THEN
        ALTER TABLE biz_sales_order
            ADD CONSTRAINT fk_biz_sales_order_contract
            FOREIGN KEY (contract_id) REFERENCES biz_contract (id) ON DELETE SET NULL;
    END IF;
END $$;

-- ---------- ③ 建索引（合同维度回查：这张合同下有哪些订单） ----------
-- 部分索引：绝大多数订单不挂合同（NULL），只为有合同的行建条目。
-- 排序键 order_date DESC 与「累计下单额」的扫描顺序一致（守卫只 SUM，不排序；
-- 列表侧「按合同查订单」按 date DESC 展示）。
CREATE INDEX IF NOT EXISTS idx_biz_sales_order_contract
    ON biz_sales_order (contract_id, order_date DESC)
    WHERE contract_id IS NOT NULL;

-- ---------- ④ 迁移内自检（结构 + 存量口径播报 + 方向性异常即中止） ----------
DO $$
DECLARE
    col_count  bigint;
    fk_count   bigint;
    idx_count  bigint;
    bad_type   bigint;
    attached   bigint;
    total_rows bigint;
BEGIN
    SELECT count(*) INTO col_count
      FROM information_schema.columns
     WHERE table_name = 'biz_sales_order' AND column_name = 'contract_id';
    IF col_count <> 1 THEN
        RAISE EXCEPTION '① 自检失败：biz_sales_order.contract_id 不存在';
    END IF;

    SELECT count(*) INTO fk_count
      FROM pg_constraint
     WHERE conname = 'fk_biz_sales_order_contract' AND contype = 'f';
    IF fk_count <> 1 THEN
        RAISE EXCEPTION '② 自检失败：fk_biz_sales_order_contract 不是外键或不存在';
    END IF;

    SELECT count(*) INTO idx_count
      FROM pg_indexes
     WHERE tablename = 'biz_sales_order' AND indexname = 'idx_biz_sales_order_contract';
    IF idx_count <> 1 THEN
        RAISE EXCEPTION '③ 自检失败：idx_biz_sales_order_contract 不存在';
    END IF;

    -- 方向性异常：存量订单若已挂着非 SALES 合同（只可能是人工 SQL 塞的），先停下来问人
    SELECT count(*) INTO bad_type
      FROM biz_sales_order o
      JOIN biz_contract c ON c.id = o.contract_id
     WHERE c.contract_type <> 'SALES';
    IF bad_type > 0 THEN
        RAISE EXCEPTION '④ 自检失败：% 张订单挂着非 SALES 合同，先确认数据来源', bad_type;
    END IF;

    SELECT count(*) INTO attached FROM biz_sales_order WHERE contract_id IS NOT NULL;
    SELECT count(*) INTO total_rows FROM biz_sales_order;

    RAISE NOTICE '④ 自检通过：1 列 / 1 外键（ON DELETE SET NULL）/ 1 部分索引；'
                 '存量订单 % 张，其中已挂合同 % 张（不回填，历史单一律 NULL）',
                 total_rows, attached;
END $$;
