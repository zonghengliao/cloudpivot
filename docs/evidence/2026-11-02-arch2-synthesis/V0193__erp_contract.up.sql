-- ============================================================
-- V0193：合同主表 biz_contract（ARCH-2 汇总定稿 · 第一处关注点）
--
-- 口径来源：docs/architecture/合同与勾稽关系设计.md §5.1
-- 上级证据：docs/evidence/2026-11-02-arch2-contract/model-poc.sql（真库 exit=0）
--
-- 与 docs/evidence/2026-11-02-arch2-contract/V0193__erp_contract.up.sql（草稿）的 4 处差异，
-- 本文件已按汇总卡 C-01..C-04 收敛：
--   ① party_a_dept_id → party_a_id
--   ② amount NUMERIC(20,4) → NUMERIC(18,4)
--   ③ amount >= 0（0=不限额） → amount > 0
--   ④ 草稿的「合同 + PO 三列 + 漂移表 + 两视图」一锅端 → 本文件只含合同表
--
-- 写法纪律（V0137 头注释）：
--   ① 文件内不写 BEGIN/COMMIT —— 迁移器自己在事务里跑，文件内 COMMIT 会提前提交掉迁移事务
--   ② CHECK 不得依赖 NULL 三值逻辑「通过」—— 见 ck_biz_contract_signed_dates 的 CASE ... ELSE TRUE
--   ③ 自检块只断言结构，不写业务数据
-- ============================================================

CREATE TABLE IF NOT EXISTS biz_contract (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    contract_no          VARCHAR(32)   NOT NULL,
    contract_name        VARCHAR(200)  NOT NULL,
    contract_type        VARCHAR(16)   NOT NULL,
    party_a_id           UUID,
    party_a_name         VARCHAR(200),
    party_b_type         VARCHAR(16)   NOT NULL,
    party_b_id           UUID          NOT NULL,
    party_b_name         VARCHAR(200)  NOT NULL,
    amount               NUMERIC(18,4) NOT NULL,
    currency_code        CHAR(3)       NOT NULL DEFAULT 'CNY',
    sign_date            DATE,
    start_date           DATE,
    end_date             DATE,
    status               VARCHAR(16)   NOT NULL DEFAULT 'DRAFT',
    owner_id             UUID,
    dept_id              UUID,
    remark               TEXT,
    created_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at           TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by            UUID,
    update_by            UUID,
    deleted_at           TIMESTAMPTZ,

    CONSTRAINT ck_biz_contract_type
        CHECK (contract_type IN ('PURCHASE','SALES','SERVICE','LEASE')),
    CONSTRAINT ck_biz_contract_party_b_type
        CHECK (party_b_type IN ('SUPPLIER','CUSTOMER')),
    CONSTRAINT ck_biz_contract_type_party_pair
        CHECK (CASE contract_type
                 WHEN 'PURCHASE' THEN party_b_type = 'SUPPLIER'
                 WHEN 'SALES'    THEN party_b_type = 'CUSTOMER'
                 ELSE party_b_type IN ('SUPPLIER','CUSTOMER')
               END),
    CONSTRAINT ck_biz_contract_amount
        CHECK (amount > 0),
    CONSTRAINT ck_biz_contract_date_order
        CHECK (start_date IS NULL OR end_date IS NULL OR end_date >= start_date),
    CONSTRAINT ck_biz_contract_signed_dates
        CHECK (CASE WHEN status IN ('SIGNED','ACTIVE','COMPLETED')
                    THEN sign_date IS NOT NULL AND start_date IS NOT NULL AND end_date IS NOT NULL
                    ELSE TRUE END),
    CONSTRAINT ck_biz_contract_status
        CHECK (status IN ('DRAFT','SIGNED','ACTIVE','COMPLETED','TERMINATED')),
    CONSTRAINT ck_biz_contract_currency
        CHECK (currency_code ~ '^[A-Z]{3}$'),
    CONSTRAINT ck_biz_contract_no_charset
        CHECK (contract_no ~ '^[A-Za-z0-9][A-Za-z0-9_-]{5,31}$')
);

COMMENT ON TABLE  biz_contract IS '合同（采购/销售/服务/租赁）：乙方是供应商/客户的唯一真相；下游单据只引用合同，不各自维护乙方';
COMMENT ON COLUMN biz_contract.contract_no       IS '合同号：biz_seq(seq_type=CT) 取号，格式 HT-yyyyMMdd-000001；DB 只校验字符集与长度';
COMMENT ON COLUMN biz_contract.contract_name     IS '合同名称';
COMMENT ON COLUMN biz_contract.contract_type     IS 'PURCHASE 采购 / SALES 销售 / SERVICE 服务 / LEASE 租赁';
COMMENT ON COLUMN biz_contract.party_a_id        IS '甲方：NULL=本组织本体；非空=sys_dept.id（弱引用，本库无"公司"表）';
COMMENT ON COLUMN biz_contract.party_a_name      IS '甲方名称【快照】：签约时名称，随合同冻结';
COMMENT ON COLUMN biz_contract.party_b_type      IS '乙方类型：SUPPLIER→biz_supplier / CUSTOMER→biz_customer（客户表属批次2）';
COMMENT ON COLUMN biz_contract.party_b_id        IS '乙方 id：弱引用（非外键）。存在性由应用层写入校验 + 每日巡检兜底';
COMMENT ON COLUMN biz_contract.party_b_name      IS '乙方名称【快照】：签约时名称；主档改名不回溯历史合同（审计要"当时是谁"）';
COMMENT ON COLUMN biz_contract.amount            IS '合同金额（不含税口径与采购单 total_amount 一致），精度 NUMERIC(18,4)；必须 > 0';
COMMENT ON COLUMN biz_contract.currency_code     IS '币种 ISO 4217 三位大写';
COMMENT ON COLUMN biz_contract.sign_date         IS '签订日期；status>=SIGNED 时必填';
COMMENT ON COLUMN biz_contract.start_date        IS '生效日；status>=SIGNED 时必填，且 <= end_date';
COMMENT ON COLUMN biz_contract.end_date          IS '到期日；下游建单必须落在 [start_date, end_date] 内';
COMMENT ON COLUMN biz_contract.status            IS '状态机：DRAFT→SIGNED→ACTIVE→COMPLETED/TERMINATED，另有 SIGNED→TERMINATED；边表在应用层';
COMMENT ON COLUMN biz_contract.owner_id          IS '负责人（弱引用 sys_user.id）';
COMMENT ON COLUMN biz_contract.dept_id           IS '归属部门（弱引用 sys_dept.id），数据权限过滤用';
COMMENT ON COLUMN biz_contract.updated_at        IS '最后更新时间：由模型层写入（本库零触发器，不靠 DB 自动维护）';
COMMENT ON COLUMN biz_contract.deleted_at        IS '软删除时间；合同号唯一约束只对未删除行生效（软删后可复用号码，审计仍可查旧行）';

-- 软删唯一：PG 不支持在建表语句里给 UNIQUE 带谓词，只能用部分唯一索引
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_contract_no
    ON biz_contract (contract_no) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_contract_party_b
    ON biz_contract (party_b_id, sign_date DESC) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_contract_status
    ON biz_contract (status) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_contract_end
    ON biz_contract (end_date) WHERE deleted_at IS NULL;

-- 合同号取号规则（幂等：复跑不报唯一冲突）
INSERT INTO biz_seq (seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status, remark)
SELECT 'CT', '合同号', 'HT', 'yyyyMMdd', '', 0, 1, '-', 1,
       '合同号：HT-yyyyMMdd-000001，按天滚动，与 PO/IN/TR/SN 同款取号器'
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'CT' AND deleted_at IS NULL);

-- 结构自检（只断言结构，不写业务数据）
DO $$
DECLARE
    n_col INT; n_chk INT; n_idx INT; n_seq INT;
BEGIN
    SELECT count(*) INTO n_col FROM information_schema.columns
     WHERE table_schema='public' AND table_name='biz_contract';
    SELECT count(*) INTO n_chk FROM pg_constraint
     WHERE conrelid='public.biz_contract'::regclass AND contype='c';
    SELECT count(*) INTO n_idx FROM pg_indexes
     WHERE schemaname='public' AND tablename='biz_contract';
    SELECT count(*) INTO n_seq FROM biz_seq WHERE seq_type='CT' AND deleted_at IS NULL;

    IF n_col <> 23 THEN RAISE EXCEPTION 'biz_contract 列数 %，期望 23', n_col; END IF;
    IF n_chk <> 9  THEN RAISE EXCEPTION 'biz_contract CHECK 数 %，期望 9', n_chk; END IF;
    IF n_idx <> 5  THEN RAISE EXCEPTION 'biz_contract 索引数 %（含 PK），期望 5', n_idx; END IF;
    IF n_seq <> 1  THEN RAISE EXCEPTION 'CT 取号规则数 %，期望 1', n_seq; END IF;

    -- 外键基线：biz_contract 不允许成为任何表的外键目标
    IF EXISTS (SELECT 1 FROM pg_constraint
                WHERE confrelid = 'public.biz_contract'::regclass AND contype='f') THEN
        RAISE EXCEPTION 'biz_contract 被建成了外键目标，违反本仓弱引用基线';
    END IF;

    RAISE NOTICE 'V0193 自检通过：biz_contract % 列 / % CHECK / % 索引（含 PK）/ CT 取号，零外键', n_col, n_chk, n_idx;
END
$$;
