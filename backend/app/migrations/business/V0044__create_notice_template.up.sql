-- ======================================================
-- V0044：通知模板（{{变量}} 占位符 + 变量声明 + 通道预留）
--
-- 依据：`docs/批次A-技术架构.md` §4.1 / §4.6 与 §7 附录 A（V0043 段 sys_notice_template）。
-- 本卡额外增加 `channels` 列（任务书「通道预留」要求）：模板自带「默认投递通道」清单，
-- 发送时与 `sys_notice.channel_mask` 取交集；未配置的通道由投递记录记 SKIPPED（不报错）。
--
-- 模板唯一性用**部分唯一索引**（软删后可复用同编码），本文件内先行建好，
-- 末尾种子依赖它做 `ON CONFLICT (id) DO NOTHING` 幂等写入。
-- ======================================================

CREATE TABLE sys_notice_template (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    template_code VARCHAR(64) NOT NULL,
    template_name VARCHAR(100) NOT NULL,
    notice_type VARCHAR(32) NOT NULL DEFAULT 'SYSTEM',
    default_level VARCHAR(16) NOT NULL DEFAULT 'INFO',
    title_template VARCHAR(255) NOT NULL,
    content_template TEXT NOT NULL,
    content_format VARCHAR(8) NOT NULL DEFAULT 'TEXT',
    link_template VARCHAR(500),
    channels VARCHAR(64) NOT NULL DEFAULT 'INBOX',
    variables JSONB,
    status SMALLINT NOT NULL DEFAULT 1,
    remark TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by UUID,
    update_by UUID,
    deleted_at TIMESTAMPTZ,
    CONSTRAINT ck_sys_notice_template_format CHECK (content_format IN ('TEXT', 'HTML', 'MARKDOWN')),
    CONSTRAINT ck_sys_notice_template_level CHECK (default_level IN ('INFO', 'SUCCESS', 'WARN', 'ERROR')),
    CONSTRAINT ck_sys_notice_template_code CHECK (template_code ~ '^[A-Za-z][A-Za-z0-9_]{1,63}$')
);

COMMENT ON TABLE sys_notice_template IS '通知模板：{{变量}} 占位符 + 变量声明，发送时渲染为最终标题/正文';
COMMENT ON COLUMN sys_notice_template.template_code IS '模板编码（字母开头，大写下划线约定；软删后可复用）';
COMMENT ON COLUMN sys_notice_template.variables IS '变量声明 [{name,label,required,sample}]，用于模板编辑页校验与示例渲染';
COMMENT ON COLUMN sys_notice_template.content_format IS '正文格式：TEXT 纯文本 / HTML（入库前净化）/ MARKDOWN';
COMMENT ON COLUMN sys_notice_template.channels IS '默认投递通道（逗号分隔：INBOX,EMAIL,WEBHOOK,SMS）；站内信恒开';
COMMENT ON COLUMN sys_notice_template.status IS '状态：1 启用 / 0 停用（停用模板不可用于发送）';

CREATE UNIQUE INDEX uk_sys_notice_template_code ON sys_notice_template (template_code)
    WHERE deleted_at IS NULL;

-- ---------- 默认模板（固定 UUID + ON CONFLICT (id) DO NOTHING：重跑幂等、不覆盖人工改名） ----------
INSERT INTO sys_notice_template (id, template_code, template_name, notice_type, default_level,
                                 title_template, content_template, content_format, link_template,
                                 channels, variables, status, remark)
VALUES
    ('40000000-0000-0000-0000-000000000001'::uuid, 'NOTICE_IMPORT_DONE', '导入完成通知',
     'SYSTEM', 'SUCCESS',
     '导入完成：{{bizName}}',
     '{{bizName}} 导入已完成：成功 {{successRows}} 行，失败 {{failedRows}} 行。',
     'TEXT', '/impexp/import',
     'INBOX',
     '[{"name":"bizName","label":"业务名称","required":true,"sample":"用户导入"},
       {"name":"successRows","label":"成功行数","required":true,"sample":"980"},
       {"name":"failedRows","label":"失败行数","required":false,"sample":"20"}]'::jsonb,
     1, '批次 A 内置模板：导入任务完成时由 notice::dispatch 发送（C/D 批次接入）'),
    ('40000000-0000-0000-0000-000000000002'::uuid, 'NOTICE_EXPORT_DONE', '导出完成通知',
     'SYSTEM', 'SUCCESS',
     '导出完成：{{bizName}}',
     '{{bizName}} 已导出，共 {{totalRows}} 行，下载有效期至 {{expireAt}}。',
     'TEXT', '/impexp/export',
     'INBOX',
     '[{"name":"bizName","label":"业务名称","required":true,"sample":"订单导出"},
       {"name":"totalRows","label":"导出行数","required":true,"sample":"1200"},
       {"name":"expireAt","label":"过期时间","required":true,"sample":"2026-11-04 10:00:00"}]'::jsonb,
     1, '批次 A 内置模板：导出产物就绪后发送'),
    ('40000000-0000-0000-0000-000000000003'::uuid, 'NOTICE_WELCOME', '新用户欢迎通知',
     'SYSTEM', 'INFO',
     '欢迎加入 {{systemName}}',
     '{{realName}} 你好，欢迎使用 {{systemName}}。请尽快修改初始密码。',
     'TEXT', '/user/profile',
     'INBOX',
     '[{"name":"realName","label":"用户姓名","required":true,"sample":"张三"},
       {"name":"systemName","label":"系统名称","required":false,"sample":"云枢"}]'::jsonb,
     1, '批次 A 内置模板：创建用户后的欢迎消息')
ON CONFLICT (id) DO NOTHING;
