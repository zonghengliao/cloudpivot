-- ======================================================
-- V0327 down：把税率标度退回「小数」（0.13 = 13%）
--
-- 语义：V0327 之后**表里所有值都是百分数**（存量的 0.13 已迁成 13，新录入的也是 13），
--       所以回滚就是把**每一行**都除回 100 —— 不是只除 `tax_rate_mode = 'DECIMAL'` 的行。
--       （两个模式只记录「当初按哪种标度录的」，不是「哪些行被本迁移改过」。）
--
-- 执行顺序：先除数据、再收窄类型与约束 —— 反过来的话 13.0000 会先撞 NUMERIC(6,4)。
--
-- 幂等：数据段用「列是否还在」做闸。down 双跑时第二遍已经删了列 → 直接 NOTICE 跳过，
--       不会在已经除回 100 的数据上再除一次（0.13 → 0.0013 那种翻车）。
-- ======================================================

DO $$
DECLARE
    has_column BOOLEAN;
BEGIN
    SELECT EXISTS (
        SELECT 1 FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = 'biz_invoice'
           AND column_name = 'tax_rate_mode'
    ) INTO has_column;

    IF NOT has_column THEN
        RAISE NOTICE 'V0327 down：tax_rate_mode 已删除（down 已执行过），跳过数据回滚';
        RETURN;
    END IF;

    UPDATE biz_invoice  SET tax_rate = round(tax_rate / 100, 4);
    UPDATE biz_material SET tax_rate = round(tax_rate / 100, 4);
END
$$;

ALTER TABLE biz_invoice DROP CONSTRAINT IF EXISTS ck_biz_invoice_tax_rate;
ALTER TABLE biz_invoice
    ADD CONSTRAINT ck_biz_invoice_tax_rate CHECK (tax_rate >= 0 AND tax_rate <= 1);
ALTER TABLE biz_invoice ALTER COLUMN tax_rate SET DEFAULT 0.1300;
ALTER TABLE biz_invoice
    ALTER COLUMN tax_rate TYPE NUMERIC(6,4) USING tax_rate::numeric(6,4);

ALTER TABLE biz_material DROP CONSTRAINT IF EXISTS ck_biz_material_tax;
ALTER TABLE biz_material
    ADD CONSTRAINT ck_biz_material_tax CHECK (tax_rate >= 0 AND tax_rate <= 1);

ALTER TABLE biz_invoice DROP CONSTRAINT IF EXISTS ck_biz_invoice_tax_rate_mode;
ALTER TABLE biz_material DROP CONSTRAINT IF EXISTS ck_biz_material_tax_mode;
ALTER TABLE biz_invoice DROP COLUMN IF EXISTS tax_rate_mode;
ALTER TABLE biz_material DROP COLUMN IF EXISTS tax_rate_mode;

COMMENT ON COLUMN biz_invoice.tax_rate IS '税率 0~1（0.1300 = 13%；默认 0.13 取自 ARCH-7 §3.1.8）';
COMMENT ON COLUMN biz_material.tax_rate IS '默认进项税率';
