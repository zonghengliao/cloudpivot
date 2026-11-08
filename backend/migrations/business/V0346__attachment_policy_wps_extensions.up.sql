-- ======================================================
-- V0346：单据附件策略补 WPS 三件套扩展名（ATT-05）
--
-- 依据：ATT-05 卡面「Office 类型（doc/docx/xls/xlsx/ppt/pptx/**wps/et/dps**）不显示预览、
--       只给下载」—— 前端要显示 WPS 图标并允许下载，前提是**能传上来**。
--
-- 问题：`attachment` 策略（V0048 种下的 24000000-…-0001）的 `allowed_extensions` 是
--       'jpg,jpeg,png,gif,bmp,webp,pdf,doc,docx,xls,xlsx,ppt,pptx,txt,csv,md,zip,rar,
--        7z,gz,tar,mp3,wav,mp4,mov' —— **没有 wps / et / dps**。
--       白名单语义是 fail-closed（没列的一律拒绝），所以 WPS 文件上传会被 storage 策略
--       以「不支持的文件类型 .wps」拒掉，卡面要求的「WPS 红图标 + 可下载」根本走不到。
--
-- 做法：只**追加**三个扩展名，不动上限（100 MB 保留，20 MB 的业务闸在
--       `business/attachment/dto.rs::MAX_FILE_BYTES`，比策略更严）与 MIME 口径
--       （allowed_mimes 为空 = 不校验 MIME，与 V0048 / V0107 同款）。
--
-- ── 幂等 ──────────────────────────────────────────────────────────────
-- ① 用「数组展开 → 去重 → 按原顺序拼回」的方式追加，重复执行结果不变；
-- ② 三个都在时整条 UPDATE 不命中（`NOT (exts @> ARRAY[...])`），不会重写行、不会改
--    updated_at，也不会覆盖运维手工调过的顺序；
-- ③ 末尾自检块：策略行不存在、或补完仍缺 WPS 扩展名 → RAISE（让脏配置在迁移期暴露，
--    而不是等用户上传 wps 时才 400）。
--
-- 回滚：V0346__attachment_policy_wps_extensions.down.sql（只摘掉这三个，别的不动）
-- ======================================================

-- ---------- 1) 追加 wps / et / dps（已有则原样保留，顺序不变） ----------
UPDATE sys_storage_policy AS p
   SET allowed_extensions = (
         SELECT string_agg(token, ',' ORDER BY first_ord)
           FROM (
             SELECT token, min(ord) AS first_ord
               FROM unnest(
                      string_to_array(lower(btrim(p.allowed_extensions)), ',')
                      || ARRAY['wps', 'et', 'dps']
                    ) WITH ORDINALITY AS t(token, ord)
              WHERE token <> ''
              GROUP BY token
           ) AS merged
       ),
       updated_at = now()
 WHERE p.category = 'attachment'
   AND p.deleted_at IS NULL
   AND NOT (
         string_to_array(lower(btrim(p.allowed_extensions)), ',') @> ARRAY['wps', 'et', 'dps']
       );

-- ---------- 2) 自检：策略行必须存在，且 Office + WPS 九种扩展名齐全 ----------
DO $$
DECLARE
    exts text;
BEGIN
    SELECT allowed_extensions INTO exts
      FROM sys_storage_policy
     WHERE category = 'attachment'
       AND deleted_at IS NULL;

    IF exts IS NULL THEN
        RAISE EXCEPTION 'V0346：找不到 category = attachment 的存储策略行（V0048 种子缺失？）';
    END IF;

    IF NOT (string_to_array(lower(btrim(exts)), ',') @> ARRAY[
              'doc', 'docx', 'xls', 'xlsx', 'ppt', 'pptx', 'wps', 'et', 'dps']) THEN
        RAISE EXCEPTION 'V0346：attachment 策略仍缺 Office/WPS 扩展名，当前白名单 = %', exts;
    END IF;
END $$;
