-- ======================================================
-- V0305：销售链 —— 退货过账结构补丁（SO-30）
--
--   ① biz_stock_in 新增 `source_type` / `source_id` 两列
--      + 部分唯一索引 uk_biz_si_source_doc（同一来源单据只能生成一张入库单）
--   ② biz_invoice 的来源白名单补 'SALES_RETURN'（红字发票的来源是退货单）
--
-- ── 号段（开工实测复核）──────────────────────────────────
--   卡面未点名号段；写入本文件时 backend/migrations/business/ 最大号 = V0304
--   （V0304__sales_return_permissions，SO-29 提交 2c353b7），本卡取 **V0305**。
--   迁移号会被并行卡抢走，**提交前**再 `ls business | tail` 复核一次。
--
-- ── 为什么给 biz_stock_in 加来源列（与 V0285 给出库单加 source_type 同一手法）──
--   ARCH-7 §15.2.3 逐字要求「生成入库单（复用 biz_stock_in，source_type=SALES_RETURN）」。
--   `biz_stock_in.in_type` 的域是 PURCHASE / RETURN / OTHER —— 那是**入库性质**
--   （ck_biz_si_type 钉死），不是**单据来源**：一张退货入库和一张手工退货入库在
--   in_type 上完全一样，从入库单侧看不出「是哪张退货单退回来的」。
--   所以按 V0285 的口径补两列：来源类型 + 来源单据 id，退货过账写
--   ('SALES_RETURN', biz_sales_return.id)。**不新增 in_type 域值**：
--   那要同时改 ck_biz_si_type、前端字典、序列号来源映射与巡检词表，改动面远大于收益；
--   退货入库的 in_type 仍是既有的 'RETURN'（`serial_source_of_in_type` 已把它映射到
--   序列号来源 RETURN —— 这条链一行都不用改）。
--
-- ── 两列可空 ────────────────────────────────────────────
--   既有入库单（采购收货 / 期初 / 复制 / 其他）没有来源单据，`source_type IS NULL`
--   的老数据一行不回填，行为完全不变。索引是**部分索引**
--   （`WHERE source_type IS NOT NULL AND deleted_at IS NULL`），NULL 行不参与唯一性。
--
-- ── 幂等 ──────────────────────────────────────────────
--   `ADD COLUMN IF NOT EXISTS` + `CREATE INDEX IF NOT EXISTS` + 约束先 DROP IF EXISTS 再 ADD；
--   末尾 DO 块自检：列存在且可空 / 索引是部分唯一索引 / 重复来源被拒 / NULL 来源可多行 /
--   biz_invoice 来源白名单含 SALES_RETURN。任一不符 → 迁移失败（不留半套结构）。
-- ======================================================

-- ---------- ① 入库单来源两列 ----------
ALTER TABLE biz_stock_in ADD COLUMN IF NOT EXISTS source_type VARCHAR(20);
ALTER TABLE biz_stock_in ADD COLUMN IF NOT EXISTS source_id   UUID;

COMMENT ON COLUMN biz_stock_in.source_type IS
    '单据来源类型（SO-30：SALES_RETURN = 销售退货过账生成的入库单；NULL = 采购收货 / 期初 / 复制 / 其他）。与 source_id 一起由来源模块写入';
COMMENT ON COLUMN biz_stock_in.source_id IS
    '来源单据 id（source_type=SALES_RETURN 时为 biz_sales_return.id）；uk_biz_si_source_doc 保证同一来源只生成一张入库单（过账幂等）';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_si_source_doc
    ON biz_stock_in (source_type, source_id)
 WHERE source_type IS NOT NULL AND deleted_at IS NULL;

-- ---------- ② 发票来源白名单补 SALES_RETURN ----------
-- 红字发票（OUT_RED）的来源单据就是那张退货单；不补这个域值，退货过账造红票
-- 只能谎报成 MANUAL / SHIPMENT（追溯给假答案）。
ALTER TABLE biz_invoice DROP CONSTRAINT IF EXISTS ck_biz_invoice_source;
ALTER TABLE biz_invoice
    ADD CONSTRAINT ck_biz_invoice_source
    CHECK (source_type IS NULL
        OR source_type IN ('SALES_ORDER','SHIPMENT','SALES_OUT','MANUAL','SALES_RETURN'));

COMMENT ON COLUMN biz_invoice.source_type IS
    '来源类型：SALES_ORDER / SHIPMENT / SALES_OUT / MANUAL / SALES_RETURN（SO-30 退货红冲票）；NULL = 未标注来源';

-- ---------- ③ 自检 ----------
DO $$
DECLARE
    n INTEGER;
    d TEXT;
BEGIN
    -- 3.1 两列存在
    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_stock_in'
       AND column_name IN ('source_type', 'source_id');
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0305 自检失败：biz_stock_in 缺 source_type/source_id 列（找到 % 列，期望 2）', n;
    END IF;

    -- 3.2 两列都可空（老数据不回填、采购路径不传）
    SELECT string_agg(column_name || ':' || is_nullable, ',' ORDER BY column_name) INTO d
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_stock_in'
       AND column_name IN ('source_type', 'source_id');
    IF d IS DISTINCT FROM 'source_id:YES,source_type:YES' THEN
        RAISE EXCEPTION 'V0305 自检失败：两列必须可空（实际 %）', d;
    END IF;

    -- 3.3 唯一索引是**部分**唯一索引（谓词里必须有 source_type IS NOT NULL）
    SELECT count(*) INTO n
      FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_stock_in'
       AND indexname = 'uk_biz_si_source_doc'
       AND indexdef LIKE '%UNIQUE%'
       AND indexdef LIKE '%source_type IS NOT NULL%';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0305 自检失败：uk_biz_si_source_doc 不是「source_type IS NOT NULL」上的唯一索引';
    END IF;

    -- 3.4 行为自检：同一来源第二次插入必须被唯一索引拒（事务内探针，异常即回滚）
    BEGIN
        INSERT INTO biz_stock_in (in_no, in_type, warehouse_id, in_date, source_type, source_id,
                                  is_sourceless)
        VALUES ('__V0305_PROBE__', 'RETURN', gen_random_uuid(), CURRENT_DATE,
                'SALES_RETURN', '00000000-0000-0000-0000-0000000a3050'::uuid, TRUE);
        INSERT INTO biz_stock_in (in_no, in_type, warehouse_id, in_date, source_type, source_id,
                                  is_sourceless)
        VALUES ('__V0305_PROBE2__', 'RETURN', gen_random_uuid(), CURRENT_DATE,
                'SALES_RETURN', '00000000-0000-0000-0000-0000000a3050'::uuid, TRUE);
        RAISE EXCEPTION 'V0305 自检失败：同一来源（SALES_RETURN + 同一 source_id）的第二张入库单未被 uk_biz_si_source_doc 拒绝';
    EXCEPTION
        WHEN unique_violation THEN
            NULL;  -- 期望路径：撞唯一索引
    END;

    -- 3.5 NULL 来源可以多行（老数据与采购路径不被误伤）
    INSERT INTO biz_stock_in (in_no, in_type, warehouse_id, in_date, is_sourceless)
    VALUES ('__V0305_NULL1__', 'OTHER', gen_random_uuid(), CURRENT_DATE, TRUE),
           ('__V0305_NULL2__', 'OTHER', gen_random_uuid(), CURRENT_DATE, TRUE);

    -- 探针全部撤掉（自检不往业务表留垃圾）
    DELETE FROM biz_stock_in WHERE in_no LIKE '__V0305%';

    -- 3.6 发票来源白名单已含 SALES_RETURN（且旧四值一个没丢）
    SELECT pg_get_constraintdef(oid) INTO d
      FROM pg_constraint
     WHERE conname = 'ck_biz_invoice_source' AND conrelid = 'public.biz_invoice'::regclass;
    IF d IS NULL OR d NOT LIKE '%SALES_RETURN%' THEN
        RAISE EXCEPTION 'V0305 自检失败：ck_biz_invoice_source 不含 SALES_RETURN（实际 %）', d;
    END IF;
    IF d NOT LIKE '%SALES_ORDER%' OR d NOT LIKE '%SALES_OUT%'
       OR d NOT LIKE '%SHIPMENT%' OR d NOT LIKE '%MANUAL%' THEN
        RAISE EXCEPTION 'V0305 自检失败：ck_biz_invoice_source 丢掉了旧域值（实际 %）', d;
    END IF;

    RAISE NOTICE 'V0305 OK：biz_stock_in 来源两列 + 部分唯一索引 + 发票来源白名单 SALES_RETURN';
END
$$;
