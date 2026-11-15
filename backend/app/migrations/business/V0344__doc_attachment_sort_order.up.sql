-- ============================================================
-- V0344：单据附件「手动排序」列（`biz_doc_attachment.sort_order`）—— ATT-03
--
-- # 这条迁移补的是什么
--
-- 附件面板此前只能按 `created_at` 排（ADD 时升序）。ATT-03 要求：
--   ① 默认按**上传时间倒序**（最近的在上，与任务附件 / 文件管理列表同口径）；
--   ② 支持**手动排序** —— 用户可以把「合同正文」拖到扫描件前面。
--
-- ② 需要一列可写的序号，故补 `sort_order integer NOT NULL DEFAULT 0`。
--
-- # 为什么序号从 0 开始、新行取 max+1
--
--   · `DEFAULT 0` 让历史行与「没有排序过」的库有确定行为（按时间倒序兜底）；
--   · 写入侧（`attachment::model::insert`）取 `COALESCE(max(sort_order) + 1, 0)`，
--     于是**新上传的附件永远追加在末尾**，不会因为默认 0 而插到手动排好的序列最前面；
--   · 列表排序键：`ORDER BY sort_order ASC, created_at DESC, id DESC`（手动模式），
--     `ORDER BY created_at DESC, id DESC`（时间模式）。同序号时时间倒序 + 主键兜底，
--     保证分页 / 刷新顺序稳定（`id` 是 UUID v7，时间有序，作最终 tiebreak 不会抖）。
--
-- # 幂等
--
-- `ADD COLUMN IF NOT EXISTS` + 回填走 `IS DISTINCT FROM`（重放不会把用户排好的序号覆盖成
-- 「按时间」的顺序，只在值确实不同时更新）。**不写** BEGIN/COMMIT（迁移器自己在事务里跑）。
--
-- # 回滚
--
-- `V0344__doc_attachment_sort_order.down.sql`：先断言「没有非 0 的手工序号」再删列 ——
-- 有手工排序时删列会**静默丢用户排序结果**，宁可让 down 失败让人先决定。
-- ============================================================

-- ============================================================
-- §1 列
-- ============================================================
ALTER TABLE biz_doc_attachment
    ADD COLUMN IF NOT EXISTS sort_order integer NOT NULL DEFAULT 0;

COMMENT ON COLUMN biz_doc_attachment.sort_order IS
    '手动排序序号（升序）：0 = 未排序；新上传取 COALESCE(max+1, 0) 追加到末尾（ATT-03）';

-- ============================================================
-- §2 回填既有序
--
-- 历史行全部是默认 0（等同于「没排序过」），列表按 `created_at DESC` 兜底本来也正确；
-- 但一旦用户进入手动模式，全 0 会让「上移/下移」的初始顺序与看到的顺序不一致。
-- 故按上传时间给出初始序号（0..n-1），让「看到什么 = 库里什么」。
--
-- `IS DISTINCT FROM` 保证重放幂等：值已经正确就不写。
-- ============================================================
WITH ranked AS (
    SELECT id,
           (row_number() OVER (PARTITION BY doc_type, doc_id
                               ORDER BY created_at ASC, id ASC) - 1)::integer AS rn
      FROM biz_doc_attachment
     WHERE deleted_at IS NULL
)
UPDATE biz_doc_attachment a
   SET sort_order = r.rn
  FROM ranked r
 WHERE a.id = r.id
   AND a.sort_order IS DISTINCT FROM r.rn;

-- ============================================================
-- §3 自检（up 后必须全为真）
-- ============================================================
DO $$
DECLARE
    n_col   integer;
    n_null  integer;
    n_neg   integer;
BEGIN
    SELECT count(*) INTO n_col
      FROM information_schema.columns
     WHERE table_schema = 'public'
       AND table_name = 'biz_doc_attachment'
       AND column_name = 'sort_order';

    IF n_col <> 1 THEN
        RAISE EXCEPTION 'V0344 自检失败：biz_doc_attachment.sort_order 未创建（%）', n_col;
    END IF;

    -- 列是 NOT NULL DEFAULT 0，NULL 只可能来自「列存在但约束没跟上」的漂移
    SELECT count(*) INTO n_null FROM biz_doc_attachment WHERE sort_order IS NULL;
    SELECT count(*) INTO n_neg  FROM biz_doc_attachment WHERE sort_order < 0;

    IF n_null > 0 OR n_neg > 0 THEN
        RAISE EXCEPTION 'V0344 自检失败：sort_order 存在 NULL % 行 / 负值 % 行', n_null, n_neg;
    END IF;

    -- 既有表的 CHECK 约束不能被这条迁移搞掉（只加了列，这里顺带守住）
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_doc_attachment_type'
    ) THEN
        RAISE EXCEPTION 'V0344 自检失败：ck_biz_doc_attachment_type 约束丢失';
    END IF;

    RAISE NOTICE 'V0344 完成：biz_doc_attachment.sort_order 就绪（NOT NULL / 默认 0 / 已回填）';
END $$;
