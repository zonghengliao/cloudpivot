-- ============================================================
-- V0345 回滚：文件角色词表退回 V0212 的 4 值
--
-- ⚠ 降级会**丢分类信息**：老约束里没有「补充协议 / 验收单 / 银行回单」的对应值，
--   它们的行只能落到 `OTHER`。执行前请确认这三类附件可以接受被并到「其他」。
--   （发票影像 INVOICE_SCAN 与 其他附件 DEFAULT 是无损映射，只是恢复老拼写。）
--
-- # 顺序（与 up 同一条坑，方向相反）
--
--   ① 先拆 7 值 CHECK —— 否则把 `INVOICE_SCAN` 改回 `INVOICE` 会撞新约束；
--   ② 新值映射回老值；
--   ③ 列默认值改回 `OTHER`；
--   ④ 建回 4 值 CHECK；
--   ⑤ 自检（不允许残留 4 值以外的角色）。
--
-- 幂等：每步都是「匹配即改 / 定义不同才重建」，重放第二遍匹配 0 行。
-- ============================================================

-- ============================================================
-- §1 先拆新 CHECK（旧的下游映射会撞它）
-- ============================================================
DO $$
DECLARE
    v_def text;
BEGIN
    SELECT pg_get_constraintdef(oid) INTO v_def
      FROM pg_constraint
     WHERE conname = 'ck_biz_doc_attachment_role'
       AND conrelid = 'biz_doc_attachment'::regclass;

    IF v_def IS NOT NULL AND (position('SUPPLEMENT' IN v_def) > 0 OR position('BANK_RECEIPT' IN v_def) > 0) THEN
        ALTER TABLE biz_doc_attachment
            DROP CONSTRAINT ck_biz_doc_attachment_role;
        RAISE NOTICE 'V0345 down：先拆 7 值 CHECK';
    END IF;
END $$;

-- ============================================================
-- §2 新值 → 老值
-- ============================================================
UPDATE biz_doc_attachment SET file_role = 'INVOICE' WHERE file_role = 'INVOICE_SCAN';
UPDATE biz_doc_attachment SET file_role = 'OTHER'   WHERE file_role = 'DEFAULT';

-- 新角色在老词表里没有对应值：并到 OTHER（信息损失，见文件头）
UPDATE biz_doc_attachment
   SET file_role = 'OTHER'
 WHERE file_role IN ('SUPPLEMENT', 'ACCEPTANCE', 'BANK_RECEIPT');

-- ============================================================
-- §3 列默认值 + 注释
-- ============================================================
ALTER TABLE biz_doc_attachment
    ALTER COLUMN file_role SET DEFAULT 'OTHER';

COMMENT ON COLUMN biz_doc_attachment.file_role IS
    '文件角色：CONTRACT_SCAN 合同扫描件 / SIGN_RECEIPT 签收单 / INVOICE 发票影像 / OTHER 其他';

-- ============================================================
-- §4 建回 4 值 CHECK（已是老定义就跳过）
-- ============================================================
DO $$
DECLARE
    v_def text;
BEGIN
    SELECT pg_get_constraintdef(oid) INTO v_def
      FROM pg_constraint
     WHERE conname = 'ck_biz_doc_attachment_role'
       AND conrelid = 'biz_doc_attachment'::regclass;

    IF v_def IS NULL OR position('SUPPLEMENT' IN v_def) > 0 OR position('BANK_RECEIPT' IN v_def) > 0 THEN
        ALTER TABLE biz_doc_attachment
            DROP CONSTRAINT IF EXISTS ck_biz_doc_attachment_role;
        ALTER TABLE biz_doc_attachment
            ADD CONSTRAINT ck_biz_doc_attachment_role
            CHECK (file_role IN ('CONTRACT_SCAN', 'SIGN_RECEIPT', 'INVOICE', 'OTHER'));
        RAISE NOTICE 'V0345 down：ck_biz_doc_attachment_role 已退回 4 值';
    END IF;
END $$;

-- ============================================================
-- §5 自检
-- ============================================================
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM biz_doc_attachment
                WHERE file_role NOT IN ('CONTRACT_SCAN', 'SIGN_RECEIPT', 'INVOICE', 'OTHER')) THEN
        RAISE EXCEPTION 'V0345 down 自检失败：仍有非 4 值角色残留';
    END IF;
    RAISE NOTICE 'V0345 down 完成：数据已退回 4 值词表';
END $$;
