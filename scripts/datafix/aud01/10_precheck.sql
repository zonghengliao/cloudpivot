-- ============================================================
-- AUD-01 · 回填前**只读**体检（不写库）
--
--   psql -X -h <h> -U <u> -d <db> -f scripts/datafix/aud01/10_precheck.sql
--
-- 回答四件事：
--   ① V0352 是否已应用（两列在不在、类型对不对）
--   ② 空值有多少（回填要处理的行数）
--   ③ 空值里「能查到人 / 已删 / 账号不存在 / 无上传人」各多少（预测回填结果）
--   ④ 存量里有没有「姓名列里塞了 UUID」的脏数据（回填跑不出来的那类）
-- ============================================================

\pset pager off

\echo '=== ① 列是否就绪（应为 2 行，data_type = character varying）==='
SELECT table_name, column_name, data_type, character_maximum_length, is_nullable
  FROM information_schema.columns
 WHERE table_schema = 'public'
   AND column_name = 'uploader_name'
   AND table_name IN ('biz_doc_attachment', 'biz_task_attachment')
 ORDER BY table_name;

\echo '=== ② 空值数（回填要处理的行数）==='
SELECT 'biz_doc_attachment' AS tbl,
       count(*) AS total_rows,
       count(*) FILTER (WHERE uploader_name IS NULL) AS null_rows,
       count(*) FILTER (WHERE uploader_name IS NOT NULL) AS filled_rows
  FROM biz_doc_attachment
UNION ALL
SELECT 'biz_task_attachment', count(*),
       count(*) FILTER (WHERE uploader_name IS NULL),
       count(*) FILTER (WHERE uploader_name IS NOT NULL)
  FROM biz_task_attachment;

\echo '=== ③ 空值分布（预测回填会写什么）==='
SELECT 'biz_doc_attachment' AS tbl,
       count(*) FILTER (WHERE a.uploaded_by IS NULL)                          AS no_uploader_id,
       count(*) FILTER (WHERE u.id IS NOT NULL AND u.deleted_at IS NULL)       AS live_user,
       count(*) FILTER (WHERE u.id IS NOT NULL AND u.deleted_at IS NOT NULL)   AS soft_deleted_user,
       count(*) FILTER (WHERE a.uploaded_by IS NOT NULL AND u.id IS NULL)      AS user_row_missing
  FROM biz_doc_attachment a
  LEFT JOIN sys_user u ON u.id = a.uploaded_by
 WHERE a.uploader_name IS NULL
UNION ALL
SELECT 'biz_task_attachment',
       count(*) FILTER (WHERE a.create_by IS NULL),
       count(*) FILTER (WHERE u.id IS NOT NULL AND u.deleted_at IS NULL),
       count(*) FILTER (WHERE u.id IS NOT NULL AND u.deleted_at IS NOT NULL),
       count(*) FILTER (WHERE a.create_by IS NOT NULL AND u.id IS NULL)
  FROM biz_task_attachment a
  LEFT JOIN sys_user u ON u.id = a.create_by
 WHERE a.uploader_name IS NULL;

\echo '=== ④ 脏数据：姓名列里像 UUID 的行（应为 0）==='
SELECT 'biz_doc_attachment' AS tbl, count(*) AS uuid_like_rows
  FROM biz_doc_attachment
 WHERE uploader_name ~ '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-'
UNION ALL
SELECT 'biz_task_attachment', count(*)
  FROM biz_task_attachment
 WHERE uploader_name ~ '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-';
