-- ======================================================
-- V0346 回滚：从 attachment 策略的扩展名白名单里摘掉 wps / et / dps
--
-- 只做一件事：把这三个 token 从数组里过滤掉，其余扩展名与顺序原样保留。
--
-- 提醒：
--   1. 回滚**不会**删除已上传的 WPS 文件（`sys_file` 是值引用、无外键；读路径不看策略），
--      只是**新的上传**会重新被拒 —— 与 V0107 down 的说明同款。
--   2. 如果这三个本来就不在（比如从没跑过 up），本语句 0 命中，不会报错。
-- ======================================================

UPDATE sys_storage_policy AS p
   SET allowed_extensions = (
         SELECT string_agg(token, ',' ORDER BY ord)
           FROM unnest(
                  string_to_array(lower(btrim(p.allowed_extensions)), ',')
                ) WITH ORDINALITY AS t(token, ord)
          WHERE token <> ''
            AND token NOT IN ('wps', 'et', 'dps')
       ),
       updated_at = now()
 WHERE p.category = 'attachment'
   AND p.deleted_at IS NULL
   AND string_to_array(lower(btrim(p.allowed_extensions)), ',') && ARRAY['wps', 'et', 'dps'];
