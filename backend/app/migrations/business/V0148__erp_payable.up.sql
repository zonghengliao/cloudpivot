-- V0148 应付单 `biz_payable`（批次 D · P2-C1，方案 A：入库即生成）
--
-- 依据：`docs/ERP-批次1-技术架构.md` §2.4.1（687-737 行）+ 卡面 §二。
--
-- 【迁移号】卡面写「V0147 应付 / V0148 凭证」，但 V0147 已被 P2-B1 占用
--   （`V0147__erp_stock_in_permissions.up.sql`，收货入库 6 个权限码）
--   → 本卡顺延为 **V0148 应付** / **V0149 凭证 + 分录 + 科目映射规则**。
--
-- 【与卡面的字段差异】卡面 §二 只列了
--   id / payable_no / supplier_id / source_type / source_id / source_no / amount /
--   paid_amount / unpaid_amount / status / voucher_id / due_date + 审计五件套；
--   架构 §2.4.1 另有 invoice_no / invoice_date / currency_code / credit_days /
--   period_code / settled_at / remark —— **按架构全量建表**（卡面字段是其子集，
--   少了 period_code / credit_days 就没法做账龄与期间归属）。列名以架构为准。
--
-- 【幂等铁律】`uk_biz_payable_source (source_type, source_id)`：同一来源单据只能生成
--   一张有效应付单 → 过账重试 / 并发不会产生重复应付（卡面 ⑭ 事务性的最后一道闸）。
CREATE TABLE IF NOT EXISTS biz_payable (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    payable_no    VARCHAR(32)   NOT NULL,
    supplier_id   UUID          NOT NULL,
    source_type   VARCHAR(24)   NOT NULL DEFAULT 'STOCK_IN',
    source_id     UUID          NOT NULL,
    source_no     VARCHAR(32)   NOT NULL,
    invoice_no    VARCHAR(50),
    invoice_date  DATE,
    currency_code CHAR(3)       NOT NULL DEFAULT 'CNY',
    amount        NUMERIC(18,4) NOT NULL DEFAULT 0,
    paid_amount   NUMERIC(18,4) NOT NULL DEFAULT 0,
    unpaid_amount NUMERIC(18,4) NOT NULL DEFAULT 0,
    credit_days   INT           NOT NULL DEFAULT 0,
    due_date      DATE          NOT NULL,
    period_code   VARCHAR(7)    NOT NULL,
    status        VARCHAR(20)   NOT NULL DEFAULT 'OPEN',
    settled_at    TIMESTAMPTZ,
    voucher_id    UUID,
    remark        TEXT,
    created_at    TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at    TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by     UUID,
    update_by     UUID,
    deleted_at    TIMESTAMPTZ,

    CONSTRAINT ck_biz_payable_status CHECK (status IN ('OPEN','PARTIAL_PAID','SETTLED','CANCELLED')),
    CONSTRAINT ck_biz_payable_amount CHECK (amount >= 0),
    CONSTRAINT ck_biz_payable_paid   CHECK (paid_amount >= 0 AND paid_amount <= amount),
    -- 未付金额恒等：程序算错会被数据库拦住（架构 §2.4.1 的解释：不用生成列，
    -- 因为框架 `#[derive(Model)]` 的 insert 会写结构体全部字段）
    CONSTRAINT ck_biz_payable_unpaid CHECK (unpaid_amount = amount - paid_amount),
    CONSTRAINT ck_biz_payable_credit CHECK (credit_days >= 0),
    -- 已结清 ⇔ 未付为 0（状态与金额不许互相矛盾）
    CONSTRAINT ck_biz_payable_settled CHECK (status <> 'SETTLED' OR unpaid_amount = 0)
);

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_payable_no
    ON biz_payable (payable_no) WHERE deleted_at IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_payable_source
    ON biz_payable (source_type, source_id) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_payable_supplier ON biz_payable (supplier_id, status);
CREATE INDEX IF NOT EXISTS idx_biz_payable_due      ON biz_payable (due_date, status);
CREATE INDEX IF NOT EXISTS idx_biz_payable_period   ON biz_payable (period_code);
CREATE INDEX IF NOT EXISTS idx_biz_payable_voucher  ON biz_payable (voucher_id);

-- 自检：4 条业务约束 + 6 个索引必须真的落地（迁移只前进，别把断言留给下游）
DO $$
DECLARE
    n_constraint INT;
    n_index      INT;
BEGIN
    SELECT count(*) INTO n_constraint
      FROM pg_constraint
     WHERE conrelid = 'public.biz_payable'::regclass
       AND conname IN ('ck_biz_payable_status', 'ck_biz_payable_amount',
                       'ck_biz_payable_paid', 'ck_biz_payable_unpaid',
                       'ck_biz_payable_credit', 'ck_biz_payable_settled');
    IF n_constraint < 6 THEN
        RAISE EXCEPTION 'V0148 自检失败：biz_payable 约束应有 6 条，实际 %', n_constraint;
    END IF;

    SELECT count(*) INTO n_index
      FROM pg_indexes
     WHERE tablename = 'biz_payable';
    IF n_index < 7 THEN  -- pkey + 6 个显式索引
        RAISE EXCEPTION 'V0148 自检失败：biz_payable 索引应有 7 个，实际 %', n_index;
    END IF;

    RAISE NOTICE 'V0148 自检通过：biz_payable（% 条约束 / % 个索引）', n_constraint, n_index;
END $$;
