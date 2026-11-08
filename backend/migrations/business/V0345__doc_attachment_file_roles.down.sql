-- ============================================================
-- V0345 回滚：文件角色词表退回 V0212 的 4 值
--
-- ⚠ 降级会**丢分类信息**：老约束里没有「补充协议 / 验收单 / 银行回单」的对应值，
--   它们的行只能落到 `OTHER`。执行前请确认这三类附件可以接受被并到「其他」。
--   （发票影像 INVOICE_SCAN 与 其他附件 DEFAULT 是无损映射，只是恢复老拼写。）
--
-- 顺序：先改数据 → 再恢复默认值 → 最后收紧约束（顺序反了会撞新 CHECK / 旧 CHECK）。
-- 幂等：每步都是「匹配即改」，重放第二遍匹配 0 行。
-- ============================================================

UPDATE biz_doc_attachment SET file_role = 'INVOICE' WHERE file_role = 'INVOICE_SCAN';
UPDATE biz_doc_attachment SET file_role = 'OTHER'   WHERE file_role = 'DEFAULT';

-- 新角色在老词表里没有对应值：并到 OTHER（信息损失，见文件头）
UPDATE biz_doc_attachment
   SET file_role = 'OTHER'
 WHERE file_role IN ('SUPPLEMENT', 'ACCEPTANCE', 'BANK_RECEIPT');

ALTER TABLE biz_doc_attachment
    ALTER COLUMN file_role SET DEFAULT 'OTHER';

COMMENT ON COLUMN biz_doc_attachment.file_role IS
    '文件角色：CONTRACT_SCAN 合同扫描件 / SIGN_RECEIPT 签收单 / INVOICE 发票影像 / OTHER 其他';

DO $$
DECLARE
    v_def text;
BEGIN
    SELECT pg_get_constraintdef(oid) INTO v_def
      FROM pg_constraint
     WHERE conname = 'ck_biz_doc_attachment_role'
       AND conrelid = 'biz_doc_attachment'::regclass;

    -- 已是老定义（4 值）就跳过，保持重放幂等
    IF v_def IS NULL OR position('SUPPLEMENT' IN v_def) > 0 OR position('BANK_RECEIPT' IN v_def) > 0 THEN
        ALTER TABLE biz_doc_attachment
            DROP CONSTRAINT IF EXISTS ck_biz_doc_attachment_role;
        ALTER TABLE biz_doc_attachment
            ADD CONSTRAINT ck_biz_doc_attachment_role
            CHECK (file_role IN ('CONTRACT_SCAN', 'SIGN_RECEIPT', 'INVOICE', 'OTHER'));
        RAISE NOTICE 'V0345 down：ck_biz_doc_attachment_role 已退回 4 值';
    END IF;

    -- 自检：不允许残留新值（否则约束加不上 / 数据与约束不一致）
    IF EXISTS (SELECT 1 FROM biz_doc_attachment
                WHERE file_role NOT IN ('CONTRACT_SCAN', 'SIGN_RECEIPT', 'INVOICE', 'OTHER')) THEN
        RAISE EXCEPTION 'V0345 down 自检失败：仍有非 4 值角色残留';
    END IF;
END $$;
