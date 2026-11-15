-- V0149 凭证 + 分录 + 科目映射规则（批次 D · P2-C1，方案 A：入库即生成）
--
-- 依据：`docs/ERP-批次1-技术架构.md` §2.5（738-893 行）/ §2.6 + PRD §3.3 / §3.4。
--
-- 【迁移号】V0147 已被 P2-B1 占用 → 本卡为 V0148（应付）+ **V0149（本文件）**。
--
-- 【列名与卡面的差异（三处，都是为了下游能写 SQL）】
--   1. 卡面写 `period`，架构 §2.5.1 写 `period_code` —— **按卡面用 `period`**
--      （P2-D1 卡面也按 `period` 写查询），类型 `VARCHAR(7)`（yyyy-MM）。
--   2. 卡面写 `reversed_by` / `reverse_of`；架构写 `reversed_voucher_id` + `is_red`。
--      本迁移两者都要：`reverse_of`（红字凭证 → 原凭证）+ `reversed_by`（原凭证 → 红字凭证）
--      + `is_red`（红字标记，便于总账/报表按红字过滤）。
--      **原凭证状态保持 POSTED 并打 `reversed_by`**（卡面 §五），不引入 `REVERSED` 状态
--      —— 状态白名单与卡面一致：POSTED / DRAFT / AUDITED / CANCELLED。
--   3. 卡面没写 `voucher_word` / `entry_count` / `voucher_date` / `summary` /
--      `posted_at`；架构有 → 全部建上（`entry_count >= 2` 是会计上的最低要求）。
--
-- 【★ 红冲口径（与卡面 §五「金额取负」的差异，必读）】
--   卡面写「红字凭证金额取负」，但同一张卡面又要求
--   `CHECK (debit_amount >= 0 AND credit_amount >= 0)` —— 负数根本写不进库。
--   架构 §2.5.2 明确选择**方案 B：借贷互换**（原借 100 → 红字凭证贷 100，金额恒为正），
--   并给出理由（负数会让试算平衡/报表/对外导出全部要特判）。本卡按架构落地：
--   * 红字凭证 `is_red=1`、`reverse_of=原凭证`、`reverse_by` 反向指针写在原凭证上；
--   * 分录 = 原分录的 `debit_amount` / `credit_amount` **互换**、金额不变；
--   * 净效果 = 原凭证的负数（借方合计 = 原贷方合计），`SUM(debit) - SUM(credit)` 恒为负。
--   这条差异同时写进 `backend/src/business/erp/voucher.rs` 的模块文档与卡面留言。
--
-- 【★ 科目种子】`biz_account` 原本只有 1405 库存商品 / 2202 应付账款两条，
--   缺进项税科目 → 分录根本写不出来。本迁移补：
--     · 2221    应交税费（一级，LIABILITY / CREDIT / 非末级）
--     · 2221.01    应交税费-进项税（二级，parent = 2221，末级）
--   全部 `ON CONFLICT DO NOTHING` / `WHERE NOT EXISTS`，可重放。

-- ==================== ① 凭证主表 ====================
CREATE TABLE IF NOT EXISTS biz_voucher (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    voucher_word  VARCHAR(8)    NOT NULL DEFAULT '记',
    voucher_no    VARCHAR(32)   NOT NULL,
    period        VARCHAR(7)    NOT NULL,
    voucher_date  DATE          NOT NULL,
    voucher_type  VARCHAR(16)   NOT NULL DEFAULT 'AUTO',
    source_type   VARCHAR(24)   NOT NULL DEFAULT 'MANUAL',
    source_id     UUID,
    source_no     VARCHAR(32),
    summary       VARCHAR(255)  NOT NULL DEFAULT '',
    total_debit   NUMERIC(20,4) NOT NULL DEFAULT 0,
    total_credit  NUMERIC(20,4) NOT NULL DEFAULT 0,
    entry_count   INT           NOT NULL DEFAULT 0,
    status        VARCHAR(16)   NOT NULL DEFAULT 'POSTED',
    is_red        SMALLINT      NOT NULL DEFAULT 0,
    reverse_of    UUID,
    reversed_by   UUID,
    posted_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    posted_by     UUID,
    remark        TEXT,
    created_at    TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at    TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by     UUID,
    update_by     UUID,
    deleted_at    TIMESTAMPTZ,

    CONSTRAINT ck_biz_voucher_status CHECK (status IN ('POSTED','DRAFT','AUDITED','CANCELLED')),
    CONSTRAINT ck_biz_voucher_type   CHECK (voucher_type IN ('AUTO','MANUAL')),
    CONSTRAINT ck_biz_voucher_red    CHECK (is_red IN (0, 1)),
    -- ★ 借贷平衡：数据库层强制，任何绕过引擎的写路径都写不进不平的凭证（卡面硬红线 3）
    CONSTRAINT ck_biz_voucher_balance  CHECK (total_debit = total_credit),
    CONSTRAINT ck_biz_voucher_positive CHECK (total_debit >= 0 AND total_credit >= 0),
    -- 单分录凭证在会计上无意义（至少一借一贷）
    CONSTRAINT ck_biz_voucher_entries  CHECK (entry_count >= 2),
    -- 红字凭证必须指向原凭证；普通凭证不许携带该字段
    CONSTRAINT ck_biz_voucher_red_ref  CHECK ((is_red = 1 AND reverse_of IS NOT NULL)
                                           OR (is_red = 0 AND reverse_of IS NULL)),
    CONSTRAINT ck_biz_voucher_self     CHECK (reverse_of IS NULL OR reverse_of <> id)
);

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_voucher_no
    ON biz_voucher (voucher_word, period, voucher_no) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_voucher_date    ON biz_voucher (voucher_date DESC);
CREATE INDEX IF NOT EXISTS idx_biz_voucher_period  ON biz_voucher (period, voucher_word, voucher_no);
CREATE INDEX IF NOT EXISTS idx_biz_voucher_source  ON biz_voucher (source_type, source_id);
CREATE INDEX IF NOT EXISTS idx_biz_voucher_reverse ON biz_voucher (reverse_of);
CREATE INDEX IF NOT EXISTS idx_biz_voucher_status  ON biz_voucher (status);

-- ==================== ② 凭证分录 ====================
CREATE TABLE IF NOT EXISTS biz_voucher_entry (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    voucher_id       UUID          NOT NULL REFERENCES biz_voucher(id) ON DELETE CASCADE,
    line_no          INT           NOT NULL,
    summary          VARCHAR(255)  NOT NULL,
    account_id       UUID          NOT NULL,
    -- 科目编码 / 名称快照：科目改名后历史凭证语义不漂移（架构 §2.5.2）
    account_code     VARCHAR(30)   NOT NULL,
    account_name     VARCHAR(120)  NOT NULL,
    debit_amount     NUMERIC(20,4) NOT NULL DEFAULT 0,
    credit_amount    NUMERIC(20,4) NOT NULL DEFAULT 0,
    aux_supplier_id  UUID,
    aux_material_id  UUID,
    aux_warehouse_id UUID,
    aux_dept_id      UUID,
    aux_qty          NUMERIC(18,4),
    aux_extra        JSONB,
    created_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT uk_biz_voucher_entry_line UNIQUE (voucher_id, line_no),
    CONSTRAINT ck_biz_voucher_entry_positive CHECK (debit_amount >= 0 AND credit_amount >= 0),
    -- 单边原则：一条分录只能落在借方或贷方（同 0 / 同非 0 都不行，卡面硬红线 4）
    CONSTRAINT ck_biz_voucher_entry_side CHECK ((debit_amount = 0 AND credit_amount > 0)
                                             OR (debit_amount > 0 AND credit_amount = 0))
);

CREATE INDEX IF NOT EXISTS idx_biz_voucher_entry_voucher  ON biz_voucher_entry (voucher_id, line_no);
CREATE INDEX IF NOT EXISTS idx_biz_voucher_entry_account  ON biz_voucher_entry (account_id);
CREATE INDEX IF NOT EXISTS idx_biz_voucher_entry_supplier ON biz_voucher_entry (aux_supplier_id);
CREATE INDEX IF NOT EXISTS idx_biz_voucher_entry_material ON biz_voucher_entry (aux_material_id);

-- ==================== ③ 科目映射规则 ====================
CREATE TABLE IF NOT EXISTS biz_voucher_rule (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    rule_code         VARCHAR(40) NOT NULL,
    biz_type          VARCHAR(24) NOT NULL,
    debit_account_id  UUID,
    credit_account_id UUID,
    tax_account_id    UUID,
    is_active         SMALLINT    NOT NULL DEFAULT 1,
    remark            TEXT,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by         UUID,
    update_by         UUID,
    deleted_at        TIMESTAMPTZ,

    CONSTRAINT ck_biz_voucher_rule_active CHECK (is_active IN (0, 1))
);

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_voucher_rule_code
    ON biz_voucher_rule (rule_code) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_voucher_rule_biz
    ON biz_voucher_rule (biz_type, is_active);

-- ==================== ④ 科目种子（缺进项税科目，凭证根本写不出来） ====================
INSERT INTO biz_account (id, account_code, account_name, parent_id, level,
                         account_category, balance_direction, is_leaf, status, remark)
VALUES ('d1370004-0000-4000-8000-000000000003', '2221', '应交税费', NULL, 1,
        'LIABILITY', 'CREDIT', 0, 1, 'P2-C1 补种：应交税费（一级，非末级不记账）')
ON CONFLICT DO NOTHING;

INSERT INTO biz_account (id, account_code, account_name, parent_id, level,
                         account_category, balance_direction, is_leaf, status, remark)
SELECT 'd1370004-0000-4000-8000-000000000004', '2221.01', '应交税费-进项税',
       (SELECT id FROM biz_account WHERE account_code = '2221' AND deleted_at IS NULL),
       2, 'LIABILITY', 'CREDIT', 1, 1, 'P2-C1 补种：进项税额（采购入库借方）'
 WHERE NOT EXISTS (SELECT 1 FROM biz_account
                    WHERE account_code = '2221.01' AND deleted_at IS NULL);

-- ==================== ⑤ 默认科目映射规则（PRD §3.4） ====================
INSERT INTO biz_voucher_rule (rule_code, biz_type, debit_account_id, credit_account_id,
                              tax_account_id, is_active, remark)
SELECT 'PURCHASE_IN_DEFAULT', 'PURCHASE_IN',
       (SELECT id FROM biz_account WHERE account_code = '1405'    AND deleted_at IS NULL),
       (SELECT id FROM biz_account WHERE account_code = '2202'    AND deleted_at IS NULL),
       (SELECT id FROM biz_account WHERE account_code = '2221.01' AND deleted_at IS NULL),
       1,
       '采购入库默认映射：借 1405 库存商品 / 借 2221.01 进项税 / 贷 2202 应付账款'
 WHERE NOT EXISTS (SELECT 1 FROM biz_voucher_rule
                    WHERE rule_code = 'PURCHASE_IN_DEFAULT' AND deleted_at IS NULL);

-- ==================== ⑥ 自检（约束真的能拦、种子真的落地） ====================
DO $$
DECLARE
    n_account INT;
    n_rule    INT;
    n_ck      INT;
BEGIN
    SELECT count(*) INTO n_account
      FROM biz_account
     WHERE account_code IN ('1405', '2202', '2221', '2221.01') AND deleted_at IS NULL;
    IF n_account < 4 THEN
        RAISE EXCEPTION 'V0149 自检失败：科目种子应有 4 条（1405/2202/2221/2221.01），实际 %', n_account;
    END IF;

    SELECT count(*) INTO n_rule
      FROM biz_voucher_rule
     WHERE rule_code = 'PURCHASE_IN_DEFAULT' AND deleted_at IS NULL
       AND debit_account_id IS NOT NULL AND credit_account_id IS NOT NULL
       AND tax_account_id IS NOT NULL;
    IF n_rule <> 1 THEN
        RAISE EXCEPTION 'V0149 自检失败：PURCHASE_IN_DEFAULT 规则未完整落地（% 行）', n_rule;
    END IF;

    SELECT count(*) INTO n_ck
      FROM pg_constraint
     WHERE conrelid = 'public.biz_voucher'::regclass
       AND conname IN ('ck_biz_voucher_balance', 'ck_biz_voucher_entries',
                       'ck_biz_voucher_red_ref', 'ck_biz_voucher_status');
    IF n_ck < 4 THEN
        RAISE EXCEPTION 'V0149 自检失败：biz_voucher 关键约束缺失（应有 4 条，实际 %）', n_ck;
    END IF;

    -- ★ 借贷不平衡必须被库里拦住（卡面 ⑮ 的迁移侧证据）
    BEGIN
        INSERT INTO biz_voucher (voucher_no, period, voucher_date,
                                 total_debit, total_credit, entry_count)
        VALUES ('__V0149_SELFCHECK__', '1999-01', DATE '1999-01-01', 100, 90, 2);
        RAISE EXCEPTION 'V0149 自检失败：借贷不平衡的凭证写进了库（ck_biz_voucher_balance 失效）';
    EXCEPTION WHEN check_violation THEN
        RAISE NOTICE 'V0149 自检通过：ck_biz_voucher_balance 拦住了不平的凭证';
    END;

    -- ★ 借贷双边同时有值必须被拦住（卡面硬红线 4）
    BEGIN
        INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id,
                                       account_code, account_name, debit_amount, credit_amount)
        VALUES ('00000000-0000-0000-0000-000000000000', 1, '自检', gen_random_uuid(),
                'X', 'X', 10, 10);
        RAISE EXCEPTION 'V0149 自检失败：借贷双边同时有值的分录写进了库';
    EXCEPTION
        WHEN check_violation THEN
            RAISE NOTICE 'V0149 自检通过：ck_biz_voucher_entry_side 拦住了双边分录';
        WHEN foreign_key_violation THEN
            RAISE EXCEPTION 'V0149 自检失败：分录自检先撞了外键，未能验证单边约束';
    END;

    RAISE NOTICE 'V0149 自检通过：凭证/分录/规则三表 + 科目种子 + 映射规则';
END $$;
