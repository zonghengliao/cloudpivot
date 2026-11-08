-- ============================================================
-- V0347：附件必传规则表（`biz_attachment_rule`）+ 单据类型放宽到 12 值 —— **ATT-06**
--
-- # 这条迁移补的是什么
--
-- 「哪些单据在什么动作时必须传附件」以前没有任何落点：要么硬编码在各自模块里，
-- 要么根本没人校验（合同扫描件、银行回单、盘点表全靠人记）。本迁移把它做成
-- **配置表**：一条规则 = (doc_type, required_at, 可选 file_role) + 数量区间 +
-- 扩展名白名单 + 可选附加条件。运行时由
-- `business::attachment::rule::assert_attachments()` 读取并校验，
-- **表里改 `enabled = false` 立即生效（不重启服务，没有任何缓存）**。
--
-- # 表结构
--
--   biz_attachment_rule
--     id            主键（迁移内种子的 9 条用固定 UUID：重放幂等）
--     doc_type      单据类型（与 `attachment::model::DocKind` 逐字对应，CHECK 12 值）
--     file_role     指定角色；**NULL = 任意角色都算**（本卡 9 条规则全为 NULL）
--     required_at   触发动作：SUBMIT / CONFIRM / ACTIVATE
--     min_count     至少要几个**扩展名命中白名单**的附件（默认 1）
--     max_count     最多几个（NULL = 不限）
--     allow_exts    扩展名白名单（小写、逗号分隔，如 'pdf,ofd,xml'）
--     condition_sql 可选附加条件：对**单据表当前行**求值的 SQL 片段
--                   （如 `invoice_code IS NULL`）。为空 = 恒适用。
--     enabled       是否启用（false = 该规则不参与校验）
--     remark        人话名，**直接进错误文案**：
--                   「{单据中文名}必须上传{remark}（pdf/ofd/xml）」
--     created_at / updated_at / create_by / update_by / deleted_at  审计四件套 + 软删
--
-- # ⚠ 条件字段的落地口径与卡面的差异（必须留档）
--
-- 卡面写的是「电子票 ext=pdf,ofd,xml / 纸质票 ext=jpg,png,pdf，按 `is_electronic` 分支」。
-- 本仓 `biz_invoice`（V0298）**没有 `is_electronic` 列** —— 销售发票表只区分
-- `invoice_type`（OUT / OUT_RED）与 `invoice_code`（税控发票代码，数电票为空）。
-- 因此电子 / 纸质的分支条件是：
--
--   电子票：`invoice_code IS NULL`      → pdf,ofd,xml（数电票只出 PDF/OFD/XML）
--   纸质票：`invoice_code IS NOT NULL`  → jpg,png,pdf（扫描件）
--
-- 两条条件互补且互斥，新建发票（不带税控代码）命中电子票规则 —— 与卡面验收②
-- 「不传附件提交发票 → 发票必须上传发票影像（pdf/ofd/xml）」一致。
-- 若将来发票表补了 `is_electronic`，只改这两条规则的 `condition_sql`，**不用改代码**。
--
-- # 预置规则（9 条，覆盖卡面 8 个单据；INVOICE 拆电子 / 纸质两条）
--
--   INVOICE          SUBMIT    pdf,ofd,xml   invoice_code IS NULL       发票影像
--   INVOICE          SUBMIT    pdf,jpg,png   invoice_code IS NOT NULL   纸质发票扫描件
--   RECEIPT          SUBMIT    pdf,jpg,png                              银行回单
--   PAYMENT          ACTIVATE  pdf,jpg,png                              银行回单
--   EXPENSE_REPORT   SUBMIT    pdf,jpg,png,ofd                          发票影像或费用凭证
--   FIXED_ASSET      ACTIVATE  pdf,jpg,png                              发票或验收单
--   STOCK_TAKE       CONFIRM   pdf,jpg,png                              盘点表或盈亏说明
--   PURCHASE_RETURN  SUBMIT    pdf,jpg,png                              退货单或签收凭证
--   SALES_RETURN     SUBMIT    pdf,jpg,png                              退货单或物流凭证
--
--   PAYMENT 的 required_at 从卡面的 SUBMIT 改成 **ACTIVATE**：本仓付款记录
--   （`biz_payment`）是**提交动作内部一次性创建**的（`payable_payment::submit_payment`
--   → `submit_for_approval_tx` / `pay_payable_with_reason`），提交那一刻还没有单据
--   主键可以挂附件 —— 在 SUBMIT 上校验等于把付款永久锁死。真实可控的闸口是
--   **出款前**（`execute_approved_payment`，此时 PENDING 付款单已存在，可在附件面板
--   挂银行回单）。卡面验收不覆盖 PAYMENT，此偏差在此留档，不改业务流程。
--
-- # 幂等 / 回滚
--
--   CREATE TABLE / INDEX IF NOT EXISTS；种子 `ON CONFLICT DO NOTHING`（固定 id）。
--   文件内不写 BEGIN/COMMIT（迁移器自己在事务里跑）。
--   回滚：V0347.down.sql（删表 + 把 ck_biz_doc_attachment_type 收回 7 值；
--   已有新类型附件时主动 RAISE，不静默删数据）。
-- ============================================================

-- ============================================================
-- §1 附件类型 CHECK 7 → 12（+ INVOICE / RECEIPT / STOCK_TAKE / PURCHASE_RETURN / SALES_RETURN）
-- ============================================================
-- 为什么先放宽：ATT-06 要能往发票 / 收款 / 盘点 / 采购退货 / 销售退货上挂附件，
-- 这五类单据本来就在 `DocKind` 里没有（附件只能挂在合同/采购/收货/应付/付款/资产/报销 7 类上）。
ALTER TABLE biz_doc_attachment DROP CONSTRAINT IF EXISTS ck_biz_doc_attachment_type;
ALTER TABLE biz_doc_attachment
    ADD CONSTRAINT ck_biz_doc_attachment_type
    CHECK (doc_type IN ('CONTRACT', 'PURCHASE_ORDER', 'STOCK_IN', 'PAYABLE', 'PAYMENT',
                        'FIXED_ASSET', 'EXPENSE_REPORT',
                        'INVOICE', 'RECEIPT', 'STOCK_TAKE', 'PURCHASE_RETURN', 'SALES_RETURN'));

COMMENT ON COLUMN biz_doc_attachment.doc_type IS
    '单据类型：CONTRACT / PURCHASE_ORDER / STOCK_IN / PAYABLE / PAYMENT / FIXED_ASSET / EXPENSE_REPORT / INVOICE / RECEIPT / STOCK_TAKE / PURCHASE_RETURN / SALES_RETURN（与 attachment::model::DocKind 一一对应，V0347 放宽到 12 值）';

-- ============================================================
-- §2 规则表
-- ============================================================
CREATE TABLE IF NOT EXISTS biz_attachment_rule (
    id            uuid         PRIMARY KEY DEFAULT gen_random_uuid(),
    doc_type      varchar(40)  NOT NULL,
    file_role     varchar(40),
    required_at   varchar(20)  NOT NULL,
    min_count     int          NOT NULL DEFAULT 1,
    max_count     int,
    allow_exts    varchar(200),
    condition_sql varchar(300),
    enabled       boolean      NOT NULL DEFAULT true,
    remark        varchar(200),
    created_at    timestamptz  NOT NULL DEFAULT now(),
    updated_at    timestamptz  NOT NULL DEFAULT now(),
    create_by     uuid,
    update_by     uuid,
    deleted_at    timestamptz,
    CONSTRAINT ck_biz_attachment_rule_doc_type
        CHECK (doc_type IN ('CONTRACT', 'PURCHASE_ORDER', 'STOCK_IN', 'PAYABLE', 'PAYMENT',
                            'FIXED_ASSET', 'EXPENSE_REPORT',
                            'INVOICE', 'RECEIPT', 'STOCK_TAKE', 'PURCHASE_RETURN', 'SALES_RETURN')),
    CONSTRAINT ck_biz_attachment_rule_required_at
        CHECK (required_at IN ('SUBMIT', 'CONFIRM', 'ACTIVATE')),
    -- min_count ≥ 1：min_count = 0 的规则是空规则（永远成立），只会让运维误以为配了闸
    CONSTRAINT ck_biz_attachment_rule_min
        CHECK (min_count >= 1),
    -- max_count 要么不限（NULL），要么 ≥ min_count（否则规则恒不成立）
    CONSTRAINT ck_biz_attachment_rule_max
        CHECK (max_count IS NULL OR max_count >= min_count),
    -- allow_exts 为 NULL = 不限格式；非 NULL 时不许是空串（空串会被解析成「什么都不许」，静默拦死）
    CONSTRAINT ck_biz_attachment_rule_exts
        CHECK (allow_exts IS NULL OR btrim(allow_exts) <> ''),
    -- 条件里不许出现语句分隔与注释：它是拼进 WHERE 的片段，不是完整 SQL
    CONSTRAINT ck_biz_attachment_rule_condition
        CHECK (condition_sql IS NULL
               OR (position(';' IN condition_sql) = 0
                   AND position('--' IN condition_sql) = 0
                   AND position('/*' IN condition_sql) = 0))
);

COMMENT ON TABLE biz_attachment_rule IS
    '附件必传规则（ATT-06）：哪些单据在哪个动作必须传附件、要什么格式、至少要几个。运行时逐次读取，改 enabled 立即生效（无缓存、不重启）';
COMMENT ON COLUMN biz_attachment_rule.doc_type IS
    '单据类型（与 attachment::model::DocKind 字面值逐字对应）';
COMMENT ON COLUMN biz_attachment_rule.file_role IS
    '指定文件角色（FileRole 字面值）；NULL = 任意角色都算';
COMMENT ON COLUMN biz_attachment_rule.required_at IS
    '触发动作：SUBMIT 提交 / CONFIRM 确认 / ACTIVATE 生效（入账 / 出款）';
COMMENT ON COLUMN biz_attachment_rule.min_count IS
    '至少要几个扩展名命中 allow_exts 的附件（≥ 1）';
COMMENT ON COLUMN biz_attachment_rule.max_count IS
    '最多允许几个（NULL = 不限）';
COMMENT ON COLUMN biz_attachment_rule.allow_exts IS
    '扩展名白名单，小写逗号分隔（pdf,ofd,xml）；NULL = 不限格式';
COMMENT ON COLUMN biz_attachment_rule.condition_sql IS
    '可选附加条件：对单据表当前行求值的 WHERE 片段（如 invoice_code IS NULL）；NULL = 恒适用';
COMMENT ON COLUMN biz_attachment_rule.remark IS
    '人话名（直接进错误文案：「{单据}必须上传{remark}（pdf/ofd/xml）」）';

-- 同一 (doc_type, required_at, file_role, condition_sql) 只允许一条存活规则：
-- 否则两条规则会给出互相矛盾的文案，而运维看不出哪条生效。
-- 用**表达式**部分唯一索引：NULL 在普通唯一索引里互不相等，必须 COALESCE 成空串才判重。
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_attachment_rule_key
    ON biz_attachment_rule (
        doc_type,
        required_at,
        (COALESCE(file_role, '')),
        (COALESCE(condition_sql, ''))
    )
    WHERE deleted_at IS NULL;

-- 运行时查询只走这一条：WHERE doc_type = ? AND required_at = ? AND enabled
CREATE INDEX IF NOT EXISTS idx_biz_attachment_rule_lookup
    ON biz_attachment_rule (doc_type, required_at)
    WHERE deleted_at IS NULL;

-- ============================================================
-- §3 预置规则（9 条；固定 id → 重放幂等）
-- ============================================================
INSERT INTO biz_attachment_rule (
    id, doc_type, file_role, required_at, min_count, max_count,
    allow_exts, condition_sql, enabled, remark
) VALUES
    -- 发票：数电票（无税控代码）只认 PDF / OFD / XML
    ('d3470001-0000-4000-8000-000000000001'::uuid, 'INVOICE', NULL, 'SUBMIT',
     1, NULL, 'pdf,ofd,xml', 'invoice_code IS NULL', TRUE, '发票影像'),
    -- 发票：纸质票（有税控代码）认扫描件
    ('d3470001-0000-4000-8000-000000000002'::uuid, 'INVOICE', NULL, 'SUBMIT',
     1, NULL, 'pdf,jpg,png', 'invoice_code IS NOT NULL', TRUE, '纸质发票扫描件'),
    -- 收款单：银行回单
    ('d3470001-0000-4000-8000-000000000003'::uuid, 'RECEIPT', NULL, 'SUBMIT',
     1, NULL, 'pdf,jpg,png', NULL, TRUE, '银行回单'),
    -- 付款单：出款前必须有银行回单（见文件头 §预置规则 的 required_at 说明）
    ('d3470001-0000-4000-8000-000000000004'::uuid, 'PAYMENT', NULL, 'ACTIVATE',
     1, NULL, 'pdf,jpg,png', NULL, TRUE, '银行回单'),
    -- 费用报销：发票影像（OFD 也算，电子发票常见）
    ('d3470001-0000-4000-8000-000000000005'::uuid, 'EXPENSE_REPORT', NULL, 'SUBMIT',
     1, NULL, 'pdf,jpg,png,ofd', NULL, TRUE, '发票影像或费用凭证'),
    -- 固定资产：入账（启用）时要发票 / 验收单
    ('d3470001-0000-4000-8000-000000000006'::uuid, 'FIXED_ASSET', NULL, 'ACTIVATE',
     1, NULL, 'pdf,jpg,png', NULL, TRUE, '发票或验收单'),
    -- 盘点单：确认（提交审批）时要盘点表 / 盈亏说明
    ('d3470001-0000-4000-8000-000000000007'::uuid, 'STOCK_TAKE', NULL, 'CONFIRM',
     1, NULL, 'pdf,jpg,png', NULL, TRUE, '盘点表或盈亏说明'),
    -- 采购退货：退货单 / 签收凭证
    ('d3470001-0000-4000-8000-000000000008'::uuid, 'PURCHASE_RETURN', NULL, 'SUBMIT',
     1, NULL, 'pdf,jpg,png', NULL, TRUE, '退货单或签收凭证'),
    -- 销售退货：退货单 / 物流凭证
    ('d3470001-0000-4000-8000-000000000009'::uuid, 'SALES_RETURN', NULL, 'SUBMIT',
     1, NULL, 'pdf,jpg,png', NULL, TRUE, '退货单或物流凭证')
ON CONFLICT DO NOTHING;

-- ============================================================
-- §4 自检（迁移失败要响：规则表建了但种子丢了 = 闸门静默失效）
-- ============================================================
DO $$
DECLARE
    n_rules int;
    v_missing text;
BEGIN
    -- 4.1 九条种子规则齐全且启用
    SELECT count(*) INTO n_rules
      FROM biz_attachment_rule
     WHERE deleted_at IS NULL AND enabled
       AND id IN ('d3470001-0000-4000-8000-000000000001'::uuid,
                  'd3470001-0000-4000-8000-000000000002'::uuid,
                  'd3470001-0000-4000-8000-000000000003'::uuid,
                  'd3470001-0000-4000-8000-000000000004'::uuid,
                  'd3470001-0000-4000-8000-000000000005'::uuid,
                  'd3470001-0000-4000-8000-000000000006'::uuid,
                  'd3470001-0000-4000-8000-000000000007'::uuid,
                  'd3470001-0000-4000-8000-000000000008'::uuid,
                  'd3470001-0000-4000-8000-000000000009'::uuid);
    IF n_rules <> 9 THEN
        RAISE EXCEPTION 'V0347 自检失败：预置附件规则只落库 % 条（期望 9）', n_rules;
    END IF;

    -- 4.2 八个单据类型全覆盖（INVOICE 两条分支）
    SELECT string_agg(v.doc_type || '/' || v.required_at, ', ') INTO v_missing
      FROM (VALUES ('INVOICE','SUBMIT'), ('RECEIPT','SUBMIT'), ('PAYMENT','ACTIVATE'),
                   ('EXPENSE_REPORT','SUBMIT'), ('FIXED_ASSET','ACTIVATE'),
                   ('STOCK_TAKE','CONFIRM'), ('PURCHASE_RETURN','SUBMIT'),
                   ('SALES_RETURN','SUBMIT')) AS v(doc_type, required_at)
     WHERE NOT EXISTS (SELECT 1 FROM biz_attachment_rule r
                        WHERE r.doc_type = v.doc_type
                          AND r.required_at = v.required_at
                          AND r.enabled AND r.deleted_at IS NULL);
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0347 自检失败：这些单据没有启用中的必传规则：%', v_missing;
    END IF;

    -- 4.3 发票的电子 / 纸质两条条件必须互补（两条都恒适用或都不适用 = 配错了）
    IF NOT EXISTS (SELECT 1 FROM biz_attachment_rule
                    WHERE doc_type = 'INVOICE' AND condition_sql = 'invoice_code IS NULL'
                      AND allow_exts = 'pdf,ofd,xml' AND enabled AND deleted_at IS NULL) THEN
        RAISE EXCEPTION 'V0347 自检失败：发票电子票规则（pdf,ofd,xml / invoice_code IS NULL）缺失或内容不符';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM biz_attachment_rule
                    WHERE doc_type = 'INVOICE' AND condition_sql = 'invoice_code IS NOT NULL'
                      AND allow_exts = 'pdf,jpg,png' AND enabled AND deleted_at IS NULL) THEN
        RAISE EXCEPTION 'V0347 自检失败：发票纸质票规则（pdf,jpg,png / invoice_code IS NOT NULL）缺失或内容不符';
    END IF;

    -- 4.4 附件类型 CHECK 必须真的放宽到 12 值（直接读约束定义，不试插 —— 试插会撞别的约束）
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_biz_doc_attachment_type'
           AND pg_get_constraintdef(oid) LIKE '%SALES_RETURN%'
           AND pg_get_constraintdef(oid) LIKE '%STOCK_TAKE%') THEN
        RAISE EXCEPTION 'V0347 自检失败：ck_biz_doc_attachment_type 未放宽到含 STOCK_TAKE / SALES_RETURN';
    END IF;
END $$;
