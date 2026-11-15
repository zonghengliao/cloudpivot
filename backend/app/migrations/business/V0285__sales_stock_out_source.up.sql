-- ======================================================
-- V0285：销售链 —— 出库单来源列 `source_type` / `source_id`（SO-10）
--
--   ① biz_stock_out 新增两列：`source_type`（单据来源类型，本卡只写 'SALES'）
--      与 `source_id`（来源单据 id，销售链 = biz_shipment.id）
--   ② 部分唯一索引 uk_biz_so_source：同一来源单据**只能生成一张出库单**
--      （幂等硬保证：并发 / 重试过账最多落一张出库单，不会重复扣库存）
--
-- ── 为什么要加这两列（而不是只靠 biz_shipment.stock_out_id）──────────
--   `biz_shipment.stock_out_id` 是发货单 → 出库单的**单向**回链，只能回答
--   「这张发货单出了哪张出库单」；从出库单侧看，销售发货出的库和一张领用出库
--   没有任何字段能区分（`out_type` 四值 ISSUE/SCRAP/LOSS/OTHER 是**领用性质**，
--   不是单据来源）。卡面要求「source_type=SALES」，且销售链后续的对账 / 退货红冲
--   都要按来源反查，所以补两列：来源类型 + 来源单据 id。
--   不新增 `out_type='SALES'`：那会把「领用性质」和「单据来源」混成一个维度，
--   并且要同时改 ck_biz_so_type、voucher 借方科目映射、前端字典与巡检词表
--   （四值域被 check.sh 的 out_type 词表闸钉住），改动面远大于收益。
--   销售出库的 out_type 仍用 OTHER（借方 6401 主营业务成本，与销售成本结转一致），
--   来源靠 `source_type='SALES'` 表达。
--
-- ── 号段 ──────────────────────────────────────────────
--   写入本文件时 backend/migrations/business/ 最大号 = V0284（SO-09 权限码卡）。
--   提交前再 `ls business | tail` 复核一次（并行卡会抢号段，已应用的不可改）。
--
-- ── 兼容性 ────────────────────────────────────────────
--   两列**可空**：既有出库单（手工领用 / 报废 / 盘亏 / 其他）没有来源单据，
--   `source_type IS NULL` 的老数据一行都不用回填，历史单据行为完全不变。
--   索引是**部分索引**（`WHERE source_type IS NOT NULL`），NULL 行不参与唯一性，
--   所以老数据里同物料同库位的多张出库单不会互相撞索引。
--
-- ── 幂等 ──────────────────────────────────────────────
--   `ADD COLUMN IF NOT EXISTS` + `CREATE INDEX IF NOT EXISTS`；
--   末尾 DO 块自检：列存在且可空 / 索引存在且为部分唯一索引 / 重复来源被拒 /
--   NULL 来源可多行。任一不符 → 迁移失败（不留半套结构）。
-- ======================================================

-- ---------- ① 两列 ----------
ALTER TABLE biz_stock_out ADD COLUMN IF NOT EXISTS source_type VARCHAR(20);
ALTER TABLE biz_stock_out ADD COLUMN IF NOT EXISTS source_id   UUID;

-- ---------- ② 来源唯一（部分索引） ----------
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_so_source
    ON biz_stock_out (source_type, source_id)
 WHERE source_type IS NOT NULL AND deleted_at IS NULL;

COMMENT ON COLUMN biz_stock_out.source_type IS
    '单据来源类型（SO-10：SALES = 销售发货；NULL = 手工建单：领用 / 报废 / 盘亏 / 其他）。与 source_id 一起由来源模块写入，手工建单入口不传';
COMMENT ON COLUMN biz_stock_out.source_id IS
    '来源单据 id（source_type=SALES 时为 biz_shipment.id）；uk_biz_so_source 保证同一来源只生成一张出库单（过账幂等）';

-- ---------- ③ 自检 ----------
DO $$
DECLARE
    n        INTEGER;
    d        TEXT;
BEGIN
    -- 3.1 两列存在
    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_stock_out'
       AND column_name IN ('source_type', 'source_id');
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0285 自检失败：biz_stock_out 缺 source_type/source_id 列（找到 % 列，期望 2）', n;
    END IF;

    -- 3.2 两列都可空（老数据不回填、手工建单不传）
    SELECT string_agg(column_name || ':' || is_nullable, ',' ORDER BY column_name) INTO d
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_stock_out'
       AND column_name IN ('source_type', 'source_id');
    IF d IS DISTINCT FROM 'source_id:YES,source_type:YES' THEN
        RAISE EXCEPTION 'V0285 自检失败：两列必须可空（实际 %）', d;
    END IF;

    -- 3.3 唯一索引存在且是**部分**唯一索引（谓词里必须有 source_type IS NOT NULL）
    SELECT count(*) INTO n
      FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_stock_out'
       AND indexname = 'uk_biz_so_source'
       AND indexdef LIKE '%UNIQUE%'
       AND indexdef LIKE '%source_type IS NOT NULL%';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0285 自检失败：uk_biz_so_source 不是「source_type IS NOT NULL」上的唯一索引';
    END IF;

    -- 3.4 行为自检：同一来源第二次插入必须被唯一索引拒（事务内探针，异常即回滚）
    BEGIN
        INSERT INTO biz_stock_out (out_no, out_type, warehouse_id, out_date, source_type, source_id)
        VALUES ('__V0285_PROBE__', 'OTHER', gen_random_uuid(), CURRENT_DATE,
                'SALES', '00000000-0000-0000-0000-0000000a2850'::uuid);
        INSERT INTO biz_stock_out (out_no, out_type, warehouse_id, out_date, source_type, source_id)
        VALUES ('__V0285_PROBE2__', 'OTHER', gen_random_uuid(), CURRENT_DATE,
                'SALES', '00000000-0000-0000-0000-0000000a2850'::uuid);
        RAISE EXCEPTION 'V0285 自检失败：同一来源（SALES + 同一 source_id）的第二张出库单未被 uk_biz_so_source 拒绝';
    EXCEPTION
        WHEN unique_violation THEN
            NULL;  -- 期望路径：撞唯一索引
    END;

    -- 3.5 NULL 来源可以多行（老数据与手工建单不被误伤）
    INSERT INTO biz_stock_out (out_no, out_type, warehouse_id, out_date)
    VALUES ('__V0285_NULL1__', 'ISSUE', gen_random_uuid(), CURRENT_DATE),
           ('__V0285_NULL2__', 'ISSUE', gen_random_uuid(), CURRENT_DATE);

    -- 探针全部撤掉（自检不往业务表留垃圾）
    DELETE FROM biz_stock_out WHERE out_no LIKE '__V0285%';
END
$$;
