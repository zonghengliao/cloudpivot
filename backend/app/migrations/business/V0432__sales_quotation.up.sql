-- ============================================================================
-- V0432__sales_quotation（M3-01 · 销售报价单：表结构 biz_quotation + biz_quotation_line · 2026-11-17）
--
-- 卡：t_6a61726c（M3-01【M3·销售】报价单：表结构 biz_quotation + biz_quotation_line）
-- 拆卡依据 `docs/planning/20261113-新卡清单.md` §「M3 销售（26 卡）」M3-01 行
--   （「报价单表（biz_quotation + biz_quotation_line）」，卡面补充口径「含有效期、客户、行明细」）。
-- 下游：M3-02（报价单 CRUD + 状态机 草稿→已发→已接受/已拒绝）、M3-03（报价单 → 销售订单转换，
--       带行）、M3-04（审批流接线）、M3-05（单测）、M3-06（前端列表/详情）、M3-07（PDF 模板）、
--       M3-24（报价 → 订单 → 出库全链 E2E）都按本表的契约写 SQL。
--
-- 【本卡只做一件事】建**两张**表 + 一条取号规则：
--   ① biz_quotation       报价单头（客户 / 报价日期 / 有效期 / 状态 / 金额合计 / 转单回链）
--   ② biz_quotation_line  报价单行明细（物料 / 数量 / 单价 / 金额 / 税率 / 税额）
--   ③ biz_seq(seq_type='QT')  单号取号规则：QT-yyyyMMdd-000001（按天滚动）
--   不种业务数据、不种权限码与菜单（M3-02 之后才出接口，码要跟接口一起种，见 AGENTS.md 约定 7）、
--   不动任何存量表（biz_customer / biz_sales_order / biz_seq 一律不 ALTER）。
--
-- ── 为什么另建两张表，而不是塞进 biz_price_list / biz_sales_order ────────────
--   ① `biz_price_list` 是**价目表**（主数据）：一个 (价目表, 物料) 一个当前价，「现在卖多少钱」。
--      报价单是**带客户、带有效期、带状态的一次性单据**（同一个物料对不同客户、不同日期报不同价，
--      而且报出去之后单价就冻结了，不能被下次改价目表带走）。塞进价目表 = 历史报价被改价覆盖。
--   ② `biz_sales_order` 是**已成交**的单据（要过审批、要占信用、要出库、要生成应收与凭证）。
--      报价阶段客户还没下单，提前开销售订单会污染订单列表、占用出库与信用口径。
--      两者是「报价 →（客户接受）→ 转订单」的前后手关系，转换动作本身在 M3-03。
--   ③ 仓库里已有的 `biz_price_quote`（V0321）是**采购询价**的供应商报价（inquiry_item × supplier），
--      方向相反（我方买），与本表无任何关系，**不要复用**（复用它当客户报价会把采购口径写脏）。
--
-- ── 状态域（`ck_biz_quotation_status`，5 值）────────────────────────────────
--   DRAFT     草稿：可改行明细、可删（软删）
--   SENT      已发出：报价已给客户，单价与行明细**冻结**（改价要走新报价单，否则客户手里那份和库里对不上）
--   ACCEPTED  客户已接受：M3-03 允许转销售订单（卡面状态机终点之一）
--   REJECTED  客户已拒绝：终点；不再接受修改，要重报就开新单
--   CANCELLED 已作废：我方撤回/报错重来（ARCH-7 §12.3「正向必配反向」：每张单据必须有反向动作，
--             否则报价单一发出去就永久停在 SENT，界面上没有任何可点的按钮 —— 与约定 11 的
--             「单据停在中间态」同一类坑）
--   本卡只建表，**不建状态迁移入口**：迁移边须登记到 `shared/state_machine`（届时 LEGAL_EDGES
--   与本 CHECK 的域值逐字对齐），由 M3-02 落地 —— 与 SO-04 / SO-08 的分工一致
--   （V0264 / V0283 建表时不登记 DocKind，后面的卡才登记）。
--   `ACCEPTED` / `REJECTED` 由客户反馈回写（M3-02 的 `POST /quotation/{id}/status`），
--   不是人工随手选的普通值。
--
-- ── 有效期（卡面点名「含有效期」）────────────────────────────────────────────
--   `quote_date` + `valid_until` 两列，**都 NOT NULL**，且 `ck_biz_quotation_valid_until`
--   钉死 `valid_until >= quote_date`。理由：
--     · 没有有效期的报价 = 客户可以永远拿旧价压价，而销售没法说「过期了」；
--     · 允许「有效期早于报价日」= 建单当天就过期，界面会显示一张天生过期的单，
--       谁都不会注意到是录错了（这类脏数据只在客户砍价时才暴露）。
--   过期本身**不做状态**：过期是 `valid_until < today` 的时间事实，不是单据状态。
--   把它做成状态就要有定时任务翻转 + 时钟回拨对账，且「过期后又被迫接受」会造出逆向迁移边。
--   ⇒ M3-02/M3-06 的「已过期」一律**现算** `CURRENT_DATE > valid_until`，不进库、不进状态机。
--   `valid_until = quote_date`（当天有效）是**允许**的，不是脏数据。
--
-- ── 金额恒等（`ck_biz_quotation_amount`）─────────────────────────────────────
--   `total_with_tax = total_amount + total_tax`（**精确恒等，不是近似**），与
--   `ck_sales_order_amount` / `ck_biz_po_tax_sum` 同口径：算错就是 23514（400），不是 500。
--   金额列全 `NUMERIC(18,4)`，单价 `NUMERIC(18,6)`、税率 `NUMERIC(7,4)`（百分数 0–100）——
--   与 `biz_sales_order(_item)` 逐列同口径，M3-03 转单时可以整列搬，不会出尾差。
--   行级 `amount` / `tax_amount` 只做非负（四舍五入口径在 model 层，库层不猜业务舍入规则）。
--
-- ── 转单回链（M3-03 的幂等键）───────────────────────────────────────────────
--   `converted_so_id` + `converted_at`：报价单转成销售订单后回填「转成了哪一张」。
--     · 没有这两列，M3-03 就没有幂等判据 —— 同一个报价单点两次「转订单」会出两张订单，
--       两张订单都会去占信用、都要出库，谁都不会发现是重复的；
--     · `uk_biz_quotation_converted_so`（部分唯一索引）再钉一层：一张销售订单只能由一个
--       报价单认领，防止两个报价单指向同一张订单把「这单是哪个报价换来的」写乱；
--     · 两列**成对出现**（`ck_biz_quotation_converted`）：只填 id 不填时间 = 「转过了但不知何时」，
--       审计上无法回答「什么时候转的」；
--     · 无外键（弱引用 biz_sales_order.id）：订单是后建的单据，加外键会让「删单重开」被报价单卡住；
--       且 M3-03 的事务里先建订单再回填，外键一旦加上，M3-03/M3-17 的作废路径都会变成环。
--   **刻意不把「只有 ACCEPTED 才能转单」写进 CHECK**：现实里客户口头接受了、销售先下单、
--   事后补状态是常态；写进库层会让这种路径只能在「改库」和「改迁移」之间二选一。
--   这条判断放 model 层（M3-03 可读可单测），库层只管「两列成对」。
--
-- ── 外键口径（与 V0264 销售订单 / V0283 发货单同风格）────────────────────────
--   `quotation_line.quotation_id → biz_quotation(id) ON DELETE CASCADE`：
--     明细离开单头没有独立意义，单头物理删（仅测试/清场）时明细必须一起走。
--   `customer_id` / `material_id` 是**弱引用**（无外键）：客户与物料都可软删，历史报价必须永远可读
--   （AGENTS.md 约定 13「主数据关联 → join 实时取名」；外键会把「客户软删但历史单据要留」堵死）。
--
-- ── 单号唯一 = 部分唯一索引（存活行唯一）────────────────────────────────────
--   `uk_biz_quotation_no (quote_no) WHERE deleted_at IS NULL`：与 `uk_biz_sales_order_no` /
--   `uk_biz_shipment_no` 同口径 —— 软删后单号可复用（草案单删了重开不会被「单号已存在」挡住）。
--   ⚠ 索引名一律带 `quotation` 全词：PG 索引名是**库内 schema 级唯一**，撞名 + `IF NOT EXISTS`
--     会被**静默跳过**（V0264 实测栽过：`uk_biz_so_no` 已被 biz_stock_out 占用，唯一索引根本没建出来）。
--
-- ── 行号唯一（`uk_biz_quotation_line_no`）───────────────────────────────────
--   `(quotation_id, line_no)` 唯一：行号是 M3-03 转订单时「订单行 ↔ 报价行」的对应键，
--   单内重号会让「这行订单是从哪一报价行搬来的」变成多义。
--   （`biz_sales_order_item` 只 CHECK 了 `line_no >= 1`、没做唯一 —— 那里是历史选择；
--     本表从建表起就钉死，后续改单只能用 UPDATE，不能靠「删了重插换个号」。）
--
-- ── 幂等 / 回滚 ───────────────────────────────────────────────────────────
--   `CREATE TABLE IF NOT EXISTS` + 全部 `CREATE INDEX IF NOT EXISTS` + biz_seq 种子
--   `INSERT … WHERE NOT EXISTS`；自检块可反复执行（探针单号 `__M301_PROBE_*`，块尾自清场
--   ⇒ 重放零残留）。
--   down 幂等（`DROP TABLE IF EXISTS`），并加护栏：表里已有非作废报价单时**拒绝回滚**
--   （报价单是客户认过的价，删表之后「当时报了多少」在系统里再无痕迹，且无法从别的表重建）。
--
-- ── 号段（开工实测复核）────────────────────────────────────────────────────
--   写入本文件时 backend/app/migrations/business/ 最大号 = V0431
--   （V0431__erp_stock_policy_page 安全库存设置页）。并行卡会抢号，
--   **提交前**再 `ls backend/app/migrations/business | grep -oE 'V[0-9]{4}' | sort -u | tail -3` 复核。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 前置护栏：本迁移依赖的两张存量表必须在（缺表说明迁移没跑到 V0137 / V0260）
-- ---------------------------------------------------------------------------
DO $$
BEGIN
    IF to_regclass('public.biz_customer') IS NULL THEN
        RAISE EXCEPTION 'V0432: biz_customer 不存在，请先应用 V0260（销售客户档案两张表）';
    END IF;
    IF to_regclass('public.biz_seq') IS NULL THEN
        RAISE EXCEPTION 'V0432: biz_seq 不存在，请先应用 V0137（ERP 主数据与仓库基线）';
    END IF;
    IF to_regclass('public.biz_quotation') IS NOT NULL THEN
        RAISE NOTICE 'V0432: biz_quotation 已存在，本迁移按幂等路径重放（复验约束/索引/取号行）';
    END IF;
    RAISE NOTICE 'V0432: 前置表齐备，开始建报价单两张表';
END $$;

-- ---------------------------------------------------------------------------
-- ② biz_quotation（报价单头）
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_quotation (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),          -- 主键
    quote_no       VARCHAR(32)   NOT NULL,                              -- 报价单号（biz_seq 生成，存活行唯一）
    customer_id    UUID          NOT NULL,                              -- 客户（弱引用 biz_customer.id）
    quote_date     DATE          NOT NULL,                              -- 报价日期
    valid_until    DATE          NOT NULL,                              -- 有效期至（含当天；过期状态不落库，现算）
    status         VARCHAR(24)   NOT NULL DEFAULT 'DRAFT',              -- 状态机：DRAFT/SENT/ACCEPTED/REJECTED/CANCELLED
    total_amount   NUMERIC(18,4) NOT NULL DEFAULT 0,                    -- 不含税金额合计
    total_tax      NUMERIC(18,4) NOT NULL DEFAULT 0,                    -- 税额合计
    total_with_tax NUMERIC(18,4) NOT NULL DEFAULT 0,                    -- 价税合计
    converted_so_id UUID,                                               -- 转成的销售订单（M3-03 回填，弱引用）
    converted_at   TIMESTAMPTZ,                                         -- 转单时间（与 converted_so_id 成对）
    remark         TEXT,                                                -- 备注（报价说明/付款条件等）
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,     -- 审计：创建时间
    updated_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,     -- 审计：更新时间（model 层维护）
    create_by      UUID,                                                -- 审计：创建人（弱引用 sys_user.id）
    update_by      UUID,                                                -- 审计：更新人（弱引用 sys_user.id）
    deleted_at     TIMESTAMPTZ,                                         -- 软删时间（NULL = 存活）

    CONSTRAINT ck_biz_quotation_status
        CHECK (status IN ('DRAFT','SENT','ACCEPTED','REJECTED','CANCELLED')),
    -- 有效期：不允许「有效期早于报价日」（建单当天就过期，只会在客户砍价时才暴露）
    CONSTRAINT ck_biz_quotation_valid_until
        CHECK (valid_until >= quote_date),
    CONSTRAINT ck_biz_quotation_amount_nonneg
        CHECK (total_amount >= 0 AND total_tax >= 0 AND total_with_tax >= 0),
    -- 价税合计恒等（会计兜底，与 ck_sales_order_amount 同口径：算错是 400 不是 500）
    CONSTRAINT ck_biz_quotation_amount
        CHECK (total_with_tax = total_amount + total_tax),
    -- 转单回链成对：只有 id 没时间 = 「转过了但不知何时」，审计无法追溯
    CONSTRAINT ck_biz_quotation_converted
        CHECK (   (converted_so_id IS NULL AND converted_at IS NULL)
               OR (converted_so_id IS NOT NULL AND converted_at IS NOT NULL))
);

COMMENT ON TABLE  biz_quotation IS
    '销售报价单头（status 为 VARCHAR 状态机；有效期是时间事实不是状态；ck_biz_quotation_amount 是会计兜底）';
COMMENT ON COLUMN biz_quotation.quote_no IS
    '报价单号：QT-yyyyMMdd-000001（biz_seq seq_type=QT 取号，按 CST 的天滚动；软删后可复用）';
COMMENT ON COLUMN biz_quotation.customer_id IS
    '客户 id（弱引用 biz_customer.id，无外键：客户软删后历史报价仍要可读）';
COMMENT ON COLUMN biz_quotation.quote_date IS
    '报价日期（报价口径的归属日；与 valid_until 一起构成有效期区间）';
COMMENT ON COLUMN biz_quotation.valid_until IS
    '有效期至（含当天，>= quote_date 由 ck_biz_quotation_valid_until 钉死）；⚠「已过期」一律现算 CURRENT_DATE > valid_until，不落状态列';
COMMENT ON COLUMN biz_quotation.status IS
    '状态机：DRAFT 草稿 / SENT 已发出（价格冻结）/ ACCEPTED 客户已接受 / REJECTED 客户已拒绝 / CANCELLED 已作废（ARCH-7 §12.3 正向必配反向）';
COMMENT ON COLUMN biz_quotation.total_with_tax IS
    '价税合计 = total_amount + total_tax（ck_biz_quotation_amount 精确恒等，与 biz_sales_order 同口径）';
COMMENT ON COLUMN biz_quotation.converted_so_id IS
    '转成的销售订单 id（M3-03 回填；弱引用 biz_sales_order.id，无外键以免转单/作废事务成环）';
COMMENT ON COLUMN biz_quotation.converted_at IS
    '转单时间（与 converted_so_id 成对出现，ck_biz_quotation_converted 钉死）';
COMMENT ON COLUMN biz_quotation.deleted_at IS
    '软删标记；单号唯一索引带 WHERE deleted_at IS NULL ⇒ 软删后单号可复用（仅 DRAFT/CANCELLED 允许删，由 M3-02 守卫）';

-- 单号唯一：**部分唯一索引**（存活行唯一），与 uk_biz_sales_order_no / uk_biz_shipment_no 同口径
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_quotation_no
    ON biz_quotation (quote_no) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_quotation_customer
    ON biz_quotation (customer_id, quote_date DESC);
CREATE INDEX IF NOT EXISTS idx_biz_quotation_status
    ON biz_quotation (status);
-- 有效期取数（「即将过期/已过期」列表按 valid_until 筛，M3-02/M3-06）
CREATE INDEX IF NOT EXISTS idx_biz_quotation_valid_until
    ON biz_quotation (valid_until);
-- 一张销售订单只能由一个报价单认领（M3-03 的转单幂等：反向唯一）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_quotation_converted_so
    ON biz_quotation (converted_so_id) WHERE converted_so_id IS NOT NULL;

-- ---------------------------------------------------------------------------
-- ③ biz_quotation_line（报价单行明细）
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_quotation_line (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),           -- 主键
    quotation_id  UUID          NOT NULL,                               -- 所属报价单（硬外键 CASCADE）
    line_no       INTEGER       NOT NULL,                               -- 行号（1 起，单内唯一）
    material_id   UUID          NOT NULL,                               -- 物料（弱引用 biz_material.id）
    qty           NUMERIC(18,4) NOT NULL,                               -- 报价数量（> 0）
    price         NUMERIC(18,6) NOT NULL DEFAULT 0,                     -- 单价（不含税，>= 0）
    amount        NUMERIC(18,4) NOT NULL DEFAULT 0,                     -- 行不含税金额
    tax_rate      NUMERIC(7,4)  NOT NULL DEFAULT 0,                     -- 税率百分数（0–100）
    tax_amount    NUMERIC(18,4) NOT NULL DEFAULT 0,                     -- 行税额
    remark        VARCHAR(500),                                         -- 行备注（交期/替代料说明等）
    created_at    TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,     -- 审计：创建时间
    updated_at    TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,     -- 审计：更新时间（model 层维护）

    CONSTRAINT fk_biz_quotation_line_quotation
        FOREIGN KEY (quotation_id) REFERENCES biz_quotation (id) ON DELETE CASCADE,
    CONSTRAINT ck_biz_quotation_line_no
        CHECK (line_no >= 1),
    CONSTRAINT ck_biz_quotation_line_qty
        CHECK (qty > 0),
    CONSTRAINT ck_biz_quotation_line_price
        CHECK (price >= 0),
    CONSTRAINT ck_biz_quotation_line_rate
        CHECK (tax_rate >= 0 AND tax_rate <= 100),
    CONSTRAINT ck_biz_quotation_line_amount
        CHECK (amount >= 0 AND tax_amount >= 0)
);

COMMENT ON TABLE  biz_quotation_line IS
    '销售报价单行明细（quotation_id 硬外键 ON DELETE CASCADE；行号单内唯一是 M3-03 转订单的对应键）';
COMMENT ON COLUMN biz_quotation_line.line_no IS
    '行号（1 起，单内唯一：uk_biz_quotation_line_no；M3-03 靠它把报价行对应到订单行）';
COMMENT ON COLUMN biz_quotation_line.material_id IS
    '物料 id（弱引用 biz_material.id，无外键：物料软删后历史报价仍要可读）';
COMMENT ON COLUMN biz_quotation_line.qty IS
    '报价数量（> 0；NUMERIC(18,4) 与 biz_sales_order_item.qty 同口径，转单整列搬不出尾差）';
COMMENT ON COLUMN biz_quotation_line.price IS
    '单价（不含税，NUMERIC(18,6)，>= 0；与 biz_sales_order_item.price 同口径）';
COMMENT ON COLUMN biz_quotation_line.tax_rate IS
    '税率百分数 0–100（与 biz_sales_order_item.tax_rate 同口径，税额 = amount × rate / 100）';
COMMENT ON COLUMN biz_quotation_line.amount IS
    '行不含税金额（= qty × price 按 model 层舍入规则算；库层只做非负兜底）';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_quotation_line_no
    ON biz_quotation_line (quotation_id, line_no);
CREATE INDEX IF NOT EXISTS idx_biz_quotation_line_material
    ON biz_quotation_line (material_id);

-- ---------------------------------------------------------------------------
-- ④ 取号规则：QT（报价单号）
-- ---------------------------------------------------------------------------
INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status, remark)
SELECT v.id::uuid, 'QT', '报价单号', 'QT', 'yyyyMMdd', '', 0, 1, '-', 1,
       '报价单单号：QT-yyyyMMdd-000001，按天（CST）滚动'
  FROM (VALUES ('d0432000-0000-4000-8000-000000000001')) AS v(id)
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq q WHERE q.seq_type = 'QT' AND q.deleted_at IS NULL);

-- ---------------------------------------------------------------------------
-- ⑤ 自检 + 行为探针（任一不符即 RAISE EXCEPTION 回滚整个迁移）
--   探针只用 `__M301_PROBE_*` 单号，块尾一律删除 ⇒ 重放零残留。
--   本表两张表的"外键"只有 明细→单头（CASCADE），客户/物料是弱引用，
--   所以探针**不需要**往 biz_customer / biz_material 插夹具行（不会碰任何存量业务数据）。
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n INT;
    d TEXT;
    probe_q   UUID := 'd0432fff-0000-4000-8000-000000000001'::uuid;
    probe_q2  UUID := 'd0432fff-0000-4000-8000-000000000002'::uuid;
    probe_q3  UUID := 'd0432fff-0000-4000-8000-000000000003'::uuid;
    probe_so  UUID := 'd0432fff-0000-4000-8000-000000000004'::uuid;
    probe_cus UUID := 'd0432fff-0000-4000-8000-000000000005'::uuid;
    probe_mat UUID := 'd0432fff-0000-4000-8000-000000000006'::uuid;
BEGIN
    -- ===== 5.1 两张表都在 =====
    IF to_regclass('public.biz_quotation') IS NULL
       OR to_regclass('public.biz_quotation_line') IS NULL THEN
        RAISE EXCEPTION 'V0432 自检失败：biz_quotation / biz_quotation_line 未建全';
    END IF;

    -- ===== 5.2 列数逐表比对（多一列少一列都算漂移）=====
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_quotation';
    IF n <> 17 THEN
        RAISE EXCEPTION 'V0432 自检失败：biz_quotation 应有 17 列，实际 % 列', n;
    END IF;
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_quotation_line';
    IF n <> 12 THEN
        RAISE EXCEPTION 'V0432 自检失败：biz_quotation_line 应有 12 列，实际 % 列', n;
    END IF;

    -- ===== 5.3 头表关键列的类型 / 精度 / 可空性 =====
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_quotation'
       AND (   (column_name = 'quote_no'       AND character_maximum_length = 32 AND is_nullable = 'NO')
            OR (column_name = 'customer_id'    AND data_type = 'uuid' AND is_nullable = 'NO')
            OR (column_name = 'quote_date'     AND data_type = 'date' AND is_nullable = 'NO')
            OR (column_name = 'valid_until'    AND data_type = 'date' AND is_nullable = 'NO')
            OR (column_name = 'status'         AND character_maximum_length = 24 AND is_nullable = 'NO')
            OR (column_name = 'total_amount'   AND numeric_precision = 18 AND numeric_scale = 4 AND is_nullable = 'NO')
            OR (column_name = 'total_tax'      AND numeric_precision = 18 AND numeric_scale = 4 AND is_nullable = 'NO')
            OR (column_name = 'total_with_tax' AND numeric_precision = 18 AND numeric_scale = 4 AND is_nullable = 'NO'));
    IF n <> 8 THEN
        RAISE EXCEPTION 'V0432 自检失败：头表 8 个关键列的类型/精度/可空性不对（实际符合 % 列）—— 金额列必须 NUMERIC(18,4)', n;
    END IF;

    -- ===== 5.4 行表关键列的类型 / 精度 / 可空性 =====
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_quotation_line'
       AND (   (column_name = 'quotation_id' AND data_type = 'uuid' AND is_nullable = 'NO')
            OR (column_name = 'line_no'      AND data_type = 'integer' AND is_nullable = 'NO')
            OR (column_name = 'material_id'  AND data_type = 'uuid' AND is_nullable = 'NO')
            OR (column_name = 'qty'          AND numeric_precision = 18 AND numeric_scale = 4 AND is_nullable = 'NO')
            OR (column_name = 'price'        AND numeric_precision = 18 AND numeric_scale = 6 AND is_nullable = 'NO')
            OR (column_name = 'amount'       AND numeric_precision = 18 AND numeric_scale = 4 AND is_nullable = 'NO')
            OR (column_name = 'tax_rate'     AND numeric_precision = 7  AND numeric_scale = 4 AND is_nullable = 'NO')
            OR (column_name = 'tax_amount'   AND numeric_precision = 18 AND numeric_scale = 4 AND is_nullable = 'NO'));
    IF n <> 8 THEN
        RAISE EXCEPTION 'V0432 自检失败：行表 8 个关键列的类型/精度/可空性不对（实际符合 % 列）—— 单价 NUMERIC(18,6) / 金额 NUMERIC(18,4) / 税率 NUMERIC(7,4)', n;
    END IF;

    -- ===== 5.5 CHECK / 外键在位，且外键是 CASCADE =====
    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid = 'public.biz_quotation'::regclass AND contype = 'c'
       AND conname IN ('ck_biz_quotation_status', 'ck_biz_quotation_valid_until',
                       'ck_biz_quotation_amount_nonneg', 'ck_biz_quotation_amount',
                       'ck_biz_quotation_converted');
    IF n <> 5 THEN
        RAISE EXCEPTION 'V0432 自检失败：头表 5 条 CHECK 只建成 % 条', n;
    END IF;
    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid = 'public.biz_quotation_line'::regclass AND contype = 'c'
       AND conname IN ('ck_biz_quotation_line_no', 'ck_biz_quotation_line_qty',
                       'ck_biz_quotation_line_price', 'ck_biz_quotation_line_rate',
                       'ck_biz_quotation_line_amount');
    IF n <> 5 THEN
        RAISE EXCEPTION 'V0432 自检失败：行表 5 条 CHECK 只建成 % 条', n;
    END IF;
    SELECT pg_get_constraintdef(oid) INTO d FROM pg_constraint
     WHERE conname = 'fk_biz_quotation_line_quotation' AND contype = 'f';
    IF d IS NULL OR d NOT LIKE '%REFERENCES biz_quotation(id)%' OR d NOT LIKE '%ON DELETE CASCADE%' THEN
        RAISE EXCEPTION 'V0432 自检失败：明细外键缺失或不是 CASCADE（def=%）', d;
    END IF;

    -- ===== 5.6 索引形状：单号唯一必须带 WHERE deleted_at IS NULL；转单唯一必须部分索引 =====
    SELECT indexdef INTO d FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_quotation' AND indexname = 'uk_biz_quotation_no';
    IF d IS NULL THEN
        RAISE EXCEPTION 'V0432 自检失败：唯一索引 uk_biz_quotation_no 缺失（是否与既有索引撞名被静默跳过？）';
    END IF;
    IF d NOT LIKE '%UNIQUE%' OR d NOT LIKE '%WHERE (deleted_at IS NULL)%' THEN
        RAISE EXCEPTION 'V0432 自检失败：uk_biz_quotation_no 必须是「存活行唯一」的部分索引（def=%）', d;
    END IF;
    SELECT indexdef INTO d FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_quotation' AND indexname = 'uk_biz_quotation_converted_so';
    IF d IS NULL OR d NOT LIKE '%UNIQUE%' OR d NOT LIKE '%WHERE (converted_so_id IS NOT NULL)%' THEN
        RAISE EXCEPTION 'V0432 自检失败：uk_biz_quotation_converted_so 必须是部分唯一索引（def=%）', d;
    END IF;
    SELECT indexdef INTO d FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_quotation_line' AND indexname = 'uk_biz_quotation_line_no';
    IF d IS NULL OR d NOT LIKE '%UNIQUE%' THEN
        RAISE EXCEPTION 'V0432 自检失败：行号唯一索引 uk_biz_quotation_line_no 缺失（def=%）', d;
    END IF;

    -- ===== 5.7 取号规则行在位 =====
    SELECT count(*) INTO n FROM biz_seq
     WHERE seq_type = 'QT' AND prefix = 'QT' AND date_pattern = 'yyyyMMdd'
       AND status = 1 AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0432 自检失败：biz_seq(seq_type=QT) 取号规则应恰好 1 行，实际 % 行', n;
    END IF;

    -- ===== 5.8 行为：正常报价单可写，默认值正确 =====
    INSERT INTO biz_quotation (id, quote_no, customer_id, quote_date, valid_until)
    VALUES (probe_q, '__M301_PROBE_Q1__', probe_cus, DATE '2026-11-17', DATE '2026-11-30');
    SELECT status || '|' || total_amount::text || '|' || total_tax::text || '|' || total_with_tax::text
      INTO d FROM biz_quotation WHERE id = probe_q;
    IF d <> 'DRAFT|0.0000|0.0000|0.0000' THEN
        RAISE EXCEPTION 'V0432 自检失败：报价单落库默认值 = %（期望 DRAFT|0.0000|0.0000|0.0000）', d;
    END IF;

    --     5.9 有效期早于报价日 → 必须被拒
    BEGIN
        INSERT INTO biz_quotation (id, quote_no, customer_id, quote_date, valid_until)
        VALUES (probe_q2, '__M301_PROBE_Q2__', probe_cus, DATE '2026-11-17', DATE '2026-11-16');
        RAISE EXCEPTION 'V0432 自检失败：valid_until 早于 quote_date 未被 ck_biz_quotation_valid_until 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    --     5.10 同一天有效（valid_until = quote_date）→ 必须允许（不是脏数据）
    BEGIN
        INSERT INTO biz_quotation (id, quote_no, customer_id, quote_date, valid_until)
        VALUES (probe_q2, '__M301_PROBE_Q2__', probe_cus, DATE '2026-11-17', DATE '2026-11-17');
    EXCEPTION WHEN check_violation THEN
        RAISE EXCEPTION 'V0432 自检失败：valid_until = quote_date（当天有效）被拒了 —— 这是合法报价';
    END;
    -- 立刻清掉，5.18 的级联探针要复用同一个单号
    DELETE FROM biz_quotation WHERE id = probe_q2;

    --     5.11 状态域外的值 → 必须被拒
    BEGIN
        INSERT INTO biz_quotation (id, quote_no, customer_id, quote_date, valid_until, status)
        VALUES (probe_q3, '__M301_PROBE_Q3__', probe_cus, DATE '2026-11-17', DATE '2026-11-30', 'WIN');
        RAISE EXCEPTION 'V0432 自检失败：status = WIN 未被 ck_biz_quotation_status 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    --     5.12 价税合计不等 → 必须被拒
    BEGIN
        INSERT INTO biz_quotation (id, quote_no, customer_id, quote_date, valid_until,
                                   total_amount, total_tax, total_with_tax)
        VALUES (probe_q3, '__M301_PROBE_Q3__', probe_cus, DATE '2026-11-17', DATE '2026-11-30',
                100, 13, 112);
        RAISE EXCEPTION 'V0432 自检失败：total_with_tax <> total_amount + total_tax 未被 ck_biz_quotation_amount 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    --     5.13 转单回链只填一半 → 必须被拒
    BEGIN
        INSERT INTO biz_quotation (id, quote_no, customer_id, quote_date, valid_until, converted_so_id)
        VALUES (probe_q3, '__M301_PROBE_Q3__', probe_cus, DATE '2026-11-17', DATE '2026-11-30', probe_so);
        RAISE EXCEPTION 'V0432 自检失败：只填 converted_so_id 不填 converted_at 未被 ck_biz_quotation_converted 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    --     5.14 存活行单号重号 → 必须被 uk_biz_quotation_no 拒绝
    BEGIN
        INSERT INTO biz_quotation (id, quote_no, customer_id, quote_date, valid_until)
        VALUES (probe_q3, '__M301_PROBE_Q1__', probe_cus, DATE '2026-11-17', DATE '2026-11-30');
        RAISE EXCEPTION 'V0432 自检失败：存活行单号重号未被 uk_biz_quotation_no 拒绝';
    EXCEPTION WHEN unique_violation THEN
        NULL;   -- 期望路径
    END;

    --     5.15 软删后单号可复用（部分唯一索引就是为这个存在）
    UPDATE biz_quotation SET deleted_at = CURRENT_TIMESTAMP WHERE id = probe_q;
    BEGIN
        INSERT INTO biz_quotation (id, quote_no, customer_id, quote_date, valid_until)
        VALUES (probe_q3, '__M301_PROBE_Q1__', probe_cus, DATE '2026-11-17', DATE '2026-11-30');
    EXCEPTION WHEN unique_violation THEN
        RAISE EXCEPTION 'V0432 自检失败：软删后同单号仍被拒 —— 唯一索引不是「存活行唯一」的部分索引';
    END;
    -- 复用一个未被软删的 id 继续后面的行明细探针
    DELETE FROM biz_quotation WHERE id = probe_q3;

    --     5.16 行明细：行号 0 → 被拒；qty = 0 → 被拒；税率 101 → 被拒
    BEGIN
        INSERT INTO biz_quotation_line (quotation_id, line_no, material_id, qty)
        VALUES (probe_q, 0, probe_mat, 1);
        RAISE EXCEPTION 'V0432 自检失败：line_no = 0 未被 ck_biz_quotation_line_no 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO biz_quotation_line (quotation_id, line_no, material_id, qty)
        VALUES (probe_q, 1, probe_mat, 0);
        RAISE EXCEPTION 'V0432 自检失败：qty = 0 未被 ck_biz_quotation_line_qty 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    BEGIN
        INSERT INTO biz_quotation_line (quotation_id, line_no, material_id, qty, tax_rate)
        VALUES (probe_q, 1, probe_mat, 1, 101);
        RAISE EXCEPTION 'V0432 自检失败：tax_rate = 101 未被 ck_biz_quotation_line_rate 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    --     5.17 正常行可写 + 单内行号唯一
    INSERT INTO biz_quotation_line (quotation_id, line_no, material_id, qty, price, amount, tax_rate, tax_amount)
    VALUES (probe_q, 1, probe_mat, 10, 9.5, 95, 13, 12.35);
    BEGIN
        INSERT INTO biz_quotation_line (quotation_id, line_no, material_id, qty)
        VALUES (probe_q, 1, probe_mat, 1);
        RAISE EXCEPTION 'V0432 自检失败：同一报价单内行号重号未被 uk_biz_quotation_line_no 拒绝';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;

    --     5.18 删单头 → 明细 CASCADE 一起走
    INSERT INTO biz_quotation (id, quote_no, customer_id, quote_date, valid_until)
    VALUES (probe_q2, '__M301_PROBE_Q2__', probe_cus, DATE '2026-11-17', DATE '2026-11-17');
    INSERT INTO biz_quotation_line (quotation_id, line_no, material_id, qty)
    VALUES (probe_q2, 1, probe_mat, 3);
    IF (SELECT count(*) FROM biz_quotation_line WHERE quotation_id = probe_q2) <> 1 THEN
        RAISE EXCEPTION 'V0432 自检失败：探针明细未写入（级联测试前提不成立）';
    END IF;
    DELETE FROM biz_quotation WHERE id = probe_q2;
    SELECT count(*) INTO n FROM biz_quotation_line WHERE quotation_id = probe_q2;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0432 自检失败：删报价单后仍剩 % 行明细（外键不是 ON DELETE CASCADE）', n;
    END IF;

    -- ===== 5.19 探针自清场（重放必须零残留）=====
    DELETE FROM biz_quotation_line WHERE quotation_id IN (probe_q, probe_q2, probe_q3);
    DELETE FROM biz_quotation WHERE quote_no LIKE '__M301_PROBE%' OR id IN (probe_q, probe_q2, probe_q3);
    SELECT count(*) INTO n FROM biz_quotation WHERE quote_no LIKE '__M301_PROBE%';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0432 自检失败：头表探针残留 % 行（重放会污染真库）', n;
    END IF;
    SELECT count(*) INTO n FROM biz_quotation_line WHERE quotation_id IN (probe_q, probe_q2, probe_q3);
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0432 自检失败：明细探针残留 % 行（重放会污染真库）', n;
    END IF;

    RAISE NOTICE 'V0432 自检通过：2 张表 / 17+12 列 / 10 条 CHECK / 1 条 CASCADE 外键 / 7 条索引 / QT 取号行 / 13 组行为探针';
END $$;
