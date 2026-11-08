-- ============================================================
-- AUD-01 · 附件上传人姓名**存量回填**（一次性数据修复，**不是迁移**）
--
--   对象：`biz_doc_attachment.uploader_name` / `biz_task_attachment.uploader_name`
--         （两列由迁移 `V0352__attachment_uploader_name` 加，本脚本只填值）
--   判据：只填 `uploader_name IS NULL` 的行 —— 已经有值的一律不动
--         （重跑安全；人工改过的名字也不会被覆盖）
--
-- # 为什么不做成迁移
--
-- ① 迁移在启动链上跑，大库回填会拖慢启动 / 拉长滚动窗口；
-- ② 回填依赖 `sys_user` 的**当下现状**（谁还在、谁已删），属于数据修复而非结构变更；
--    仓库惯例：一次性数据修复放 `scripts/datafix/`（见 `scripts/datafix/p10/README.md`）。
--
-- # 五条取值口径（与 Rust 侧 `business/shared/user_name.rs` 逐字一致）
--
--   ① 账号在库、未删、real_name 非空        → real_name
--   ② 账号在库、未删、real_name 为空        → username
--   ③ 账号已软删（deleted_at 非空）         → '(已删除用户)'
--   ④ uploaded_by 有值但账号行已不存在      → '(已删除用户)'   （物理清理过，追溯不到）
--   ⑤ uploaded_by 本身为空                  → '(未记录上传人)' （历史夹具 / 系统代传）
--
--   `uploader_name` 列里「空值」只能有一个含义：**本脚本还没跑**。
--   所以 ③④⑤ 都写占位串而不是留空 —— 否则「没回填」和「没上传人」分不开。
--
-- # 幂等
--
-- 全脚本只有 `WHERE uploader_name IS NULL` 的 UPDATE 与 `ON CONFLICT DO NOTHING` 的
-- 备份插入：连跑第二遍，每一段的影响行数都必须是 0（脚本会把实测行数打出来）。
--
-- # 可回滚
--
-- ① 改之前把「行 id + 改前值 + 改后值」写进 `datafix_aud01_uploader_name_backup`；
-- ② 回滚语句在文件末尾（只还原「还是回填写进去的那个值」的行 —— 回填之后人工改过的不动）。
--
-- # 用法
--
--   bash scripts/datafix/aud01/apply.sh --check      # 只读体检（列在不在、空值多少）
--   bash scripts/datafix/aud01/apply.sh             # 回填（幂等，可重复跑）
--   bash scripts/datafix/aud01/apply.sh --rollback  # 按台账还原
--
--   库连接：AUD01_DB / PGHOST / PGPORT / PGUSER / PGPASSWORD 环境变量（默认
--   cloudpivot@127.0.0.1:5432）。口令只走环境变量，别写进命令行历史。
-- ============================================================

\set ON_ERROR_STOP on
\pset pager off

BEGIN;

-- ============================================================
-- §1 台账（先备份「要改的行」：改前值 + 改后值，用于精确回滚）
-- ============================================================
CREATE TABLE IF NOT EXISTS datafix_aud01_uploader_name_backup (
    tbl                  text        NOT NULL,
    row_id               uuid        NOT NULL,
    uploaded_by          uuid,
    uploader_name_before varchar(64),
    uploader_name_after  varchar(64),
    captured_at          timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT pk_datafix_aud01_uploader_name_backup PRIMARY KEY (tbl, row_id)
);

-- 老版本台账（本卡第一版没有 after 列）补列
ALTER TABLE datafix_aud01_uploader_name_backup
    ADD COLUMN IF NOT EXISTS uploader_name_after varchar(64);

COMMENT ON TABLE datafix_aud01_uploader_name_backup IS
    'AUD-01 附件上传人姓名回填台账（tbl = biz_doc_attachment / biz_task_attachment；before/after 两列可精确回滚）';

INSERT INTO datafix_aud01_uploader_name_backup (tbl, row_id, uploaded_by, uploader_name_before)
SELECT 'biz_doc_attachment', id, uploaded_by, uploader_name
  FROM biz_doc_attachment
 WHERE uploader_name IS NULL
ON CONFLICT (tbl, row_id) DO NOTHING;

INSERT INTO datafix_aud01_uploader_name_backup (tbl, row_id, uploaded_by, uploader_name_before)
SELECT 'biz_task_attachment', id, create_by, uploader_name
  FROM biz_task_attachment
 WHERE uploader_name IS NULL
ON CONFLICT (tbl, row_id) DO NOTHING;

-- ============================================================
-- §2 回填：单据附件（biz_doc_attachment.uploaded_by）
-- ============================================================
DO $$
DECLARE
    n1 integer;
    n2 integer;
    n3 integer;
    n4 integer;
BEGIN
    -- ① 账号在库且未删 → real_name 优先，回落 username
    UPDATE biz_doc_attachment a
       SET uploader_name = COALESCE(NULLIF(u.real_name, ''), NULLIF(u.username, ''), '(已删除用户)')
      FROM sys_user u
     WHERE a.uploaded_by = u.id
       AND a.uploader_name IS NULL
       AND u.deleted_at IS NULL;
    GET DIAGNOSTICS n1 = ROW_COUNT;

    -- ② 账号已软删 → 占位串（改名 / 删号后要能看出「人已不在」）
    UPDATE biz_doc_attachment a
       SET uploader_name = '(已删除用户)'
      FROM sys_user u
     WHERE a.uploaded_by = u.id
       AND a.uploader_name IS NULL
       AND u.deleted_at IS NOT NULL;
    GET DIAGNOSTICS n2 = ROW_COUNT;

    -- ③ 上传人 id 有值但 sys_user 里没有这一行（物理清理过）
    UPDATE biz_doc_attachment a
       SET uploader_name = '(已删除用户)'
     WHERE a.uploader_name IS NULL
       AND a.uploaded_by IS NOT NULL
       AND NOT EXISTS (SELECT 1 FROM sys_user u WHERE u.id = a.uploaded_by);
    GET DIAGNOSTICS n3 = ROW_COUNT;

    -- ④ 上传人 id 本身为空（历史夹具 / 系统代传）
    UPDATE biz_doc_attachment
       SET uploader_name = '(未记录上传人)'
     WHERE uploader_name IS NULL
       AND uploaded_by IS NULL;
    GET DIAGNOSTICS n4 = ROW_COUNT;

    RAISE NOTICE 'AUD-01 / biz_doc_attachment：实名 % 行，已删 % 行，账号不存在 % 行，无上传人 % 行',
        n1, n2, n3, n4;
END $$;

-- ============================================================
-- §3 回填：任务附件（biz_task_attachment.create_by 是上传人 id）
-- ============================================================
DO $$
DECLARE
    n1 integer;
    n2 integer;
    n3 integer;
    n4 integer;
BEGIN
    UPDATE biz_task_attachment a
       SET uploader_name = COALESCE(NULLIF(u.real_name, ''), NULLIF(u.username, ''), '(已删除用户)')
      FROM sys_user u
     WHERE a.create_by = u.id
       AND a.uploader_name IS NULL
       AND u.deleted_at IS NULL;
    GET DIAGNOSTICS n1 = ROW_COUNT;

    UPDATE biz_task_attachment a
       SET uploader_name = '(已删除用户)'
      FROM sys_user u
     WHERE a.create_by = u.id
       AND a.uploader_name IS NULL
       AND u.deleted_at IS NOT NULL;
    GET DIAGNOSTICS n2 = ROW_COUNT;

    UPDATE biz_task_attachment a
       SET uploader_name = '(已删除用户)'
     WHERE a.uploader_name IS NULL
       AND a.create_by IS NOT NULL
       AND NOT EXISTS (SELECT 1 FROM sys_user u WHERE u.id = a.create_by);
    GET DIAGNOSTICS n3 = ROW_COUNT;

    UPDATE biz_task_attachment
       SET uploader_name = '(未记录上传人)'
     WHERE uploader_name IS NULL
       AND create_by IS NULL;
    GET DIAGNOSTICS n4 = ROW_COUNT;

    RAISE NOTICE 'AUD-01 / biz_task_attachment：实名 % 行，已删 % 行，账号不存在 % 行，无上传人 % 行',
        n1, n2, n3, n4;
END $$;

-- ============================================================
-- §4 把「回填后的值」写进台账（回滚只还原这些值，不动之后的人工修改）
-- ============================================================
UPDATE datafix_aud01_uploader_name_backup b
   SET uploader_name_after = a.uploader_name
  FROM biz_doc_attachment a
 WHERE b.tbl = 'biz_doc_attachment'
   AND b.row_id = a.id
   AND b.uploader_name_after IS NULL;

UPDATE datafix_aud01_uploader_name_backup b
   SET uploader_name_after = a.uploader_name
  FROM biz_task_attachment a
 WHERE b.tbl = 'biz_task_attachment'
   AND b.row_id = a.id
   AND b.uploader_name_after IS NULL;

-- ============================================================
-- §5 自检：跑完两张表都不许再有空值（验收③「空值数 = 0」）
-- ============================================================
DO $$
DECLARE
    n_doc  integer;
    n_task integer;
BEGIN
    SELECT count(*) INTO n_doc  FROM biz_doc_attachment  WHERE uploader_name IS NULL;
    SELECT count(*) INTO n_task FROM biz_task_attachment WHERE uploader_name IS NULL;

    IF n_doc <> 0 OR n_task <> 0 THEN
        RAISE EXCEPTION 'AUD-01 回填自检失败：仍有空值（biz_doc_attachment % 行 / biz_task_attachment % 行）',
            n_doc, n_task;
    END IF;

    RAISE NOTICE 'AUD-01 回填完成：两张表 uploader_name 空值数均为 0';
END $$;

COMMIT;

-- ============================================================
-- §6 报告（已提交后打印，查询失败不影响数据）
-- ============================================================
\echo '--- 回填分布（biz_doc_attachment）'
SELECT COALESCE(uploader_name, '<NULL>') AS uploader_name, count(*) AS rows
  FROM biz_doc_attachment
 GROUP BY uploader_name
 ORDER BY rows DESC
 LIMIT 20;

\echo '--- 回填分布（biz_task_attachment）'
SELECT COALESCE(uploader_name, '<NULL>') AS uploader_name, count(*) AS rows
  FROM biz_task_attachment
 GROUP BY uploader_name
 ORDER BY rows DESC
 LIMIT 20;

\echo '--- 台账（本库累计回填过的行数）'
SELECT tbl, count(*) AS backed_up_rows,
       count(uploader_name_after) AS with_after_value,
       min(captured_at) AS first_captured, max(captured_at) AS last_captured
  FROM datafix_aud01_uploader_name_backup
 GROUP BY tbl
 ORDER BY tbl;

-- ============================================================
-- 回滚（`apply.sh --rollback` 跑的就是下面这段；只还原 untouched 的行）
--
--   BEGIN;
--   UPDATE biz_doc_attachment a SET uploader_name = b.uploader_name_before
--     FROM datafix_aud01_uploader_name_backup b
--    WHERE b.tbl = 'biz_doc_attachment' AND a.id = b.row_id
--      AND b.uploader_name_after IS NOT NULL
--      AND a.uploader_name = b.uploader_name_after;
--   UPDATE biz_task_attachment a SET uploader_name = b.uploader_name_before
--     FROM datafix_aud01_uploader_name_backup b
--    WHERE b.tbl = 'biz_task_attachment' AND a.id = b.row_id
--      AND b.uploader_name_after IS NOT NULL
--      AND a.uploader_name = b.uploader_name_after;
--   COMMIT;
--
--   ⚠ 回填之后又被新代码写过姓名（上传时快照）的行不会命中（值已不是回填值），这是刻意的：
--     那些行是**真实上传快照**，回滚数据修复不该把它们抹掉。
-- ============================================================
