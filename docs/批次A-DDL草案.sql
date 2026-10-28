-- ============================================================
-- 云枢 批次 A 通用底座 DDL（V0040–V0045 建表部分，真实 PG 18.6 验证用）
-- ============================================================
-- 说明：验证库内不建 sys_file / sys_user / sys_dept 等底座表的完整定义，
--       只建最小替身（列集合与真实迁移一致），用于验证 ALTER 与扇出 SQL。

-- ---------- 底座替身（仅验证用，真实迁移已存在） ----------
CREATE TABLE sys_user (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    username VARCHAR(50) NOT NULL,
    real_name VARCHAR(50),
    email VARCHAR(100),
    mobile VARCHAR(20),
    dept_id UUID,
    status SMALLINT NOT NULL DEFAULT 1,
    deleted_at TIMESTAMPTZ
);
CREATE TABLE sys_dept (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    dept_code VARCHAR(50) NOT NULL,
    dept_name VARCHAR(100) NOT NULL,
    parent_id UUID,
    dept_path VARCHAR(500),
    status SMALLINT NOT NULL DEFAULT 1,
    deleted_at TIMESTAMPTZ
);
CREATE TABLE sys_role (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    role_code VARCHAR(50) NOT NULL,
    role_name VARCHAR(100) NOT NULL,
    status SMALLINT NOT NULL DEFAULT 1,
    deleted_at TIMESTAMPTZ
);
CREATE TABLE sys_user_role (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES sys_user(id),
    role_id UUID NOT NULL REFERENCES sys_role(id),
    expire_time TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (user_id, role_id)
);
CREATE TABLE sys_file (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    file_name VARCHAR(255) NOT NULL,
    original_name VARCHAR(255),
    file_key VARCHAR(500) NOT NULL,
    file_size BIGINT NOT NULL,
    file_hash VARCHAR(64),
    mime_type VARCHAR(100),
    extension VARCHAR(50),
    storage_path VARCHAR(500),
    category VARCHAR(50),
    status SMALLINT NOT NULL DEFAULT 1,
    uploaded_by UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by UUID,
    update_by UUID,
    deleted_at TIMESTAMPTZ
);
CREATE UNIQUE INDEX uk_sys_file_key ON sys_file (file_key) WHERE deleted_at IS NULL;

-- ============================================================
-- V0040：对象存储后端配置与上传策略
-- ============================================================

CREATE TABLE sys_storage_config (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    config_code VARCHAR(64) NOT NULL,
    config_name VARCHAR(100) NOT NULL,
    backend_type VARCHAR(16) NOT NULL,
    endpoint VARCHAR(255),
    bucket VARCHAR(128),
    region VARCHAR(64) NOT NULL DEFAULT 'us-east-1',
    base_path VARCHAR(255) NOT NULL DEFAULT '',
    use_tls BOOLEAN NOT NULL DEFAULT TRUE,
    path_style BOOLEAN NOT NULL DEFAULT FALSE,
    ak_cipher TEXT,
    ak_nonce VARCHAR(64),
    sk_cipher TEXT,
    sk_nonce VARCHAR(64),
    key_version SMALLINT NOT NULL DEFAULT 1,
    is_default BOOLEAN NOT NULL DEFAULT FALSE,
    status SMALLINT NOT NULL DEFAULT 1,
    health_status VARCHAR(16) NOT NULL DEFAULT 'UNKNOWN',
    last_check_at TIMESTAMPTZ,
    last_error VARCHAR(500),
    sort_order INT NOT NULL DEFAULT 0,
    remark TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by UUID,
    update_by UUID,
    deleted_at TIMESTAMPTZ,
    CONSTRAINT ck_sys_storage_config_backend CHECK (backend_type IN ('LOCAL', 'S3')),
    CONSTRAINT ck_sys_storage_config_health CHECK (health_status IN ('UNKNOWN', 'HEALTHY', 'DOWN')),
    -- 远端后端必须给全 endpoint / bucket / 双密钥密文，本地后端必须为空（密钥不外泄到本地行）
    CONSTRAINT ck_sys_storage_config_remote CHECK (
        (backend_type = 'LOCAL'
            AND endpoint IS NULL AND (bucket IS NULL OR bucket = '')
            AND ak_cipher IS NULL AND sk_cipher IS NULL)
        OR
        (backend_type = 'S3'
            AND endpoint IS NOT NULL AND length(btrim(endpoint)) > 0
            AND bucket IS NOT NULL AND length(btrim(bucket)) > 0
            AND ak_cipher IS NOT NULL AND ak_nonce IS NOT NULL
            AND sk_cipher IS NOT NULL AND sk_nonce IS NOT NULL)
    ),
    CONSTRAINT ck_sys_storage_config_code CHECK (config_code ~ '^[a-z][a-z0-9_-]{1,63}$')
);

COMMENT ON TABLE sys_storage_config IS '存储后端配置：本地磁盘 / S3 兼容（MinIO、AWS S3、阿里 OSS），AK/SK 只存国密级密文';
COMMENT ON COLUMN sys_storage_config.config_code IS '后端编码（小写字母开头，路由键，写入 sys_file.storage_code）';
COMMENT ON COLUMN sys_storage_config.backend_type IS '后端类型：LOCAL 本地磁盘 / S3 S3 兼容对象存储';
COMMENT ON COLUMN sys_storage_config.base_path IS '对象键前缀（如 public/），空串表示桶根';
COMMENT ON COLUMN sys_storage_config.path_style IS '是否 path 风格寻址（MinIO 必须 true，AWS S3/OSS 用 false）';
COMMENT ON COLUMN sys_storage_config.ak_cipher IS 'AccessKey 密文（AES-256-GCM，Base64）';
COMMENT ON COLUMN sys_storage_config.ak_nonce IS 'AccessKey 加密随机数（Base64，每条独立）';
COMMENT ON COLUMN sys_storage_config.sk_cipher IS 'SecretKey 密文（AES-256-GCM，Base64）';
COMMENT ON COLUMN sys_storage_config.sk_nonce IS 'SecretKey 加密随机数（Base64）';
COMMENT ON COLUMN sys_storage_config.key_version IS '主密钥版本号，用于主密钥轮换（旧行保留旧版本即可解密）';
COMMENT ON COLUMN sys_storage_config.health_status IS '连通性探测结果：UNKNOWN / HEALTHY / DOWN';

CREATE UNIQUE INDEX uk_sys_storage_config_code ON sys_storage_config (config_code) WHERE deleted_at IS NULL;
-- 全局最多一个默认后端：把「唯一默认」交给数据库，不依赖应用层判断
CREATE UNIQUE INDEX uk_sys_storage_config_default ON sys_storage_config ((is_default))
    WHERE is_default AND deleted_at IS NULL;
CREATE INDEX idx_sys_storage_config_status ON sys_storage_config (status, sort_order) WHERE deleted_at IS NULL;

CREATE TABLE sys_storage_policy (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    category VARCHAR(50) NOT NULL,
    category_name VARCHAR(100) NOT NULL,
    max_size_mb INT NOT NULL DEFAULT 50,
    allowed_extensions TEXT NOT NULL DEFAULT '',
    allowed_mimes TEXT NOT NULL DEFAULT '',
    allow_overwrite BOOLEAN NOT NULL DEFAULT FALSE,
    direct_upload BOOLEAN NOT NULL DEFAULT FALSE,
    presign_ttl_secs INT NOT NULL DEFAULT 900,
    storage_code VARCHAR(64),
    status SMALLINT NOT NULL DEFAULT 1,
    sort_order INT NOT NULL DEFAULT 0,
    remark TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by UUID,
    update_by UUID,
    deleted_at TIMESTAMPTZ,
    CONSTRAINT ck_sys_storage_policy_size CHECK (max_size_mb BETWEEN 1 AND 5120),
    CONSTRAINT ck_sys_storage_policy_ttl CHECK (presign_ttl_secs BETWEEN 60 AND 604800),
    CONSTRAINT ck_sys_storage_policy_ext CHECK (allowed_extensions NOT LIKE '%.%')
);

COMMENT ON TABLE sys_storage_policy IS '上传策略：按业务分类配置大小上限、扩展名/MIME 白名单、是否直传与覆盖';
COMMENT ON COLUMN sys_storage_policy.category IS '业务分类（与 sys_file.category 一一对应，如 attachment / avatar / import）';
COMMENT ON COLUMN sys_storage_policy.allowed_extensions IS '扩展名白名单，小写逗号分隔且不带点（如 jpg,png,pdf）；空串表示沿用全局默认';
COMMENT ON COLUMN sys_storage_policy.allowed_mimes IS 'MIME 白名单，逗号分隔；空串表示不校验 MIME';
COMMENT ON COLUMN sys_storage_policy.direct_upload IS '是否允许浏览器预签名直传（大文件必开）';
COMMENT ON COLUMN sys_storage_policy.presign_ttl_secs IS '预签名 URL 有效期（秒）';
COMMENT ON COLUMN sys_storage_policy.storage_code IS '指定后端编码（NULL 表示使用默认后端）';

CREATE UNIQUE INDEX uk_sys_storage_policy_category ON sys_storage_policy (category) WHERE deleted_at IS NULL;

-- sys_file 扩展：记录对象落在哪个后端 + 是否直传（底座最小改动，两列均可空、向后兼容）
ALTER TABLE sys_file ADD COLUMN IF NOT EXISTS storage_code VARCHAR(64);
ALTER TABLE sys_file ADD COLUMN IF NOT EXISTS upload_mode VARCHAR(16) NOT NULL DEFAULT 'SERVER';
ALTER TABLE sys_file ADD CONSTRAINT ck_sys_file_upload_mode CHECK (upload_mode IN ('SERVER', 'DIRECT'));
-- 跨后端秒传：同一内容哈希只保留一条有效记录（含它所在后端），命中即复用，不再复制对象
CREATE UNIQUE INDEX uk_sys_file_hash ON sys_file (file_hash)
    WHERE file_hash IS NOT NULL AND deleted_at IS NULL;
CREATE INDEX idx_sys_file_storage_code ON sys_file (storage_code);

-- ============================================================
-- V0041：导入任务 + 行级错误明细
-- ============================================================

CREATE TABLE sys_import_task (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_no VARCHAR(32) NOT NULL,
    biz_type VARCHAR(64) NOT NULL,
    biz_name VARCHAR(100) NOT NULL,
    module_code VARCHAR(32) NOT NULL DEFAULT 'business',
    file_id UUID,
    file_name VARCHAR(255),
    file_size BIGINT,
    mode VARCHAR(16) NOT NULL DEFAULT 'STRICT',
    status VARCHAR(16) NOT NULL DEFAULT 'PENDING',
    total_rows INT NOT NULL DEFAULT 0,
    valid_rows INT NOT NULL DEFAULT 0,
    error_rows INT NOT NULL DEFAULT 0,
    imported_rows INT NOT NULL DEFAULT 0,
    progress SMALLINT NOT NULL DEFAULT 0,
    error_summary JSONB,
    options JSONB,
    result_file_id UUID,
    message VARCHAR(500),
    started_at TIMESTAMPTZ,
    finished_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by UUID,
    update_by UUID,
    deleted_at TIMESTAMPTZ,
    CONSTRAINT ck_sys_import_task_status CHECK (
        status IN ('PENDING', 'PARSING', 'VALIDATED', 'IMPORTING', 'SUCCESS', 'PARTIAL', 'FAILED', 'CANCELED')),
    CONSTRAINT ck_sys_import_task_mode CHECK (mode IN ('STRICT', 'PARTIAL')),
    CONSTRAINT ck_sys_import_task_progress CHECK (progress BETWEEN 0 AND 100),
    CONSTRAINT ck_sys_import_task_rows CHECK (total_rows >= 0 AND valid_rows >= 0 AND error_rows >= 0
                                              AND imported_rows >= 0 AND valid_rows + error_rows <= total_rows)
);

COMMENT ON TABLE sys_import_task IS '导入任务：一次 Excel/CSV 导入的全生命周期（解析 → 校验 → 入库），可按 biz_type 复用于任意业务';
COMMENT ON COLUMN sys_import_task.biz_type IS '导入定义编码（由业务模块注册，如 erp.material / system.user）';
COMMENT ON COLUMN sys_import_task.mode IS 'STRICT 全通过才入库 / PARTIAL 跳过错误行入库';
COMMENT ON COLUMN sys_import_task.status IS 'PENDING 待处理 / PARSING 解析中 / VALIDATED 校验完成待确认 / IMPORTING 入库中 / SUCCESS / PARTIAL 部分成功 / FAILED / CANCELED';
COMMENT ON COLUMN sys_import_task.error_summary IS '前 100 条错误摘要（JSONB，用于列表页直接回显，明细见 sys_import_task_error）';
COMMENT ON COLUMN sys_import_task.options IS '导入选项快照（字典翻译开关、冲突策略等），保证任务可复现';

CREATE UNIQUE INDEX uk_sys_import_task_no ON sys_import_task (task_no);
CREATE INDEX idx_sys_import_task_claim ON sys_import_task (created_at)
    WHERE status IN ('PENDING', 'VALIDATED', 'IMPORTING');
CREATE INDEX idx_sys_import_task_creator ON sys_import_task (create_by, created_at DESC) WHERE deleted_at IS NULL;
CREATE INDEX idx_sys_import_task_biz ON sys_import_task (biz_type, created_at DESC) WHERE deleted_at IS NULL;

CREATE TABLE sys_import_task_error (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_id UUID NOT NULL REFERENCES sys_import_task(id) ON DELETE CASCADE,
    row_no INT NOT NULL,
    column_key VARCHAR(64),
    column_header VARCHAR(100),
    raw_value TEXT,
    error_code VARCHAR(32) NOT NULL,
    error_message VARCHAR(500) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

COMMENT ON TABLE sys_import_task_error IS '导入错误明细：行号 + 列 + 原因，供前端逐行回显与导出「错误行」Excel';
COMMENT ON COLUMN sys_import_task_error.row_no IS 'Excel 行号（含表头，从 1 开始，与用户在 Excel 里看到的行号一致）';
COMMENT ON COLUMN sys_import_task_error.column_key IS '列键（业务定义里的字段名，整行级错误为空）';
COMMENT ON COLUMN sys_import_task_error.error_code IS '错误码：REQUIRED / TYPE / ENUM / DICT / UNIQUE / FK_NOT_FOUND / RULE / ROW_LIMIT';

CREATE INDEX idx_sys_import_task_error_task ON sys_import_task_error (task_id, row_no);

-- ============================================================
-- V0042：导出任务
-- ============================================================

CREATE TABLE sys_export_task (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_no VARCHAR(32) NOT NULL,
    biz_type VARCHAR(64) NOT NULL,
    biz_name VARCHAR(100) NOT NULL,
    module_code VARCHAR(32) NOT NULL DEFAULT 'business',
    export_name VARCHAR(200) NOT NULL,
    format VARCHAR(8) NOT NULL DEFAULT 'XLSX',
    query_params JSONB,
    columns_snapshot JSONB,
    status VARCHAR(16) NOT NULL DEFAULT 'PENDING',
    total_rows BIGINT NOT NULL DEFAULT 0,
    exported_rows BIGINT NOT NULL DEFAULT 0,
    progress SMALLINT NOT NULL DEFAULT 0,
    file_id UUID,
    file_name VARCHAR(255),
    file_size BIGINT,
    expire_at TIMESTAMPTZ,
    message VARCHAR(500),
    started_at TIMESTAMPTZ,
    finished_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by UUID,
    update_by UUID,
    deleted_at TIMESTAMPTZ,
    CONSTRAINT ck_sys_export_task_status CHECK (
        status IN ('PENDING', 'RUNNING', 'SUCCESS', 'FAILED', 'CANCELED', 'EXPIRED')),
    CONSTRAINT ck_sys_export_task_format CHECK (format IN ('XLSX', 'CSV')),
    CONSTRAINT ck_sys_export_task_progress CHECK (progress BETWEEN 0 AND 100),
    CONSTRAINT ck_sys_export_task_rows CHECK (total_rows >= 0 AND exported_rows >= 0)
);

COMMENT ON TABLE sys_export_task IS '导出任务：大数据量异步导出（小数据量走同步接口，不落表）';
COMMENT ON TABLE sys_export_task IS '导出任务：大数据量异步导出（小数据量走同步接口，不落表）';
COMMENT ON COLUMN sys_export_task.columns_snapshot IS '导出列定义快照（字段/表头/字典翻译），保证任务完成后列语义不随定义变更而错位';
COMMENT ON COLUMN sys_export_task.expire_at IS '下载有效期截止时间，过期后文件由清理任务回收';
COMMENT ON COLUMN sys_export_task.format IS 'XLSX（≤20 万行）/ CSV（超 20 万行或纯数据交换）';

CREATE UNIQUE INDEX uk_sys_export_task_no ON sys_export_task (task_no);
CREATE INDEX idx_sys_export_task_claim ON sys_export_task (created_at) WHERE status = 'PENDING';
CREATE INDEX idx_sys_export_task_creator ON sys_export_task (create_by, created_at DESC) WHERE deleted_at IS NULL;
CREATE INDEX idx_sys_export_task_expire ON sys_export_task (expire_at) WHERE status = 'SUCCESS';

-- ============================================================
-- V0043：通知主体 + 模板
-- ============================================================

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
    variables JSONB,
    status SMALLINT NOT NULL DEFAULT 1,
    remark TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by UUID,
    update_by UUID,
    deleted_at TIMESTAMPTZ,
    CONSTRAINT ck_sys_notice_template_format CHECK (content_format IN ('TEXT', 'HTML', 'MARKDOWN')),
    CONSTRAINT ck_sys_notice_template_level CHECK (default_level IN ('INFO', 'SUCCESS', 'WARN', 'ERROR'))
);

COMMENT ON TABLE sys_notice_template IS '通知模板：{{变量}} 占位符 + 变量声明，发送时渲染为最终标题/正文';
COMMENT ON COLUMN sys_notice_template.variables IS '变量声明 [{name,label,required,sample}]，用于模板编辑页校验与示例渲染';
COMMENT ON COLUMN sys_notice_template.content_format IS '正文格式：TEXT 纯文本 / HTML（入库前净化）/ MARKDOWN';

CREATE UNIQUE INDEX uk_sys_notice_template_code ON sys_notice_template (template_code) WHERE deleted_at IS NULL;

CREATE TABLE sys_notice (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    notice_no VARCHAR(32) NOT NULL,
    notice_type VARCHAR(32) NOT NULL DEFAULT 'SYSTEM',
    level VARCHAR(16) NOT NULL DEFAULT 'INFO',
    title VARCHAR(255) NOT NULL,
    content TEXT NOT NULL,
    content_format VARCHAR(8) NOT NULL DEFAULT 'TEXT',
    link_url VARCHAR(500),
    biz_type VARCHAR(64),
    biz_id VARCHAR(64),
    template_code VARCHAR(64),
    template_vars JSONB,
    sender_id UUID,
    sender_name VARCHAR(50),
    target_type VARCHAR(16) NOT NULL,
    target_ids JSONB,
    channel_mask SMALLINT NOT NULL DEFAULT 1,
    receiver_count INT NOT NULL DEFAULT 0,
    read_count INT NOT NULL DEFAULT 0,
    idempotency_key VARCHAR(128),
    status SMALLINT NOT NULL DEFAULT 1,
    expire_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by UUID,
    update_by UUID,
    deleted_at TIMESTAMPTZ,
    CONSTRAINT ck_sys_notice_level CHECK (level IN ('INFO', 'SUCCESS', 'WARN', 'ERROR')),
    CONSTRAINT ck_sys_notice_target CHECK (target_type IN ('USER', 'ROLE', 'DEPT', 'ALL')),
    CONSTRAINT ck_sys_notice_format CHECK (content_format IN ('TEXT', 'HTML', 'MARKDOWN')),
    CONSTRAINT ck_sys_notice_channel CHECK (channel_mask BETWEEN 1 AND 15),
    CONSTRAINT ck_sys_notice_counts CHECK (receiver_count >= 0 AND read_count >= 0 AND read_count <= receiver_count)
);

COMMENT ON TABLE sys_notice IS '通知主体：一条通知 = 一次业务事件，接收人落在 sys_notice_receiver';
COMMENT ON COLUMN sys_notice.target_type IS '发送目标类型：USER 指定用户 / ROLE 角色 / DEPT 部门（含下级）/ ALL 全员';
COMMENT ON COLUMN sys_notice.target_ids IS '目标 ID 数组（UUID 字符串），ALL 时为空数组；仅留痕，实际接收人已扇出到 receiver 表';
COMMENT ON COLUMN sys_notice.channel_mask IS '通道位掩码：1 站内信 / 2 邮件 / 4 Webhook / 8 短信（本批只实现 1，其余留投递表与配置）';
COMMENT ON COLUMN sys_notice.idempotency_key IS '幂等键（业务事件唯一标识），同一键只发一次通知';

CREATE UNIQUE INDEX uk_sys_notice_no ON sys_notice (notice_no);
CREATE UNIQUE INDEX uk_sys_notice_idem ON sys_notice (idempotency_key) WHERE idempotency_key IS NOT NULL;
CREATE INDEX idx_sys_notice_created ON sys_notice (created_at DESC) WHERE deleted_at IS NULL;
CREATE INDEX idx_sys_notice_biz ON sys_notice (biz_type, biz_id) WHERE biz_id IS NOT NULL;

-- ============================================================
-- V0044：通知接收与已读状态
-- ============================================================

CREATE TABLE sys_notice_receiver (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    notice_id UUID NOT NULL REFERENCES sys_notice(id) ON DELETE CASCADE,
    user_id UUID NOT NULL,
    read_flag BOOLEAN NOT NULL DEFAULT FALSE,
    read_at TIMESTAMPTZ,
    is_deleted BOOLEAN NOT NULL DEFAULT FALSE,
    deleted_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT ck_sys_notice_receiver_read CHECK ((read_flag = FALSE AND read_at IS NULL)
                                                  OR (read_flag = TRUE AND read_at IS NOT NULL))
);

COMMENT ON TABLE sys_notice_receiver IS '通知接收人：扇出后的每用户一行，承载已读状态与用户侧删除';
COMMENT ON COLUMN sys_notice_receiver.is_deleted IS '用户侧删除（不影响其他接收人，物理行保留以便统计送达率）';

-- 扇出幂等：同一通知对同一用户只允许一行，重放安全
CREATE UNIQUE INDEX uk_sys_notice_receiver ON sys_notice_receiver (notice_id, user_id);
-- 未读数高频查询：覆盖索引 + 部分索引，只索引未读行
CREATE INDEX idx_sys_notice_receiver_unread ON sys_notice_receiver (user_id, notice_id)
    WHERE read_flag = FALSE AND is_deleted = FALSE;
-- 我的通知列表：按时间倒序分页
CREATE INDEX idx_sys_notice_receiver_list ON sys_notice_receiver (user_id, created_at DESC)
    WHERE is_deleted = FALSE;

-- ============================================================
-- V0045：通知多通道投递（邮件 / Webhook / 短信，本批只落状态不实现发送）
-- ============================================================

CREATE TABLE sys_notice_delivery (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    notice_id UUID NOT NULL REFERENCES sys_notice(id) ON DELETE CASCADE,
    receiver_id UUID REFERENCES sys_notice_receiver(id) ON DELETE CASCADE,
    channel VARCHAR(16) NOT NULL,
    target VARCHAR(255) NOT NULL,
    status VARCHAR(16) NOT NULL DEFAULT 'PENDING',
    attempts SMALLINT NOT NULL DEFAULT 0,
    max_attempts SMALLINT NOT NULL DEFAULT 3,
    next_retry_at TIMESTAMPTZ,
    last_error VARCHAR(500),
    sent_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT ck_sys_notice_delivery_channel CHECK (channel IN ('EMAIL', 'WEBHOOK', 'SMS')),
    CONSTRAINT ck_sys_notice_delivery_status CHECK (
        status IN ('PENDING', 'SENDING', 'SENT', 'FAILED', 'SKIPPED')),
    CONSTRAINT ck_sys_notice_delivery_attempts CHECK (attempts >= 0 AND attempts <= max_attempts)
);

COMMENT ON TABLE sys_notice_delivery IS '外部通道投递记录：本批只预留接口与状态机，不实现真实发送';
COMMENT ON COLUMN sys_notice_delivery.next_retry_at IS '下次重试时间（指数退避：1min / 5min / 25min）';

CREATE UNIQUE INDEX uk_sys_notice_delivery ON sys_notice_delivery (notice_id, COALESCE(receiver_id, notice_id), channel);
CREATE INDEX idx_sys_notice_delivery_retry ON sys_notice_delivery (next_retry_at)
    WHERE status IN ('PENDING', 'FAILED');
