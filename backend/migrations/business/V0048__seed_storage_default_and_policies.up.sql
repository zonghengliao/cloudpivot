-- V0048：默认存储后端与上传策略种子（批次 A · A2 存储接口）
--
-- 依据：docs/批次A-技术架构.md §5.2（V0048 行）与 §2.4 / §2.7
--
-- 本迁移只做「数据种子」，不建表、不改列（表结构在 V0040）：
--   1. sys_storage_config：一行 `local-default`，且**是默认后端**。
--      endpoint / bucket / 密钥全为空 —— 走 ck_sys_storage_config_remote 的 LOCAL 分支；
--      base_path = ''  ⇒ 存储根 = `alun::upload_path()`，**历史文件零迁移**。
--   2. sys_storage_policy：5 个常用分类（attachment / avatar / document / import / template）。
--      没建策略的分类不会报错：handler 回落到内置默认策略（50 MB、不限制扩展名、不允许直传）。
--
-- 幂等：固定 UUID + `ON CONFLICT DO NOTHING`，重复执行不报错、不产生重复行。
-- 回滚：V0048__seed_storage_default_and_policies.down.sql 只删本迁移插入的行。

-- ---------- 1. 默认本地后端 ----------
INSERT INTO sys_storage_config (
    id, config_code, config_name, backend_type, endpoint, bucket, region, base_path,
    use_tls, path_style, ak_cipher, ak_nonce, sk_cipher, sk_nonce, key_version,
    is_default, status, health_status, sort_order, remark
) VALUES (
    '23000000-0000-4000-8000-000000000001',
    'local-default',
    '本地磁盘（默认）',
    'LOCAL',
    NULL,
    NULL,
    'us-east-1',
    '',
    FALSE,
    TRUE,
    NULL, NULL, NULL, NULL,
    1,
    TRUE,
    1,
    'UNKNOWN',
    0,
    'A2 种子：默认本地后端，存储根 = [upload].path，历史 sys_file 记录（storage_code IS NULL）按此读取'
)
ON CONFLICT DO NOTHING;

-- ---------- 2. 上传策略（5 类） ----------
-- 扩展名清单一律小写、不带点、不含 '.'（ck_sys_storage_policy_ext 会拒绝带点的写法）。
INSERT INTO sys_storage_policy (
    id, category, category_name, max_size_mb, allowed_extensions, allowed_mimes,
    allow_overwrite, direct_upload, presign_ttl_secs, storage_code, status, sort_order, remark
) VALUES
    (
        '24000000-0000-4000-8000-000000000001',
        'attachment', '附件', 100,
        'jpg,jpeg,png,gif,bmp,webp,pdf,doc,docx,xls,xlsx,ppt,pptx,txt,csv,md,zip,rar,7z,gz,tar,mp3,wav,mp4,mov',
        '',
        FALSE, TRUE, 900, NULL, 1, 10,
        '通用附件：100 MB 上限，图片/文档/压缩包/音视频'
    ),
    (
        '24000000-0000-4000-8000-000000000002',
        'avatar', '头像', 10,
        'jpg,jpeg,png,gif,webp',
        'image/jpeg,image/png,image/gif,image/webp',
        TRUE, TRUE, 600, NULL, 1, 20,
        '头像：10 MB 上限，只允许位图（不收 svg，避免内嵌脚本）'
    ),
    (
        '24000000-0000-4000-8000-000000000003',
        'document', '文档', 200,
        'pdf,doc,docx,xls,xlsx,ppt,pptx,txt,csv,md,json,xml',
        '',
        FALSE, TRUE, 900, NULL, 1, 30,
        '资料文档：200 MB 上限'
    ),
    (
        '24000000-0000-4000-8000-000000000004',
        'import', '导入数据', 50,
        'xlsx,xls,csv',
        '',
        FALSE, FALSE, 900, NULL, 1, 40,
        '导入数据文件：只收表格类；**关闭直传**（导入必须经服务端校验，不能绕过）'
    ),
    (
        '24000000-0000-4000-8000-000000000005',
        'template', '导入模板', 20,
        'xlsx,csv',
        '',
        FALSE, FALSE, 900, NULL, 1, 50,
        '导入/导出模板：体积小、不允许直传'
    )
ON CONFLICT DO NOTHING;
