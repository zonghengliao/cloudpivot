-- ============================================================
-- V0348：发票「票面形式」`biz_invoice.is_electronic`（电子票 / 纸质票）—— **ATT-07**
--
-- # 这条迁移补的是什么
--
-- ATT-06 的附件必传规则框架里，发票有两条互斥规则（电子票 pdf/ofd/xml、纸质票 jpg/png/pdf），
-- 但当时 `biz_invoice`（V0298）**没有 `is_electronic` 列**，分支只能退化成
-- `invoice_code IS NULL`（数电票）/ `IS NOT NULL`（税控纸质票）。
-- 「有没有发票代码」只是**相关性**，不是票据形式本身：纸质票可以先不填代码（现场开票后补录），
-- 电子票也可能带上税控代码。ATT-07 把口径落成真实字段：
--
--   电子票 `is_electronic = TRUE`   → 发票影像只认 pdf / ofd / xml（数电票只出这三种）
--   纸质票 `is_electronic = FALSE`  → 认扫描件 jpg / png / pdf
--
-- # 为什么默认 TRUE
--
-- 建单请求不传 `is_electronic` 时按**电子票**处理（当前业务以数电票为主，
-- 也是 ATT-06 预置规则对「新建发票（无税控代码）」的既有行为）。前端开票弹窗有
-- 「电子票 / 纸质票」单选，落库值以用户选择为准。
--
-- # 幂等 / 回滚
--
--   `ADD COLUMN IF NOT EXISTS` → 回填**只补 NULL 行** → `SET NOT NULL` / `SET DEFAULT`。
--   重放时列已存在且有值，回填 0 行、默认值不变（不会覆盖用户后来改过的选择）。
--   §2 的规则改口径按**旧值匹配**（`condition_sql = 'invoice_code IS NULL'`），重放 0 行。
--   文件内不写 BEGIN/COMMIT（迁移器自己在事务里跑）。
--   回滚：V0348.down.sql（规则改回 invoice_code 口径 + 删列）。
-- ============================================================

-- ============================================================
-- §1 加列：可空 → 回填 → NOT NULL + DEFAULT TRUE
-- ============================================================
ALTER TABLE biz_invoice ADD COLUMN IF NOT EXISTS is_electronic boolean;

-- 历史行按 ATT-06 当时的口径回填（有税控代码=纸质票），只补 NULL 行：
-- 重放/回滚再 up 时不会把用户后来选的「纸质票」改回电子票。
UPDATE biz_invoice
   SET is_electronic = (invoice_code IS NULL)
 WHERE is_electronic IS NULL;

ALTER TABLE biz_invoice ALTER COLUMN is_electronic SET NOT NULL;
ALTER TABLE biz_invoice ALTER COLUMN is_electronic SET DEFAULT TRUE;

COMMENT ON COLUMN biz_invoice.is_electronic IS
    '票面形式：TRUE=电子票（数电票，影像只认 pdf/ofd/xml）/ FALSE=纸质票（扫描件 jpg/png/pdf）。默认 TRUE；ATT-07 起附件必传规则按它分支';

-- ============================================================
-- §2 附件必传规则改按 is_electronic 分支（V0347 种子用的是 invoice_code）
-- ============================================================
-- 两条规则按固定 id（V0347 种子）定位；`condition_sql` 的旧值也一起匹配，
-- 这样重放/回滚-再-up 都是 0 行，不会与部分唯一索引
-- `uk_biz_attachment_rule_key(doc_type, required_at, COALESCE(file_role,''), COALESCE(condition_sql,''))`
-- 撞车。
UPDATE biz_attachment_rule
   SET condition_sql = 'is_electronic IS TRUE',
       updated_at    = now()
 WHERE id = 'd3470001-0000-4000-8000-000000000001'::uuid
   AND doc_type = 'INVOICE'
   AND required_at = 'SUBMIT'
   AND condition_sql = 'invoice_code IS NULL'
   AND deleted_at IS NULL;

UPDATE biz_attachment_rule
   SET condition_sql = 'is_electronic IS FALSE',
       updated_at    = now()
 WHERE id = 'd3470001-0000-4000-8000-000000000002'::uuid
   AND doc_type = 'INVOICE'
   AND required_at = 'SUBMIT'
   AND condition_sql = 'invoice_code IS NOT NULL'
   AND deleted_at IS NULL;

-- ============================================================
-- §3 自检（写错就整脚本回滚，不留半截状态）
-- ============================================================
DO $$
DECLARE
    col_nullable   text;
    col_default    text;
    electronic_cnt int;
    paper_cnt      int;
    legacy_cnt     int;
BEGIN
    SELECT is_nullable, column_default
      INTO col_nullable, col_default
      FROM information_schema.columns
     WHERE table_name = 'biz_invoice' AND column_name = 'is_electronic';

    IF col_nullable IS NULL THEN
        RAISE EXCEPTION 'V0348 自检失败：biz_invoice.is_electronic 没加上';
    END IF;
    IF col_nullable <> 'NO' THEN
        RAISE EXCEPTION 'V0348 自检失败：is_electronic 必须 NOT NULL（实际 is_nullable=%）', col_nullable;
    END IF;
    IF col_default IS NULL OR col_default NOT LIKE '%true%' THEN
        RAISE EXCEPTION 'V0348 自检失败：is_electronic 默认值必须 TRUE（实际 %）', col_default;
    END IF;

    SELECT count(*) INTO electronic_cnt
      FROM biz_attachment_rule
     WHERE doc_type = 'INVOICE' AND required_at = 'SUBMIT' AND enabled AND deleted_at IS NULL
       AND condition_sql = 'is_electronic IS TRUE'
       AND allow_exts = 'pdf,ofd,xml';

    SELECT count(*) INTO paper_cnt
      FROM biz_attachment_rule
     WHERE doc_type = 'INVOICE' AND required_at = 'SUBMIT' AND enabled AND deleted_at IS NULL
       AND condition_sql = 'is_electronic IS FALSE'
       AND allow_exts = 'pdf,jpg,png';

    SELECT count(*) INTO legacy_cnt
      FROM biz_attachment_rule
     WHERE doc_type = 'INVOICE' AND required_at = 'SUBMIT' AND deleted_at IS NULL
       AND condition_sql LIKE 'invoice_code%';

    IF electronic_cnt <> 1 OR paper_cnt <> 1 THEN
        RAISE EXCEPTION 'V0348 自检失败：发票电子/纸质规则条数不对（electronic=%，paper=%）',
            electronic_cnt, paper_cnt;
    END IF;
    IF legacy_cnt <> 0 THEN
        RAISE EXCEPTION 'V0348 自检失败：仍有 % 条发票规则按 invoice_code 分支', legacy_cnt;
    END IF;

    RAISE NOTICE 'V0348 自检通过：is_electronic NOT NULL DEFAULT TRUE，发票规则已改按 is_electronic 分支';
END $$;
