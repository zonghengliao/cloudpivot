-- ============================================================================
-- V0382 down：把 ck_biz_si_sourceless_pair 装回 V0199 的原定义
-- 对应 up：backend/migrations/business/V0382__stock_in_production_sourceless.up.sql
--
-- # 顺序（不能反）
--   ① 断言「库里没有 PRODUCTION 单据」—— 完工入库行必然是 source_order_id IS NULL
--      且 is_sourceless = FALSE，原定义 `is_sourceless = (source_order_id IS NULL)`
--      对这些行**必然判假**，装回去就会失败。有就点名数量并拒绝执行（先红冲 / 删单）。
--   ② 删新约束 → 装回原定义（逐字照抄 V0199 第 86–90 行）。
--   ③ 复核：约束回来了、且不再包含 PRODUCTION 分支。
--
-- # 幂等
--   DROP CONSTRAINT IF EXISTS + ADD CONSTRAINT；重复执行结果不变。
--   ① 的断言在「没有 PRODUCTION 行」时恒成立，故重复执行不会误报。
-- ============================================================================

DO $$
DECLARE
    n_prod BIGINT;
BEGIN
    SELECT count(*) INTO n_prod FROM biz_stock_in WHERE in_type = 'PRODUCTION';
    IF n_prod > 0 THEN
        RAISE EXCEPTION
            'V0382 回滚被拒：库里还有 % 张 PRODUCTION（完工入库）单据，先红冲 / 删除再回滚 '
            '（它们的 source_order_id IS NULL 与 V0199 的原定义互斥）', n_prod;
    END IF;
END
$$;

ALTER TABLE biz_stock_in DROP CONSTRAINT IF EXISTS ck_biz_si_sourceless_pair;
ALTER TABLE biz_stock_in ADD CONSTRAINT ck_biz_si_sourceless_pair
    CHECK ((is_sourceless = (source_order_id IS NULL)) IS TRUE);

COMMENT ON COLUMN biz_stock_in.is_sourceless IS
    '无源单据标记（A4）：true = 不绑采购单（期初/复制/其他，E4 情形），由建单时显式声明；与 source_order_id 由 ck_biz_si_sourceless_pair 钉死';

-- ---------- 复核：原定义回来了、PRODUCTION 分支不见了 ----------
DO $$
DECLARE
    def TEXT;
BEGIN
    SELECT pg_get_constraintdef(oid) INTO def
      FROM pg_constraint
     WHERE conname = 'ck_biz_si_sourceless_pair' AND conrelid = 'public.biz_stock_in'::regclass;
    IF def IS NULL THEN
        RAISE EXCEPTION 'V0382 down 失败：ck_biz_si_sourceless_pair 没装回来';
    END IF;
    IF def LIKE '%PRODUCTION%' THEN
        RAISE EXCEPTION 'V0382 down 失败：约束里仍有 PRODUCTION 分支（实际 %）', def;
    END IF;
    RAISE NOTICE 'V0382 down 通过：ck_biz_si_sourceless_pair 已回到 V0199 原定义';
END
$$;
