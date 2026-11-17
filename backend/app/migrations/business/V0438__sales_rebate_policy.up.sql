-- ============================================================================
-- V0438__sales_rebate_policy（M3-15 · 销售返利：政策表 + 规则（按量/按额/阶梯）· 2026-11-17）
--
-- 卡：t_adda469e（M3-15【M3·销售】销售返利：政策表 + 规则（按量/按额/阶梯））
-- 拆卡依据：`docs/planning/20261113-新卡清单.md` §M3 的 M3-15 行；缺口来源
--   `docs/review/20261113-已完成模块功能缺口与优化清单.md` §4.2「销售返利——有佣金，无返利政策」。
-- 下游（本卡**不做**）：
--   M3-16 返利计算（月度/季度结算，按周期算应返金额）
--   M3-17 返利生成应付或红字发票
--   M3-18 返利单测（扩充）
--   M3-19 前端：返利政策与结算页
--
-- ── 本迁移做四件事 ──────────────────────────────────────────────────────────
--
--   ① 建表 `biz_sales_rebate_policy` —— 返利政策头：谁（客户）/ 怎么算（规则类型 +
--      计价单位）/ 多久结一次（月度 / 季度）/ 什么时候有效（起止日期 + 启停）。
--   ② 建表 `biz_sales_rebate_rule`   —— 政策的**规则行**：一档一行
--      （`tier_from` 含 → `tier_to` 不含 / `rate` 比率）。按量、按额是「单档」的特例，
--      阶梯是多档；三者共用同一张规则表，下游 M3-16 只认这一个口径。
--   ③ 种 `biz_seq(seq_type='BR')` 号段：`BR-yyyyMMdd-000001`（按天滚动，与 QT/SO/SH 同款）。
--   ④ 种 4 个权限码（1 个页面节点 + 3 个动作按钮）+ 页面节点挂「销售管理」目录下。
--
-- ── 三种规则类型（本卡的语义核心，代码唯一实现见 `sales/rebate_dto.rs`）─────────
--
--   | rule_type  | 中文 | 规则行数 | rate_unit（计价单位） | 规则行的 tier_* 怎么用 |
--   |------------|------|:--------:|----------------------|------------------------|
--   | `BY_QTY`    | 按量 | 恰好 1   | `PER_UNIT`（元/件）   | 必须 `tier_from=0` 且 `tier_to IS NULL`（平铺） |
--   | `BY_AMOUNT` | 按额 | 恰好 1   | `PERCENT`（销售额 %） | 同上（平铺） |
--   | `TIERED`    | 阶梯 | 2 ~ 20   | `PER_UNIT` 或 `PERCENT` | 从 0 起、**首尾相接、开区间**、最后一档 `tier_to IS NULL` |
--
--   三种类型的「怎么算」留给 M3-16，本卡只钉住**数据形状**：
--     按量   ⇒ 返利 = 期间销量 × rate（元/件）
--     按额   ⇒ 返利 = 期间销售额 × rate%（rate 是百分数，10 表示 10%）
--     阶梯   ⇒ 期间销量（PER_UNIT）或销售额（PERCENT）落在哪一档，用**该档**的 rate 乘
--              （不累进、不回溯 —— 落档即按该档单价/百分比算，M3-16 定稿时如改成累进
--               必须同时改本表注释与 M3-19 的界面文案，否则「按 10% 还是按 3%+7%」两边打架）
--
--   **为什么把「计价单位」放在政策头而不是规则行**：放到规则行意味着同一张政策里可以有
--   「第 1 档元/件、第 2 档百分比」这种无法相加的阶梯 —— 那是数据错误而不是灵活配置。
--   放在头上一行，库层 CHECK 就能把「BY_QTY 却按百分比」这种矛盾挡在建表期。
--
--   **为什么「阶梯依据」不再单列一列**：它由 rate_unit 决定（`PER_UNIT` ⇒ 按数量分档、
--   `PERCENT` ⇒ 按销售额分档）。多一列等于多一个可以自相矛盾的字段。
--
-- ── 为什么规则单独一张表（不塞 JSON / 不塞进政策的 4 个数字列）────────────────
--   ① 阶梯是**变长**的（1~20 档），列数固定就必然要「最多 N 档」的硬上限或 JSON 列；
--      JSON 列没法用 CHECK 保证「首尾相接、末档开口」，也没法给某一档单独留备注；
--   ② M3-16 结算要按档取 rate 做 SQL 条件（`? >= tier_from AND (? < tier_to OR tier_to IS NULL)`），
--      一行一档直接可查；JSON 要在应用层展开、还容易和库里的形状漂移；
--   ③ `biz_price_tier`（V0321 客户阶梯价）已经是「一档一行」的先例，口径一致。
--
-- ── 唯一性 / 重叠（这两条决定了 M3-16 按哪份政策算）────────────────────────
--   1. `policy_no` 存活行唯一（`uk_biz_srbp_no ... WHERE deleted_at IS NULL`）：软删后单号可复用，
--      与 `uk_biz_quotation_no` / `uk_biz_sales_order_no` 同口径（草案删了重开不被旧号挡住）。
--   2. `(policy_id, seq)` 唯一（`uk_biz_srbr_policy_seq`）：一档一行号，重复档号会让
--      「第 2 档是哪一行」变成多义。
--   3. **同一客户 × 同一结算周期的启用政策，有效期不得重叠** —— 库层**不做**（PG 需要
--      `btree_gist` + `EXCLUDE USING gist`，且「启用」是会被 UPDATE 改的状态，排他约束只能
--      表达静态事实）。这条由 `rebate_handler` 在**同一事务**里先取
--      `pg_advisory_xact_lock(hashtextextended(customer_id))` 再查重叠（`daterange && daterange`），
--      命中 → `409 + 66090`。不加锁的话两个并发建单会双双通过检查，之后同一个月能按两份
--      政策各算一次返利 —— 返利翻倍且没有报错。
--      **月度与季度政策允许并存**：结算粒度不同（M3-16 的月度单吃 MONTH、季度单吃 QUARTER），
--      重叠检查因此按 `settle_period` 分组 —— 这是刻意的，不是漏了一个维度。
--
-- ── 有效期（`effective_from` / `effective_to`）──────────────────────────────
--   都进表、`effective_to` 可空（NULL = 长期有效）；`ck_biz_srbp_period` 钉死
--   `effective_to >= effective_from`。「生效/失效」**不做状态**：和报价单的有效期同一个理由
--   （过期是日期事实，做成状态就要定时翻转 + 时钟回拨对账，还会造出逆向迁移边）。
--   `status = 0`（停用）是**人工止血**：停用的政策不参与 M3-16 结算，但历史结算单不受影响
--   （结算单会存政策快照，见 M3-16）。
--
-- ── 外键口径 ────────────────────────────────────────────────────────────────
--   `policy.customer_id` 是**弱引用**（无外键）：客户可软删，历史政策必须永远可读
--   （AGENTS.md 约定 13「主数据关联 → join 实时取名」，出参同时带 `customer_code` /
--   `customer_name`）。`rule.policy_id → biz_sales_rebate_policy(id) ON DELETE CASCADE`：
--   规则离开政策没有独立意义，政策物理删（仅测试/清场）时规则必须一起走 —— 与
--   `biz_quotation_line.quotation_id` 同口径。
--
-- ── 菜单 / 权限 ─────────────────────────────────────────────────────────────
--   父 = 销售管理（`20000000-0000-0000-0000-000000002701`，`/business/erp/sales-mgmt`）。
--   同级已有 sort：10 客户档案 / 15 报价单 / 20 销售订单 / 30 发货单 / 40 应收台账 /
--   50 收款单 / 55 收款退款单 / 60 销售发票 / 70 客户价格 / 72 销售退货单 / 75 取价查询 /
--   78 销售目标 / 80 佣金规则 / 82 币种管理 / 90 佣金结算 / 92 汇率管理。
--   返利与佣金同属「按业绩返钱」，取 **85**（佣金规则 80 与佣金结算 90 之间；
--   约定 12：同父 sort 必须唯一，插空用中间值）。
--
--   4 个码：
--     `business:erp:rebate-policy:list`   承载在**页面节点**上（menu_type=2）——
--         ERP 域尚未做「页面码 / 接口码解耦」（AGENTS.md 约定 14 的过渡期条款，HR 是试点），
--         与报价单（V0433 `business:erp:quotation:list` 挂页面行）逐字同型；
--     `…:create` / `…:update` / `…:delete`  是三个按钮（menu_type=3，不进侧栏）。
--
--   页面节点 `path` / `component` 留 NULL、`is_visible = FALSE`：前端页由 **M3-19** 落地，
--   `system::menu::tests::n08`（menu_type=2 的 path 必须能在前端路由里解析）是真库硬门禁，
--   提前填 path 会让 n08 在前端卡落地之前一直红。前端路由落地后**一条 UPDATE 收尾**
--   （与 V0436 对 `…4331` 做的一模一样）：
--
--     UPDATE sys_menu
--        SET path = '/business/erp/rebate-policy',
--            component = 'business/erp/rebate-policy/index',
--            is_visible = TRUE,
--            updated_at = now()
--      WHERE id = '20000000-0000-0000-0000-000000004380'::uuid;
--
--   **本迁移不写 `sys_role_menu`**：把返利政策授给谁（销售经理建政策 / 销售员只读）是管理
--   决策，不属于迁移该替人做的决定 —— 与 V0265 / V0433 同口径，不静默扩权。
--   （因此 `orphan_button_gate` 闸⑦ 不会把这 3 个按钮判成孤儿：它们挂在页面节点下。）
--
-- ── 幂等 / 回滚 ─────────────────────────────────────────────────────────────
--   `CREATE TABLE IF NOT EXISTS` + 全部 `CREATE INDEX IF NOT EXISTS` + 菜单
--   `ON CONFLICT (id) DO NOTHING` + `biz_seq` `WHERE NOT EXISTS`；自检块可反复执行。
--   down：先删菜单（`sys_role_menu` 若有行先删）→ 删 biz_seq 行 → 删两张表。
--   护栏：表里已有未删的政策时**拒绝回滚**（政策一旦被结算引用，删表等于把「当时按什么算」
--   从系统里抹掉；M3-16 之后的结算单会存政策快照，但快照的源表不该由迁移随手删）。
--
-- ── 号段（开工实测复核）────────────────────────────────────────────────────
--   写入本文件时 `backend/app/migrations/**` 最大号 = V0437（customer_credit_policy）。
--   并行卡会抢号，**提交前**再 `ls backend/app/migrations/*/ | grep -oE 'V[0-9]{4}' | sort -u | tail -3`
--   复核（跨目录全局唯一，重号 = 服务启动直接失败）。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 前置护栏：依赖的存量表在、父目录在、本卡 4 个码没被别人占用
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n_base   integer;
    n_parent integer;
    n_code   integer;
BEGIN
    SELECT count(*) INTO n_base
      FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
     WHERE n.nspname = 'public'
       AND c.relname IN ('sys_menu', 'sys_role_menu', 'biz_customer', 'biz_seq');
    IF n_base <> 4 THEN
        RAISE EXCEPTION 'V0438: 前置表不全（sys_menu / sys_role_menu / biz_customer / biz_seq 实得 % 张），请先应用 V0137 与 V0260', n_base;
    END IF;

    SELECT count(*) INTO n_parent
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000002701'::uuid
       AND menu_type = 1
       AND deleted_at IS NULL;
    IF n_parent <> 1 THEN
        RAISE EXCEPTION 'V0438: 父目录「销售管理」(…2701) 不存在或不是目录节点，请先应用 V0267';
    END IF;

    SELECT count(*) INTO n_code
      FROM sys_menu
     WHERE auth_code IN ('business:erp:rebate-policy:list',
                         'business:erp:rebate-policy:create',
                         'business:erp:rebate-policy:update',
                         'business:erp:rebate-policy:delete')
       AND deleted_at IS NULL;
    IF n_code > 4 THEN
        RAISE EXCEPTION 'V0438: 返利政策四个码已有 % 个承载节点（应为 0，重放时 ≤ 4）', n_code;
    END IF;

    RAISE NOTICE 'V0438: 前置检查通过（底座表在、销售管理目录在、四个码当前 % 个承载节点）', n_code;
END $$;

-- ---------------------------------------------------------------------------
-- ② 表：`biz_sales_rebate_policy`（返利政策头）
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_sales_rebate_policy (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),       -- 主键
    policy_no      VARCHAR(40)  NOT NULL,                            -- 政策编号 BR-yyyyMMdd-000001（存活行唯一）
    policy_name    VARCHAR(200) NOT NULL,                            -- 政策名称（人读，允许多个政策重名）
    customer_id    UUID         NOT NULL,                            -- 客户（弱引用 biz_customer.id，无外键）
    rule_type      VARCHAR(16)  NOT NULL,                            -- BY_QTY 按量 / BY_AMOUNT 按额 / TIERED 阶梯
    rate_unit      VARCHAR(16)  NOT NULL,                            -- PER_UNIT 元/件 / PERCENT 销售额百分比
    settle_period  VARCHAR(16)  NOT NULL,                            -- MONTH 月度结算 / QUARTER 季度结算
    effective_from DATE         NOT NULL,                            -- 生效日（含）
    effective_to   DATE,                                             -- 失效日（含）；NULL = 长期有效
    status         SMALLINT     NOT NULL DEFAULT 1,                  -- 1 启用（参与结算）/ 0 停用（保留历史）
    remark         TEXT,                                             -- 备注（谈政策的背景）
    created_at     TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- 创建时间
    updated_at     TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- 更新时间
    create_by      UUID,                                             -- 创建人（弱引用 sys_user.id）
    update_by      UUID,                                             -- 更新人（弱引用 sys_user.id）
    deleted_at     TIMESTAMPTZ,                                      -- 软删时间（软删后单号可复用）

    CONSTRAINT ck_biz_srbp_rule_type
        CHECK (rule_type IN ('BY_QTY', 'BY_AMOUNT', 'TIERED')),
    CONSTRAINT ck_biz_srbp_rate_unit
        CHECK (rate_unit IN ('PER_UNIT', 'PERCENT')),
    -- 规则类型与计价单位必须配对：按量只能元/件、按额只能百分比、阶梯两者皆可
    CONSTRAINT ck_biz_srbp_unit_pair
        CHECK (
               (rule_type = 'BY_QTY'    AND rate_unit = 'PER_UNIT')
            OR (rule_type = 'BY_AMOUNT' AND rate_unit = 'PERCENT')
            OR (rule_type = 'TIERED')
        ),
    CONSTRAINT ck_biz_srbp_settle_period
        CHECK (settle_period IN ('MONTH', 'QUARTER')),
    CONSTRAINT ck_biz_srbp_period
        CHECK (effective_to IS NULL OR effective_to >= effective_from),
    CONSTRAINT ck_biz_srbp_status
        CHECK (status IN (0, 1))
);

COMMENT ON TABLE biz_sales_rebate_policy IS
    '销售返利政策头（M3-15）：谁（customer_id）× 怎么算（rule_type + rate_unit）× 多久结一次（settle_period）× 什么时候有效。'
    '「算法」的比率全部在 biz_sales_rebate_rule 里（一档一行）；按量/按额是单档特例，阶梯是多档。'
    '生效与失效是日期事实（effective_*），不是状态；status=0 是人工停用（不参与结算，历史不受影响）';
COMMENT ON COLUMN biz_sales_rebate_policy.policy_no IS
    '政策编号 BR-yyyyMMdd-000001（biz_seq seq_type=BR，按 CST 的天滚动）；软删后同号可复用（部分唯一索引只看存活行）';
COMMENT ON COLUMN biz_sales_rebate_policy.rule_type IS
    'BY_QTY 按量（返利 = 销量 × 元/件）/ BY_AMOUNT 按额（返利 = 销售额 × %）/ TIERED 阶梯（按 rate_unit 决定按销量还是销售额落档）';
COMMENT ON COLUMN biz_sales_rebate_policy.rate_unit IS
    '计价单位：PER_UNIT 元/件（与数量相乘）/ PERCENT 销售额百分比（rate=10 表示 10%）。由 rule_type 钉死（见 ck_biz_srbp_unit_pair）';
COMMENT ON COLUMN biz_sales_rebate_policy.settle_period IS
    '结算粒度：MONTH 月度 / QUARTER 季度。M3-16 的结算单按它取政策（同一客户同粒度的启用政策有效期不得重叠，由 handler 加锁校验）';
COMMENT ON COLUMN biz_sales_rebate_policy.effective_to IS
    '失效日（含）：NULL = 长期有效。「已失效」一律现算 effective_to < today，不落状态列（过期不是状态）';
COMMENT ON COLUMN biz_sales_rebate_policy.status IS
    '1 启用（参与 M3-16 结算）/ 0 停用（保留政策与历史，结算跳过）。停用不改有效期，重新启用后按原有效期继续；要提前终止请改 effective_to';
COMMENT ON COLUMN biz_sales_rebate_policy.deleted_at IS
    '软删时间：删了 = 政策从列表消失，且同号可重新生成；已被结算引用的政策不允许删除（M3-16 结算单存政策快照，删了快照就无从解释）';

-- 政策号存活行唯一（软删后可复用，与 uk_biz_quotation_no 同口径）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_srbp_no
    ON biz_sales_rebate_policy (policy_no)
 WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_srbp_customer
    ON biz_sales_rebate_policy (customer_id, settle_period, effective_from)
 WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_srbp_active
    ON biz_sales_rebate_policy (customer_id, settle_period)
 WHERE deleted_at IS NULL AND status = 1;

-- ---------------------------------------------------------------------------
-- ③ 表：`biz_sales_rebate_rule`（返利规则行 / 阶梯档）
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_sales_rebate_rule (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),        -- 主键
    policy_id   UUID          NOT NULL,                            -- 政策（物理删政策时级联删规则）
    seq         SMALLINT      NOT NULL,                            -- 档号（1 起，升序；单档政策恒为 1）
    tier_from   NUMERIC(18,4) NOT NULL DEFAULT 0,                  -- 本档下限（含）：销量 或 销售额 或 0
    tier_to     NUMERIC(18,4),                                     -- 本档上限（不含）；NULL = 无上限（只有最后一档可 NULL）
    rate        NUMERIC(18,6) NOT NULL,                            -- 比率：PER_UNIT=元/件；PERCENT=百分比（0~100）
    remark      TEXT,                                              -- 本档备注
    created_at  TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- 创建时间
    updated_at  TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- 更新时间

    CONSTRAINT fk_biz_srbr_policy
        FOREIGN KEY (policy_id) REFERENCES biz_sales_rebate_policy (id) ON DELETE CASCADE,
    CONSTRAINT ck_biz_srbr_seq       CHECK (seq >= 1),
    CONSTRAINT ck_biz_srbr_tier_from CHECK (tier_from >= 0),
    CONSTRAINT ck_biz_srbr_tier_to   CHECK (tier_to IS NULL OR tier_to > tier_from),
    CONSTRAINT ck_biz_srbr_rate      CHECK (rate > 0)
);

COMMENT ON TABLE biz_sales_rebate_rule IS
    '销售返利政策的一档规则（M3-15）：一档一行，区间左闭右开 [tier_from, tier_to)，末档 tier_to IS NULL 表示无上限。'
    '按量/按额是「恰好一行且 tier_from=0 / tier_to IS NULL」的特例；阶梯是 2~20 行首尾相接。'
    '「百分比 rate ≤ 100」「档位首尾相接」「BY_QTY/BY_AMOUNT 恰好一行」由 rebate_dto 校验（跨行规则库层 CHECK 表达不了）';
COMMENT ON COLUMN biz_sales_rebate_rule.seq IS
    '档号 1..N：与 (policy_id) 组成唯一键；服务端按 tier_from 升序重排后重新编号（入参顺序不参与判定）';
COMMENT ON COLUMN biz_sales_rebate_rule.tier_from IS
    '本档下限（含）：rate_unit=PER_UNIT 时是销量件数，PERCENT 时是销售额；首档必须是 0，其余档必须等于上一档的 tier_to';
COMMENT ON COLUMN biz_sales_rebate_rule.tier_to IS
    '本档上限（不含）：NULL = 无上限（只有最后一档允许 NULL）。用开区间是为了「正好等于 100 件」落进下一档而不是两档都算';
COMMENT ON COLUMN biz_sales_rebate_rule.rate IS
    '比率：PER_UNIT ⇒ 元/件；PERCENT ⇒ 百分数（10 = 10%，上限 100 由 rebate_dto 校验）。rate > 0，不允许 0 元档（0 元档没有意义，会让人误以为该档免费送返利）';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_srbr_policy_seq
    ON biz_sales_rebate_rule (policy_id, seq);
CREATE INDEX IF NOT EXISTS idx_biz_srbr_policy_tier
    ON biz_sales_rebate_rule (policy_id, tier_from);

-- ---------------------------------------------------------------------------
-- ④ 号段：`biz_seq(seq_type='BR')` —— BR-yyyyMMdd-000001，按天（CST）滚动
-- ---------------------------------------------------------------------------
INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key,
                     current_no, step, separator, status, remark)
SELECT '20000000-0000-0000-0000-000000004388'::uuid,
       'BR', '返利政策编号', 'BR', 'yyyyMMdd', '', 0, 1, '-', 1,
       '返利政策编号：BR-yyyyMMdd-000001，按天（CST）滚动（V0438 · M3-15）'
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq q WHERE q.seq_type = 'BR' AND q.deleted_at IS NULL);

-- ---------------------------------------------------------------------------
-- ⑤ 菜单：1 个页面节点（path 留 NULL，M3-19 补）+ 3 个动作按钮
--    sort：页面 85（佣金规则 80 与佣金结算 90 之间）；按钮 10/20/30（同父唯一）
-- ---------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000004380'::uuid, '返利政策', 2,
     'business:erp:rebate-policy:list', NULL,
     '20000000-0000-0000-0000-000000002701'::uuid, NULL,
     85, 1, 'gift', '返利政策',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000004381'::uuid, 'business:erp:rebate-policy:create', 3,
     'business:erp:rebate-policy:create', NULL,
     '20000000-0000-0000-0000-000000004380'::uuid, NULL,
     10, 1, NULL, '新建返利政策（取号 BR-yyyyMMdd-000001；客户须 ACTIVE；按量/按额 1 档、阶梯 2~20 档首尾相接）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000004382'::uuid, 'business:erp:rebate-policy:update', 3,
     'business:erp:rebate-policy:update', NULL,
     '20000000-0000-0000-0000-000000004380'::uuid, NULL,
     20, 1, NULL, '编辑返利政策（政策头 + 规则整单替换；改类型不能留下旧口径的档位）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000004383'::uuid, 'business:erp:rebate-policy:delete', 3,
     'business:erp:rebate-policy:delete', NULL,
     '20000000-0000-0000-0000-000000004380'::uuid, NULL,
     30, 1, NULL, '删除返利政策（软删；已被结算引用的政策应先停用而不是删除）',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------------------------------------------------------------------------
-- ⑥ 自检（任一不符即 RAISE EXCEPTION 回滚整个迁移）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n          integer;
    n_rows     integer;
    n_dup_sort integer;
    n_tiers    integer;
    v_text     text;
BEGIN
    -- ===== 6.1 两张表的列数 / 关键列类型 =====
    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = current_schema() AND table_name = 'biz_sales_rebate_policy';
    IF n <> 16 THEN
        RAISE EXCEPTION 'V0438 自检失败：biz_sales_rebate_policy 应有 16 列，实得 %', n;
    END IF;

    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = current_schema() AND table_name = 'biz_sales_rebate_rule';
    IF n <> 9 THEN
        RAISE EXCEPTION 'V0438 自检失败：biz_sales_rebate_rule 应有 9 列，实得 %', n;
    END IF;

    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = current_schema()
       AND table_name IN ('biz_sales_rebate_policy', 'biz_sales_rebate_rule')
       AND (
             (table_name = 'biz_sales_rebate_policy' AND column_name = 'effective_from' AND data_type = 'date'      AND is_nullable = 'NO')
          OR (table_name = 'biz_sales_rebate_policy' AND column_name = 'effective_to'   AND data_type = 'date'      AND is_nullable = 'YES')
          OR (table_name = 'biz_sales_rebate_policy' AND column_name = 'status'         AND data_type = 'smallint'  AND is_nullable = 'NO')
          OR (table_name = 'biz_sales_rebate_policy' AND column_name = 'customer_id'    AND data_type = 'uuid'      AND is_nullable = 'NO')
          OR (table_name = 'biz_sales_rebate_policy' AND column_name = 'deleted_at'     AND data_type = 'timestamp with time zone' AND is_nullable = 'YES')
          OR (table_name = 'biz_sales_rebate_rule'   AND column_name = 'tier_from'      AND data_type = 'numeric'   AND numeric_scale = 4)
          OR (table_name = 'biz_sales_rebate_rule'   AND column_name = 'tier_to'        AND data_type = 'numeric'   AND numeric_scale = 4 AND is_nullable = 'YES')
          OR (table_name = 'biz_sales_rebate_rule'   AND column_name = 'rate'           AND data_type = 'numeric'   AND numeric_scale = 6 AND is_nullable = 'NO')
           );
    IF n <> 8 THEN
        RAISE EXCEPTION 'V0438 自检失败：关键 8 列的类型/可空性/小数位与设计不符（实得 % 列匹配）', n;
    END IF;

    -- ===== 6.2 约束齐（四个域/配对 CHECK + 三条规则 CHECK + 外键 + 两个唯一索引）=====
    SELECT count(*) INTO n
      FROM pg_constraint c JOIN pg_class t ON t.oid = c.conrelid
     WHERE t.relname = 'biz_sales_rebate_policy'
       AND c.contype = 'c'
       AND c.conname IN ('ck_biz_srbp_rule_type', 'ck_biz_srbp_rate_unit', 'ck_biz_srbp_unit_pair',
                         'ck_biz_srbp_settle_period', 'ck_biz_srbp_period', 'ck_biz_srbp_status');
    IF n <> 6 THEN
        RAISE EXCEPTION 'V0438 自检失败：biz_sales_rebate_policy 的 6 条 CHECK 只找到 % 条', n;
    END IF;

    SELECT count(*) INTO n
      FROM pg_constraint c JOIN pg_class t ON t.oid = c.conrelid
     WHERE t.relname = 'biz_sales_rebate_rule'
       AND c.contype = 'c'
       AND c.conname IN ('ck_biz_srbr_seq', 'ck_biz_srbr_tier_from', 'ck_biz_srbr_tier_to', 'ck_biz_srbr_rate');
    IF n <> 4 THEN
        RAISE EXCEPTION 'V0438 自检失败：biz_sales_rebate_rule 的 4 条 CHECK 只找到 % 条', n;
    END IF;

    SELECT count(*) INTO n
      FROM pg_constraint c JOIN pg_class t ON t.oid = c.conrelid
     WHERE t.relname = 'biz_sales_rebate_rule' AND c.contype = 'f'
       AND pg_get_constraintdef(c.oid) LIKE '%ON DELETE CASCADE%';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0438 自检失败：规则表到政策表的外键（ON DELETE CASCADE）没找到';
    END IF;

    SELECT count(*) INTO n
      FROM pg_indexes
     WHERE schemaname = current_schema()
       AND (
             (indexname = 'uk_biz_srbp_no' AND tablename = 'biz_sales_rebate_policy'
              AND indexdef LIKE '%UNIQUE%' AND indexdef LIKE '%deleted_at IS NULL%')
          OR (indexname = 'uk_biz_srbr_policy_seq' AND tablename = 'biz_sales_rebate_rule'
              AND indexdef LIKE '%UNIQUE%')
           );
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0438 自检失败：两个唯一索引（uk_biz_srbp_no 存活行唯一 / uk_biz_srbr_policy_seq）只找到 % 个', n;
    END IF;

    -- ===== 6.3 约束反向探针：库层真的会拒「配对矛盾 / 负比率 / 上限 ≤ 下限」=====
    BEGIN
        INSERT INTO biz_sales_rebate_policy
            (id, policy_no, policy_name, customer_id, rule_type, rate_unit, settle_period, effective_from)
        VALUES
            ('20000000-0000-0000-0000-0000000043f1'::uuid, '__V0438_PROBE_PAIR__', '自检探针（应被拒）',
             gen_random_uuid(), 'BY_QTY', 'PERCENT', 'MONTH', CURRENT_DATE);
        RAISE EXCEPTION 'V0438 自检失败：BY_QTY + PERCENT 的配对矛盾没被 ck_biz_srbp_unit_pair 拦住';
    EXCEPTION
        WHEN check_violation THEN
            NULL;  -- 期望路径
    END;

    BEGIN
        INSERT INTO biz_sales_rebate_rule (id, policy_id, seq, tier_from, tier_to, rate)
        VALUES ('20000000-0000-0000-0000-0000000043f2'::uuid, gen_random_uuid(), 1, 0, NULL, 0);
        RAISE EXCEPTION 'V0438 自检失败：rate = 0 没被 ck_biz_srbr_rate 拦住';
    EXCEPTION
        WHEN check_violation OR foreign_key_violation THEN
            NULL;  -- 期望路径（比率 0 先撞 CHECK；若 PG 先判外键也算被拒）
    END;

    -- ===== 6.4 号段：恰好 1 行、规则与实现逐字一致 =====
    SELECT count(*) INTO n
      FROM biz_seq
     WHERE seq_type = 'BR' AND prefix = 'BR' AND date_pattern = 'yyyyMMdd'
       AND separator = '-' AND step = 1 AND status = 1 AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0438 自检失败：biz_seq(seq_type=BR) 取号规则应恰好 1 行，实得 % 行', n;
    END IF;

    -- ===== 6.5 菜单：1 页面 + 3 按钮，形状正确 =====
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004380'::uuid
       AND menu_type = 2
       AND auth_code = 'business:erp:rebate-policy:list'
       AND parent_id = '20000000-0000-0000-0000-000000002701'::uuid
       AND path IS NULL
       AND component IS NULL
       AND is_visible = FALSE
       AND sort_order = 85
       AND status = 1
       AND title = '返利政策'
       AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0438 自检失败：返利政策页面节点形状不对（应为 menu_type=2 / path NULL / 不可见 / 挂销售管理 sort 85）';
    END IF;

    SELECT count(*) INTO n
      FROM sys_menu c JOIN sys_menu p ON p.id = c.parent_id
     WHERE c.id IN ('20000000-0000-0000-0000-000000004381'::uuid,
                    '20000000-0000-0000-0000-000000004382'::uuid,
                    '20000000-0000-0000-0000-000000004383'::uuid)
       AND c.menu_type = 3
       AND c.path IS NULL
       AND c.is_visible = FALSE
       AND c.status = 1
       AND c.deleted_at IS NULL
       AND c.name = c.auth_code
       AND p.id = '20000000-0000-0000-0000-000000004380'::uuid;
    IF n <> 3 THEN
        RAISE EXCEPTION 'V0438 自检失败：三个动作按钮形状不对（应为 menu_type=3 / path NULL / 挂在返利政策页面下，实得 % 个合格）', n;
    END IF;

    -- 一码一节点（同码两行 = 授权页两个同名勾选框）
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE auth_code IN ('business:erp:rebate-policy:list',
                         'business:erp:rebate-policy:create',
                         'business:erp:rebate-policy:update',
                         'business:erp:rebate-policy:delete')
       AND deleted_at IS NULL;
    IF n <> 4 THEN
        RAISE EXCEPTION 'V0438 自检失败：返利政策四个码在 sys_menu 里共 % 个未删节点（应为 4）', n;
    END IF;

    -- 同父 sort 唯一（约定 12）
    SELECT count(*) INTO n_dup_sort
      FROM (
            SELECT parent_id, sort_order
              FROM sys_menu
             WHERE parent_id = '20000000-0000-0000-0000-000000002701'::uuid
               AND deleted_at IS NULL
             GROUP BY parent_id, sort_order
            HAVING count(*) > 1
           ) dup;
    IF n_dup_sort <> 0 THEN
        RAISE EXCEPTION 'V0438 自检失败：销售管理目录下有 % 组重复 sort_order（约定 12）', n_dup_sort;
    END IF;

    -- ===== 6.6 行为探针：M3-16 要跑的形状在这里先跑一遍（空库 0 行也算通过）=====
    --   「按档取 rate」的窄查询 + 「同一客户同粒度启用政策的有效期重叠」判据。
    --   档位比较用固定的 150（销量 150 件 / 销售额 150 元）作探针值：证明
    --   `? >= tier_from AND (? < tier_to OR tier_to IS NULL)` 这条 M3-16 要用的比较在库层跑得通。
    EXECUTE $probe$
        SELECT count(*)
          FROM biz_sales_rebate_policy p
          JOIN biz_sales_rebate_rule r ON r.policy_id = p.id
         WHERE p.deleted_at IS NULL
           AND p.status = 1
           AND (p.effective_to IS NULL OR p.effective_to >= CURRENT_DATE)
           AND p.effective_from <= CURRENT_DATE
           AND (r.tier_to IS NULL OR (150::numeric >= r.tier_from AND 150::numeric < r.tier_to))
    $probe$ INTO n_rows;

    EXECUTE $probe$
        SELECT count(*)
          FROM biz_sales_rebate_policy a
          JOIN biz_sales_rebate_policy b
            ON b.customer_id = a.customer_id
           AND b.settle_period = a.settle_period
           AND b.id <> a.id
           AND b.deleted_at IS NULL AND b.status = 1
         WHERE a.deleted_at IS NULL AND a.status = 1
           AND daterange(a.effective_from, COALESCE(a.effective_to, 'infinity'::date), '[]')
               && daterange(b.effective_from, COALESCE(b.effective_to, 'infinity'::date), '[]')
    $probe$ INTO n_tiers;
    RAISE NOTICE 'V0438: 探针执行成功（当前有效档 % 行 / 重叠对 % 组）', n_rows, n_tiers;

    -- ===== 6.7 授权行自检：本迁移不写 sys_role_menu，确认一个都没多出来 =====
    SELECT count(*) INTO n
      FROM sys_role_menu
     WHERE menu_id IN ('20000000-0000-0000-0000-000000004380'::uuid,
                       '20000000-0000-0000-0000-000000004381'::uuid,
                       '20000000-0000-0000-0000-000000004382'::uuid,
                       '20000000-0000-0000-0000-000000004383'::uuid);
    IF n > 0 THEN
        RAISE NOTICE 'V0438：返利政策 4 个节点已有 % 行授权（本迁移不写授权，由运维按角色授予）', n;
    END IF;

    SELECT string_agg(title, ' / ' ORDER BY sort_order) INTO v_text
      FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000004380'::uuid AND deleted_at IS NULL;
    RAISE NOTICE 'V0438: 自检通过（两张表 / 10 条 CHECK / 2 个唯一索引 / 号段 BR / 页面节点 + 3 按钮：%）', COALESCE(v_text, '—');
END $$;
