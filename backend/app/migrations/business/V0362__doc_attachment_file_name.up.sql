-- ============================================================
-- V0362：单据附件**文件名快照**列（`biz_doc_attachment.file_name`）—— **E2E07-FIX-3**
--
-- # 这条迁移补的是什么
--
-- `biz_doc_attachment` 只存 `file_id`，文件名一律读 `sys_file.original_name`。
-- 但 `sys_file` 是**按内容 hash 秒传复用**的（同字节的文件共用一条记录，
-- `uk_sys_file_hash`），于是：
--
--   用户 A 上传 `drill-invoice.pdf`（内容 X）   → sys_file.original_name = 'drill-invoice.pdf'
--   用户 B 上传 `e2e07-contract-scan.pdf`（同内容 X）→ 复用同一条 sys_file（不写新行）
--
--   B 打开附件列表 → 读 `sys_file.original_name` → 显示 **drill-invoice.pdf**
--
-- 这不是显示瑕疵而是**审计隐患**：附件名是人对"这份材料是什么"的第一判据，
-- 显示别人的名字会让审计把扫描件误认成发票。
--
-- # 为什么是「加一列快照」而不是「改 sys_file」
--
-- 文件名是**绑定关系（这次上传）的属性**，不是**文件内容**的属性。
-- 同字节的两份上传可以有完全不同的名字，共享的 sys_file 上只能存一个名字 ——
-- 那是「首个上传者的名字」，天然不能代表所有引用者（与 AUD-01 的
-- `uploader_name` 快照同一个判断：审计属性必须落在这条业务事实上）。
--
-- 读路径（`backend/src/business/attachment/model.rs` 的 `COLUMNS`）：
--
--   COALESCE(NULLIF(a.file_name, ''), f.original_name, '')  → 显示名 → 空则退回对象键 basename
--
--   · 有新快照 → 用快照（秒传复用也各显其名）；
--   · 没有（本迁移之前的历史行，回填没跑 / 回填不到）→ 回落 `sys_file.original_name`，
--     界面永不为空，也**永不回落 UUID**（同 AUD-01 的三段回落口径）。
--
-- # 存量回填为什么放迁移里（与 V0352 不同）
--
-- V0352 的 `uploader_name` 回填要 JOIN `sys_user` 的现状，属于数据修复；
-- 本列的回填只 JOIN `sys_file`（一行一个、无筛选），且 `biz_doc_attachment`
-- 是绑定表（99 上 1276 行、145 dev 库 200 行量级），一次 UPDATE 是毫秒级，
-- 启动链上跑没有风险。**留空比回填更糟**：留空会走进回落分支，
-- 而回落分支正是本卡要修的那个错名来源。
--
-- # 列的性质
--
--   varchar(255)  可空（与 sys_file.original_name 同宽）
--     可空是必须的：① 新增行在「插入时没给名字」的极端路径（测试夹具 / 历史脚本）
--     仍然要能写进去；② 迁移与代码不同步的窗口（先上代码后跑迁移）不能 500。
--     读路径对 NULL 有明确回落，不靠 NOT NULL 兜底。
--
--   不加索引：本列只用于显示，没有任何查询条件落在它上面。
--
-- # 幂等 / 回滚
--
--   `ADD COLUMN IF NOT EXISTS` + 回填只碰 `NULLIF(file_name,'') IS NULL` 的行
--   （重复执行 = 0 行受影响，不会覆盖已经有快照的值）。
--   文件内**不写** BEGIN/COMMIT（迁移器自己在事务里跑）。
--   回滚：`V0362__doc_attachment_file_name.down.sql` —— 列可丢、数据不可恢复。
-- ============================================================

-- ============================================================
-- §1 加列
-- ============================================================
ALTER TABLE biz_doc_attachment
    ADD COLUMN IF NOT EXISTS file_name varchar(255);

-- ============================================================
-- §2 列注释（语义写进库，别只写在代码注释里）
-- ============================================================
COMMENT ON COLUMN biz_doc_attachment.file_name IS
    '本次上传的文件名**快照**（E2E07-FIX-3）：上传时写入 multipart 的 filename，此后不回改；秒传复用同一个 sys_file 时，每条绑定各显其名（读路径 COALESCE(NULLIF(a.file_name,''), f.original_name, '')）';

-- ============================================================
-- §3 存量回填（只能用「当下」能拿到的名字：sys_file.original_name）
--
-- 说明：历史行被秒传污染过的那部分，回填出来的名字本来就可能是**别人**的
-- （信息已经丢了，DB 里没有第二处记录「当时 B 传的名字」）。
-- 回填的价值是「不留空 → 不进回落分支 → 未来不会再被 sys_file 的名字牵着走」；
-- 真正准确的名字只对**本迁移之后**的上传有效。
-- ============================================================
UPDATE biz_doc_attachment a
   SET file_name = COALESCE(
           NULLIF(f.original_name, ''),
           NULLIF(regexp_replace(f.file_name, '^.*/', ''), ''),
           '附件'
       )
  FROM sys_file f
 WHERE a.file_id = f.id
   AND NULLIF(a.file_name, '') IS NULL;

-- ============================================================
-- §4 自检（up 后必须全为真）
-- ============================================================
DO $$
DECLARE
    n_col   integer;
    n_type  integer;
    n_null  integer;
    n_empty bigint;
BEGIN
    -- ① 列在
    SELECT count(*) INTO n_col
      FROM information_schema.columns
     WHERE table_schema = 'public'
       AND table_name = 'biz_doc_attachment'
       AND column_name = 'file_name';
    IF n_col <> 1 THEN
        RAISE EXCEPTION 'V0362 自检失败：预期 1 列 biz_doc_attachment.file_name，实际 %', n_col;
    END IF;

    -- ② 类型 = character varying(255)（与 sys_file.original_name 同宽）
    SELECT count(*) INTO n_type
      FROM information_schema.columns
     WHERE table_schema = 'public'
       AND table_name = 'biz_doc_attachment'
       AND column_name = 'file_name'
       AND data_type = 'character varying'
       AND character_maximum_length = 255;
    IF n_type <> 1 THEN
        RAISE EXCEPTION 'V0362 自检失败：预期 file_name 为 varchar(255)，实际 %', n_type;
    END IF;

    -- ③ 必须可空（历史 / 夹具行，读路径有回落）
    SELECT count(*) INTO n_null
      FROM information_schema.columns
     WHERE table_schema = 'public'
       AND table_name = 'biz_doc_attachment'
       AND column_name = 'file_name'
       AND is_nullable = 'YES';
    IF n_null <> 1 THEN
        RAISE EXCEPTION 'V0362 自检失败：file_name 必须可空，实际可空列 % 个', n_null;
    END IF;

    -- ④ 回填后不许有空值（验收②：空值为 0）
    --    只统计「绑定的 sys_file 还在」的行：sys_file 外键是 ON DELETE RESTRICT，
    --    正常库里不存在悬空 file_id，这里再兜一层是为了让自检在脏库上也说得清。
    SELECT count(*) INTO n_empty
      FROM biz_doc_attachment a
      JOIN sys_file f ON f.id = a.file_id
     WHERE NULLIF(a.file_name, '') IS NULL;
    IF n_empty <> 0 THEN
        RAISE EXCEPTION 'V0362 自检失败：回填后仍有 % 行 file_name 为空', n_empty;
    END IF;

    RAISE NOTICE 'V0362 完成：biz_doc_attachment.file_name varchar(255) 就绪，存量回填无空值';
END $$;
