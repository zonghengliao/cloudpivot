-- ======================================================
-- V0327：税率标度统一为「百分数」（FIX-6）
--
-- 背景（100 倍税差风险）：
--   · `biz_sales_order_item` / `biz_purchase_order_item` / `biz_stock_in_item` 一直存百分数（13 = 13%）；
--   · `biz_invoice.tax_rate` / `biz_material.tax_rate` 存小数（0.13 = 13%），库里还有
--     `ck_biz_invoice_tax_rate` / `ck_biz_material_tax` 把上界钉在 1 —— 于是同一个税率
--     在两张表里差 100 倍，谁按错标度相乘就是 100 倍税差。
--
-- 本迁移做四件事（**不改任何金额列**，历史单据金额一分不动）：
--   ① `biz_invoice.tax_rate`：NUMERIC(6,4) → NUMERIC(7,4)（放得下 100.0000），默认 13.0000；
--   ② `biz_material.tax_rate`：默认值仍是 0，只放宽上界；
--   ③ 两表各加 `tax_rate_mode`（`PERCENT` / `DECIMAL`，可空；FIX-6 过渡期审计位，
--      「这行是按百分数还是按小数录进来的」可查，过渡期结束删列）；
--   ④ 存量 0.13 → 13：**只迁 `(0, 1]`** 区间（0 = 免税永远不放大），
--      并据此把 `tax_rate_mode` 标成 `DECIMAL`（其余标 `PERCENT`）。
--
-- 幂等：
--   · ④ 的两步 UPDATE 都以 `tax_rate_mode IS NULL` 为闸 —— 第一次跑完表里就没有 NULL 了，
--     第二次跑「更新 0 行」，不会把已经 ×100 的值再乘一次（0.005 → 0.5 → 50 那种翻车）；
--   · ①②③ 都是 DDL，`ADD COLUMN IF NOT EXISTS` / `DROP CONSTRAINT IF EXISTS` / 类型放宽
--     重复执行无副作用。
--
-- 回滚（down）：
--   · 按 `tax_rate_mode = 'DECIMAL'` 精确除回 100（只回滚本迁移放大过的行）；
--   · 再把上界约束改回 ≤ 1、`biz_invoice.tax_rate` 收窄回 NUMERIC(6,4)、删列。
--     down 之前先把仍是百分数的行（PERCENT）拦下来——收窄回 (6,4) 且 ≤1 会撞约束，
--     宁可让 down 报错也不要静默改数据。
-- ======================================================

-- ---------- ① biz_invoice.tax_rate：放得下 100 ----------
ALTER TABLE biz_invoice
    ALTER COLUMN tax_rate TYPE NUMERIC(7,4) USING tax_rate::numeric(7,4);

ALTER TABLE biz_invoice ALTER COLUMN tax_rate SET DEFAULT 13.0000;

ALTER TABLE biz_invoice
    ADD COLUMN IF NOT EXISTS tax_rate_mode VARCHAR(8);

ALTER TABLE biz_invoice DROP CONSTRAINT IF EXISTS ck_biz_invoice_tax_rate;
ALTER TABLE biz_invoice
    ADD CONSTRAINT ck_biz_invoice_tax_rate CHECK (tax_rate >= 0 AND tax_rate <= 100);

ALTER TABLE biz_invoice DROP CONSTRAINT IF EXISTS ck_biz_invoice_tax_rate_mode;
ALTER TABLE biz_invoice
    ADD CONSTRAINT ck_biz_invoice_tax_rate_mode
    CHECK (tax_rate_mode IS NULL OR tax_rate_mode IN ('PERCENT', 'DECIMAL'));

COMMENT ON COLUMN biz_invoice.tax_rate IS
    '税率百分数 0–100（FIX-6 / V0327 起：13 = 13%，与 biz_sales_order_item / biz_stock_in_item 同口径）';
COMMENT ON COLUMN biz_invoice.tax_rate_mode IS
    '税率标度（FIX-6 过渡期审计位）：PERCENT = 按百分数录入 / DECIMAL = 由小数迁移或按小数录入；过渡期后删列';

-- ---------- ② biz_material.tax_rate：上界放到 100 ----------
ALTER TABLE biz_material
    ADD COLUMN IF NOT EXISTS tax_rate_mode VARCHAR(8);

ALTER TABLE biz_material DROP CONSTRAINT IF EXISTS ck_biz_material_tax;
ALTER TABLE biz_material
    ADD CONSTRAINT ck_biz_material_tax CHECK (tax_rate >= 0 AND tax_rate <= 100);

ALTER TABLE biz_material DROP CONSTRAINT IF EXISTS ck_biz_material_tax_mode;
ALTER TABLE biz_material
    ADD CONSTRAINT ck_biz_material_tax_mode
    CHECK (tax_rate_mode IS NULL OR tax_rate_mode IN ('PERCENT', 'DECIMAL'));

COMMENT ON COLUMN biz_material.tax_rate IS
    '默认税率百分数 0–100（FIX-6 / V0327 起：13 = 13%，与订单 / 收货明细同口径）';
COMMENT ON COLUMN biz_material.tax_rate_mode IS
    '税率标度（FIX-6 过渡期审计位）：PERCENT = 按百分数录入 / DECIMAL = 由小数迁移或按小数录入；过渡期后删列';

-- ---------- ③ 存量数据：0.13 → 13（只迁 (0, 1]） ----------
-- 顺序不能反：先标 DECIMAL（此时 tax_rate_mode 仍为 NULL 的行 = 还没迁过的行），
-- 再给剩下的行标 PERCENT —— 第二步跑完表里再无 NULL，两段都变成幂等空操作。
UPDATE biz_invoice
   SET tax_rate = tax_rate * 100,
       tax_rate_mode = 'DECIMAL'
 WHERE tax_rate_mode IS NULL
   AND tax_rate > 0
   AND tax_rate <= 1;

UPDATE biz_invoice
   SET tax_rate_mode = 'PERCENT'
 WHERE tax_rate_mode IS NULL;

UPDATE biz_material
   SET tax_rate = tax_rate * 100,
       tax_rate_mode = 'DECIMAL'
 WHERE tax_rate_mode IS NULL
   AND tax_rate > 0
   AND tax_rate <= 1;

UPDATE biz_material
   SET tax_rate_mode = 'PERCENT'
 WHERE tax_rate_mode IS NULL;

-- ---------- ④ 自检（失败即迁移失败 → 启动失败） ----------
DO $$
DECLARE
    invoice_numeric   TEXT;
    invoice_default   TEXT;
    invoice_bad       INT;
    material_bad      INT;
    invoice_untyped   INT;
    material_untyped  INT;
    invoice_check     TEXT;
    material_check    TEXT;
BEGIN
    SELECT numeric_precision || ',' || numeric_scale INTO invoice_numeric
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_invoice' AND column_name = 'tax_rate';
    IF invoice_numeric <> '7,4' THEN
        RAISE EXCEPTION 'V0327 自检失败：biz_invoice.tax_rate 不是 NUMERIC(7,4)（实际 %）', invoice_numeric;
    END IF;

    SELECT coalesce(column_default, '') INTO invoice_default
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_invoice' AND column_name = 'tax_rate';
    IF invoice_default NOT LIKE '13%' THEN
        RAISE EXCEPTION 'V0327 自检失败：biz_invoice.tax_rate 默认值不是 13（实际 %）', invoice_default;
    END IF;

    -- 迁完之后两表都不该还有「(0,1] 且未标注」的行
    SELECT count(*) INTO invoice_bad
      FROM biz_invoice
     WHERE tax_rate > 0 AND tax_rate <= 1 AND tax_rate_mode IS NULL;
    SELECT count(*) INTO material_bad
      FROM biz_material
     WHERE tax_rate > 0 AND tax_rate <= 1 AND tax_rate_mode IS NULL;
    IF invoice_bad > 0 OR material_bad > 0 THEN
        RAISE EXCEPTION 'V0327 自检失败：仍有未标注标度的税率（invoice=% material=%）',
            invoice_bad, material_bad;
    END IF;

    SELECT count(*) INTO invoice_untyped
      FROM biz_invoice WHERE tax_rate_mode IS NULL;
    SELECT count(*) INTO material_untyped
      FROM biz_material WHERE tax_rate_mode IS NULL;
    IF invoice_untyped > 0 OR material_untyped > 0 THEN
        RAISE EXCEPTION 'V0327 自检失败：税率标度位未回填（invoice=% material=%）',
            invoice_untyped, material_untyped;
    END IF;

    -- 上界约束真的放宽了：读约束定义文本（插探针行会被别的非空/外键先拦住，看不准）
    SELECT pg_get_constraintdef(oid) INTO invoice_check
      FROM pg_constraint
     WHERE conname = 'ck_biz_invoice_tax_rate';
    IF invoice_check IS NULL OR invoice_check NOT LIKE '%100%' THEN
        RAISE EXCEPTION 'V0327 自检失败：ck_biz_invoice_tax_rate 上界没放宽到 100（实际 %）',
            coalesce(invoice_check, '<缺失>');
    END IF;

    SELECT pg_get_constraintdef(oid) INTO material_check
      FROM pg_constraint
     WHERE conname = 'ck_biz_material_tax';
    IF material_check IS NULL OR material_check NOT LIKE '%100%' THEN
        RAISE EXCEPTION 'V0327 自检失败：ck_biz_material_tax 上界没放宽到 100（实际 %）',
            coalesce(material_check, '<缺失>');
    END IF;
END
$$;
