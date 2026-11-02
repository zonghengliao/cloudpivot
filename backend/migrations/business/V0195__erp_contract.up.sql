-- ============================================================
-- V0195：合同主表 `biz_contract`（ARCH-2 合同表数据模型）
--
-- 依据：`docs/architecture/ARCH-2-合同表数据模型与校验规则.md`
--       §2.2 完整 DDL（POC 已实跑，exit=0；P1/P6 自检 23 列 / 9 CHECK / 5 索引含主键）
--
-- 本迁移**只建表**，逐字照抄 §2.2：23 列 + 9 个 CHECK + 4 个业务索引（+ 主键共 5 个）
-- + 19 条 COMMENT（表 1 + 列 18）+ `biz_seq` 的 CT 取号规则 1 行。
-- 权限码 / 菜单不在这里（见 `V0197__erp_contract_permissions`），
-- CRUD 接口不在本批次（A2），前端页面不在本批次（A5）。
--
-- # 三条建表红线（违反即返工）
--
--   ① **乙方没有外键**（DEC-02）：`party_b_id` 是弱引用，指向 `biz_supplier`
--      （批次 2 起还有 `biz_customer`）。存在性由应用层写入校验
--      （`contract_model::party_b_ref`）+ 每日巡检兜底，DB 层不拦 —— 这是设计意图，
--      不是漏建外键，**不要"顺手"补 FK**。
--   ② **唯一性只对活跃行生效**（§2.3）：`contract_no` 用部分唯一索引
--      `WHERE deleted_at IS NULL`。禁止写成列级 `UNIQUE`（软删后号码永久不可复用）
--      或 `UNIQUE (contract_no, deleted_at)`（同号可重复插入）。
--   ③ **零触发器**（§3.4）：`updated_at` 由模型层写，DB 不装触发器。
--
-- # 为什么没有 `supplier_id` / `used_amount` / `version` 列
--
--   见 §2.1 末表：冗余乙方列与 `party_b_id` 同义重复（正是本次重构要消灭的东西）；
--   已下单额缓存列违反「单一数据源」（R1），累计值必须从采购单现算（V-21/V-23）。
--
-- # 幂等
--
--   建表语句本身不可重入（表已存在会报 42P07），由迁移框架的
--   `schema_migrations` 记录保证只跑一次；`biz_seq` 种子用
--   `INSERT ... WHERE NOT EXISTS` 双保险（POC 复跑实测不撞唯一索引）。
-- ============================================================

CREATE TABLE biz_contract (
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
COMMENT ON COLUMN biz_contract.party_a_id        IS '甲方：NULL=本组织本体；非空=sys_dept.id（弱引用，本库无“公司”表）';
COMMENT ON COLUMN biz_contract.party_a_name      IS '甲方名称【快照】：签约时名称，随合同冻结';
COMMENT ON COLUMN biz_contract.party_b_type      IS '乙方类型：SUPPLIER→biz_supplier / CUSTOMER→biz_customer（客户表属批次2）';
COMMENT ON COLUMN biz_contract.party_b_id        IS '乙方 id：弱引用（非外键）。存在性由应用层写入校验 + 每日巡检兜底';
COMMENT ON COLUMN biz_contract.party_b_name      IS '乙方名称【快照】：签约时名称；主档改名不回溯历史合同（审计要“当时是谁”）';
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

CREATE UNIQUE INDEX uk_biz_contract_no
    ON biz_contract (contract_no) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_contract_party_b
    ON biz_contract (party_b_id, sign_date DESC) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_contract_status
    ON biz_contract (status) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_contract_end
    ON biz_contract (end_date) WHERE deleted_at IS NULL;

INSERT INTO biz_seq (seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status, remark)
SELECT 'CT', '合同号', 'HT', 'yyyyMMdd', '', 0, 1, '-', 1,
       '合同号：HT-yyyyMMdd-000001，按天滚动，与 PO/IN/TR/SN 同款取号器'
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'CT' AND deleted_at IS NULL);

-- ---------- 自检（POC P1/P6 口径：23 列 / 9 CHECK / 5 索引含主键 / CT 取号 1 行） ----------
-- 放进迁移里是为了让「设计文档被改坏」在部署时就炸，而不是等 A2 建单时才发现列少了。
DO $$
DECLARE
    col_count     int;
    check_count   int;
    index_count   int;
    seq_count     int;
BEGIN
    SELECT count(*) INTO col_count
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_contract';
    IF col_count <> 23 THEN
        RAISE EXCEPTION 'V0195 自检失败：biz_contract 应有 23 列，实际 %', col_count;
    END IF;

    SELECT count(*) INTO check_count
      FROM pg_constraint
     WHERE conrelid = 'public.biz_contract'::regclass AND contype = 'c';
    IF check_count <> 9 THEN
        RAISE EXCEPTION 'V0195 自检失败：biz_contract 应有 9 个 CHECK，实际 %', check_count;
    END IF;

    SELECT count(*) INTO index_count
      FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_contract';
    IF index_count <> 5 THEN
        RAISE EXCEPTION 'V0195 自检失败：biz_contract 应有 5 个索引（4 业务 + 主键），实际 %', index_count;
    END IF;

    SELECT count(*) INTO seq_count
      FROM biz_seq
     WHERE seq_type = 'CT' AND deleted_at IS NULL;
    IF seq_count <> 1 THEN
        RAISE EXCEPTION 'V0195 自检失败：biz_seq 应有 1 条 CT 取号规则，实际 %', seq_count;
    END IF;

    RAISE NOTICE 'V0195 自检通过：cols=% / checks=% / indexes=% / CT 取号=%',
        col_count, check_count, index_count, seq_count;
END $$;
