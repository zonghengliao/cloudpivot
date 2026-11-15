-- ============================================================================
-- V0376 down：撤销「完工入库 in_type + PRODUCTION + 两列」
--
-- 只回滚本迁移动过的东西，**不删行、不动金额、不动其他列**：
--   ① 断言「没有 PRODUCTION 单据」（有就拒绝执行并点名数量）；
--   ② 删 ck_biz_si_production（它引用两个新列，必须先删）；
--   ③ ck_biz_si_type 值域回滚到 4 值（PURCHASE/RETURN/OTHER/SOURCELESS）；
--   ④ 删两条局部索引 + 两列；
--   ⑤ 列注释恢复为 V0330 的写法。
--
-- # 顺序不能反（本迁移最容易踩的坑）
--   PG 没有 `ADD CONSTRAINT IF NOT EXISTS`，回滚只能 DROP + ADD 重建；
--   而旧 4 值 CHECK **加不上去**只要库里还有一行 PRODUCTION ⇒ 所以 ① 的数据护栏必须在 ③ 之前，
--   并且要么让人先处理单据，要么整条回滚失败（绝不静默丢单）。
--
-- # 幂等
--   DROP … IF EXISTS 全家桶；护栏读的 in_type 列一直存在，第二遍执行时
--   PRODUCTION 行数必为 0（第一遍已断言过），所以重复回滚不会误报。
-- ============================================================================

-- ---------- ① 数据护栏：有 PRODUCTION 单就拒绝回滚 ----------
DO $$
DECLARE
    n_prod BIGINT;
BEGIN
    SELECT count(*) INTO n_prod FROM biz_stock_in WHERE in_type = 'PRODUCTION';
    IF n_prod > 0 THEN
        RAISE EXCEPTION
            'V0376 down 拒绝执行：还有 % 张完工入库单（in_type=PRODUCTION）。'
            '旧值域只有 4 个值，回滚会把这些单据变成非法行（或直接删掉它们的成本对象/批次归属）。'
            '请先作废/删除这些单据（或把状态与归属清干净）后再回滚', n_prod;
    END IF;
END
$$;

-- ---------- ② 先删引用新列的约束 ----------
ALTER TABLE biz_stock_in DROP CONSTRAINT IF EXISTS ck_biz_si_production;

-- ---------- ③ 值域回滚：5 值 → 4 值（V0330 原文） ----------
ALTER TABLE biz_stock_in DROP CONSTRAINT IF EXISTS ck_biz_si_type;
ALTER TABLE biz_stock_in ADD CONSTRAINT ck_biz_si_type
    CHECK (in_type IN ('PURCHASE', 'RETURN', 'OTHER', 'SOURCELESS'));

COMMENT ON COLUMN biz_stock_in.in_type IS
    '入库类型（词表 DocSubtype::IN_TYPES）：PURCHASE 采购入库（有采购单）/ RETURN 退货入库 / '
    'OTHER 其他入库（无采购单、无供应商）/ SOURCELESS 无源入库（无采购单、有供应商，is_sourceless=true）';

-- ---------- ④ 删索引、删列 ----------
DROP INDEX IF EXISTS idx_biz_si_cost_obj;
DROP INDEX IF EXISTS idx_biz_si_cost_period;
ALTER TABLE biz_stock_in DROP COLUMN IF EXISTS cost_object_id;
ALTER TABLE biz_stock_in DROP COLUMN IF EXISTS cost_period_id;

-- ---------- ⑤ 反向自检 ----------
DO $$
DECLARE
    ck_type  TEXT;
    ck_prod  BIGINT;
    col_left BIGINT;
    idx_left BIGINT;
    n_scope  BIGINT;
BEGIN
    SELECT pg_get_constraintdef(oid) INTO ck_type
      FROM pg_constraint
     WHERE conname = 'ck_biz_si_type' AND conrelid = 'public.biz_stock_in'::regclass;
    IF ck_type IS NULL OR ck_type LIKE '%PRODUCTION%' THEN
        RAISE EXCEPTION 'V0376 down 自检失败：ck_biz_si_type 仍含 PRODUCTION（实际 %）',
            coalesce(ck_type, '<缺失>');
    END IF;

    SELECT count(*) INTO ck_prod
      FROM pg_constraint
     WHERE conname = 'ck_biz_si_production' AND conrelid = 'public.biz_stock_in'::regclass;
    IF ck_prod > 0 THEN
        RAISE EXCEPTION 'V0376 down 自检失败：ck_biz_si_production 仍在';
    END IF;

    SELECT count(*) INTO col_left
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_stock_in'
       AND column_name IN ('cost_object_id', 'cost_period_id');
    IF col_left > 0 THEN
        RAISE EXCEPTION 'V0376 down 自检失败：还有 % 个新列没删掉', col_left;
    END IF;

    SELECT count(*) INTO idx_left
      FROM pg_indexes
     WHERE schemaname = 'public' AND indexname IN ('idx_biz_si_cost_obj', 'idx_biz_si_cost_period');
    IF idx_left > 0 THEN
        RAISE EXCEPTION 'V0376 down 自检失败：还有 % 条新索引没删掉', idx_left;
    END IF;

    -- 供应商两向约束不许被回滚顺手拆掉（本迁移没动它）
    SELECT count(*) INTO n_scope
      FROM pg_constraint
     WHERE conname = 'ck_biz_si_supplier_scope' AND conrelid = 'public.biz_stock_in'::regclass;
    IF n_scope = 0 THEN
        RAISE EXCEPTION 'V0376 down 自检失败：ck_biz_si_supplier_scope 被顺手删掉了（本迁移不动它）';
    END IF;

    RAISE NOTICE 'V0376 down 完成：ck_biz_si_production 已删、值域回到 4 值、两列与两条索引已删';
END
$$;
