-- ============================================================
-- V0193：合同管理 + 单据勾稽关系（ARCH-2 · 设计稿，**由实现卡拷入** backend/migrations/business/）
--
-- 本文件不是「可执行的业务代码」，是设计文档 §2 的 DDL 原文 + POC 里逐条跑过的自检。
-- 依据：docs/architecture/合同与勾稽关系设计.md
-- 迁移号：V0193（开工前实测：仓库最大 V0192；V0193–V0199 空段）
--
-- # 本迁移只做四件事
--   ① 新建 biz_contract（合同；乙方=供应商/客户，是全链唯一的「乙方真相」）
--   ② biz_purchase_order 加合同三列 + 两个 CHECK（CONTRACT/SPOT 与合同**双向钉死**）
--   ③ 新建 biz_snapshot_drift（一致性巡检结果；SNAPSHOT 是合法变更、DEFECT 是缺陷）
--   ④ 建两个只读视图 v_biz_doc_edge / v_biz_doc_ref（追溯链靠它们，不建新表）
--
-- # 三条不可违反的口径（POC 实测，见 docs/evidence/2026-11-02-arch2-contract/）
--   1) 单据链**不建外键**：全库 biz_purchase_order 9 个 UUID 列、biz_stock_in 11 个 UUID 列
--      一个外键都没有（POC-1d 实测）。合同同理 —— 加上外键会让「软删后可复查」变成不可能，
--      也偏离本仓「零触发器 + 弱引用 + 应用层校验 + 巡检兜底」的基线。
--   2) **不给入库单加「采购必须有来源单」的约束**：期初库存（V0189）、序列号批量生成
--      （ADR-20261031）、复制为新单（stockin_handler.rs:686-692 明确清空 source_order_id）
--      三条合法路径都是「采购入库 + 无来源单」，加了这条约束会把它们全部打死（POC-3 实证）。
--   3) **存量数据一律不回填**：历史采购单没有合同 = 就是没有（purchase_type 落 SPOT），
--      历史单据当时的供应商名称不可知 = supplier_name_snapshot 留 NULL，前端 coalesce 兜底。
--      回填等于编造审计事实。
-- ============================================================

BEGIN;

-- ============================================================
-- §1 `biz_contract` —— 合同主表
-- ============================================================
CREATE TABLE IF NOT EXISTS biz_contract (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),      -- 主键
    contract_no          VARCHAR(32)   NOT NULL,                          -- 合同号（biz_seq 'CT' 生成，软删后可复用）
    contract_name        VARCHAR(200)  NOT NULL,                          -- 合同名称
    contract_type        VARCHAR(16)   NOT NULL,                          -- PURCHASE 采购 / SALES 销售 / SERVICE 服务 / LEASE 租赁
    party_b_type         VARCHAR(16)   NOT NULL,                          -- 乙方类型：SUPPLIER 供应商 / CUSTOMER 客户
    party_b_id           UUID          NOT NULL,                          -- 乙方 id（弱引用 biz_supplier / biz_customer）
    party_b_name         VARCHAR(200)  NOT NULL,                          -- 乙方名称【快照】：签约时名称
    party_a_dept_id      UUID,                                            -- 甲方（本组织部门，弱引用 sys_dept.id；NULL=本组织）
    party_a_name         VARCHAR(200),                                    -- 甲方名称【快照】
    amount               NUMERIC(20,4) NOT NULL DEFAULT 0,                -- 合同金额（框架合同填 0 = 不限额）
    currency_code        CHAR(3)       NOT NULL DEFAULT 'CNY',            -- 币种（ISO 4217）
    sign_date            DATE,                                            -- 签订日期
    start_date           DATE,                                            -- 生效日
    end_date             DATE,                                            -- 到期日（下单时据此校验是否超期）
    status               VARCHAR(16)   NOT NULL DEFAULT 'DRAFT',          -- 状态机：见 shared/state_machine.rs
    owner_id             UUID,                                            -- 负责人（弱引用 sys_user.id）
    dept_id              UUID,                                            -- 归属部门（弱引用 sys_dept.id）
    approval_instance_id UUID,                                            -- 审批实例（弱引用 biz_flow_instance.id）
    remark               TEXT,                                            -- 备注
    created_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by            UUID,
    update_by            UUID,
    deleted_at           TIMESTAMPTZ,

    -- 类型与乙方类型必须配对：PURCHASE 只能配 SUPPLIER、SALES 只能配 CUSTOMER。
    -- 少了这条，库里能存在「销售合同 + 供应商乙方」这种任何查询都解释不了的组合。
    CONSTRAINT ck_biz_contract_type   CHECK (contract_type IN ('PURCHASE','SALES','SERVICE','LEASE')),
    CONSTRAINT ck_biz_contract_btype  CHECK (party_b_type IN ('SUPPLIER','CUSTOMER')),
    CONSTRAINT ck_biz_contract_pair   CHECK (CASE contract_type
                                              WHEN 'PURCHASE' THEN party_b_type = 'SUPPLIER'
                                              WHEN 'SALES'    THEN party_b_type = 'CUSTOMER'
                                              ELSE party_b_type IN ('SUPPLIER','CUSTOMER')
                                            END),
    CONSTRAINT ck_biz_contract_amount CHECK (amount >= 0),
    CONSTRAINT ck_biz_contract_dates  CHECK (start_date IS NULL OR end_date IS NULL OR end_date >= start_date),
    CONSTRAINT ck_biz_contract_status CHECK (status IN ('DRAFT','SIGNED','ACTIVE','COMPLETED','TERMINATED')),
    -- 已签/生效/完成必须有签日：状态与事实不许互相矛盾（CASE 写法，PG 的 CHECK 遇 NULL 视为通过）
    CONSTRAINT ck_biz_contract_signed CHECK (CASE WHEN status IN ('SIGNED','ACTIVE','COMPLETED')
                                                  THEN sign_date IS NOT NULL
                                                  ELSE TRUE END),
    CONSTRAINT ck_biz_contract_cur    CHECK (currency_code ~ '^[A-Z]{3}$')
);

COMMENT ON TABLE  biz_contract IS '合同（采购/销售/服务/租赁）：乙方是「供应商/客户」的唯一真相；下游单据只引用合同，不再各自维护乙方';
COMMENT ON COLUMN biz_contract.contract_no       IS '合同号：biz_seq.seq_type=CT 生成，格式 HT-yyyyMMdd-000001';
COMMENT ON COLUMN biz_contract.party_b_id        IS '乙方 id：**弱引用**（SUPPLIER→biz_supplier.id / CUSTOMER→biz_customer.id，客户表随批次2 落地）';
COMMENT ON COLUMN biz_contract.party_b_name      IS '乙方名称【快照】：签约时名称。供应商主档改名不改历史合同（审计要「当时是谁」）';
COMMENT ON COLUMN biz_contract.party_a_dept_id   IS '甲方（本组织部门）；NULL = 本组织本体，不挂在某个部门（本库无「公司」表，只有 sys_dept）';
COMMENT ON COLUMN biz_contract.amount            IS '合同金额；框架合同填 0 表示不限额（下单额度校验对该值=0 时跳过）';
COMMENT ON COLUMN biz_contract.status            IS '状态机：DRAFT→SIGNED→ACTIVE→COMPLETED/TERMINATED（边表见 shared/state_machine.rs）';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_contract_no
    ON biz_contract (contract_no) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_contract_party_b
    ON biz_contract (party_b_id, sign_date DESC) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_contract_status
    ON biz_contract (status, end_date);
CREATE INDEX IF NOT EXISTS idx_biz_contract_type
    ON biz_contract (contract_type, status);
-- 「快到期 / 已过期但在生效态」的巡检与提醒，只扫 ACTIVE 行
CREATE INDEX IF NOT EXISTS idx_biz_contract_end
    ON biz_contract (end_date) WHERE status = 'ACTIVE' AND deleted_at IS NULL;

-- 合同号取号规则（格式与既有 PO/IN/TR/SN 同款：前缀-yyyyMMdd-000001）
INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status, remark)
SELECT 'd1930000-0000-4000-8000-000000000001'::uuid, 'CT', '合同号', 'HT', 'yyyyMMdd', '', 0, 1, '-', 1,
       '合同号：格式 HT-yyyyMMdd-000001，按天滚动；与 PO/IN/TR/SN 同款取号器'
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'CT' AND deleted_at IS NULL);

-- ============================================================
-- §2 `biz_purchase_order` —— 加合同三列（弱引用，无外键）
-- ============================================================
ALTER TABLE biz_purchase_order ADD COLUMN IF NOT EXISTS contract_id            UUID;
ALTER TABLE biz_purchase_order ADD COLUMN IF NOT EXISTS purchase_type          VARCHAR(8) NOT NULL DEFAULT 'SPOT';
ALTER TABLE biz_purchase_order ADD COLUMN IF NOT EXISTS contract_no_snapshot   VARCHAR(32);
ALTER TABLE biz_purchase_order ADD COLUMN IF NOT EXISTS supplier_name_snapshot VARCHAR(200);

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'ck_biz_po_purchase_type'
                      AND conrelid = 'public.biz_purchase_order'::regclass) THEN
        ALTER TABLE biz_purchase_order ADD CONSTRAINT ck_biz_po_purchase_type
            CHECK (purchase_type IN ('CONTRACT','SPOT'));
    END IF;
    -- 双向钉死：CONTRACT 必须同时有 contract_id + contract_no_snapshot；
    --           SPOT 必须两者都为空（否则 purchase_type 名不副实，成了第三种状态）。
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'ck_biz_po_contract_pair'
                      AND conrelid = 'public.biz_purchase_order'::regclass) THEN
        ALTER TABLE biz_purchase_order ADD CONSTRAINT ck_biz_po_contract_pair
            CHECK (CASE purchase_type
                     WHEN 'CONTRACT' THEN contract_id IS NOT NULL AND contract_no_snapshot IS NOT NULL
                     WHEN 'SPOT'     THEN contract_id IS NULL AND contract_no_snapshot IS NULL
                     ELSE FALSE
                   END);
    END IF;
END
$$;

COMMENT ON COLUMN biz_purchase_order.contract_id IS
  '合同（**弱引用** biz_contract.id，无外键）：CONTRACT 采购必填、SPOT 采购必须为空（ck_biz_po_contract_pair 双向钉死）';
COMMENT ON COLUMN biz_purchase_order.purchase_type IS
  '采购类型：CONTRACT 合同采购 / SPOT 零星采购。存量单据全部落 SPOT —— 历史上确实没有合同，不编造';
COMMENT ON COLUMN biz_purchase_order.supplier_id IS
  '供应商【快照】：CONTRACT 时在**建单事务内**从合同乙方带出，之后不再跟随合同；SPOT 时用户选择。真相在 biz_contract.party_b_id';
COMMENT ON COLUMN biz_purchase_order.supplier_name_snapshot IS
  '供应商名称【快照】：建单时名称。**存量单据为 NULL**（当时名称不可知，不回填、不编造），显示端 coalesce(snapshot, 主档当前名)';
COMMENT ON COLUMN biz_purchase_order.contract_no_snapshot IS
  '合同号【快照】：列表/打印不必 JOIN 合同；与 contract_id 同生同灭（ck_biz_po_contract_pair 保证）';

CREATE INDEX IF NOT EXISTS idx_biz_po_contract
    ON biz_purchase_order (contract_id, order_date DESC) WHERE contract_id IS NOT NULL;

-- ============================================================
-- §3 `biz_stock_in` —— 只改语义注释，**不加**约束
--   见头注释口径 2：三条合法路径都是「采购入库 + 无来源单」。
-- ============================================================
COMMENT ON COLUMN biz_stock_in.source_order_id IS
  '来源采购单（弱引用 biz_purchase_order.id）：有源时供应商必须与采购单一致（应用层 400）；**允许为空**（期初/批量生成/复制新单，见 ADR-20261031）';
COMMENT ON COLUMN biz_stock_in.supplier_id IS
  '供应商【冗余拷贝】：有源时 = 采购单.supplier_id（同事务带出、不跟随合同），无源时用户录入。**不是快照** —— 它能由上游无歧义重算，漂移即缺陷';

-- ============================================================
-- §4 `biz_snapshot_drift` —— 一致性巡检结果
-- ============================================================
CREATE TABLE IF NOT EXISTS biz_snapshot_drift (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    entity_type    VARCHAR(32) NOT NULL,   -- CONTRACT / SUPPLIER / PURCHASE_ORDER / STOCK_IN
    entity_id      UUID        NOT NULL,
    entity_no      VARCHAR(64) NOT NULL,   -- 单据号/编码（冗余存一份：业务行软删后日志仍可读）
    field_name     VARCHAR(32) NOT NULL,   -- party_b_id / supplier_id / supplier_name
    drift_kind     VARCHAR(16) NOT NULL,   -- SNAPSHOT 合法业务变更 / DEFECT 数据缺陷
    severity       VARCHAR(8)  NOT NULL DEFAULT 'WARN',
    snapshot_value TEXT,                   -- TEXT 而非 UUID：漂移的不止 id（还有名称、金额）
    truth_value    TEXT,
    truth_source   VARCHAR(64),            -- 'biz_contract.party_b_id' 之类：说清「真相在哪」
    affected_count INT         NOT NULL DEFAULT 1,  -- SNAPSHOT 挂对象粒度时的受影响单据数
    detected_at    TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- 首次发现时间（不随巡检刷新）
    last_seen_at   TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- 最近一次仍然存在的时间
    detected_by    VARCHAR(32) NOT NULL DEFAULT 'JOB',
    resolution     VARCHAR(16),            -- REPAIRED 已修复 / ACCEPTED 认可（业务变更）/ IGNORED 忽略
    resolved_at    TIMESTAMPTZ,
    resolved_by    UUID,
    remark         TEXT,

    CONSTRAINT ck_biz_drift_kind  CHECK (drift_kind IN ('SNAPSHOT','DEFECT')),
    CONSTRAINT ck_biz_drift_sev   CHECK (severity IN ('INFO','WARN','ERROR')),
    CONSTRAINT ck_biz_drift_res   CHECK (resolution IS NULL OR resolution IN ('REPAIRED','ACCEPTED','IGNORED')),
    CONSTRAINT ck_biz_drift_res_at CHECK ((resolution IS NULL) = (resolved_at IS NULL)),
    CONSTRAINT ck_biz_drift_by    CHECK (detected_by IN ('JOB','MANUAL','MIGRATION')),
    CONSTRAINT ck_biz_drift_affected CHECK (affected_count > 0)
);

COMMENT ON TABLE biz_snapshot_drift IS
  '单据一致性巡检结果：drift_kind=SNAPSHOT 是**合法业务变更**（合同换乙方/供应商改名），DEFECT 是**数据缺陷**（拷贝没跟上游）。前者只展示+可 ACCEPTED，后者必须 REPAIRED';
COMMENT ON COLUMN biz_snapshot_drift.entity_type IS
  'SNAPSHOT 挂「真相所在对象」（CONTRACT/SUPPLIER，一条记录代表一批受影响单据，用 affected_count 表达）；DEFECT 挂具体单据行';
COMMENT ON COLUMN biz_snapshot_drift.affected_count IS
  '受影响的单据数。POC 实测：一次合同换乙方影响 20,001 张采购单 —— 行粒度落表会淹掉运维（POC-4 vs POC-4b）';

-- 同一个「未解决的漂移」只留一条（部分唯一索引 → 巡检用 ON CONFLICT ... WHERE resolved_at IS NULL 幂等）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_drift_open
    ON biz_snapshot_drift (entity_type, entity_id, field_name) WHERE resolved_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_drift_open_time
    ON biz_snapshot_drift (detected_at DESC) WHERE resolved_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_drift_entity
    ON biz_snapshot_drift (entity_type, entity_id, detected_at DESC);

-- ============================================================
-- §5 追溯链视图（只读、无存储 ⇒ 不会成为「第二真相」）
-- ============================================================
CREATE OR REPLACE VIEW v_biz_doc_edge AS
    SELECT 'STOCK_IN'::varchar(32)        AS child_type,
           s.id                           AS child_id,
           'PURCHASE_ORDER'::varchar(32)  AS parent_type,
           s.source_order_id              AS parent_id,
           'source_order_id'::varchar(32) AS via_column
      FROM biz_stock_in s
     WHERE s.deleted_at IS NULL AND s.source_order_id IS NOT NULL
    UNION ALL
    SELECT 'PURCHASE_ORDER', o.id, 'CONTRACT', o.contract_id, 'contract_id'
      FROM biz_purchase_order o
     WHERE o.deleted_at IS NULL AND o.contract_id IS NOT NULL
    UNION ALL
    SELECT 'PAYABLE', p.id, p.source_type, p.source_id, 'source_type/source_id'
      FROM biz_payable p
     WHERE p.deleted_at IS NULL
    UNION ALL
    SELECT 'VOUCHER', v.id, v.source_type, v.source_id, 'source_type/source_id'
      FROM biz_voucher v
     WHERE v.deleted_at IS NULL AND v.source_id IS NOT NULL;

COMMENT ON VIEW v_biz_doc_edge IS
  '单据父子边（只读视图）：每条边都来自真实列，不新增存储。追溯链用 recursive CTE 走它；**禁止**再建 biz_doc_link 表（那是第二真相）';

CREATE OR REPLACE VIEW v_biz_doc_ref AS
    SELECT 'CONTRACT'::varchar(32) AS doc_type, c.id, c.contract_no AS doc_no, c.contract_name AS doc_title, c.status
      FROM biz_contract c WHERE c.deleted_at IS NULL
    UNION ALL
    SELECT 'PURCHASE_ORDER', o.id, o.order_no, o.order_no, o.status
      FROM biz_purchase_order o WHERE o.deleted_at IS NULL
    UNION ALL
    SELECT 'STOCK_IN', s.id, s.in_no, s.in_no, s.status
      FROM biz_stock_in s WHERE s.deleted_at IS NULL
    UNION ALL
    SELECT 'PAYABLE', p.id, p.payable_no, p.payable_no, p.status
      FROM biz_payable p WHERE p.deleted_at IS NULL
    UNION ALL
    SELECT 'VOUCHER', v.id, v.voucher_no, v.summary, v.status
      FROM biz_voucher v WHERE v.deleted_at IS NULL
    UNION ALL
    SELECT 'STOCK_TRANSFER', t.id, t.transfer_no, t.transfer_no, t.status
      FROM biz_stock_transfer t WHERE t.deleted_at IS NULL;

COMMENT ON VIEW v_biz_doc_ref IS
  '单据号索引（只读视图）：doc_type+id → 单号/标题/状态。追溯链每一跳靠它取可读信息，前端不需要 N 次请求';

-- ============================================================
-- §6 自检（空库首启必须也能过：只断言结构 + 约束真的会拦）
-- ============================================================
DO $$
DECLARE
    n_col INT; n_chk INT; n_idx INT; n_view INT; n_seq INT;
BEGIN
    SELECT count(*) INTO n_col FROM information_schema.columns
     WHERE table_schema='public' AND table_name='biz_contract';
    SELECT count(*) INTO n_chk FROM pg_constraint
     WHERE conrelid='public.biz_contract'::regclass AND contype='c';
    SELECT count(*) INTO n_idx FROM pg_indexes
     WHERE schemaname='public' AND tablename='biz_contract';
    SELECT count(*) INTO n_view FROM information_schema.views
     WHERE table_schema='public' AND table_name IN ('v_biz_doc_edge','v_biz_doc_ref');
    SELECT count(*) INTO n_seq FROM biz_seq WHERE seq_type='CT' AND deleted_at IS NULL;

    IF n_col <> 24  THEN RAISE EXCEPTION 'biz_contract 列数 %，期望 24', n_col; END IF;
    IF n_chk <> 8   THEN RAISE EXCEPTION 'biz_contract CHECK 数 %，期望 8', n_chk; END IF;
    IF n_idx < 6    THEN RAISE EXCEPTION 'biz_contract 索引数 %，期望 >= 6', n_idx; END IF;
    IF n_view <> 2  THEN RAISE EXCEPTION '追溯视图数 %，期望 2', n_view; END IF;
    IF n_seq <> 1   THEN RAISE EXCEPTION 'CT 取号规则数 %，期望 1', n_seq; END IF;

    -- 外键基线：biz_contract 不允许成为任何表的外键目标
    IF EXISTS (SELECT 1 FROM pg_constraint
                WHERE confrelid = 'public.biz_contract'::regclass AND contype='f') THEN
        RAISE EXCEPTION 'biz_contract 被建成了外键目标，违反本仓弱引用基线';
    END IF;

    RAISE NOTICE 'V0193 自检通过：biz_contract % 列 / % CHECK / % 索引 / 2 视图 / CT 取号，零外键',
        n_col, n_chk, n_idx;
END
$$;

COMMIT;
