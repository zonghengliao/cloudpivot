-- ============================================================================
-- V0439：销售返利结算单（M3-16 —— 返利「计算」（月度 / 季度结算））
--
--   ① biz_sales_rebate_settlement —— 返利结算单（客户 × 结算粒度 × 期间，一期一张）
--   ② biz_seq(seq_type='RB')       —— 结算单号取号规则：RB-yyyyMMdd-000001（按天滚动）
--   ③ sys_menu                     —— 3 个按钮码（挂在「返利政策」页面节点 …4380 下）
--
-- ── 本卡算什么（一句话）─────────────────────────────────────────────────────
--   某客户在某个结算期间内的**净销售**（已确认订单 − 已过账退货），按该客户该粒度
--   **当日生效且覆盖整期**的返利政策落档，乘出应返金额，落一张结算单。
--
--   基数口径（与销售目标 / SO-40 的「订单口径」同一句话，别另立一套）：
--
--     销售额 = Σ 订单本位币含税金额（`base_total_with_tax`，为 0 时回落 `total_with_tax`）
--              − Σ 退货含税金额（按所属应收的「本位币 / 原币」比例折算）
--     销量   = Σ 订单明细 qty − Σ 退货明细 qty
--     订单白名单 = ('APPROVED','PARTIAL_OUT','OUTBOUND','CLOSED')（DRAFT / SUBMITTED 不算销售，
--                  CANCELLED 不算）；期间按 `biz_sales_order.order_date`
--     退货白名单 = status = 'POSTED'（未过账的退货还没冲减账，不能先减返利）；期间按 `return_date`
--
--   区间一律**左闭右开** `[period_from, period_to)`，与 `period_code` 的行区间同构：
--     `MONTH`   ⇒  `yyyy-MM`   ⇒  [当月 1 日, 次月 1 日)
--     `QUARTER` ⇒  `yyyy-QN`   ⇒  [季首 1 日, 次季 1 日)
--   粒度与期间码形状**互相钉死**（`ck_biz_srs_period_shape`）：月度政策不可能记成季度期间码。
--
-- ── 为什么结算单要存**政策快照**（不是只存 policy_id）──────────────────────
--   结算单是「按当时的条款算出来的结果」，要能回答「为什么是这个数」：
--   政策后来被改名、改档位、停用、软删之后，只看 policy_id 就回溯不出当时的口径。
--   因此快照三件套落库：
--     · 政策头快照：policy_no / policy_name / rule_type / rate_unit / 有效期（独立列，可筛选）
--     · 档位全量快照：`policy_snapshot` JSONB（`tiers` 数组，含每档 tier_from/tier_to/rate）
--     · 命中档快照：tier_seq / tier_from / tier_to / rate（结算时命中的那一档，平铺成列）
--
-- ── 三条不变量（库层能表达的都表达）──────────────────────────────────────
--   1) `(customer_id, settle_period, period_code)` **存活且未作废** 的行唯一
--      （`uk_biz_srs_period`，`WHERE deleted_at IS NULL AND status <> 'CANCELLED'`）
--      ⇒ 同一客户同一期间不可能被结算两次（重复返利 = 重复付钱）。
--      **作废后允许重开**：这条索引刻意排除 CANCELLED，否则「算错了只能改库」。
--   2) 金额 / 数量非负（`ck_biz_srs_amounts`）：净基数可能为负（退货多于销售）⇒
--      负基数落 `base_*` 原值，但 `rebate_amount` 恒 ≥ 0（负返利是红冲的事，不是结算的事）。
--   3) 计价单位与规则类型配对（`ck_biz_srs_unit_pair`）：与 V0438 的 `ck_biz_srbp_unit_pair`
--      同一条口径 —— 按量必是元/件、按额必是百分比、阶梯两者皆可。
--
-- ── 状态机（3 值，`ck_biz_srs_status`）────────────────────────────────────
--     DRAFT     —— 已计算落库；可重算（recalc）、可作废（DELETE）
--     SETTLED   —— 已转应付 / 红字发票（**M3-17 置位**）；不可重算、不可作废
--     CANCELLED —— 人工作废；行保留（审计可查），同期间可重开一张
--
-- ── 外键口径 ────────────────────────────────────────────────────────────
--   全部**弱引用（零外键）**，按 ADR-06「单据链零外键」基线：
--     customer_id → biz_customer.id（客户软删后历史结算单必须永远可读）
--     policy_id   → biz_sales_rebate_policy.id（政策可停用 / 软删，快照已存条款）
--   引用有效性由 handler 在事务内校验（客户须 ACTIVE、政策须覆盖整期）。
--
-- ── 幂等 / 回滚 ─────────────────────────────────────────────────────────
--   `CREATE TABLE IF NOT EXISTS` + 全部 `CREATE INDEX IF NOT EXISTS` +
--   菜单 `ON CONFLICT (id) DO NOTHING` + `biz_seq` `WHERE NOT EXISTS`；自检块可反复执行。
--   down：护栏（有未作废的结算单时**拒绝回滚** —— 删表等于把已算出来的返利从系统里抹掉）
--        → 删菜单（`sys_role_menu` 若有行先删）→ 删 biz_seq 行 → 删表。
--
-- ── 号段（开工实测复核）──────────────────────────────────────────────────
--   写入本文件时 `backend/app/migrations/**` 最大号 = V0438（sales_rebate_policy，
--   M3-15 提交 76aa69bd）。并行卡会抢号，**提交前**再
--   `ls backend/app/migrations/*/ | grep -oE 'V[0-9]{4}' | sort -u | tail -3` 复核
--   （跨目录全局唯一，重号 = 服务启动直接失败）。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 前置护栏：依赖的存量表在、父页面节点在、本卡 3 个码没被别人占用
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n_base   integer;
    n_parent integer;
    n_code   integer;
BEGIN
    SELECT count(*) INTO n_base
      FROM pg_class c JOIN pg_namespace ns ON ns.oid = c.relnamespace
     WHERE ns.nspname = current_schema() AND c.relkind = 'r'
       AND c.relname IN ('biz_sales_rebate_policy', 'biz_sales_rebate_rule',
                         'biz_sales_order', 'biz_sales_order_item',
                         'biz_sales_return', 'biz_sales_return_item',
                         'sys_menu', 'sys_role_menu', 'biz_seq');
    IF n_base <> 9 THEN
        RAISE EXCEPTION 'V0439: 前置表不全（实得 % 张 / 期望 9：返利政策两表 / 销售订单两表 / 退货两表 / 菜单两表 / 号段），请先应用 V0264 / V0303 / V0438', n_base;
    END IF;

    SELECT count(*) INTO n_parent
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004380'::uuid
       AND menu_type = 2
       AND deleted_at IS NULL;
    IF n_parent <> 1 THEN
        RAISE EXCEPTION 'V0439: 父页面节点「返利政策」(…4380) 不存在或不是页面节点，请先应用 V0438';
    END IF;

    SELECT count(*) INTO n_code
      FROM sys_menu
     WHERE auth_code IN ('business:erp:rebate-settlement:list',
                         'business:erp:rebate-settlement:calc',
                         'business:erp:rebate-settlement:delete')
       AND deleted_at IS NULL;
    IF n_code > 3 THEN
        RAISE EXCEPTION 'V0439: 返利结算三个码已有 % 个承载节点（应为 0，重放时 ≤ 3）', n_code;
    END IF;

    RAISE NOTICE 'V0439: 前置检查通过（9 张依赖表在、返利政策页面节点在、三个码当前 % 个承载节点）', n_code;
END $$;

-- ---------------------------------------------------------------------------
-- ② 表：`biz_sales_rebate_settlement`（返利结算单）
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_sales_rebate_settlement (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),   -- 主键
    settlement_no         VARCHAR(40)   NOT NULL,                       -- 结算单号 RB-yyyyMMdd-000001（存活行唯一）
    customer_id           UUID          NOT NULL,                       -- 客户（弱引用 biz_customer.id）
    settle_period         VARCHAR(16)   NOT NULL,                       -- MONTH 月度 / QUARTER 季度
    period_code           VARCHAR(8)    NOT NULL,                       -- 期间码 yyyy-MM 或 yyyy-QN
    period_from           DATE          NOT NULL,                       -- 期间首日（含）
    period_to             DATE          NOT NULL,                       -- 期间末日 +1（不含）：左闭右开
    policy_id             UUID          NOT NULL,                       -- 适用返利政策（弱引用 biz_sales_rebate_policy.id）
    policy_no             VARCHAR(40)   NOT NULL,                       -- 政策编号快照
    policy_name           VARCHAR(200)  NOT NULL,                       -- 政策名称快照
    rule_type             VARCHAR(16)   NOT NULL,                       -- 规则类型快照 BY_QTY / BY_AMOUNT / TIERED
    rate_unit             VARCHAR(16)   NOT NULL,                       -- 计价单位快照 PER_UNIT / PERCENT
    policy_effective_from DATE          NOT NULL,                       -- 政策生效日快照（含）
    policy_effective_to   DATE,                                         -- 政策失效日快照（含；NULL = 长期）
    policy_snapshot       JSONB         NOT NULL,                       -- 政策全量快照（含 tiers 数组：每档区间与比率）
    order_count           INTEGER       NOT NULL DEFAULT 0,             -- 期间内计入的已确认销售订单张数
    order_amount          NUMERIC(18,4) NOT NULL DEFAULT 0,             -- 订单本位币含税金额合计
    order_qty             NUMERIC(18,4) NOT NULL DEFAULT 0,             -- 订单明细数量合计
    return_count          INTEGER       NOT NULL DEFAULT 0,             -- 期间内计入的已过账退货张数
    return_amount         NUMERIC(18,4) NOT NULL DEFAULT 0,             -- 退货含税金额合计（折本位币）
    return_qty            NUMERIC(18,4) NOT NULL DEFAULT 0,             -- 退货明细数量合计
    base_amount           NUMERIC(18,4) NOT NULL DEFAULT 0,             -- 净销售额 = order_amount − return_amount
    base_qty              NUMERIC(18,4) NOT NULL DEFAULT 0,             -- 净销量   = order_qty − return_qty
    tier_seq              SMALLINT,                                     -- 命中档号（1 起；未命中为 NULL）
    tier_from             NUMERIC(18,4) NOT NULL DEFAULT 0,             -- 命中档下限快照（含）
    tier_to               NUMERIC(18,4),                                -- 命中档上限快照（不含；NULL = 无上限）
    rate                  NUMERIC(18,6) NOT NULL DEFAULT 0,             -- 命中档比率快照（元/件 或 百分数）
    rebate_amount         NUMERIC(18,4) NOT NULL DEFAULT 0,             -- 应返金额（分位；恒 ≥ 0）
    status                VARCHAR(16)   NOT NULL DEFAULT 'DRAFT',       -- DRAFT / SETTLED / CANCELLED
    remark                TEXT,                                         -- 备注（重算原因 / 作废原因）
    created_at            TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at            TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by             UUID,                                         -- 创建人（弱引用 sys_user.id）
    update_by             UUID,                                         -- 更新人
    deleted_at            TIMESTAMPTZ,                                  -- 软删标记（本表作废走 status，保留审计；列留给底座统一口径）

    CONSTRAINT ck_biz_srs_status
        CHECK (status IN ('DRAFT', 'SETTLED', 'CANCELLED')),
    CONSTRAINT ck_biz_srs_settle_period
        CHECK (settle_period IN ('MONTH', 'QUARTER')),
    -- 粒度 ↔ 期间码形状互相钉死：月度不可能记成 yyyy-QN，反之亦然
    CONSTRAINT ck_biz_srs_period_shape
        CHECK ((settle_period = 'MONTH'   AND period_code ~ '^[0-9]{4}-(0[1-9]|1[0-2])$')
            OR (settle_period = 'QUARTER' AND period_code ~ '^[0-9]{4}-[Qq][1-4]$')),
    CONSTRAINT ck_biz_srs_period_range
        CHECK (period_to > period_from),
    CONSTRAINT ck_biz_srs_rule_type
        CHECK (rule_type IN ('BY_QTY', 'BY_AMOUNT', 'TIERED')),
    CONSTRAINT ck_biz_srs_rate_unit
        CHECK (rate_unit IN ('PER_UNIT', 'PERCENT')),
    -- 计价单位与规则类型配对（与 V0438 的 ck_biz_srbp_unit_pair 同一条口径）
    CONSTRAINT ck_biz_srs_unit_pair
        CHECK ((rule_type = 'BY_QTY'    AND rate_unit = 'PER_UNIT')
            OR (rule_type = 'BY_AMOUNT' AND rate_unit = 'PERCENT')
            OR (rule_type = 'TIERED')),
    -- 金额 / 数量非负：净基数可负（退货多于销售），但各分项与应返金额不能为负
    CONSTRAINT ck_biz_srs_amounts
        CHECK (order_amount >= 0 AND order_qty >= 0 AND return_amount >= 0 AND return_qty >= 0
           AND rebate_amount >= 0 AND rate >= 0 AND tier_from >= 0),
    CONSTRAINT ck_biz_srs_tier
        CHECK ((tier_seq IS NULL OR tier_seq >= 1)
           AND (tier_to IS NULL OR tier_to > tier_from)),
    CONSTRAINT ck_biz_srs_counts
        CHECK (order_count >= 0 AND return_count >= 0)
);

COMMENT ON TABLE  biz_sales_rebate_settlement IS '返利结算单（M3-16；客户 × 结算粒度 × 期间，一期一张；存政策快照，可回溯「当时按什么算」）';
COMMENT ON COLUMN biz_sales_rebate_settlement.settlement_no IS '结算单号：RB-yyyyMMdd-000001（biz_seq seq_type=RB，按 CST 的天滚动）';
COMMENT ON COLUMN biz_sales_rebate_settlement.customer_id IS '客户 id（弱引用 biz_customer.id，无外键：客户软删后历史结算单仍可读）';
COMMENT ON COLUMN biz_sales_rebate_settlement.period_code IS '期间码：月度 yyyy-MM / 季度 yyyy-QN（ck_biz_srs_period_shape 与 settle_period 互钉）';
COMMENT ON COLUMN biz_sales_rebate_settlement.period_to IS '期间末日 +1（不含）：结算是左闭右开区间 [period_from, period_to)';
COMMENT ON COLUMN biz_sales_rebate_settlement.policy_id IS '适用返利政策 id（弱引用 biz_sales_rebate_policy.id；条款已快照，政策被改/停用/软删不影响本单）';
COMMENT ON COLUMN biz_sales_rebate_settlement.policy_snapshot IS '政策全量快照 JSONB：{policy_no,policy_name,rule_type,rate_unit,effective_from,effective_to,tiers:[{seq,tier_from,tier_to,rate,remark}]}';
COMMENT ON COLUMN biz_sales_rebate_settlement.order_amount IS '订单口径金额：本位币含税（base_total_with_tax，为 0 时回落 total_with_tax）';
COMMENT ON COLUMN biz_sales_rebate_settlement.return_amount IS '退货口径金额：含税，按所属应收的「本位币/原币」比例折算（无应收时按原币）';
COMMENT ON COLUMN biz_sales_rebate_settlement.base_amount IS '净销售额 = order_amount − return_amount（可能为负：退货多于销售）';
COMMENT ON COLUMN biz_sales_rebate_settlement.base_qty IS '净销量 = order_qty − return_qty（可能为负）';
COMMENT ON COLUMN biz_sales_rebate_settlement.rebate_amount IS '应返金额（2 位小数，恒 ≥ 0；负基数按 0 计，负返利是红冲的事不是结算的事）';
COMMENT ON COLUMN biz_sales_rebate_settlement.status IS 'DRAFT 已计算（可重算/可作废）/ SETTLED 已转应付或红字发票（M3-17 置位）/ CANCELLED 已作废';

-- 期间唯一：**存活且未作废**的行唯一 ⇒ 同客户同期间不会被结算两次（重复返利 = 重复付钱）。
-- 排除 CANCELLED 是刻意的：作废后必须能重开（否则算错一次就只能改库）。
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_srs_period
    ON biz_sales_rebate_settlement (customer_id, settle_period, period_code)
    WHERE deleted_at IS NULL AND status <> 'CANCELLED';

-- 单号唯一（存活行唯一，与 uk_biz_sales_rebate_policy_no 同口径）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_srs_no
    ON biz_sales_rebate_settlement (settlement_no) WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_biz_srs_status   ON biz_sales_rebate_settlement (status);
CREATE INDEX IF NOT EXISTS idx_biz_srs_period   ON biz_sales_rebate_settlement (settle_period, period_code);
CREATE INDEX IF NOT EXISTS idx_biz_srs_policy   ON biz_sales_rebate_settlement (policy_id);
CREATE INDEX IF NOT EXISTS idx_biz_srs_customer ON biz_sales_rebate_settlement (customer_id, period_code DESC);

-- ---------------------------------------------------------------------------
-- ③ 号段：`RB` 结算单号（幂等；缺失时补一行）
-- ---------------------------------------------------------------------------
INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key,
                     current_no, step, separator, status, remark)
SELECT gen_random_uuid(), 'RB', '返利结算单编号', 'RB', 'yyyyMMdd', '', 0, 1, '-', 1,
       '返利结算单编号：RB-yyyyMMdd-000001，按天（CST）滚动（V0439 · M3-16）'
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq q WHERE q.seq_type = 'RB' AND q.deleted_at IS NULL);

-- ---------------------------------------------------------------------------
-- ④ 菜单：3 个按钮（挂在「返利政策」页面节点 …4380 下；本迁移**不建新页面节点**）
--    理由：M3-19 的前端卡是「返利政策与结算页」一个页面（两个 tab），
--    结算的按钮码挂同一个页面节点下最贴近实际导航；也避免菜单总数继续长。
--    sort：40/50/60（同父已有 10/20/30 被政策的三个按钮占用，同父必须唯一）。
--    **本迁移不写 `sys_role_menu`**：把返利结算授给谁是管理决策，不静默扩权（与 V0438 同口径）。
-- ---------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000004390'::uuid, 'business:erp:rebate-settlement:list', 3,
     'business:erp:rebate-settlement:list', NULL,
     '20000000-0000-0000-0000-000000004380'::uuid, NULL,
     40, 1, NULL, '查看返利结算（列表 + 详情：期间净销售额/净销量、命中档、应返金额、政策快照）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000004391'::uuid, 'business:erp:rebate-settlement:calc', 3,
     'business:erp:rebate-settlement:calc', NULL,
     '20000000-0000-0000-0000-000000004380'::uuid, NULL,
     50, 1, NULL, '返利试算 / 计算落库 / 重算 / 整期跑批（按期间净销售落档乘出应返金额）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000004392'::uuid, 'business:erp:rebate-settlement:delete', 3,
     'business:erp:rebate-settlement:delete', NULL,
     '20000000-0000-0000-0000-000000004380'::uuid, NULL,
     60, 1, NULL, '作废返利结算（仅 DRAFT；已转应付/红字发票的结算单不可作废）',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------------------------------------------------------------------------
-- ⑤ 自检（任一不符即 RAISE EXCEPTION 回滚整个迁移）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n          integer;
    n_rows     integer;
    n_dup_sort integer;
    v_id       uuid;
    v_cus      uuid;
BEGIN
    -- ===== 5.1 表存在 / 列数 / 关键列类型 =====
    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = current_schema() AND table_name = 'biz_sales_rebate_settlement';
    IF n <> 35 THEN
        RAISE EXCEPTION 'V0439 自检失败：biz_sales_rebate_settlement 应有 35 列，实得 %', n;
    END IF;

    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = current_schema() AND table_name = 'biz_sales_rebate_settlement'
       AND (
             (column_name = 'settlement_no'         AND data_type = 'character varying' AND is_nullable = 'NO')
          OR (column_name = 'settle_period'         AND data_type = 'character varying' AND is_nullable = 'NO')
          OR (column_name = 'period_from'           AND data_type = 'date'              AND is_nullable = 'NO')
          OR (column_name = 'period_to'             AND data_type = 'date'              AND is_nullable = 'NO')
          OR (column_name = 'policy_id'             AND data_type = 'uuid'              AND is_nullable = 'NO')
          OR (column_name = 'policy_snapshot'       AND data_type = 'jsonb'             AND is_nullable = 'NO')
          OR (column_name = 'base_amount'           AND data_type = 'numeric'           AND numeric_scale = 4)
          OR (column_name = 'base_qty'              AND data_type = 'numeric'           AND numeric_scale = 4)
          OR (column_name = 'rate'                  AND data_type = 'numeric'           AND numeric_scale = 6)
          OR (column_name = 'rebate_amount'         AND data_type = 'numeric'           AND numeric_scale = 4)
          OR (column_name = 'tier_seq'              AND data_type = 'smallint'          AND is_nullable = 'YES')
          OR (column_name = 'status'                AND data_type = 'character varying' AND is_nullable = 'NO')
          OR (column_name = 'deleted_at'            AND data_type = 'timestamp with time zone' AND is_nullable = 'YES')
           );
    IF n <> 13 THEN
        RAISE EXCEPTION 'V0439 自检失败：关键 13 列的类型/可空性/小数位与设计不符（实得 % 列匹配）', n;
    END IF;

    -- ===== 5.2 三张索引在 =====
    SELECT count(*) INTO n
      FROM pg_indexes
     WHERE schemaname = current_schema() AND tablename = 'biz_sales_rebate_settlement'
       AND indexname IN ('uk_biz_srs_period', 'uk_biz_srs_no', 'idx_biz_srs_period');
    IF n <> 3 THEN
        RAISE EXCEPTION 'V0439 自检失败：三个关键索引应全在（实得 % 个）', n;
    END IF;

    -- ===== 5.3 CHECK 约束在（10 条）=====
    SELECT count(*) INTO n
      FROM pg_constraint c
      JOIN pg_class t ON t.oid = c.conrelid
      JOIN pg_namespace ns ON ns.oid = t.relnamespace
     WHERE ns.nspname = current_schema() AND t.relname = 'biz_sales_rebate_settlement'
       AND c.contype = 'c'
       AND c.conname IN ('ck_biz_srs_status', 'ck_biz_srs_settle_period', 'ck_biz_srs_period_shape',
                         'ck_biz_srs_period_range', 'ck_biz_srs_rule_type', 'ck_biz_srs_rate_unit',
                         'ck_biz_srs_unit_pair', 'ck_biz_srs_amounts', 'ck_biz_srs_tier',
                         'ck_biz_srs_counts');
    IF n <> 10 THEN
        RAISE EXCEPTION 'V0439 自检失败：10 条 CHECK 应全在（实得 %）', n;
    END IF;

    -- ===== 5.4 行为探针：坏数据必须被拒（真插一次再回滚，空表也真跑）=====
    v_id  := gen_random_uuid();
    v_cus := gen_random_uuid();

    -- 合法插入（后续各条探针的基准）
    INSERT INTO biz_sales_rebate_settlement
        (settlement_no, customer_id, settle_period, period_code, period_from, period_to,
         policy_id, policy_no, policy_name, rule_type, rate_unit,
         policy_effective_from, policy_snapshot, order_count, order_amount, order_qty,
         base_amount, base_qty, tier_seq, tier_from, rate, rebate_amount)
    VALUES ('RB-V0439-PROBE', v_cus, 'MONTH', '2026-10', DATE '2026-10-01', DATE '2026-11-01',
            gen_random_uuid(), 'BR-20261001-000001', '探针政策', 'BY_QTY', 'PER_UNIT',
            DATE '2026-01-01', '{"tiers":[]}'::jsonb, 1, 1000, 100,
            1000, 100, 1, 0, 0.5, 50);

    BEGIN
        INSERT INTO biz_sales_rebate_settlement
            (settlement_no, customer_id, settle_period, period_code, period_from, period_to,
             policy_id, policy_no, policy_name, rule_type, rate_unit,
             policy_effective_from, policy_snapshot, rate)
        VALUES ('RB-V0439-BAD1', v_cus, 'MONTH', '2026-Q4', DATE '2026-10-01', DATE '2027-01-01',
                gen_random_uuid(), 'BR-X', 'x', 'BY_QTY', 'PER_UNIT', DATE '2026-01-01', '{}'::jsonb, 0.5);
        RAISE EXCEPTION 'V0439 自检失败：月度 + 季度期间码必须被 ck_biz_srs_period_shape 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    BEGIN
        INSERT INTO biz_sales_rebate_settlement
            (settlement_no, customer_id, settle_period, period_code, period_from, period_to,
             policy_id, policy_no, policy_name, rule_type, rate_unit,
             policy_effective_from, policy_snapshot, rate)
        VALUES ('RB-V0439-BAD2', v_cus, 'MONTH', '2026-11', DATE '2026-10-01', DATE '2026-11-01',
                gen_random_uuid(), 'BR-X', 'x', 'BY_QTY', 'PERCENT', DATE '2026-01-01', '{}'::jsonb, 0.5);
        RAISE EXCEPTION 'V0439 自检失败：按量 + 百分比必须被 ck_biz_srs_unit_pair 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    BEGIN
        INSERT INTO biz_sales_rebate_settlement
            (settlement_no, customer_id, settle_period, period_code, period_from, period_to,
             policy_id, policy_no, policy_name, rule_type, rate_unit,
             policy_effective_from, policy_snapshot, rate, rebate_amount)
        VALUES ('RB-V0439-BAD3', v_cus, 'MONTH', '2026-12', DATE '2026-12-01', DATE '2027-01-01',
                gen_random_uuid(), 'BR-X', 'x', 'BY_QTY', 'PER_UNIT', DATE '2026-01-01', '{}'::jsonb, 0.5, -1);
        RAISE EXCEPTION 'V0439 自检失败：负应返金额必须被 ck_biz_srs_amounts 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    BEGIN
        INSERT INTO biz_sales_rebate_settlement
            (settlement_no, customer_id, settle_period, period_code, period_from, period_to,
             policy_id, policy_no, policy_name, rule_type, rate_unit,
             policy_effective_from, policy_snapshot, rate)
        VALUES ('RB-V0439-BAD4', v_cus, 'MONTH', '2026-09', DATE '2026-09-01', DATE '2026-09-01',
                gen_random_uuid(), 'BR-X', 'x', 'BY_QTY', 'PER_UNIT', DATE '2026-01-01', '{}'::jsonb, 0.5);
        RAISE EXCEPTION 'V0439 自检失败：空期间（period_to = period_from）必须被 ck_biz_srs_period_range 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- 同客户同期间再插一张（未作废）⇒ 唯一索引必须挡住
    BEGIN
        INSERT INTO biz_sales_rebate_settlement
            (settlement_no, customer_id, settle_period, period_code, period_from, period_to,
             policy_id, policy_no, policy_name, rule_type, rate_unit,
             policy_effective_from, policy_snapshot, rate)
        VALUES ('RB-V0439-DUP', v_cus, 'MONTH', '2026-10', DATE '2026-10-01', DATE '2026-11-01',
                gen_random_uuid(), 'BR-X', 'x', 'BY_QTY', 'PER_UNIT', DATE '2026-01-01', '{}'::jsonb, 0.5);
        RAISE EXCEPTION 'V0439 自检失败：同客户同粒度同期间的第二张未作废结算单必须被 uk_biz_srs_period 拒绝';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;

    -- 把探针单作废后，同期间必须能重开（唯一索引排除 CANCELLED）
    UPDATE biz_sales_rebate_settlement SET status = 'CANCELLED'
     WHERE settlement_no = 'RB-V0439-PROBE';
    INSERT INTO biz_sales_rebate_settlement
        (settlement_no, customer_id, settle_period, period_code, period_from, period_to,
         policy_id, policy_no, policy_name, rule_type, rate_unit,
         policy_effective_from, policy_snapshot, rate)
    VALUES ('RB-V0439-REOPEN', v_cus, 'MONTH', '2026-10', DATE '2026-10-01', DATE '2026-11-01',
            gen_random_uuid(), 'BR-X', 'x', 'BY_QTY', 'PER_UNIT', DATE '2026-01-01', '{}'::jsonb, 0.5);

    -- 探针清场（迁移不留业务行）
    DELETE FROM biz_sales_rebate_settlement WHERE customer_id = v_cus;

    SELECT count(*) INTO n_rows
      FROM biz_sales_rebate_settlement WHERE settlement_no LIKE 'RB-V0439-%';
    IF n_rows <> 0 THEN
        RAISE EXCEPTION 'V0439 自检失败：探针行未清干净（残留 % 行）', n_rows;
    END IF;

    -- ===== 5.5 号段行在 =====
    SELECT count(*) INTO n
      FROM biz_seq WHERE seq_type = 'RB' AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0439 自检失败：biz_seq(seq_type=RB) 应有 1 行，实得 %', n;
    END IF;

    -- ===== 5.6 菜单：3 行、挂在返利政策页面下、同父 sort 唯一 =====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000004380'::uuid
       AND menu_type = 3 AND deleted_at IS NULL
       AND auth_code LIKE 'business:erp:rebate-settlement:%';
    IF n <> 3 THEN
        RAISE EXCEPTION 'V0439 自检失败：返利结算应有 3 个按钮行，实得 %', n;
    END IF;

    SELECT count(*) INTO n_dup_sort
      FROM (
            SELECT sort_order FROM sys_menu
             WHERE parent_id = '20000000-0000-0000-0000-000000004380'::uuid
               AND deleted_at IS NULL
             GROUP BY sort_order HAVING count(*) > 1
           ) d;
    IF n_dup_sort <> 0 THEN
        RAISE EXCEPTION 'V0439 自检失败：返利政策页面下有 % 组重复 sort_order（侧边栏顺序会每次刷新都变）', n_dup_sort;
    END IF;

    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000004390'::uuid,
                  '20000000-0000-0000-0000-000000004391'::uuid,
                  '20000000-0000-0000-0000-000000004392'::uuid)
       AND deleted_at IS NULL;
    IF n <> 3 THEN
        RAISE EXCEPTION 'V0439 自检失败：三个按钮行应按固定 id 落库（实得 %）', n;
    END IF;

    RAISE NOTICE 'V0439 自检通过：35 列 / 3 索引 / 10 CHECK / 4 条拒绝探针 + 1 条重开探针 / RB 号段 / 3 个按钮行';
END $$;
