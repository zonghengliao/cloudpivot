-- ======================================================
-- V0309：销售员字段 + 佣金三表（SO-35，ARCH-7 §15.4.2）
--
--   ① biz_sales_order.salesperson_id uuid —— 这张订单算在哪个销售员头上（可空）
--      + 外键 fk_biz_sales_order_salesperson → sys_user(id) ON DELETE SET NULL
--      + 部分索引 idx_biz_sales_order_salesperson（「某销售员负责哪些订单」= SO-36 计提主查询路径）
--   ② biz_commission_rule        佣金规则（按销售员 / 物料类别 / 全局；百分比 / 固定额）
--   ③ biz_commission_settlement  佣金结算单（按 销售员 × 期间 汇总，DRAFT/APPROVED/PAID）
--   ④ biz_commission_item        佣金明细（哪笔订单的哪笔应收产生多少佣金）
--
-- ── 号段（开工实测复核）──────────────────────────────────
-- 卡面写的是 V0271，**已被占用**（V0271 属销售链既有号段，已应用迁移不可改、指纹校验会炸）。
-- 写入本文件时 backend/migrations/business/ 最大号 = V0308（SO-34 价格菜单），
-- 故本卡顺延取 **V0309**（权益/权限段取 V0310，同卡两个迁移：DDL 与授权分开，
-- 回滚粒度才干净）。提交前再 `ls backend/migrations/business | tail` 复核一次 ——
-- 迁移号会被并行卡抢走。
--
-- ── 为什么本文件建硬外键（与 ADR-06 「单据链零外键」的差异，单列说明）──
--   · biz_sales_order.salesperson_id / biz_commission_settlement.salesperson_id：
--     单态引用（只可能指向 sys_user），FK 能真实表达约束，且卡面逐字要求 FK。
--     `ON DELETE SET NULL` 而不是 CASCADE / RESTRICT：账号被物理清场时，
--     「订单 / 结算单」本体与金额一行不动，只把「算在谁头上」打个空
--     （CASCADE 会让删一个账号吞掉历史单据，RESTRICT 会让离职清号被历史单永deadlock）。
--     注意 sys_user 走软删（deleted_at），日常「删账号」不触发 SET NULL，
--     历史单据仍能显示当时的销售员 —— 这正是要的语义。
--   · biz_commission_item.settlement_id：硬外键 **ON DELETE CASCADE**（卡面点名）——
--     明细离开结算单无独立意义，与 biz_sales_order_item / biz_sales_return_item 同口径。
--   · biz_commission_item.so_id / receivable_id / rule_id：**弱引用（无外键）**，
--     按 ADR-06 单据链零外键基线（订单 / 应收可软删，规则可停用后重发版），
--     有效性由 SO-36 计提时校验。
--
-- ── 金额 / 比率列精度（卡面稿面 vs 仓内口径）──────────────
--   ARCH-7 §15.4.2 稿面写 total_* / min_amount / max_amount 为 numeric(18,2)，
--   **本仓实际口径是 NUMERIC(18,4)**（biz_sales_order / biz_receivable / biz_voucher_entry
--   全 18,4）。照稿面写 (18,2) 会在佣金基数与凭证对账时静默四舍五入（见
--   skill references/db-table-card-and-migration-probes.md §13），故按仓内口径落 18,4。
--   比率 rate 按卡面 NUMERIC(8,4)（百分比 0–100 或固定额，两种语义共用一列）。
--
-- ── 三条业务不变量（库层兜底，能表达的都表达）────────
--   1) 规则的 scope 与 scope_id 必须一致：ALL ⇒ scope_id IS NULL；
--      SALESPERSON / MATERIAL_CATEGORY ⇒ scope_id NOT NULL（否则规则匹配时会静默全命中）。
--   2) PERCENT ⇒ 0 ≤ rate ≤ 100；FIXED ⇒ rate ≥ 0（固定金额可以 >100 元）。
--   3) 结算单 (salesperson_id, period_code) 存活行唯一 —— **同一人同一期间只能有一张结算单**，
--      这是「重复计佣 = 重复发钱」的最后一道闸。需要重算时把旧行软删（deleted_at）
--      再生成，别绕过它去改 status。
--
-- ── 幂等 ──────────────────────────────────────────────
-- ALTER ... IF NOT EXISTS / CREATE TABLE IF NOT EXISTS / CREATE INDEX IF NOT EXISTS；
-- 外键与 CHECK 用 DO 块做存在性守卫。末尾 DO 块自检：列数 / 卡面列 / 外键口径 /
-- 唯一索引语义 / CHECK 域值 / 行数探针 / 行为探针（坏数据必须被拒）/ 探针清场。
-- 文件内**不写** BEGIN/COMMIT（迁移器自己开事务）。
-- ======================================================

-- ---------- ① biz_sales_order.salesperson_id ----------
ALTER TABLE biz_sales_order
    ADD COLUMN IF NOT EXISTS salesperson_id uuid;

COMMENT ON COLUMN biz_sales_order.salesperson_id IS
    '销售员（sys_user.id）。NULL = 未指定销售员（公司单 / 历史单，不回填 —— 回填等于编造审计事实）。'
    '建单 / 改单时由 handler 校验「用户存在且启用」；佣金计提（SO-36）只统计本列非空的订单';

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'fk_biz_sales_order_salesperson') THEN
        ALTER TABLE biz_sales_order
            ADD CONSTRAINT fk_biz_sales_order_salesperson
            FOREIGN KEY (salesperson_id) REFERENCES sys_user (id) ON DELETE SET NULL;
    END IF;
END $$;

-- 部分索引：绝大多数历史订单 salesperson_id 为 NULL，只为有值的行建条目。
-- 排序键 order_date DESC 与 SO-36「按期间取某销售员的订单」的扫描顺序一致。
CREATE INDEX IF NOT EXISTS idx_biz_sales_order_salesperson
    ON biz_sales_order (salesperson_id, order_date DESC)
    WHERE salesperson_id IS NOT NULL;

-- ---------- ② biz_commission_rule（佣金规则） ----------
CREATE TABLE IF NOT EXISTS biz_commission_rule (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),           -- 主键
    rule_name      VARCHAR(100)  NOT NULL,                               -- 规则名（人读）
    scope_type     VARCHAR(24)   NOT NULL,                               -- SALESPERSON / MATERIAL_CATEGORY / ALL
    scope_id       UUID,                                                 -- 销售员 id 或物料类别 id（ALL 时为 NULL）
    calc_type      VARCHAR(16)   NOT NULL,                               -- PERCENT（百分比）/ FIXED（固定额）
    rate           NUMERIC(8,4)  NOT NULL DEFAULT 0,                     -- PERCENT: 百分数 0–100；FIXED: 金额（≥0）
    min_amount     NUMERIC(18,4) NOT NULL DEFAULT 0,                     -- 起算金额（佣金基数低于此值不计佣）
    max_amount     NUMERIC(18,4),                                        -- 单笔佣金封顶（NULL = 不封顶）
    effective_from DATE          NOT NULL DEFAULT CURRENT_DATE,          -- 生效起始日（含）
    effective_to   DATE,                                                 -- 生效截止日（含；NULL = 长期有效）
    status         VARCHAR(16)   NOT NULL DEFAULT 'ACTIVE',              -- ACTIVE / INACTIVE
    remark         TEXT,                                                 -- 备注
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,     -- 审计：创建时间
    updated_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,     -- 审计：更新时间（model 层维护）
    create_by      UUID,                                                 -- 审计：创建人
    update_by      UUID,                                                 -- 审计：更新人
    deleted_at     TIMESTAMPTZ,                                          -- 软删时间（NULL = 存活）

    CONSTRAINT ck_biz_cmrule_scope
        CHECK (scope_type IN ('SALESPERSON','MATERIAL_CATEGORY','ALL')),
    -- scope 与 scope_id 必须一致（见头注释不变量 1）
    CONSTRAINT ck_biz_cmrule_scope_id
        CHECK (
            (scope_type = 'ALL' AND scope_id IS NULL)
            OR (scope_type IN ('SALESPERSON','MATERIAL_CATEGORY') AND scope_id IS NOT NULL)
        ),
    CONSTRAINT ck_biz_cmrule_calc
        CHECK (calc_type IN ('PERCENT','FIXED')),
    -- 百分比 0–100；固定额只要非负（见头注释不变量 2）
    CONSTRAINT ck_biz_cmrule_rate
        CHECK (rate >= 0 AND (calc_type <> 'PERCENT' OR rate <= 100)),
    CONSTRAINT ck_biz_cmrule_amount
        CHECK (min_amount >= 0 AND (max_amount IS NULL OR max_amount >= min_amount)),
    CONSTRAINT ck_biz_cmrule_effective
        CHECK (effective_to IS NULL OR effective_to >= effective_from),
    CONSTRAINT ck_biz_cmrule_status
        CHECK (status IN ('ACTIVE','INACTIVE'))
);

COMMENT ON TABLE  biz_commission_rule IS '佣金规则（ARCH-7 §15.4.2 ②）：按销售员 / 物料类别 / 全局，百分比或固定额';
COMMENT ON COLUMN biz_commission_rule.scope_type IS '适用范围：SALESPERSON(按销售员) / MATERIAL_CATEGORY(按物料类别) / ALL(全局)';
COMMENT ON COLUMN biz_commission_rule.scope_id IS '范围对象 id：SALESPERSON ⇒ sys_user.id；MATERIAL_CATEGORY ⇒ biz_material_category.id；ALL ⇒ NULL（ck_biz_cmrule_scope_id 强制）';
COMMENT ON COLUMN biz_commission_rule.calc_type IS '计算方式：PERCENT(按基数百分比) / FIXED(固定金额，与基数无关)';
COMMENT ON COLUMN biz_commission_rule.rate IS 'PERCENT ⇒ 百分数 0–100；FIXED ⇒ 金额（NUMERIC(8,4)，≥0 不设上限）';
COMMENT ON COLUMN biz_commission_rule.max_amount IS '单笔佣金封顶（NULL = 不封顶）；计提时 commission = LEAST(commission, max_amount)';
COMMENT ON COLUMN biz_commission_rule.status IS 'ACTIVE(启用) / INACTIVE(停用)：停用即不再参与匹配，但历史明细仍引用它';
COMMENT ON COLUMN biz_commission_rule.deleted_at IS '软删时间；规则停用应改 status，软删只用于「录错了」';

CREATE INDEX IF NOT EXISTS idx_biz_cmrule_scope
    ON biz_commission_rule (scope_type, scope_id) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_cmrule_effective
    ON biz_commission_rule (effective_from, effective_to) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_cmrule_status
    ON biz_commission_rule (status) WHERE deleted_at IS NULL;

-- ---------- ③ biz_commission_settlement（佣金结算单） ----------
CREATE TABLE IF NOT EXISTS biz_commission_settlement (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),         -- 主键
    settlement_no    VARCHAR(32)   NOT NULL,                             -- 结算单号 CM-yyyyMM-000001（存活行唯一）
    salesperson_id   UUID          NOT NULL,                             -- 销售员（FK sys_user，ON DELETE SET NULL）
    period_code      VARCHAR(8)    NOT NULL,                             -- 结算期间 yyyy-MM
    total_sales      NUMERIC(18,4) NOT NULL DEFAULT 0,                   -- 期间销售额（已收款口径，见 §15.4.3）
    total_commission NUMERIC(18,4) NOT NULL DEFAULT 0,                   -- 应发佣金合计（退货冲减可为负）
    status           VARCHAR(16)   NOT NULL DEFAULT 'DRAFT',             -- DRAFT / APPROVED / PAID
    paid_at          TIMESTAMPTZ,                                        -- 实际发放时间（PAID 必须非空）
    voucher_id       UUID,                                               -- 佣金计提凭证（弱引用 biz_voucher.id）
    remark           TEXT,                                               -- 备注
    created_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,   -- 审计：创建时间
    updated_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,   -- 审计：更新时间（model 层维护）
    create_by        UUID,                                               -- 审计：创建人
    update_by        UUID,                                               -- 审计：更新人
    deleted_at       TIMESTAMPTZ,                                        -- 软删时间（NULL = 存活）

    CONSTRAINT fk_biz_commission_settlement_salesperson
        FOREIGN KEY (salesperson_id) REFERENCES sys_user (id) ON DELETE SET NULL,
    CONSTRAINT ck_biz_cmset_status
        CHECK (status IN ('DRAFT','APPROVED','PAID')),
    -- 期间格式 yyyy-MM（月份 01–12）：SO-36 按 period_code 汇总，脏值会让汇总静默落空
    CONSTRAINT ck_biz_cmset_period
        CHECK (period_code ~ '^[0-9]{4}-(0[1-9]|1[0-2])$'),
    -- PAID ⇔ paid_at 非空（发放必须留时间证据；DRAFT/APPROVED 不许提前填发放时间）
    CONSTRAINT ck_biz_cmset_paid
        CHECK ((status = 'PAID') = (paid_at IS NOT NULL)),
    -- 期间销售额非负；佣金合计允许负（退货冲减可能冲成负数）
    CONSTRAINT ck_biz_cmset_total_sales
        CHECK (total_sales >= 0)
);

COMMENT ON TABLE  biz_commission_settlement IS '佣金结算单（ARCH-7 §15.4.2 ③）：按 销售员 × 期间 汇总，DRAFT→APPROVED→PAID';
COMMENT ON COLUMN biz_commission_settlement.settlement_no IS '结算单号 CM-yyyyMM-000001（biz_seq seq_type=CM 由 SO-36 取号；存活行唯一）';
COMMENT ON COLUMN biz_commission_settlement.salesperson_id IS '销售员（FK sys_user ON DELETE SET NULL：账号清场后结算单仍可读）';
COMMENT ON COLUMN biz_commission_settlement.period_code IS '结算期间 yyyy-MM（ck_biz_cmset_period 强制格式）';
COMMENT ON COLUMN biz_commission_settlement.total_sales IS '期间佣金基数合计 = 已核销（收到钱）的应收金额，不是开单金额（ARCH-7 §15.4.3）';
COMMENT ON COLUMN biz_commission_settlement.total_commission IS '应发佣金合计（= 明细 commission 之和；退货冲减可为负）';
COMMENT ON COLUMN biz_commission_settlement.paid_at IS '实际发放时间；ck_biz_cmset_paid 保证 status = PAID ⇔ paid_at 非空';
COMMENT ON COLUMN biz_commission_settlement.voucher_id IS '佣金计提凭证 id（弱引用 biz_voucher.id；借 6601 / 贷 2241，由 SO-36 生成）';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cmset_no
    ON biz_commission_settlement (settlement_no) WHERE deleted_at IS NULL;
-- 「同一人同一期间只能有一张存活结算单」（头注释不变量 3）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cmset_period
    ON biz_commission_settlement (salesperson_id, period_code) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_cmset_salesperson
    ON biz_commission_settlement (salesperson_id, period_code DESC) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_cmset_status
    ON biz_commission_settlement (status) WHERE deleted_at IS NULL;

-- ---------- ④ biz_commission_item（佣金明细） ----------
CREATE TABLE IF NOT EXISTS biz_commission_item (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),            -- 主键
    settlement_id UUID          NOT NULL,                                -- 所属结算单（硬外键 CASCADE）
    so_id         UUID,                                                  -- 产生佣金的销售订单（弱引用）
    receivable_id UUID,                                                  -- 对应的应收单（弱引用）
    base_amount   NUMERIC(18,4) NOT NULL DEFAULT 0,                      -- 佣金基数（该应收已核销金额）
    rule_id       UUID,                                                  -- 命中的佣金规则（弱引用 biz_commission_rule.id）
    rate          NUMERIC(8,4)  NOT NULL DEFAULT 0,                      -- 命中的比率快照（PERCENT 或 FIXED 金额）
    commission    NUMERIC(18,4) NOT NULL DEFAULT 0,                      -- 本行佣金（可为负：退货冲减）
    remark        TEXT,                                                  -- 行备注（如「退货冲减 SO-xxx」）
    created_at    TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,      -- 审计：创建时间
    updated_at    TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,      -- 审计：更新时间

    CONSTRAINT fk_biz_cmitem_settlement
        FOREIGN KEY (settlement_id) REFERENCES biz_commission_settlement (id) ON DELETE CASCADE,
    CONSTRAINT ck_biz_cmitem_base
        CHECK (base_amount >= 0),
    CONSTRAINT ck_biz_cmitem_rate
        CHECK (rate >= 0)
);

COMMENT ON TABLE  biz_commission_item IS '佣金明细（ARCH-7 §15.4.2 ④）：哪笔订单 / 应收产生多少佣金（settlement_id CASCADE）';
COMMENT ON COLUMN biz_commission_item.base_amount IS '佣金基数：该应收「已核销」金额（按收款计佣，不是开单金额）';
COMMENT ON COLUMN biz_commission_item.rate IS '命中规则的比率快照：规则后续改版 / 停用不影响已结算明细';
COMMENT ON COLUMN biz_commission_item.commission IS '本行佣金 = f(base_amount, rate, min/max_amount)；退货冲减行可为负';
COMMENT ON COLUMN biz_commission_item.so_id IS '来源销售订单（弱引用 biz_sales_order.id；ADR-06 单据链零外键）';

CREATE INDEX IF NOT EXISTS idx_biz_cmitem_settlement
    ON biz_commission_item (settlement_id);
CREATE INDEX IF NOT EXISTS idx_biz_cmitem_so
    ON biz_commission_item (so_id) WHERE so_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_biz_cmitem_receivable
    ON biz_commission_item (receivable_id) WHERE receivable_id IS NOT NULL;

-- ---------- ⑤ 自检：结构 + 外键口径 + 域值 + 行为探针 + 清场 ----------
DO $$
DECLARE
    n        INT;
    d        TEXT;
    probe_rule   UUID := 'd0309000-0000-4000-8000-000000000001'::uuid;
    probe_settle UUID := 'd0309000-0000-4000-8000-000000000002'::uuid;
    probe_user   UUID;
    probe_order  UUID := 'd0309000-0000-4000-8000-000000000003'::uuid;
BEGIN
    -- 5.1 四张表 / 一列都在
    IF to_regclass('public.biz_commission_rule') IS NULL
       OR to_regclass('public.biz_commission_settlement') IS NULL
       OR to_regclass('public.biz_commission_item') IS NULL THEN
        RAISE EXCEPTION 'V0309 自检失败：佣金三表未建全';
    END IF;
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_sales_order'
       AND column_name = 'salesperson_id' AND data_type = 'uuid';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0309 自检失败：biz_sales_order.salesperson_id 不存在或不是 uuid';
    END IF;

    -- 5.2 列数逐表比对（多一列少一列都算漂移）
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_commission_rule';
    IF n <> 17 THEN
        RAISE EXCEPTION 'V0309 自检失败：biz_commission_rule 列数 = %（期望 17）', n;
    END IF;
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_commission_settlement';
    IF n <> 15 THEN
        RAISE EXCEPTION 'V0309 自检失败：biz_commission_settlement 列数 = %（期望 15）', n;
    END IF;
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_commission_item';
    IF n <> 11 THEN
        RAISE EXCEPTION 'V0309 自检失败：biz_commission_item 列数 = %（期望 11）', n;
    END IF;

    -- 5.3 卡面点名的列必须逐字在位
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_commission_rule'
       AND column_name IN ('id','rule_name','scope_type','scope_id','calc_type','rate',
                           'min_amount','max_amount','effective_from','effective_to','status',
                           'remark','created_at','updated_at','create_by','update_by','deleted_at');
    IF n <> 17 THEN
        RAISE EXCEPTION 'V0309 自检失败：biz_commission_rule 卡面列只齐了 %/17', n;
    END IF;
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_commission_settlement'
       AND column_name IN ('id','settlement_no','salesperson_id','period_code','total_sales',
                           'total_commission','status','paid_at','voucher_id','remark',
                           'created_at','updated_at','create_by','update_by','deleted_at');
    IF n <> 15 THEN
        RAISE EXCEPTION 'V0309 自检失败：biz_commission_settlement 卡面列只齐了 %/15', n;
    END IF;
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_commission_item'
       AND column_name IN ('id','settlement_id','so_id','receivable_id','base_amount',
                           'rule_id','rate','commission','remark','created_at','updated_at');
    IF n <> 11 THEN
        RAISE EXCEPTION 'V0309 自检失败：biz_commission_item 卡面列只齐了 %/11', n;
    END IF;

    -- 5.4 金额列必须是仓内口径 NUMERIC(18,4)
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public'
       AND ((table_name = 'biz_commission_rule'
             AND column_name IN ('min_amount','max_amount'))
         OR (table_name = 'biz_commission_settlement'
             AND column_name IN ('total_sales','total_commission'))
         OR (table_name = 'biz_commission_item'
             AND column_name IN ('base_amount','commission')))
       AND data_type = 'numeric' AND numeric_precision = 18 AND numeric_scale = 4;
    IF n <> 6 THEN
        RAISE EXCEPTION 'V0309 自检失败：金额列 NUMERIC(18,4) 只有 %/6（卡面写 18,2，本仓口径 18,4）', n;
    END IF;

    -- 5.5 外键口径：订单 1 条（salesperson，SET NULL）；结算单 1 条（salesperson）：
    --     明细 1 条（settlement，CASCADE）；规则 0 条
    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid = 'public.biz_commission_rule'::regclass AND contype = 'f';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0309 自检失败：biz_commission_rule 出现 % 条外键（应为 0：scope_id 是多态弱引用）', n;
    END IF;
    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid = 'public.biz_commission_settlement'::regclass AND contype = 'f';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0309 自检失败：biz_commission_settlement 外键条数 = %（期望 1：仅 salesperson_id）', n;
    END IF;
    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid = 'public.biz_commission_item'::regclass AND contype = 'f';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0309 自检失败：biz_commission_item 外键条数 = %（期望 1：仅 settlement_id）', n;
    END IF;
    SELECT pg_get_constraintdef(oid) INTO d FROM pg_constraint
     WHERE conname = 'fk_biz_cmitem_settlement' AND contype = 'f';
    IF d IS NULL OR d NOT LIKE '%REFERENCES biz_commission_settlement(id)%' OR d NOT LIKE '%ON DELETE CASCADE%' THEN
        RAISE EXCEPTION 'V0309 自检失败：明细→结算单外键缺失或不是 CASCADE（def=%）', d;
    END IF;
    SELECT pg_get_constraintdef(oid) INTO d FROM pg_constraint
     WHERE conname = 'fk_biz_sales_order_salesperson' AND contype = 'f';
    IF d IS NULL OR d NOT LIKE '%REFERENCES sys_user(id)%' OR d NOT LIKE '%ON DELETE SET NULL%' THEN
        RAISE EXCEPTION 'V0309 自检失败：订单→销售员外键缺失或不是 SET NULL（def=%）', d;
    END IF;
    SELECT pg_get_constraintdef(oid) INTO d FROM pg_constraint
     WHERE conname = 'fk_biz_commission_settlement_salesperson' AND contype = 'f';
    IF d IS NULL OR d NOT LIKE '%REFERENCES sys_user(id)%' OR d NOT LIKE '%ON DELETE SET NULL%' THEN
        RAISE EXCEPTION 'V0309 自检失败：结算单→销售员外键缺失或不是 SET NULL（def=%）', d;
    END IF;

    -- 5.6 索引在位：订单销售员部分索引 + 结算单「单号 / 人×期间」两个唯一索引
    SELECT indexdef INTO d FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_sales_order'
       AND indexname = 'idx_biz_sales_order_salesperson';
    IF d IS NULL OR d NOT LIKE '%salesperson_id IS NOT NULL%' THEN
        RAISE EXCEPTION 'V0309 自检失败：idx_biz_sales_order_salesperson 缺失或不是部分索引（def=%）', d;
    END IF;
    SELECT indexdef INTO d FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_commission_settlement'
       AND indexname = 'uk_biz_cmset_no';
    IF d IS NULL OR d NOT LIKE '%UNIQUE%' OR d NOT LIKE '%deleted_at IS NULL%' THEN
        RAISE EXCEPTION 'V0309 自检失败：uk_biz_cmset_no 不是「存活行唯一」的部分索引（def=%）', d;
    END IF;
    SELECT indexdef INTO d FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_commission_settlement'
       AND indexname = 'uk_biz_cmset_period';
    IF d IS NULL OR d NOT LIKE '%UNIQUE%' OR d NOT LIKE '%salesperson_id, period_code%' THEN
        RAISE EXCEPTION 'V0309 自检失败：uk_biz_cmset_period 缺失或列序不对（def=%）', d;
    END IF;

    -- 5.7 默认值：规则 status=ACTIVE / 结算单 status=DRAFT / 计金列 = 0
    SELECT column_default INTO d FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_commission_rule' AND column_name = 'status';
    IF d IS NULL OR d NOT LIKE '''ACTIVE''%' THEN
        RAISE EXCEPTION 'V0309 自检失败：规则 status 默认值不是 ACTIVE（default=%）', d;
    END IF;
    SELECT column_default INTO d FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_commission_settlement' AND column_name = 'status';
    IF d IS NULL OR d NOT LIKE '''DRAFT''%' THEN
        RAISE EXCEPTION 'V0309 自检失败：结算单 status 默认值不是 DRAFT（default=%）', d;
    END IF;
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name IN ('biz_commission_rule','biz_commission_settlement')
       AND column_name IN ('rate','min_amount','total_sales','total_commission')
       AND column_default LIKE '0%';
    IF n <> 4 THEN
        RAISE EXCEPTION 'V0309 自检失败：金额 / 比率列默认值不是 0（齐了 %/4）', n;
    END IF;

    -- 5.8 行为探针 —— 坏数据必须被库层拒绝
    --     5.8a scope_type 越界
    BEGIN
        INSERT INTO biz_commission_rule (rule_name, scope_type, calc_type, rate)
        VALUES ('__V0309_PROBE_SCOPE__', 'CUSTOMER', 'PERCENT', 5);
        RAISE EXCEPTION 'V0309 自检失败：scope_type = CUSTOMER 未被 ck_biz_cmrule_scope 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    --     5.8b ALL 却给了 scope_id
    BEGIN
        INSERT INTO biz_commission_rule (rule_name, scope_type, scope_id, calc_type, rate)
        VALUES ('__V0309_PROBE_ALL__', 'ALL', gen_random_uuid(), 'PERCENT', 5);
        RAISE EXCEPTION 'V0309 自检失败：ALL + scope_id 未被 ck_biz_cmrule_scope_id 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    --     5.8c SALESPERSON 却没给 scope_id
    BEGIN
        INSERT INTO biz_commission_rule (rule_name, scope_type, calc_type, rate)
        VALUES ('__V0309_PROBE_NOSCOPE__', 'SALESPERSON', 'PERCENT', 5);
        RAISE EXCEPTION 'V0309 自检失败：SALESPERSON 缺 scope_id 未被 ck_biz_cmrule_scope_id 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    --     5.8d 百分比 150%
    BEGIN
        INSERT INTO biz_commission_rule (rule_name, scope_type, calc_type, rate)
        VALUES ('__V0309_PROBE_RATE__', 'ALL', 'PERCENT', 150);
        RAISE EXCEPTION 'V0309 自检失败：PERCENT rate = 150 未被 ck_biz_cmrule_rate 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    --     5.8e 生效区间倒挂
    BEGIN
        INSERT INTO biz_commission_rule (rule_name, scope_type, calc_type, rate, effective_from, effective_to)
        VALUES ('__V0309_PROBE_DATE__', 'ALL', 'PERCENT', 5, DATE '2026-12-01', DATE '2026-01-01');
        RAISE EXCEPTION 'V0309 自检失败：effective_to < effective_from 未被 ck_biz_cmrule_effective 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    --     5.8f 正常规则（FIXED，rate > 100 允许 —— 固定金额不是百分比）
    INSERT INTO biz_commission_rule (id, rule_name, scope_type, calc_type, rate, min_amount, max_amount)
    VALUES (probe_rule, '__V0309_PROBE_OK__', 'ALL', 'FIXED', 500, 0, 1000);
    SELECT status || '|' || calc_type || '|' || rate::text INTO d
      FROM biz_commission_rule WHERE id = probe_rule;
    IF d <> 'ACTIVE|FIXED|500.0000' THEN
        RAISE EXCEPTION 'V0309 自检失败：规则默认值 / 固定额落库 = %（期望 ACTIVE|FIXED|500.0000）', d;
    END IF;

    --     5.8g 期间格式非法
    BEGIN
        INSERT INTO biz_commission_settlement (settlement_no, salesperson_id, period_code, total_sales, total_commission)
        VALUES ('__V0309_PROBE_PERIOD__', gen_random_uuid(), '2026-13', 0, 0);
        RAISE EXCEPTION 'V0309 自检失败：period_code = 2026-13 未被 ck_biz_cmset_period 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    --     5.8h PAID 却没给 paid_at
    BEGIN
        INSERT INTO biz_commission_settlement (settlement_no, salesperson_id, period_code, status)
        VALUES ('__V0309_PROBE_PAID__', gen_random_uuid(), '2026-11', 'PAID');
        RAISE EXCEPTION 'V0309 自检失败：status = PAID 且 paid_at 空未被 ck_biz_cmset_paid 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    --     5.8i 销售员指向不存在的 sys_user → 必须被外键拦住
    BEGIN
        INSERT INTO biz_commission_settlement (settlement_no, salesperson_id, period_code)
        VALUES ('__V0309_PROBE_FK__', '00000000-0000-4000-8000-0000ffffffff'::uuid, '2026-11');
        RAISE EXCEPTION 'V0309 自检失败：salesperson_id 指向不存在的账号却插入成功';
    EXCEPTION WHEN foreign_key_violation THEN NULL;
    END;

    --     5.8j 正常结算单（取一个真实账号；没有账号时跳过该探针并播报）
    SELECT id INTO probe_user FROM sys_user WHERE deleted_at IS NULL ORDER BY created_at LIMIT 1;
    IF probe_user IS NULL THEN
        RAISE NOTICE 'V0309 自检：库里没有任何 sys_user 行，跳过「真实销售员」探针';
    ELSE
        INSERT INTO biz_commission_settlement (id, settlement_no, salesperson_id, period_code,
                                               total_sales, total_commission)
        VALUES (probe_settle, '__V0309_PROBE_SETTLE__', probe_user, '2026-11', 10000, 300);
        SELECT status || '|' || total_sales::text || '|' || coalesce(paid_at::text,'') INTO d
          FROM biz_commission_settlement WHERE id = probe_settle;
        IF d <> 'DRAFT|10000.0000|' THEN
            RAISE EXCEPTION 'V0309 自检失败：结算单默认值落库 = %（期望 DRAFT|10000.0000|）', d;
        END IF;

        -- 同一人同一期间再插一张存活单 → 必须被唯一索引拦住
        BEGIN
            INSERT INTO biz_commission_settlement (settlement_no, salesperson_id, period_code)
            VALUES ('__V0309_PROBE_DUP__', probe_user, '2026-11');
            RAISE EXCEPTION 'V0309 自检失败：同销售员同期间第二张存活结算单未被 uk_biz_cmset_period 拒绝';
        EXCEPTION WHEN unique_violation THEN NULL;
        END;

        -- 5.8k 明细：正常行（含负佣金 = 退货冲减）+ 坏外键
        INSERT INTO biz_commission_item (settlement_id, so_id, receivable_id, base_amount, rule_id, rate, commission)
        VALUES (probe_settle, gen_random_uuid(), gen_random_uuid(), 10000, probe_rule, 3, 300),
               (probe_settle, gen_random_uuid(), NULL, 500, probe_rule, 3, -15);
        SELECT count(*) INTO n FROM biz_commission_item WHERE settlement_id = probe_settle;
        IF n <> 2 THEN
            RAISE EXCEPTION 'V0309 自检失败：明细落库 % 行（期望 2，含 1 行负佣金退货冲减）', n;
        END IF;

        BEGIN
            INSERT INTO biz_commission_item (settlement_id, base_amount, rate, commission)
            VALUES ('00000000-0000-4000-8000-0000ffffffff'::uuid, 1, 1, 1);
            RAISE EXCEPTION 'V0309 自检失败：明细挂到不存在的结算单却插入成功';
        EXCEPTION WHEN foreign_key_violation THEN NULL;
        END;

        -- 5.8l 结算单硬删 → 明细必须一起消失（探针明细由此清场）
        DELETE FROM biz_commission_settlement WHERE id = probe_settle;
        SELECT count(*) INTO n FROM biz_commission_item WHERE settlement_id = probe_settle;
        IF n <> 0 THEN
            RAISE EXCEPTION 'V0309 自检失败：结算单删除后明细仍剩 % 行（ON DELETE CASCADE 未生效）', n;
        END IF;

        -- 5.8m 订单挂销售员：坏 id 被外键拒，真账号可挂（探针单随即清场）
        BEGIN
            INSERT INTO biz_sales_order (so_no, customer_id, order_date, salesperson_id)
            VALUES ('__V0309_PROBE_SO_BAD__', gen_random_uuid(), DATE '2026-11-07',
                    '00000000-0000-4000-8000-0000ffffffff'::uuid);
            RAISE EXCEPTION 'V0309 自检失败：订单挂不存在的销售员却插入成功';
        EXCEPTION WHEN foreign_key_violation THEN NULL;
        END;

        INSERT INTO biz_sales_order (id, so_no, customer_id, order_date, salesperson_id)
        VALUES (probe_order, '__V0309_PROBE_SO__', gen_random_uuid(), DATE '2026-11-07', probe_user);
        SELECT salesperson_id INTO d FROM biz_sales_order WHERE id = probe_order;
        IF d IS DISTINCT FROM probe_user::text THEN
            RAISE EXCEPTION 'V0309 自检失败：订单 salesperson_id 读回 = %（期望 %）', d, probe_user;
        END IF;
        DELETE FROM biz_sales_order WHERE id = probe_order;
    END IF;

    -- 5.9 清场：探针行全部消失
    DELETE FROM biz_commission_rule WHERE rule_name LIKE '__V0309%';
    DELETE FROM biz_commission_settlement WHERE settlement_no LIKE '__V0309%';
    SELECT (SELECT count(*) FROM biz_commission_rule WHERE rule_name LIKE '__V0309%')
         + (SELECT count(*) FROM biz_commission_settlement WHERE settlement_no LIKE '__V0309%')
         + (SELECT count(*) FROM biz_sales_order WHERE so_no LIKE '__V0309%')
      INTO n;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0309 自检失败：探针残留 % 行', n;
    END IF;

    RAISE NOTICE 'V0309 完成：salesperson_id + 佣金三表就位；存量订单 % 张（已关联销售员 % 张，不回填）',
                 (SELECT count(*) FROM biz_sales_order),
                 (SELECT count(*) FROM biz_sales_order WHERE salesperson_id IS NOT NULL);
END $$;
