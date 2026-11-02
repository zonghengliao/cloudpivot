-- ============================================================
-- V0195：一致性巡检结果表 biz_snapshot_drift（ARCH-2 汇总定稿 · 第三处关注点）
--
-- 口径来源：docs/architecture/合同与勾稽关系设计.md §7.3 / §5.3
-- 设计来源：
--   · 表骨架 = docs/evidence/2026-11-02-arch2-contract/V0193__erp_contract.up.sql §4（草稿）
--   · issue_code 与唯一键形状 = docs/architecture/snapshot-drift/V0194__biz_snapshot_drift_issue_code.up.sql
--     （巡检卡 t_c7610b31 的增强草案）
--
-- 汇总卡的收敛动作（为什么是「一次建对」而不是「先建后补」）：
--   草稿把 biz_snapshot_drift 与合同表放在同一个 V0193；巡检卡只能在表已存在的前提下
--   用 ALTER + DROP INDEX/CREATE INDEX 补 issue_code 并升级唯一键。三张卡已定「一迁移一关注点」，
--   漂移表独立成 V0195 ⇒ issue_code 直接内建，不需要 DROP/CREATE INDEX 的窗口期，
--   也不会出现「V0194 已建表但唯一键还是旧形状」的中间态。
--
-- 不建外键（同链弱引用基线）、不建触发器（全库 0 触发器）。
-- ============================================================

CREATE TABLE IF NOT EXISTS biz_snapshot_drift (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    issue_code     VARCHAR(48) NOT NULL,          -- DI-01 … DI-15：哪一条巡检项
    entity_type    VARCHAR(32) NOT NULL,          -- CONTRACT / SUPPLIER / PURCHASE_ORDER / STOCK_IN / PAYABLE / SYSTEM
    entity_id      UUID        NOT NULL,
    entity_no      VARCHAR(64) NOT NULL,          -- 单据号/编码（冗余存一份：业务行软删后日志仍可读）
    field_name     VARCHAR(32) NOT NULL,          -- party_b_id / supplier_id / contract_id / source_id / constraint
    drift_kind     VARCHAR(16) NOT NULL,          -- SNAPSHOT 合法业务变更 / DEFECT 数据缺陷
    severity       VARCHAR(8)  NOT NULL DEFAULT 'WARN',   -- ERROR=P0 / WARN=P1 / INFO=只留痕
    snapshot_value TEXT,                          -- TEXT 而非 UUID：漂移的不止 id（还有名称、金额、状态串）
    truth_value    TEXT,
    truth_source   VARCHAR(64),                   -- 'biz_contract.party_b_id' 之类：说清「真相在哪」
    affected_count INT         NOT NULL DEFAULT 1,-- SNAPSHOT 挂对象粒度时的受影响单据数（禁行粒度灌表）
    detected_at    TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- 首次发现时间（不随巡检刷新）
    last_seen_at   TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- 最近一次仍然存在的时间（收敛判据）
    detected_by    VARCHAR(32) NOT NULL DEFAULT 'JOB',
    resolution     VARCHAR(16),                   -- REPAIRED 已修复 / ACCEPTED 认可（业务变更）/ IGNORED 忽略
    resolved_at    TIMESTAMPTZ,
    resolved_by    UUID,
    remark         TEXT,

    CONSTRAINT ck_biz_drift_code    CHECK (issue_code ~ '^DI-[0-9]{2}$'),
    CONSTRAINT ck_biz_drift_kind    CHECK (drift_kind IN ('SNAPSHOT','DEFECT')),
    CONSTRAINT ck_biz_drift_sev     CHECK (severity IN ('INFO','WARN','ERROR')),
    CONSTRAINT ck_biz_drift_res     CHECK (resolution IS NULL OR resolution IN ('REPAIRED','ACCEPTED','IGNORED')),
    CONSTRAINT ck_biz_drift_res_at  CHECK ((resolution IS NULL) = (resolved_at IS NULL)),
    CONSTRAINT ck_biz_drift_by      CHECK (detected_by IN ('JOB','MANUAL','MIGRATION')),
    CONSTRAINT ck_biz_drift_affected CHECK (affected_count > 0)
);

COMMENT ON TABLE biz_snapshot_drift IS
  '单据一致性巡检结果：drift_kind=SNAPSHOT 是**合法业务变更**（合同换乙方/供应商改名），DEFECT 是**数据缺陷**（拷贝没跟上游）。前者只展示+可 ACCEPTED，后者必须 REPAIRED';
COMMENT ON COLUMN biz_snapshot_drift.issue_code IS
  '巡检项编码 DI-01..DI-15：field_name 只能表达「哪个字段漂了」，表达不了「哪条巡检项」（DI-01 与 DI-12 都看 supplier_id）';
COMMENT ON COLUMN biz_snapshot_drift.entity_type IS
  'SNAPSHOT 挂「真相所在对象」（CONTRACT/SUPPLIER，一条记录代表一批受影响单据，用 affected_count 表达）；DEFECT 挂具体单据行';
COMMENT ON COLUMN biz_snapshot_drift.affected_count IS
  '受影响的单据数。实测：一次合同换乙方影响 20000 张采购单 —— 行粒度落表会淹掉运维，真缺陷只有 0 行';

-- 幂等：同一个「未解决的同项漂移」只留一条（巡检用 ON CONFLICT ... WHERE resolved_at IS NULL）
-- 唯一键必须含 issue_code：DI-01 与 DI-12 都看 biz_purchase_order.supplier_id，
-- 只按 (entity_type, entity_id, field_name) 去重会互相覆盖 → 必有一方永远漏报
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_drift_open
    ON biz_snapshot_drift (issue_code, entity_type, entity_id, field_name) WHERE resolved_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_drift_code_open
    ON biz_snapshot_drift (issue_code, severity) WHERE resolved_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_drift_open_time
    ON biz_snapshot_drift (detected_at DESC) WHERE resolved_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_drift_entity
    ON biz_snapshot_drift (entity_type, entity_id, detected_at DESC);

-- 卡面口径视图：OPEN / RESOLVED / IGNORED 由 resolution 派生，不落第二列
-- （落两列必然有一天不一致：谁能保证两处同步改？）
CREATE OR REPLACE VIEW v_biz_data_integrity_issue AS
SELECT d.id,
       d.issue_code                            AS issue_type,
       CASE d.entity_type
           WHEN 'PURCHASE_ORDER' THEN 'biz_purchase_order'
           WHEN 'STOCK_IN'       THEN 'biz_stock_in'
           WHEN 'CONTRACT'       THEN 'biz_contract'
           WHEN 'SUPPLIER'       THEN 'biz_supplier'
           WHEN 'PAYABLE'        THEN 'biz_payable'
           ELSE lower(d.entity_type) END       AS biz_table,
       d.entity_id                             AS biz_id,
       d.entity_no                             AS biz_no,
       d.field_name,
       d.truth_value                           AS expected_value,
       d.snapshot_value                        AS actual_value,
       d.truth_source,
       d.drift_kind,
       CASE d.severity WHEN 'ERROR' THEN 'P0' WHEN 'WARN' THEN 'P1' ELSE 'P2' END AS priority,
       d.severity, d.affected_count,
       d.detected_at                           AS first_detected_at,
       d.last_seen_at                          AS last_detected_at,
       CASE WHEN d.resolved_at IS NULL THEN 'OPEN'
            WHEN d.resolution = 'IGNORED' THEN 'IGNORED'
            ELSE 'RESOLVED' END                AS status,
       d.resolution, d.resolved_by, d.resolved_at, d.remark
  FROM biz_snapshot_drift d;

COMMENT ON VIEW v_biz_data_integrity_issue IS
  '巡检结果对卡面口径的映射：priority P0/P1/P2 由 severity 派生，status OPEN/RESOLVED/IGNORED 由 resolution 派生（视图派生，不落第二列）';

-- 结构自检
DO $$
DECLARE n_col INT; n_chk INT; n_idx INT;
BEGIN
    SELECT count(*) INTO n_col FROM information_schema.columns
     WHERE table_schema='public' AND table_name='biz_snapshot_drift';
    SELECT count(*) INTO n_chk FROM pg_constraint
     WHERE conrelid='public.biz_snapshot_drift'::regclass AND contype='c';
    SELECT count(*) INTO n_idx FROM pg_indexes
     WHERE schemaname='public' AND tablename='biz_snapshot_drift';
    IF n_col <> 19 THEN RAISE EXCEPTION 'biz_snapshot_drift 列数 %，期望 19', n_col; END IF;
    IF n_chk <> 7  THEN RAISE EXCEPTION 'biz_snapshot_drift CHECK 数 %，期望 7', n_chk; END IF;
    IF n_idx <> 5  THEN RAISE EXCEPTION 'biz_snapshot_drift 索引数 %（含 PK），期望 5', n_idx; END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_indexes
                    WHERE schemaname='public' AND indexname='uk_biz_drift_open'
                      AND indexdef LIKE '%issue_code%') THEN
        RAISE EXCEPTION 'uk_biz_drift_open 未包含 issue_code（DI-01 与 DI-12 会互相覆盖）';
    END IF;
    RAISE NOTICE 'V0195 自检通过：biz_snapshot_drift % 列 / % CHECK / % 索引（含 PK），唯一键含 issue_code', n_col, n_chk, n_idx;
END
$$;
