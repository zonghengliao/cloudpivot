-- ============================================================
-- V0194（草案，本卡只出设计，不进 backend/migrations）
--   快照一致性巡检：给 biz_snapshot_drift 补「巡检项编码」+ 只读视图
--
-- 前置：V0193__erp_contract（biz_contract / PO 合同三列 / biz_snapshot_drift）
-- 依赖冲突说明见设计文档 §4.1：若架构裁决不复用 biz_snapshot_drift，
--   改跑同目录的 V0194_alt__biz_data_integrity_issue.up.sql（独立表方案）。
-- ============================================================

BEGIN;

-- ---------- ① 巡检项编码：把「哪个巡检项」变成一等字段 ----------
-- 为什么必须加：field_name 只能表达「哪个字段漂了」，表达不了「哪条巡检项」。
--   DI-03（指向的合同已软删）与 DI-01（供应商快照偏移）都落在 supplier_id/contract 上，
--   但前者是结构缺陷、后者是快照偏移，告警分级与处置动作完全不同。
ALTER TABLE biz_snapshot_drift ADD COLUMN IF NOT EXISTS issue_code VARCHAR(48);

COMMENT ON COLUMN biz_snapshot_drift.issue_code IS
  '巡检项编码（DI-01..DI-13，见 docs/architecture/快照一致性巡检与告警设计.md §2）：告警分级、看板聚合、指标标签都按它分组；历史行可为 NULL';

-- 看板「按巡检项看未决数」与告警聚合的主索引
CREATE INDEX IF NOT EXISTS idx_biz_drift_code_open
    ON biz_snapshot_drift (issue_code, severity) WHERE resolved_at IS NULL;

-- ---------- ①b 唯一键升级：把 issue_code 并入去重键 ----------
-- 为什么必须升级：不同巡检项会落在同一个字段上——DI-01（供应商快照偏移）与 DI-12
--   （供应商主档已删）都看 biz_purchase_order.supplier_id。只按
--   (entity_type, entity_id, field_name) 去重时，后写的巡检项会覆盖先写的，
--   等于「互相改写成对方」，最终漏报一方。
-- V0193 尚未发版 ⇒ 表里没有存量行，索引可无痛替换（有存量时应先回填 issue_code 再建）。
DROP INDEX IF EXISTS uk_biz_drift_open;
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_drift_open
    ON biz_snapshot_drift (issue_code, entity_type, entity_id, field_name)
 WHERE resolved_at IS NULL;

-- ---------- ② 卡面要求的字段口径视图（只读，不新增存储）----------
-- status 是派生的而非存储的：resolution IS NULL = OPEN；REPAIRED/ACCEPTED = RESOLVED；IGNORED = IGNORED。
-- 派生而非存储的理由：两列同时存在必然有一天不一致（谁能保证 resolution 与 status 同步改？）。
CREATE OR REPLACE VIEW v_biz_data_integrity_issue AS
SELECT d.id,
       d.issue_code                                   AS issue_type,
       CASE d.entity_type
           WHEN 'PURCHASE_ORDER' THEN 'biz_purchase_order'
           WHEN 'STOCK_IN'       THEN 'biz_stock_in'
           WHEN 'CONTRACT'       THEN 'biz_contract'
           WHEN 'SUPPLIER'       THEN 'biz_supplier'
           WHEN 'PAYABLE'        THEN 'biz_payable'
           WHEN 'VOUCHER'        THEN 'biz_voucher'
           ELSE lower(d.entity_type)
       END                                            AS biz_table,
       d.entity_id                                    AS biz_id,
       d.entity_no                                    AS biz_no,
       d.field_name,
       d.truth_value                                  AS expected_value,
       d.snapshot_value                               AS actual_value,
       d.truth_source,
       d.drift_kind,
       CASE d.severity WHEN 'ERROR' THEN 'P0' WHEN 'WARN' THEN 'P1' ELSE 'P2' END AS priority,
       d.severity,
       d.affected_count,
       d.detected_at                                  AS first_detected_at,
       d.last_seen_at                                 AS last_detected_at,
       CASE WHEN d.resolved_at IS NULL THEN 'OPEN'
            WHEN d.resolution = 'IGNORED' THEN 'IGNORED'
            ELSE 'RESOLVED' END                       AS status,
       d.resolution,
       d.resolved_by,
       d.resolved_at,
       d.remark
  FROM biz_snapshot_drift d;

COMMENT ON VIEW v_biz_data_integrity_issue IS
  '卡面口径视图：issue_type/biz_table/biz_id/expected_value/actual_value/first_detected_at/last_detected_at/status/resolved_by。OPEN 与 RESOLVED 由 resolution 派生，不落第二列';

COMMIT;
