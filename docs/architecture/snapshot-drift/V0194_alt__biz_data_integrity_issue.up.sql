-- ============================================================
-- V0194-ALT（备选方案，草案）：独立表 biz_data_integrity_issue
--   仅当架构裁决「不复用 biz_snapshot_drift」时启用；与 V0194 二选一，禁止并存。
--   并存 = 同一事实两处维护 = 本设计要消灭的东西。
-- ============================================================

BEGIN;

CREATE TABLE IF NOT EXISTS biz_data_integrity_issue (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    issue_type        VARCHAR(48)  NOT NULL,             -- 巡检项编码 DI-01..DI-13
    biz_table         VARCHAR(64)  NOT NULL,             -- 缺陷主体表名（biz_purchase_order / biz_stock_in / …）
    biz_id            UUID         NOT NULL,             -- 主体行 id
    biz_no            VARCHAR(64)  NOT NULL,             -- 单号（行软删后仍可读）
    field_name        VARCHAR(32),                       -- 漂移字段（结构类缺陷为 NULL）
    expected_value    TEXT,                              -- 真相值
    actual_value      TEXT,                              -- 快照/实际值
    truth_source      VARCHAR(64),                       -- 真相在哪：'biz_contract.party_b_id'
    drift_kind        VARCHAR(16)  NOT NULL DEFAULT 'DEFECT',
    severity          VARCHAR(8)   NOT NULL DEFAULT 'WARN',
    affected_count    INT          NOT NULL DEFAULT 1,
    first_detected_at TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    last_detected_at  TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    status            VARCHAR(16)  NOT NULL DEFAULT 'OPEN',
    resolved_by       UUID,
    resolved_at       TIMESTAMPTZ,
    remark            TEXT,

    CONSTRAINT ck_dii_status   CHECK (status IN ('OPEN','RESOLVED','IGNORED')),
    CONSTRAINT ck_dii_kind     CHECK (drift_kind IN ('SNAPSHOT','DEFECT')),
    CONSTRAINT ck_dii_sev      CHECK (severity IN ('INFO','WARN','ERROR')),
    CONSTRAINT ck_dii_affected CHECK (affected_count > 0),
    -- 已处置必有处置人与时间；未处置两者必须为空（三值逻辑用 CASE 写，不用 A OR (B AND C)）
    CONSTRAINT ck_dii_resolved CHECK (CASE WHEN status = 'OPEN'
                                           THEN resolved_at IS NULL AND resolved_by IS NULL
                                           ELSE resolved_at IS NOT NULL END)
);

CREATE UNIQUE INDEX IF NOT EXISTS uk_dii_open
    ON biz_data_integrity_issue (issue_type, biz_table, biz_id, coalesce(field_name, '')) WHERE status = 'OPEN';
CREATE INDEX IF NOT EXISTS idx_dii_open_time ON biz_data_integrity_issue (first_detected_at DESC) WHERE status = 'OPEN';

COMMIT;
