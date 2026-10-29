-- ======================================================
-- V0086 回滚：删掉本迁移种下的 3 类数据
--
-- 只删本迁移插入的行（固定 id / key / code 三重限定）：
--   · sys_storage_policy.category = 'announcement'
--   · sys_config 的 5 个键
--   · sys_help_category 的 3 个默认分类
--
-- 注意（与 V0048 同款提醒）：
--   1. 若上线后已经用 `category=announcement` 上传过附件（`sys_file.category`），
--      删掉策略行后这些上传会回退到 `document` → 内置默认策略（C3 handler 的三级回退），
--      不会让历史文件失效（`sys_file.category` 是值引用，没有外键）。
--   2. `sys_help_category` 被 `sys_help_doc.category_id` 以 ON DELETE RESTRICT 引用：
--      若运营已经在这 3 个分类下建过文档，本 down **会因外键失败而中止**（这是有意的：
--      不让回滚顺手删掉挂在这些分类下的业务文档）。先把文档迁走再回滚。
-- ======================================================

DELETE FROM sys_storage_policy
 WHERE category = 'announcement'
   AND id = '24000000-0000-4000-8000-000000000006';

DELETE FROM sys_config
 WHERE config_key IN (
    'announcement.default_expire_days',
    'announcement.fanout_enabled',
    'announcement.scope_refresh_on_dept_change',
    'changelog.retention_months',
    'changelog.big_field_bytes'
 )
   AND id IN (
    '25000000-0000-4000-8000-000000000001'::uuid,
    '25000000-0000-4000-8000-000000000002'::uuid,
    '25000000-0000-4000-8000-000000000003'::uuid,
    '25000000-0000-4000-8000-000000000004'::uuid,
    '25000000-0000-4000-8000-000000000005'::uuid
 );

-- 只删「本迁移种的 id」且「code 一致」且「没有子分类」的行：
-- 运维改过名的分类照样删（id 是我们的），但运营自己新建的同名 code 不受影响。
DELETE FROM sys_help_category
 WHERE id IN (
    '26000000-0000-4000-8000-000000000001'::uuid,
    '26000000-0000-4000-8000-000000000002'::uuid,
    '26000000-0000-4000-8000-000000000003'::uuid
 )
   AND code IN ('getting-started', 'faq', 'features')
   AND NOT EXISTS (
           SELECT 1 FROM sys_help_category child
            WHERE child.parent_id = sys_help_category.id
              AND child.deleted_at IS NULL
       );
