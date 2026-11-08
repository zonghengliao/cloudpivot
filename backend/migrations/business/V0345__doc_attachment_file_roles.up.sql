-- ============================================================
-- V0345：单据附件的文件角色扩容（`file_role`）—— ATT-04
--
-- # 这条迁移补的是什么
--
-- ATT-04 要求附件面板**按 `file_role` 分栏**，让用户一眼看出「合同扫描件有没有传」。
-- 卡面给的词表是 6 个：
--
--   CONTRACT_SCAN  合同扫描件
--   SUPPLEMENT     补充协议
--   ACCEPTANCE     验收单
--   INVOICE_SCAN   发票影像
--   BANK_RECEIPT   银行回单
--   DEFAULT        其他附件
--
-- 而 V0212 建表时的 CHECK 只允许 4 个老值：
--
--   CONTRACT_SCAN / SIGN_RECEIPT / INVOICE / OTHER
--
-- 也就是说**新角色一个都存不进去**（填 `SUPPLEMENT` 会直接撞 `ck_biz_doc_attachment_role`
-- 报 23514，接口变成 500）。分栏是前端需求、可写范围是数据库事实 —— 先把约束放开。
--
-- # 老值怎么处理（**不删数据，只归一**）
--
--   · `INVOICE`  → `INVOICE_SCAN`（同一个东西的两种拼法；不归一就会在界面上出现两个
--                  「发票影像」分组，用户会以为漏传了）；
--   · `OTHER`    → `DEFAULT`（同上：`OTHER 其他` 与 `DEFAULT 其他附件` 是同一个分组）；
--   · `SIGN_RECEIPT`（签收单）**保留**：它与「验收单 ACCEPTANCE」不是一件事
--                  （收货签收 ≠ 合同验收），没有可归一的去处。它继续被接受、继续显示，
--                  只是不再出现在上传下拉里（前端词表以 6 个新值为准）。
--
-- 归一用 `WHERE file_role = ...`，天然幂等：第二次重放匹配 0 行。
--
-- # 列默认值
--
-- `DEFAULT 'DEFAULT'`（原来 `DEFAULT 'OTHER'`）：卡面验收④「不传 role 的文件进 DEFAULT 组」。
-- 应用侧 `attachment::dto::parse_upload_meta` 也会把空值解析成 `DEFAULT`，双保险：
-- 直接写 SQL 的脚本 / 老客户端都落到同一组。
--
-- # 幂等
--
--   · 归一 UPDATE：匹配即改、改完不匹配；
--   · `ALTER COLUMN ... SET DEFAULT`：重复设置同一默认值是空操作；
--   · CHECK 重建：先看 `pg_get_constraintdef`，**已经是新版就跳过**（不 DROP 再 ADD，
--     避免每次重放都全表校验一遍，也避免在重放期间留下无约束的窗口）；
--   · `COMMENT ON COLUMN` 幂等。
--   文件内**不写** BEGIN/COMMIT（迁移器自己在事务里跑）。
--
-- # 回滚
--
-- `V0345__doc_attachment_file_roles.down.sql`：把新值映射回老值（`INVOICE_SCAN`→`INVOICE`、
-- `DEFAULT`→`OTHER`，`SUPPLEMENT`/`ACCEPTANCE`/`BANK_RECEIPT`→`OTHER`）再恢复 V0212 的 4 值
-- 约束。**down 会丢掉「补充协议 / 验收单 / 银行回单」的分类信息**（老约束里没有对应值），
-- 这是降级的必然代价，已在 down 文件里写明。
-- ============================================================

-- ============================================================
-- §1 老值归一（先改数据，后换约束：顺序反了会撞旧 CHECK）
-- ============================================================
UPDATE biz_doc_attachment
   SET file_role = 'INVOICE_SCAN'
 WHERE file_role = 'INVOICE';

UPDATE biz_doc_attachment
   SET file_role = 'DEFAULT'
 WHERE file_role = 'OTHER';

-- ============================================================
-- §2 列默认值 + 列注释
-- ============================================================
ALTER TABLE biz_doc_attachment
    ALTER COLUMN file_role SET DEFAULT 'DEFAULT';

COMMENT ON COLUMN biz_doc_attachment.file_role IS
    '文件角色（ATT-04 分栏）：CONTRACT_SCAN 合同扫描件 / SUPPLEMENT 补充协议 / ACCEPTANCE 验收单 / INVOICE_SCAN 发票影像 / BANK_RECEIPT 银行回单 / SIGN_RECEIPT 签收单（V0212 老值，保留兼容）/ DEFAULT 其他附件';

-- ============================================================
-- §3 CHECK 约束重建（幂等：定义已是新版就跳过）
-- ============================================================
DO $$
DECLARE
    v_def text;
BEGIN
    SELECT pg_get_constraintdef(oid)
      INTO v_def
      FROM pg_constraint
     WHERE conname = 'ck_biz_doc_attachment_role'
       AND conrelid = 'biz_doc_attachment'::regclass;

    -- 已含新值就不动（幂等）；旧定义 / 不存在则重建
    IF v_def IS NULL OR position('BANK_RECEIPT' IN v_def) = 0 THEN
        ALTER TABLE biz_doc_attachment
            DROP CONSTRAINT IF EXISTS ck_biz_doc_attachment_role;
        ALTER TABLE biz_doc_attachment
            ADD CONSTRAINT ck_biz_doc_attachment_role
            CHECK (file_role IN ('CONTRACT_SCAN', 'SUPPLEMENT', 'ACCEPTANCE',
                                 'INVOICE_SCAN', 'BANK_RECEIPT', 'SIGN_RECEIPT', 'DEFAULT'));
        RAISE NOTICE 'V0345：ck_biz_doc_attachment_role 已重建为 7 值（旧定义 %）', coalesce(v_def, '<不存在>');
    ELSE
        RAISE NOTICE 'V0345：ck_biz_doc_attachment_role 已是新版，跳过';
    END IF;
END $$;

-- ============================================================
-- §4 自检（up 后必须全为真）
-- ============================================================
DO $$
DECLARE
    v_def     text;
    v_default text;
    n_old     integer;
    n_values  integer;
BEGIN
    SELECT pg_get_constraintdef(oid) INTO v_def
      FROM pg_constraint
     WHERE conname = 'ck_biz_doc_attachment_role'
       AND conrelid = 'biz_doc_attachment'::regclass;
    IF v_def IS NULL THEN
        RAISE EXCEPTION 'V0345 自检失败：ck_biz_doc_attachment_role 不存在';
    END IF;

    -- 7 个取值必须都在约束定义里
    SELECT count(*) INTO n_values
      FROM unnest(ARRAY['CONTRACT_SCAN', 'SUPPLEMENT', 'ACCEPTANCE',
                        'INVOICE_SCAN', 'BANK_RECEIPT', 'SIGN_RECEIPT', 'DEFAULT']) AS v(code)
     WHERE position(v.code IN v_def) > 0;
    IF n_values <> 7 THEN
        RAISE EXCEPTION 'V0345 自检失败：约束里只找到 % 个角色值（应 7）：%', n_values, v_def;
    END IF;

    SELECT column_default INTO v_default
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_doc_attachment'
       AND column_name = 'file_role';
    IF v_default IS NULL OR position('DEFAULT' IN v_default) = 0 THEN
        RAISE EXCEPTION 'V0345 自检失败：file_role 默认值不是 DEFAULT（实际 %）', coalesce(v_default, '<null>');
    END IF;

    -- 老值必须已归零（否则说明 §1 没跑或约束重建把脏行放行了）
    SELECT count(*) INTO n_old
      FROM biz_doc_attachment
     WHERE file_role IN ('INVOICE', 'OTHER');
    IF n_old > 0 THEN
        RAISE EXCEPTION 'V0345 自检失败：仍有 % 行使用老角色值（INVOICE / OTHER）', n_old;
    END IF;

    RAISE NOTICE 'V0345 完成：file_role 约束 = 7 值、默认 DEFAULT、老值归一 0 行';
END $$;
