-- V0041：导入 / 导出任务表（批次 A · A3 导入导出引擎）
--
-- 依据：docs/批次A-技术架构.md §7 附录 A（V0041 / V0042 原文已在 PostgreSQL 18.6 实跑）
--       PRD docs/批次A-产品需求PRD.md §4.7（任务表与状态机）
--
-- 与架构文档的两处**有意差异**（本文件头明示，避免下游误解）：
--   1. 任务表拆成 sys_import_task / sys_export_task 两张（架构 §7 原文），
--      不采用 PRD §4.7.1 的「单表 + task_kind」——导入与导出的状态机、进度语义、
--      结果列完全不同，拆表后 CHECK 约束能真正生效（导入有 valid+error<=total，
--      导出有 exported_rows）。
--   2. sys_import_task 在架构原文基础上**追加两列**：
--      `file_path`（上传文件的落盘路径，A4 的异步 worker 与本卡的确认入库都要读它）
--      与 `file_sha256`（确认入库前校验文件未被替换）。架构原文的 `file_id` 保留，
--      等 A2 的 storage 登记接入后再回填，两者并存不冲突。
--
-- 错误明细表在 V0042（单独文件，便于按需回滚）。

-- ---------- 1. 导入任务 ----------
CREATE TABLE sys_import_task (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(), -- 主键
    task_no VARCHAR(32) NOT NULL, -- 任务号（IMP20261028-3F2A）
    biz_type VARCHAR(64) NOT NULL, -- 导入定义编码（biz_type，如 system.dict_item）
    biz_name VARCHAR(100) NOT NULL, -- 业务名称快照（定义改名后历史任务仍可读）
    module_code VARCHAR(32) NOT NULL DEFAULT 'business', -- 所属模块
    file_id UUID, -- 上传文件在 sys_file 中的 ID（A2 存储接入后回填）
    file_path VARCHAR(500), -- 上传文件落盘路径（本卡追加）
    file_sha256 VARCHAR(64), -- 上传文件 SHA-256（本卡追加，确认入库前核对）
    file_name VARCHAR(255), -- 原始文件名
    file_size BIGINT, -- 文件字节数
    mode VARCHAR(16) NOT NULL DEFAULT 'STRICT', -- STRICT 全量事务 / PARTIAL 允许部分导入
    status VARCHAR(16) NOT NULL DEFAULT 'PENDING', -- 状态机见 ck_sys_import_task_status
    total_rows INT NOT NULL DEFAULT 0, -- 数据行总数（不含表头/示例行/空行）
    valid_rows INT NOT NULL DEFAULT 0, -- 通过校验行数
    error_rows INT NOT NULL DEFAULT 0, -- 校验失败行数（按行计）
    imported_rows INT NOT NULL DEFAULT 0, -- 实际入库行数
    progress SMALLINT NOT NULL DEFAULT 0, -- 进度百分比 0-100
    error_summary JSONB, -- 前 100 条错误摘要（列表页直接回显）
    options JSONB, -- 导入选项快照（字典翻译开关、冲突策略等）
    result_file_id UUID, -- 「错误行」导出文件 ID
    message VARCHAR(500), -- 失败原因 / 分批说明（如「第 3 批（行 4001–6000）失败」）
    started_at TIMESTAMPTZ, -- 开始处理时间
    finished_at TIMESTAMPTZ, -- 结束时间
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 创建时间
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 更新时间
    create_by UUID, -- 创建人
    update_by UUID, -- 更新人
    deleted_at TIMESTAMPTZ, -- 软删时间
    CONSTRAINT ck_sys_import_task_status CHECK (
        status IN ('PENDING', 'PARSING', 'VALIDATED', 'IMPORTING', 'SUCCESS', 'PARTIAL', 'FAILED', 'CANCELED')),
    CONSTRAINT ck_sys_import_task_mode CHECK (mode IN ('STRICT', 'PARTIAL')),
    CONSTRAINT ck_sys_import_task_progress CHECK (progress BETWEEN 0 AND 100),
    -- 行数自洽：通过 + 失败不超过总数；计数不为负
    CONSTRAINT ck_sys_import_task_rows CHECK (total_rows >= 0 AND valid_rows >= 0 AND error_rows >= 0
                                              AND imported_rows >= 0 AND valid_rows + error_rows <= total_rows)
);

COMMENT ON TABLE sys_import_task IS '导入任务：一次 Excel 导入的全生命周期（解析 → 校验 → 入库），按 biz_type 复用于任意业务';
COMMENT ON COLUMN sys_import_task.biz_type IS '导入定义编码（由业务模块注册，如 system.dict_item）';
COMMENT ON COLUMN sys_import_task.mode IS 'STRICT 全通过才入库 / PARTIAL 跳过错误行入库';
COMMENT ON COLUMN sys_import_task.status IS 'PENDING 待处理 / PARSING 解析中 / VALIDATED 校验完成待确认 / IMPORTING 入库中 / SUCCESS / PARTIAL / FAILED / CANCELED';
COMMENT ON COLUMN sys_import_task.error_summary IS '前 100 条错误摘要（JSONB，列表页直接回显；明细见 sys_import_task_error）';
COMMENT ON COLUMN sys_import_task.file_path IS '上传文件落盘路径（确认入库与 A4 异步 worker 读取）';
COMMENT ON COLUMN sys_import_task.file_sha256 IS '上传文件 SHA-256，确认入库前核对文件未被替换';

CREATE UNIQUE INDEX uk_sys_import_task_no ON sys_import_task (task_no);
CREATE INDEX idx_sys_import_task_claim ON sys_import_task (created_at)
    WHERE status IN ('PENDING', 'VALIDATED', 'IMPORTING');
CREATE INDEX idx_sys_import_task_creator ON sys_import_task (create_by, created_at DESC) WHERE deleted_at IS NULL;
CREATE INDEX idx_sys_import_task_biz ON sys_import_task (biz_type, created_at DESC) WHERE deleted_at IS NULL;

-- ---------- 2. 导出任务 ----------
CREATE TABLE sys_export_task (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(), -- 主键
    task_no VARCHAR(32) NOT NULL, -- 任务号（EXP20261028-9C1D）
    biz_type VARCHAR(64) NOT NULL, -- 导出定义编码
    biz_name VARCHAR(100) NOT NULL, -- 业务名称快照
    module_code VARCHAR(32) NOT NULL DEFAULT 'business', -- 所属模块
    export_name VARCHAR(200) NOT NULL, -- 导出文件名（不含扩展名）
    format VARCHAR(8) NOT NULL DEFAULT 'XLSX', -- XLSX / CSV
    query_params JSONB, -- 过滤条件快照（可复现）
    columns_snapshot JSONB, -- 列定义快照（定义改版后列语义不错位）
    status VARCHAR(16) NOT NULL DEFAULT 'PENDING', -- 状态机
    total_rows BIGINT NOT NULL DEFAULT 0, -- 预计总行数
    exported_rows BIGINT NOT NULL DEFAULT 0, -- 已导出行数
    progress SMALLINT NOT NULL DEFAULT 0, -- 进度百分比
    file_id UUID, -- 产物文件 ID
    file_name VARCHAR(255), -- 产物文件名
    file_size BIGINT, -- 产物字节数
    expire_at TIMESTAMPTZ, -- 下载过期时间（默认 7 天）
    message VARCHAR(500), -- 失败原因
    started_at TIMESTAMPTZ, -- 开始执行时间
    finished_at TIMESTAMPTZ, -- 结束时间
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 创建时间
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 更新时间
    create_by UUID, -- 创建人
    update_by UUID, -- 更新人
    deleted_at TIMESTAMPTZ, -- 软删时间
    CONSTRAINT ck_sys_export_task_status CHECK (
        status IN ('PENDING', 'RUNNING', 'SUCCESS', 'FAILED', 'CANCELED', 'EXPIRED')),
    CONSTRAINT ck_sys_export_task_format CHECK (format IN ('XLSX', 'CSV')),
    CONSTRAINT ck_sys_export_task_progress CHECK (progress BETWEEN 0 AND 100),
    CONSTRAINT ck_sys_export_task_rows CHECK (total_rows >= 0 AND exported_rows >= 0)
);

COMMENT ON TABLE sys_export_task IS '导出任务：大数据量异步导出（小数据量走同步接口，不落表）';
COMMENT ON COLUMN sys_export_task.columns_snapshot IS '导出列定义快照（字段/表头/字典翻译），保证任务完成后列语义不随定义变更而错位';
COMMENT ON COLUMN sys_export_task.expire_at IS '下载有效期截止时间，过期后文件由清理任务回收';
COMMENT ON COLUMN sys_export_task.format IS 'XLSX（≤20 万行）/ CSV（超 20 万行或纯数据交换）';

CREATE UNIQUE INDEX uk_sys_export_task_no ON sys_export_task (task_no);
CREATE INDEX idx_sys_export_task_claim ON sys_export_task (created_at) WHERE status = 'PENDING';
CREATE INDEX idx_sys_export_task_creator ON sys_export_task (create_by, created_at DESC) WHERE deleted_at IS NULL;
CREATE INDEX idx_sys_export_task_expire ON sys_export_task (expire_at) WHERE status = 'SUCCESS';
