-- ======================================================
-- V0086：批次 C 数据种子 —— announcement 存储策略 + 5 个 sys_config + 3 个默认帮助分类
--
-- 依据：docs/批次C-技术架构.md
--   §2.x 存储：`category=announcement` 允许 `jpg/png/pdf/docx/xlsx/zip`、单文件 ≤ 50 MB、禁可执行
--   §5.3 V0086 行 + 「V0086 的 sys_config 参数」表（5 个键与默认值逐字照抄）
--   §5.3 V0086 ③ 帮助文档默认分类 3 条
-- 回滚：V0086__seed_announcement_storage_and_configs.down.sql
--
-- ── 为什么「禁可执行」没有独立列 ─────────────────────────────────────
-- `sys_storage_policy` 只有**扩展名白名单**（`allowed_extensions`），没有黑名单列。
-- 因此「禁可执行」的唯一正确实现就是「白名单里不含任何可执行扩展名」——
-- 白名单语义天然 fail-closed：没列进去的一律拒绝。文件末尾的自检块逐个断言
-- 白名单里不存在 exe/sh/bat/js/… 这类可执行扩展名，将来有人手工往白名单里加
-- 一个 `exe`，下一次 up 重放会直接炸在迁移期，而不是让上传口子悄悄打开。
--
-- ── 为什么 allowed_mimes 留空 ────────────────────────────────────────
-- `business/storage/model.rs::check_upload` 的判定是「allowed_mimes 为空 ⇒ 不校验 MIME」。
-- 公告附件是「运营上传、员工下载」的文档类文件，docx/xlsx 在部分客户端会被报成
-- `application/octet-stream`，**硬校验 MIME 会把合法文件挡在门外**；扩展名白名单
-- 已经覆盖了安全诉求（可执行被排除）。因此这里**刻意留空**，写进迁移头而不是
-- 悄悄省略，避免后来者以为是漏配。
--
-- ── 为什么 config_type 用 BOOLEAN 而不是历史种子的 BOOL ───────────────
-- `support/config/model.rs` 的白名单（写入侧 `is_valid_config_type` 的唯一口径）是
-- `STRING / NUMBER / BOOLEAN / JSON`。历史种子 `S__config_data.sql` 里
-- `log.ip_location_enabled` 写的是 `BOOL`（不在白名单内，属 `shared/enums.rs` 记录的
-- 「三重不一致」历史债）。**新数据一律按白名单写 `BOOLEAN`**，不改历史行
-- （改历史行超出「只加种子」的红线，且会影响已上线环境的配置页显示）。
--
-- ── 幂等 ──────────────────────────────────────────────────────────────
-- 三类都用固定 UUID + 与各自唯一索引匹配的 `ON CONFLICT ... DO NOTHING`：
--   · sys_storage_policy：`uk_sys_storage_policy_category (category) WHERE deleted_at IS NULL`
--   · sys_config        ：`uk_sys_config_key (config_key) WHERE deleted_at IS NULL`
--   · sys_help_category ：`uk_sys_help_category_code (code) WHERE deleted_at IS NULL`
-- 重复执行不报错、不产生重复行、不覆盖运维改过的值（种子只保证「存在」）。
-- ======================================================

-- ---------- 1) 公告附件上传策略（category = 'announcement'） ----------
-- 公告附件上传走 `POST /api/business/storage/upload?category=announcement`；
-- C3 的附件落库前会 `find_policy_by_category("announcement")` → 回退 `document` →
-- 内置默认。本行种下后，公告附件**先用本策略**（50 MB，只收 6 类扩展名）。
INSERT INTO sys_storage_policy (
    id, category, category_name, max_size_mb, allowed_extensions, allowed_mimes,
    allow_overwrite, direct_upload, presign_ttl_secs, storage_code, status, sort_order, remark
) VALUES (
    '24000000-0000-4000-8000-000000000006',
    'announcement',
    '公告附件',
    50,
    'jpg,jpeg,png,pdf,docx,xlsx,zip',
    '',
    FALSE,
    TRUE,
    900,
    NULL,
    1,
    60,
    '公告附件：50 MB 上限；白名单 jpg/jpeg/png/pdf/docx/xlsx/zip；'
    || '可执行扩展名（exe/sh/bat/js/vbs/msi/ps1/jar/so/dll…）一律不在白名单内，等同禁用；'
    || 'MIME 不校验（allowed_mimes 为空）——docx/xlsx 常被客户端报成 octet-stream'
)
ON CONFLICT (category) WHERE deleted_at IS NULL DO NOTHING;

-- ---------- 2) 5 个 sys_config（架构 §5.3 表，键与默认值逐字一致） ----------
INSERT INTO sys_config (
    id, config_key, config_value, config_type, description,
    category, config_level, is_modifiable, is_encrypted, is_restart, status,
    sort_order, ref_value
) VALUES
    (
        '25000000-0000-4000-8000-000000000001',
        'announcement.default_expire_days',
        '30',
        'NUMBER',
        '新建公告时前端预填的有效期（天）；到期由 announcement_auto_expire 任务自动下线',
        'announcement', 2, TRUE, FALSE, FALSE, 1,
        10, '7~90'
    ),
    (
        '25000000-0000-4000-8000-000000000002',
        'announcement.fanout_enabled',
        'true',
        'BOOLEAN',
        '全局扇出开关（应急止血）：false 时发布只落库不推消息，'
        || '由 announcement_dispatch_retry 补偿任务兜底；缺省/读取失败一律按 true',
        'announcement', 2, TRUE, FALSE, FALSE, 1,
        20, 'true'
    ),
    (
        '25000000-0000-4000-8000-000000000003',
        'announcement.scope_refresh_on_dept_change',
        'false',
        'BOOLEAN',
        '受众是否跟随部门变更（架构 §2.3.2）：true 时部门调整会重算已发布公告的可见范围；'
        || 'false 时按发布那一刻的受众快照',
        'announcement', 2, TRUE, FALSE, FALSE, 1,
        30, 'false'
    ),
    (
        '25000000-0000-4000-8000-000000000004',
        'changelog.retention_months',
        '36',
        'NUMBER',
        '变更日志分区保留月数（架构 §4.6）：超期分区由 changelog_partition_maintain 任务清理',
        'changelog', 2, TRUE, FALSE, FALSE, 1,
        40, '12~60'
    ),
    (
        '25000000-0000-4000-8000-000000000005',
        'changelog.big_field_bytes',
        '2048',
        'NUMBER',
        '大字段摘要化阈值（字节）：单字段新旧值超此长度时只存摘要，避免分区表被长文本撑爆',
        'changelog', 2, TRUE, FALSE, FALSE, 1,
        50, '512~65536'
    )
ON CONFLICT (config_key) WHERE deleted_at IS NULL DO NOTHING;

-- ---------- 3) 3 个默认帮助分类 ----------
-- code 必须满足 `ck_sys_help_category_code`：^[a-z][a-z0-9_-]{1,63}$
INSERT INTO sys_help_category (
    id, parent_id, name, code, icon, description, sort_order, status
) VALUES
    (
        '26000000-0000-4000-8000-000000000001',
        NULL, '快速开始', 'getting-started', 'rocket',
        '新用户上手路径：登录、界面导览、第一个业务流程', 10, 1
    ),
    (
        '26000000-0000-4000-8000-000000000002',
        NULL, '常见问题', 'faq', 'help-circle',
        '高频问题与排查步骤（含报错码解释）', 20, 1
    ),
    (
        '26000000-0000-4000-8000-000000000003',
        NULL, '功能说明', 'features', 'book',
        '按模块划分的功能清单与操作说明', 30, 1
    )
ON CONFLICT (code) WHERE deleted_at IS NULL DO NOTHING;

-- ---------- 4) 自检（失败即迁移失败） ----------
DO $$
DECLARE
    policy_row   sys_storage_policy%ROWTYPE;
    cfg_cnt      int;
    cat_cnt      int;
    dangerous    text[] := ARRAY[
        'exe','sh','bat','cmd','com','scr','msi','msix','js','mjs','vbs','vbe','wsf','wsh',
        'ps1','psm1','jar','dll','so','dylib','apk','py','pl','php','jsp','asp','aspx','hta',
        'reg','lnk','cpl','inf','pif','gadget','application'
    ];
    ext          text;
BEGIN
    -- 4.1 策略行存在且字段符合架构要求
    SELECT * INTO policy_row
      FROM sys_storage_policy
     WHERE category = 'announcement' AND deleted_at IS NULL;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'V0086 自检失败：sys_storage_policy 缺少 category=announcement 一行';
    END IF;
    IF policy_row.max_size_mb <> 50 THEN
        RAISE EXCEPTION 'V0086 自检失败：announcement 策略上限应为 50 MB，实际 %', policy_row.max_size_mb;
    END IF;
    IF policy_row.allowed_extensions <> 'jpg,jpeg,png,pdf,docx,xlsx,zip' THEN
        RAISE EXCEPTION 'V0086 自检失败：announcement 扩展名白名单不符，实际 [%]', policy_row.allowed_extensions;
    END IF;
    IF policy_row.status <> 1 THEN
        RAISE EXCEPTION 'V0086 自检失败：announcement 策略应为启用(1)，实际 %', policy_row.status;
    END IF;

    -- 4.2 白名单不得含可执行扩展名 / 不得含点 / 必须小写（ck 只挡点，大小写与黑名单靠这里）
    FOREACH ext IN ARRAY string_to_array(policy_row.allowed_extensions, ',') LOOP
        IF ext <> lower(ext) THEN
            RAISE EXCEPTION 'V0086 自检失败：扩展名 [%] 不是小写', ext;
        END IF;
        IF ext = ANY (dangerous) THEN
            RAISE EXCEPTION 'V0086 自检失败：扩展名白名单含可执行类型 [%]（策略要求禁可执行）', ext;
        END IF;
    END LOOP;

    -- 4.3 5 个配置键齐、值类型与默认值正确
    SELECT count(*) INTO cfg_cnt
      FROM sys_config
     WHERE deleted_at IS NULL
       AND (config_key, config_value, config_type) IN (
            ('announcement.default_expire_days',            '30',    'NUMBER'),
            ('announcement.fanout_enabled',                 'true',  'BOOLEAN'),
            ('announcement.scope_refresh_on_dept_change',   'false', 'BOOLEAN'),
            ('changelog.retention_months',                  '36',    'NUMBER'),
            ('changelog.big_field_bytes',                   '2048',  'NUMBER')
       );
    IF cfg_cnt <> 5 THEN
        RAISE EXCEPTION 'V0086 自检失败：5 个 sys_config 键应全部存在且默认值正确，实际命中 % 个', cfg_cnt;
    END IF;

    -- 4.4 3 个默认帮助分类齐（且父节点为空 = 都是顶级分类）
    SELECT count(*) INTO cat_cnt
      FROM sys_help_category
     WHERE deleted_at IS NULL
       AND code IN ('getting-started', 'faq', 'features')
       AND parent_id IS NULL;
    IF cat_cnt <> 3 THEN
        RAISE EXCEPTION 'V0086 自检失败：默认帮助分类应有 3 条顶级分类，实际 %', cat_cnt;
    END IF;
END
$$;
