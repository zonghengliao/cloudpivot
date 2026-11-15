-- V0040：对象存储后端配置与上传策略（批次 A · A1 存储底座）
--
-- 依据：docs/批次A-技术架构.md §7 附录 A · V0040
--       （同一份 DDL 已在 PostgreSQL 18.6 实跑，28 条断言全 PASS，见 §8.1）
--
-- 本迁移建立三件事：
--   1. sys_storage_config —— 多后端配置（本地 / S3 兼容），AK/SK 只存密文；
--   2. sys_storage_policy —— 按业务分类的上传策略（大小 / 扩展名 / MIME / 直传 / TTL）；
--   3. sys_file 扩展两列 —— 记录文件落在哪个后端（storage_code）与上传方式（upload_mode），
--      两列均可空/有默认，历史数据零迁移（storage_code IS NULL 视为本地后端）。

-- ---------- 1. 存储后端配置 ----------
CREATE TABLE sys_storage_config (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(), -- 主键
    config_code VARCHAR(64) NOT NULL, -- 后端编码（路由键，写入 sys_file.storage_code）
    config_name VARCHAR(100) NOT NULL, -- 后端名称（展示用）
    backend_type VARCHAR(16) NOT NULL, -- 后端类型：LOCAL 本地磁盘 / S3 兼容对象存储
    endpoint VARCHAR(255), -- 端点（S3 必填，LOCAL 必须为空）
    bucket VARCHAR(128), -- 桶名（S3 必填）
    region VARCHAR(64) NOT NULL DEFAULT 'us-east-1', -- 区域（S3 签名用）
    base_path VARCHAR(255) NOT NULL DEFAULT '', -- 对象键前缀（空串表示桶根 / 上传根）
    use_tls BOOLEAN NOT NULL DEFAULT TRUE, -- 是否使用 TLS
    path_style BOOLEAN NOT NULL DEFAULT FALSE, -- 是否 path 风格寻址（MinIO 必须 true）
    ak_cipher TEXT, -- AccessKey 密文（AES-256-GCM，Base64）
    ak_nonce VARCHAR(64), -- AccessKey 加密随机数（Base64）
    sk_cipher TEXT, -- SecretKey 密文（AES-256-GCM，Base64）
    sk_nonce VARCHAR(64), -- SecretKey 加密随机数（Base64）
    key_version SMALLINT NOT NULL DEFAULT 1, -- 主密钥版本（轮换用）
    is_default BOOLEAN NOT NULL DEFAULT FALSE, -- 是否默认后端（全库最多一个）
    status SMALLINT NOT NULL DEFAULT 1, -- 状态：1 启用 / 0 停用
    health_status VARCHAR(16) NOT NULL DEFAULT 'UNKNOWN', -- 连通性探测结果
    last_check_at TIMESTAMPTZ, -- 最近一次探测时间
    last_error VARCHAR(500), -- 最近一次探测失败原因（脱敏）
    sort_order INT NOT NULL DEFAULT 0, -- 排序号
    remark TEXT, -- 备注
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 创建时间
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 更新时间
    create_by UUID, -- 创建人 ID
    update_by UUID, -- 更新人 ID
    deleted_at TIMESTAMPTZ, -- 软删时间（非空即已删除）
    CONSTRAINT ck_sys_storage_config_backend CHECK (backend_type IN ('LOCAL', 'S3')),
    CONSTRAINT ck_sys_storage_config_health CHECK (health_status IN ('UNKNOWN', 'HEALTHY', 'DOWN')),
    -- 远端后端必须给全 endpoint / bucket / 双密钥密文；本地后端必须为空（密钥不外泄到本地行）
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

-- ---------- 2. 上传策略 ----------
CREATE TABLE sys_storage_policy (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(), -- 主键
    category VARCHAR(50) NOT NULL, -- 业务分类（与 sys_file.category 对应）
    category_name VARCHAR(100) NOT NULL, -- 分类名称
    max_size_mb INT NOT NULL DEFAULT 50, -- 单文件大小上限（MB）
    allowed_extensions TEXT NOT NULL DEFAULT '', -- 扩展名白名单（小写逗号分隔、不带点）
    allowed_mimes TEXT NOT NULL DEFAULT '', -- MIME 白名单（逗号分隔，空串表示不校验）
    allow_overwrite BOOLEAN NOT NULL DEFAULT FALSE, -- 是否允许覆盖同名对象
    direct_upload BOOLEAN NOT NULL DEFAULT FALSE, -- 是否允许浏览器预签名直传
    presign_ttl_secs INT NOT NULL DEFAULT 900, -- 预签名 URL 有效期（秒）
    storage_code VARCHAR(64), -- 指定后端编码（NULL 表示使用默认后端）
    status SMALLINT NOT NULL DEFAULT 1, -- 状态：1 启用 / 0 停用
    sort_order INT NOT NULL DEFAULT 0, -- 排序号
    remark TEXT, -- 备注
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 创建时间
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 更新时间
    create_by UUID, -- 创建人 ID
    update_by UUID, -- 更新人 ID
    deleted_at TIMESTAMPTZ, -- 软删时间
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

-- ---------- 3. sys_file 扩展（两列均可空/有默认，向后兼容） ----------
ALTER TABLE sys_file ADD COLUMN IF NOT EXISTS storage_code VARCHAR(64);
ALTER TABLE sys_file ADD COLUMN IF NOT EXISTS upload_mode VARCHAR(16) NOT NULL DEFAULT 'SERVER';

-- 约束不带 IF NOT EXISTS，用 DO 块保证「重复执行不报错」（up → down → up 可重放）
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'ck_sys_file_upload_mode'
    ) THEN
        ALTER TABLE sys_file ADD CONSTRAINT ck_sys_file_upload_mode
            CHECK (upload_mode IN ('SERVER', 'DIRECT'));
    END IF;
END $$;

-- 跨后端秒传：同一内容哈希只保留一条有效记录（含它所在后端），命中即复用，不再复制对象
CREATE UNIQUE INDEX IF NOT EXISTS uk_sys_file_hash ON sys_file (file_hash)
    WHERE file_hash IS NOT NULL AND deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_sys_file_storage_code ON sys_file (storage_code);
