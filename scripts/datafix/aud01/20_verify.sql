-- ============================================================
-- AUD-01 · 回填后**只读**验收（对应卡面验收 ①②③④）
--
--   psql -X -h <h> -U <u> -d <db> -f scripts/datafix/aud01/20_verify.sql
--
-- ① 两表都有 uploader_name 列
-- ② 空值数 = 0
-- ③ 姓名列里没有 UUID 形态的脏值
-- ④ 「已删除用户 / (未记录上传人)」只出现在确实追溯不到人的行上（不吞真名）
-- ============================================================

\pset pager off

\echo '=== ① 列就绪（应 2 行）==='
SELECT table_name, column_name, data_type, character_maximum_length
  FROM information_schema.columns
 WHERE table_schema = 'public'
   AND column_name = 'uploader_name'
   AND table_name IN ('biz_doc_attachment', 'biz_task_attachment')
 ORDER BY table_name;

\echo '=== ② 空值数（必须全为 0）==='
SELECT 'biz_doc_attachment' AS tbl,
       count(*) AS total_rows,
       count(*) FILTER (WHERE uploader_name IS NULL) AS null_rows,
       count(*) FILTER (WHERE btrim(uploader_name) = '') AS empty_string_rows
  FROM biz_doc_attachment
UNION ALL
SELECT 'biz_task_attachment', count(*),
       count(*) FILTER (WHERE uploader_name IS NULL),
       count(*) FILTER (WHERE btrim(uploader_name) = '')
  FROM biz_task_attachment;

\echo '=== ③ UUID 形态脏值（必须 0）==='
SELECT 'biz_doc_attachment' AS tbl, count(*) AS uuid_like_rows
  FROM biz_doc_attachment
 WHERE uploader_name ~ '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-'
UNION ALL
SELECT 'biz_task_attachment', count(*)
  FROM biz_task_attachment
 WHERE uploader_name ~ '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-';

\echo '=== ④ 占位串的合理性（"已删除用户" 的行必须真的追溯不到在职账号）==='
SELECT 'biz_doc_attachment' AS tbl,
       count(*) FILTER (WHERE a.uploader_name = '(已删除用户)')                       AS deleted_label_rows,
       count(*) FILTER (WHERE a.uploader_name = '(已删除用户)'
                          AND u.id IS NOT NULL AND u.deleted_at IS NULL)              AS wrong_deleted_label,
       count(*) FILTER (WHERE a.uploader_name = '(未记录上传人)' AND a.uploaded_by IS NOT NULL) AS wrong_no_uploader_label
  FROM biz_doc_attachment a
  LEFT JOIN sys_user u ON u.id = a.uploaded_by
UNION ALL
SELECT 'biz_task_attachment',
       count(*) FILTER (WHERE a.uploader_name = '(已删除用户)'),
       count(*) FILTER (WHERE a.uploader_name = '(已删除用户)'
                          AND u.id IS NOT NULL AND u.deleted_at IS NULL),
       count(*) FILTER (WHERE a.uploader_name = '(未记录上传人)' AND a.create_by IS NOT NULL)
  FROM biz_task_attachment a
  LEFT JOIN sys_user u ON u.id = a.create_by;

\echo '=== ⑤ 抽样 10 行（人眼确认名字不是 UUID）==='
(SELECT 'biz_doc_attachment' AS tbl, id, uploader_name, uploaded_by::text AS uploader_id, created_at
   FROM biz_doc_attachment ORDER BY created_at DESC LIMIT 5)
UNION ALL
(SELECT 'biz_task_attachment', id, uploader_name, create_by::text, created_at
   FROM biz_task_attachment ORDER BY created_at DESC LIMIT 5);
