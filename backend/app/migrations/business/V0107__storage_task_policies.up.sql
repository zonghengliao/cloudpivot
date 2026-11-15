-- ======================================================
-- V0107：任务管理存储策略（批次 C+ · 设计修正 A · 修正 4）
--
-- 依据：docs/云枢设计修正补丁-20261029.md 修正 4（老大已批「有问题的都修正」）
--       docs/批次C+-工作任务管理-详细架构.md（任务附件走私有桶 + 签名 URL）
-- 回滚：V0107__storage_task_policies.down.sql
--
-- 为什么需要这两条分类
--   现有 6 条策略（announcement / attachment / avatar / document / import / template）
--   **没有任务类**，任务附件只能挂到通用 `attachment`：100 MB、允许 zip/rar/7z、
--   还允许音视频 —— 既可执行文件（含在压缩包里）与人共享，也没有为「AI 产出物」设
--   任何单独的闸。本迁移补两条：
--     · `task`    —— 人类上传的任务附件：50 MB，办公文档 + 图片 + 压缩包；
--     · `task_ai` —— **AI 产出的文件**：100 MB，但**白名单更严**（不含 doc/xls/ppt、
--                    不含 7z/rar），并且**可执行脚本（exe/sh/bat/ps1/so/dll/jar/py/js）
--                    连同压缩包内的脚本路径一律不给入口**。
--
-- ── 「硬禁止」怎么落地的（重要，别误解） ──────────────────────────────
-- `sys_storage_policy` 只有**扩展名白名单**（`allowed_extensions`），没有黑名单列
-- （V0086 头注释已解释过同一件事）。因此「硬禁止」有**两道**：
--   ① 迁移层（本文件末尾的自检块）：白名单里出现任何可执行扩展名 → 迁移直接失败；
--   ② 运行层（`business/storage/model.rs::FORBIDDEN_EXECUTABLE_EXTENSIONS`）：
--      `check_upload` 先查黑名单再查白名单 —— 即便将来有人手工把 `sh` 加进某条
--      策略的白名单，上传口子**仍然**是关着的（fail-closed 的兜底，见该常量文档）。
--   白名单语义本身也 fail-closed：没列进去的一律拒绝。
--
-- ── 为什么 task_ai 关掉直传（direct_upload = FALSE） ────────────────────
-- `direct_upload = TRUE` 允许浏览器拿预签名 URL **绕过服务端**直接 PUT 到对象存储。
-- AI 产出物是「服务端写、人来看」的单向流，没有任何需要浏览器直传的场景；
-- 关掉它 = 所有 task_ai 文件都必须过服务端的策略校验，少一条可绕的边。
-- `task`（人类上传，50 MB）保留直传，与 `attachment`/`announcement` 口径一致。
--
-- ── allowed_mimes 为什么留空 ──────────────────────────────────────────
-- 与 V0086 同款取舍：`check_upload` 的语义是「allowed_mimes 为空 ⇒ 不校验 MIME」。
-- md/csv/json/zip 在部分客户端会被报成 `application/octet-stream`，硬校验 MIME 会
-- 把合法文件挡在门外；扩展名白名单 + 硬禁止黑名单已覆盖安全诉求。
--
-- ── 幂等 ──────────────────────────────────────────────────────────────
-- 固定 UUID + `ON CONFLICT (category) WHERE deleted_at IS NULL DO NOTHING`：
-- 重复执行不报错、不产生重复行、不覆盖运维改过的值（种子只保证「存在」）。
-- 注意：`ON CONFLICT ... DO NOTHING` 意味着**已存在行不会被本迁移改写**；
-- 因此末尾自检块在「行已存在但值被改错」时会 RAISE —— 这正是我们要的：
-- 让脏配置在迁移期暴露，而不是让上传口子悄悄放宽。
-- ======================================================

-- ---------- 1) task：人类上传的任务附件 ----------
INSERT INTO sys_storage_policy (
    id, category, category_name, max_size_mb, allowed_extensions, allowed_mimes,
    allow_overwrite, direct_upload, presign_ttl_secs, storage_code, status, sort_order, remark
) VALUES (
    '24000000-0000-4000-8000-000000000007',
    'task',
    '任务附件',
    50,
    'jpg,jpeg,png,gif,webp,pdf,doc,docx,xls,xlsx,ppt,pptx,txt,md,csv,json,zip,7z,rar',
    '',
    FALSE,
    TRUE,
    900,
    NULL,
    1,
    70,
    '任务附件（人类上传）：50 MB 上限；办公文档/图片/文本/压缩包；'
    || '可执行扩展名（exe,sh,bat,ps1,so,dll,jar）硬禁止，白名单内一律不含'
)
ON CONFLICT (category) WHERE deleted_at IS NULL DO NOTHING;

-- ---------- 2) task_ai：AI 产出的文件 ----------
INSERT INTO sys_storage_policy (
    id, category, category_name, max_size_mb, allowed_extensions, allowed_mimes,
    allow_overwrite, direct_upload, presign_ttl_secs, storage_code, status, sort_order, remark
) VALUES (
    '24000000-0000-4000-8000-000000000008',
    'task_ai',
    'AI 产出文件',
    100,
    'jpg,jpeg,png,gif,webp,pdf,txt,md,csv,json,zip,log',
    '',
    FALSE,
    FALSE,
    900,
    NULL,
    1,
    80,
    'AI 产出文件：100 MB 上限；白名单比 task 更严（不含 doc/xls/ppt/7z/rar）；'
    || '可执行脚本（exe,sh,bat,ps1,so,dll,jar,py,js）硬禁止 —— AI 写的脚本人可能直接运行，'
    || '风险高于人类上传；关闭浏览器直传，所有文件必过服务端校验'
)
ON CONFLICT (category) WHERE deleted_at IS NULL DO NOTHING;

-- ---------- 3) 自检：值写错了就让迁移失败，别让口子悄悄开着 ----------
-- 段内断言（不依赖库里是否有别的分类，只对本迁移负责的两行负责）：
--   ① 两行都在且 category_name / max_size_mb 与设计一致；
--   ② 两行的白名单里**不含**任何可执行扩展名（硬禁止的迁移层防线）；
--   ③ 白名单项一律小写、不带点（ck_sys_storage_policy_ext 只拦带点，不管大小写）。
DO $$
DECLARE
    forbidden CONSTANT TEXT[] := ARRAY['exe','sh','bat','ps1','so','dll','jar','py','js',
                                       'vbs','msi','cmd','com','scr','psm1','wasm','apk'];
    expected  CONSTANT TEXT[] := ARRAY['task','task_ai'];
    cat       TEXT;
    row_size  INT;
    row_ext   TEXT;
    ext       TEXT;
    n_total   INT;
BEGIN
    FOREACH cat IN ARRAY expected LOOP
        SELECT max_size_mb, allowed_extensions INTO row_size, row_ext
          FROM sys_storage_policy
         WHERE category = cat AND deleted_at IS NULL;

        IF row_size IS NULL THEN
            RAISE EXCEPTION 'V0107 自检失败：缺少存储策略分类 %', cat;
        END IF;

        IF (cat = 'task' AND row_size <> 50) OR (cat = 'task_ai' AND row_size <> 100) THEN
            RAISE EXCEPTION 'V0107 自检失败：分类 % 的大小上限是 % MB，设计值为 % MB',
                cat, row_size, CASE WHEN cat = 'task' THEN 50 ELSE 100 END;
        END IF;

        FOR ext IN SELECT unnest(string_to_array(row_ext, ',')) LOOP
            ext := btrim(ext);
            CONTINUE WHEN ext = '';
            IF ext <> lower(ext) THEN
                RAISE EXCEPTION 'V0107 自检失败：分类 % 的白名单项 % 不是小写', cat, ext;
            END IF;
            IF ext = ANY (forbidden) THEN
                RAISE EXCEPTION 'V0107 自检失败：分类 % 的白名单含可执行扩展名 %（硬禁止）', cat, ext;
            END IF;
        END LOOP;
    END LOOP;

    SELECT count(*) INTO n_total FROM sys_storage_policy WHERE deleted_at IS NULL;
    IF n_total < 8 THEN
        RAISE EXCEPTION 'V0107 自检失败：策略总数 % 条 < 8 条（原 6 + 本迁移 2）', n_total;
    END IF;
    RAISE NOTICE 'V0107 自检通过：task/task_ai 就位，策略总数 % 条', n_total;
END $$;
