-- ============================================================
-- V0348 回滚：发票票面形式回到 V0347 的 invoice_code 口径，然后删列
--
-- 顺序：先改规则（条件里引用了待删的列），再删列 —— 反了会让规则在两次启动之间
-- 引用一个不存在的列（`condition_applies` 拼 SQL 时会直接报错 = 500 锁死开票）。
-- ============================================================

-- §1 规则改回 ATT-06 的口径（按固定 id + 当前值匹配，重放 0 行）
UPDATE biz_attachment_rule
   SET condition_sql = 'invoice_code IS NULL',
       updated_at    = now()
 WHERE id = 'd3470001-0000-4000-8000-000000000001'::uuid
   AND doc_type = 'INVOICE'
   AND required_at = 'SUBMIT'
   AND condition_sql = 'is_electronic IS TRUE'
   AND deleted_at IS NULL;

UPDATE biz_attachment_rule
   SET condition_sql = 'invoice_code IS NOT NULL',
       updated_at    = now()
 WHERE id = 'd3470001-0000-4000-8000-000000000002'::uuid
   AND doc_type = 'INVOICE'
   AND required_at = 'SUBMIT'
   AND condition_sql = 'is_electronic IS FALSE'
   AND deleted_at IS NULL;

-- §2 删列
ALTER TABLE biz_invoice DROP COLUMN IF EXISTS is_electronic;
