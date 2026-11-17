-- ============================================================================
-- V0450：框架协议（采购链 M4-07）—— 表结构：有效期 + 价格条款 + 数量上限
--
--   ① biz_framework_agreement        框架协议单头（一家供应商 + 有效期 + 价格条款 + 总量上限）
--   ② biz_framework_agreement_item   协议物料范围（一行 = 一个物料：协议价 + 该物料数量上限）
--   ③ biz_seq：FWA（FWA-yyyyMMdd-000001，按 CST 天滚动）
--   ④ 索引 + 末尾 DO 块自检（列 / 部分唯一索引 / CHECK 真能拒坏数据 / 默认值 / 取号行 /
--      超上限必须写得进去 / 级联删除 / 探针清理）
--
-- ── 本卡只交付表结构（卡面「本卡只做这一件事，不夹带」）───────────────────
--   不做：CRUD / 审批 / 订单引用校验 / 执行量统计接口 / 权限码 / 菜单 / 路由 / 前端。
--   这些是 M4-08（订单引用，校验价格/数量）、M4-09（执行量统计）、M4-10（单测）、
--   M4-11（前端页面）。
--   本迁移**不写一行 sys_menu**（不会顶破 menu_invariants 的 400–540 上限），
--   权限码一个不种；**也不给 biz_purchase_order(_item) 加列** —— 订单侧引用字段
--   （`framework_agreement_id` / `framework_agreement_item_id`）属于 M4-08 的迁移，
--   本卡只把「被引用的一方」备好。
--
-- ── 号段核对（动手前 `ls backend/app/migrations/*/ | grep -oE 'V[0-9]{4}' | sort -u | tail`）
--   写入本文件时全局最大号 = V0449（V0449__purchase_plan_execution），V0450 空闲。
--   biz_seq 已用 seq_type 里没有 FWA；biz_seq 行 id 取 …4479（V0449 的菜单 id 到 …4478，
--   与之连号但不交叠；biz_seq 里的既有行在 …3335 / …3180 / …4461 段）。
--
-- ── 表口径（卡面逐条落地，差异与理由都写在这里）──────────────────────────
--   1) 卡面「有效期」⇒ 单头 `start_date` / `end_date` 两列 + CHECK `start_date <= end_date`。
--      **不造** `EXPIRED` / `TERMINATED` 这类状态值：到期是时间的函数，不是人的动作。
--      落成状态列就要有人（定时任务）去刷，刷漏一格协议就"永久有效"；
--      刷早了又把还在谈的续签挡掉。所以「是否在有效期内」一律现算：
--          status = 'APPROVED' AND CURRENT_DATE BETWEEN start_date AND end_date
--      model 侧给出纯函数 `is_effective_on()`，M4-08 校验、M4-09 报表、M4-11 页面
--      读的都是同一份判据（不各写一套）。
--      `sign_date`（签订日期）另存：它和生效起始日可以不是同一天（先签后生效、补签），
--      没有它就回溯不出"合同哪天签的"。
--   2) 卡面「价格条款」⇒ 条款分两层落：
--      · 单头（合同级约定）：`price_mode`（FIXED 固定价 / FLOATING 随行就市）、
--        `price_includes_tax`（协议价含税否）、`price_currency`（币种）、
--        `price_tolerance`（下单价允许高于协议价的比例上限，0 = 必须不高于协议价）、
--        `price_adjust_rule`（浮动/调价规则原文；FLOATING 时**必填**，见 ck_biz_fwa_adjust）；
--      · 明细（物料级数字）：`agreed_price`（协议不含税单价）+ `tax_rate`（进项税率）。
--      为什么拆两层：价格条款是"怎么定价"（合同条款），协议价是"这个物料多少钱"（数字）。
--      把调价规则塞进明细，同一份协议 200 行就要抄 200 遍，改一次漏一行。
--      `price_tolerance` 落库而不是写死 0：现实中"允许 5% 以内浮动不重签"是常规条款，
--      写死 0 等于每涨一分钱都要改协议。M4-08 的判据是
--          order_price <= agreed_price * (1 + price_tolerance)
--      —— 判据本身不进库层（跨表、跨单据，CHECK 做不到），这里只保证"容差是个合法比例"。
--   3) 卡面「数量上限」⇒ 两级都存，且都**允许 NULL（= 不限）**：
--      · 明细 `max_qty`：该物料在整个协议期内的累计采购量上限（最常用的一级）；
--      · 单头 `total_max_qty` / `total_max_amount`：整份协议的总量 / 总额封顶
--        （一揽子协议常只给总额，不给逐物料量）。
--      库层 CHECK 只保证"写了就必须 > 0"（0 上限等于禁止采购，不是"不限"——
--      想不限就写 NULL，语义只有一种）。**超上限不拦在库层**：与 V0446 头注释 4 同口径，
--      加急/临时放量在现实里就是会发生，拦在插行时只会 500；露头应该在 M4-08 的下单
--      校验里（超限拒绝 + 人话）与 M4-09 的执行量报表里（超 100%）。所以
--      `executed_qty` 与 `max_qty` 之间**刻意不加** CHECK。
--   4) 卡面「含供应商与物料范围」⇒ 供应商在**单头**（一份协议对一家供应商，甲方的合作方），
--      物料清单在**明细**（协议覆盖哪些物料、各多少钱、各多少量）。
--      唯一索引 `uk_biz_fwai_material`(agreement_id, material_id)：同一份协议里
--      同一物料只能有一行 —— 两行就是两个协议价，M4-08 校验时取哪一行都说不清。
--      同一供应商可以有多份协议（不同物料类目 / 不同期间），所以**不做** supplier 唯一约束；
--      "同一供应商同一物料在有效期内不应有两份生效协议"是跨行 + 时间窗语义，
--      由 M4-08 在下单时按「有效协议里取最优/唯一一条」处理，库层不拦
--      （拦了就没法"先签新协议、旧协议到期自然失效"地平滑过渡）。
--   5) `supplier_name` 存快照（约定 13：**签约主体是历史事实**——供应商后来改名/注销，
--      协议上写的还是当时的名字）。展示侧要"现在叫什么"时 join `biz_supplier` 现取，
--      与 V0449 的 `COALESCE(p.dept_name, d.dept_name)` 同款；两列并存不是冗余。
--   6) 状态域只有三个：DRAFT（拟稿）/ SUBMITTED（审批中）/ APPROVED（生效）。
--      与采购计划（V0446）逐字同口径：`SUBMITTED` 必须有审批实例（否则"谁批的"查不出来），
--      `APPROVED` 不强制实例（显式跳过审批的路径与采购申请单同口径）。
--      **不造** CLOSED / EXPIRED / TERMINATED —— 都没有入口（本卡也没有任何"终止协议"的
--      动作卡），造了就是永远停在某处的死状态。真需要归档时**追加**迁移扩 CHECK。
--   7) 已执行量：明细 `executed_qty` / `executed_amount` 是**回写快照**（M4-08 下单时 += ），
--      单头 `total_executed_qty` / `total_executed_amount` 是派生的合计（服务端按明细重算，
--      客户端不许传）。为什么留回写列而不是纯视图：M4-08 下单要在**同一事务、行锁内**
--      判"还剩多少量"，这时候必须能 `SELECT ... FOR UPDATE` 一把锁住协议行；
--      视图算不出锁。M4-09 的统计报表再按单据链现算一列 `ordered_qty` 与回写列并排对账
--      （与 V0449 的 booked_qty / req_qty 同一套做法）。
--   8) 数据权限：`biz_framework_agreement(_item)` 与采购计划同口径 **SCOPE-EXEMPT** ——
--      框架协议是公司级采购文件（采购要看全公司、财务要复核），`dept_id` 是"经办部门"
--      快照语义而不是数据范围列。所以本卡**不**登记 `shared::scope::SCOPED_TABLES`
--      （登记了启动自检就会去校 dept 列语义）。可见性由 M4-11 的权限码控制。
--
-- ── 幂等与回滚 ───────────────────────────────────────────────────────────
--   CREATE TABLE / INDEX IF NOT EXISTS；biz_seq 用 WHERE NOT EXISTS。
--   末尾 DO 块自检：表 / 列数 / 部分唯一索引 / 坏数据被拒（逐条独立）/ 默认值 /
--   超上限写得进去 / 级联删除 / 正样本 / 取号行，任一不符 → 迁移失败（不留半套结构）。
--   回滚见 V0450__framework_agreement.down.sql：**有数据就拒绝回滚**（新表里的每一行
--   都是真协议，没有"演示数据"可丢），再删索引 / 表 / 取号行。
-- ============================================================================

-- ---------- ① biz_framework_agreement（框架协议单头） ----------
CREATE TABLE IF NOT EXISTS biz_framework_agreement (
    id                     UUID PRIMARY KEY DEFAULT gen_random_uuid(),        -- 主键
    agreement_no           VARCHAR(32)   NOT NULL,                            -- 协议编号：FWA-yyyyMMdd-000001（biz_seq 取号，存活行唯一）
    agreement_name         VARCHAR(128)  NOT NULL,                            -- 协议名称（如「2026 年度钢材框架采购协议」）
    supplier_id            UUID          NOT NULL,                            -- 供应商（弱引用 biz_supplier.id）—— 卡面的「供应商」
    supplier_name          VARCHAR(200),                                      -- 供应商名快照（签约主体历史事实；展示侧 join 现名）
    dept_id                UUID,                                              -- 经办部门（弱引用 sys_dept.id；可空 = 公司级）
    dept_name              VARCHAR(128),                                      -- 经办部门名快照
    owner_id               UUID,                                              -- 经办人（弱引用 sys_user.id）
    owner_name             VARCHAR(64),                                       -- 经办人姓名快照（审计/历史类，约定 13）
    start_date             DATE          NOT NULL,                            -- 有效期起（含）
    end_date               DATE          NOT NULL,                            -- 有效期止（含）；ck_biz_fwa_period 保证 start <= end
    sign_date              DATE,                                              -- 签订日期（可与生效起始日不同：先签后生效 / 补签）
    price_mode             VARCHAR(16)   NOT NULL DEFAULT 'FIXED',            -- 定价方式：FIXED 固定价 / FLOATING 随行就市
    price_includes_tax     BOOLEAN       NOT NULL DEFAULT FALSE,              -- 协议价是否含税（明细 agreed_price 的口径由此决定）
    price_currency         VARCHAR(8)    NOT NULL DEFAULT 'CNY',              -- 协议价币种（ISO 4217 三位）
    price_tolerance        NUMERIC(7,4)  NOT NULL DEFAULT 0,                  -- 下单价允许高于协议价的比例上限（0 = 必须 ≤ 协议价）
    price_adjust_rule      TEXT,                                              -- 调价规则原文（FLOATING 必填）
    total_max_qty          NUMERIC(18,4),                                     -- 协议总量上限（NULL = 不限）
    total_max_amount       NUMERIC(18,4),                                     -- 协议总额上限（NULL = 不限）
    total_executed_qty     NUMERIC(18,4) NOT NULL DEFAULT 0,                  -- 已执行量合计（服务端按明细重算，客户端不许传）
    total_executed_amount  NUMERIC(18,4) NOT NULL DEFAULT 0,                  -- 已执行金额合计（服务端按明细重算）
    status                 VARCHAR(16)   NOT NULL DEFAULT 'DRAFT',            -- DRAFT / SUBMITTED / APPROVED
    approval_instance_id   UUID,                                              -- 提交审批的流程实例 id（SUBMITTED 必需）
    remark                 TEXT,                                              -- 备注
    created_at             TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- 审计：创建时间
    updated_at             TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- 审计：更新时间（model 层维护）
    create_by              UUID,                                              -- 审计：创建人
    update_by              UUID,                                              -- 审计：更新人
    deleted_at             TIMESTAMPTZ,                                       -- 软删时间（NULL = 存活）

    CONSTRAINT ck_biz_fwa_no_fmt
        CHECK (agreement_no ~ '^FWA-[0-9]{8}-[0-9]{6}$'),
    CONSTRAINT ck_biz_fwa_name
        CHECK (btrim(agreement_name) <> ''),
    -- 卡面「有效期」：起止同序。到期与否**不落状态**（见头注释 1）
    CONSTRAINT ck_biz_fwa_period
        CHECK (start_date <= end_date),
    CONSTRAINT ck_biz_fwa_price_mode
        CHECK (price_mode IN ('FIXED','FLOATING')),
    CONSTRAINT ck_biz_fwa_currency
        CHECK (price_currency ~ '^[A-Z]{3}$'),
    CONSTRAINT ck_biz_fwa_tolerance
        CHECK (price_tolerance >= 0 AND price_tolerance <= 1),
    -- 浮动作价必须有调价规则（没有规则的"随行就市"= 没有约定，M4-08 无从校验）
    -- ⚠ `btrim(price_adjust_rule) <> ''` 不能省：NULL 时整式求值成 NULL，CHECK 只拒 FALSE，
    --   少了它会放过「FLOATING 但规则为 NULL」；空串同理。
    CONSTRAINT ck_biz_fwa_adjust
        CHECK (price_mode <> 'FLOATING'
            OR (price_adjust_rule IS NOT NULL AND btrim(price_adjust_rule) <> '')),
    -- 上限：要么不写（NULL = 不限），写了就必须 > 0（0 上限 = 禁止采购，不是"不限"）
    CONSTRAINT ck_biz_fwa_caps
        CHECK ((total_max_qty    IS NULL OR total_max_qty    > 0)
           AND (total_max_amount IS NULL OR total_max_amount > 0)),
    CONSTRAINT ck_biz_fwa_executed
        CHECK (total_executed_qty >= 0 AND total_executed_amount >= 0),
    CONSTRAINT ck_biz_fwa_status
        CHECK (status IN ('DRAFT','SUBMITTED','APPROVED')),
    -- 审批中必须有审批实例（「谁批的」要查得出来）
    CONSTRAINT ck_biz_fwa_instance
        CHECK (status <> 'SUBMITTED' OR approval_instance_id IS NOT NULL)
);

COMMENT ON TABLE  biz_framework_agreement IS '框架协议单头（M4-07 表结构）：一家供应商 + 有效期 + 价格条款 + 总量上限；物料范围在 biz_framework_agreement_item';
COMMENT ON COLUMN biz_framework_agreement.agreement_no IS '协议编号：FWA-yyyyMMdd-000001（biz_seq.seq_type=FWA，按北京时间天滚动）';
COMMENT ON COLUMN biz_framework_agreement.supplier_id IS '供应商（弱引用 biz_supplier.id）—— 一份协议对一家供应商；同供应商可有多份协议（不同类目 / 不同期间），库层不做唯一';
COMMENT ON COLUMN biz_framework_agreement.supplier_name IS '供应商名快照（签约主体是历史事实）；展示「现名」时 join biz_supplier，两列并存不是冗余（约定 13）';
COMMENT ON COLUMN biz_framework_agreement.start_date IS '有效期起（含）；与 end_date 一起构成有效期，**不落 EXPIRED 状态**（到期是时间的函数，见头注释 1）';
COMMENT ON COLUMN biz_framework_agreement.end_date IS '有效期止（含）；ck_biz_fwa_period 保证 start_date <= end_date';
COMMENT ON COLUMN biz_framework_agreement.sign_date IS '签订日期（可与生效起始日不同：先签后生效 / 补签）；只作审计，不参与有效期判据';
COMMENT ON COLUMN biz_framework_agreement.dept_id IS '经办部门快照（弱引用 sys_dept.id）；**不是数据范围列**（本表 SCOPE-EXEMPT，见头注释 8）';
COMMENT ON COLUMN biz_framework_agreement.owner_id IS '经办人（弱引用 sys_user.id）；姓名走 owner_name 快照（审计/历史类，约定 13）';
COMMENT ON COLUMN biz_framework_agreement.price_mode IS '定价方式：FIXED 固定价 / FLOATING 随行就市（FLOATING 必须有 price_adjust_rule）';
COMMENT ON COLUMN biz_framework_agreement.price_includes_tax IS '协议价是否含税：决定明细 agreed_price 的口径（false = 不含税，与采购订单 price 列同口径）';
COMMENT ON COLUMN biz_framework_agreement.price_currency IS '协议价币种（ISO 4217 三位大写，ck_biz_fwa_currency 钉住形状）';
COMMENT ON COLUMN biz_framework_agreement.price_tolerance IS '下单价允许高于协议价的比例上限（0–1；0 = 必须 ≤ 协议价）。M4-08 判据：order_price <= agreed_price * (1 + price_tolerance)';
COMMENT ON COLUMN biz_framework_agreement.price_adjust_rule IS '调价 / 浮动规则原文（FLOATING 必填）：如「随钢材指数月度均价上浮不超过 3%」';
COMMENT ON COLUMN biz_framework_agreement.total_max_qty IS '协议总量上限（NULL = 不限；写了必须 > 0）。库层**不拦**已执行量超上限，超限拒绝属 M4-08';
COMMENT ON COLUMN biz_framework_agreement.total_max_amount IS '协议总额上限（NULL = 不限；写了必须 > 0）';
COMMENT ON COLUMN biz_framework_agreement.status IS 'DRAFT 拟稿 / SUBMITTED 审批中（必有审批实例）/ APPROVED 生效（是否在效期另看 start_date..end_date）';
COMMENT ON COLUMN biz_framework_agreement.total_executed_qty IS '已执行量合计 = Σ明细 executed_qty（服务端在写明细的同一事务里重算，客户端不许传）';
COMMENT ON COLUMN biz_framework_agreement.total_executed_amount IS '已执行金额合计 = Σ明细 executed_amount（同上：派生列，客户端不许传）';

-- 编号唯一（存活行唯一，与 uk_biz_purchase_plan_no 同口径）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_framework_agreement_no
    ON biz_framework_agreement (agreement_no) WHERE deleted_at IS NULL;
-- 按供应商找协议（M4-08 下单：给定供应商 + 物料 + 日期，捞候选协议）
CREATE INDEX IF NOT EXISTS idx_biz_fwa_supplier
    ON biz_framework_agreement (supplier_id, status) WHERE deleted_at IS NULL;
-- 按有效期扫（"当前生效的协议"列表 / 到期提醒）
CREATE INDEX IF NOT EXISTS idx_biz_fwa_period
    ON biz_framework_agreement (start_date, end_date) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_fwa_status
    ON biz_framework_agreement (status, end_date DESC) WHERE deleted_at IS NULL;

-- ---------- ② biz_framework_agreement_item（协议物料范围） ----------
-- 明细随单头整体重建（与采购订单 / 采购计划同口径：先删后插，事务内），故不设 deleted_at。
CREATE TABLE IF NOT EXISTS biz_framework_agreement_item (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),        -- 主键
    agreement_id     UUID          NOT NULL,                            -- 所属协议
    line_no          INT           NOT NULL,                            -- 行号（1 起，单内唯一）
    material_id      UUID          NOT NULL,                            -- 物料（弱引用 biz_material.id）—— 卡面的「物料范围」
    unit_id          UUID          NOT NULL,                            -- 计量单位（建单时由物料带出，客户端不许传）
    agreed_price     NUMERIC(18,6) NOT NULL,                            -- 协议单价（口径由单头 price_includes_tax 决定；>= 0）
    tax_rate         NUMERIC(7,4)  NOT NULL DEFAULT 0,                  -- 进项税率（0–1）
    max_qty          NUMERIC(18,4),                                     -- 该物料协议期内累计上限（NULL = 不限）
    executed_qty     NUMERIC(18,4) NOT NULL DEFAULT 0,                  -- 已执行量（M4-08 下单 += ；刻意不拦超上限）
    executed_amount  NUMERIC(18,4) NOT NULL DEFAULT 0,                  -- 已执行金额（M4-08 下单 +=）
    remark           TEXT,                                              -- 行备注
    created_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- 审计：创建时间
    updated_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- 审计：更新时间（执行量回写时刷新）

    CONSTRAINT fk_biz_fwai_agreement FOREIGN KEY (agreement_id)
        REFERENCES biz_framework_agreement (id) ON DELETE CASCADE,
    CONSTRAINT ck_biz_fwai_line  CHECK (line_no > 0),
    -- 0 价协议物料是合法的（赠品 / 试用料），所以是 >= 0 而不是 > 0
    CONSTRAINT ck_biz_fwai_price CHECK (agreed_price >= 0),
    CONSTRAINT ck_biz_fwai_tax   CHECK (tax_rate >= 0 AND tax_rate <= 1),
    CONSTRAINT ck_biz_fwai_cap   CHECK (max_qty IS NULL OR max_qty > 0),
    CONSTRAINT ck_biz_fwai_exec  CHECK (executed_qty >= 0 AND executed_amount >= 0)
);

COMMENT ON TABLE  biz_framework_agreement_item IS '框架协议物料范围（一行 = 一个物料：协议价 + 税率 + 该物料数量上限；M4-07 表结构）';
COMMENT ON COLUMN biz_framework_agreement_item.material_id IS '物料（弱引用 biz_material.id）：同一协议同物料只能一行（uk_biz_fwai_material）';
COMMENT ON COLUMN biz_framework_agreement_item.agreed_price IS '协议单价（>= 0；0 = 赠品/试用料）。是否含税看单头 price_includes_tax';
COMMENT ON COLUMN biz_framework_agreement_item.max_qty IS '该物料协议期内累计采购量上限（NULL = 不限；写了必须 > 0）。库层不拦超上限，超限拒绝属 M4-08';
COMMENT ON COLUMN biz_framework_agreement_item.executed_qty IS '已执行量（>= 0）：M4-08 下单时 += ；与 max_qty 之间**刻意不加** CHECK（超限由 M4-08 判、M4-09 报表露头）';

-- 卡面「物料范围」的粒度：同一份协议里同一物料只有一行
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_fwai_material
    ON biz_framework_agreement_item (agreement_id, material_id);
-- 行号单内唯一（与 uk_biz_pp_item_line 同口径）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_fwai_line
    ON biz_framework_agreement_item (agreement_id, line_no);
-- M4-08 反向查：「这个物料有哪些协议价 / 哪家供应商」
CREATE INDEX IF NOT EXISTS idx_biz_fwai_material
    ON biz_framework_agreement_item (material_id);

-- ---------- ③ 取号规则 FA ----------
INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status, remark)
SELECT '20000000-0000-0000-0000-000000004479'::uuid, 'FWA', '框架协议编号', 'FWA', 'yyyyMMdd', '', 0, 1, '-', 1,
       '框架协议编号：FWA-yyyyMMdd-000001，按天（CST）滚动'
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'FWA' AND deleted_at IS NULL);

-- ---------- ④ 自检 ----------
DO $$
DECLARE
    v_cnt     integer;
    v_missing text;
    probe     uuid := gen_random_uuid();
    probe2    uuid := gen_random_uuid();
    item      uuid := gen_random_uuid();
    probe_sup uuid := gen_random_uuid();
BEGIN
    -- ④.1 两张表必须建出来
    IF to_regclass('public.biz_framework_agreement') IS NULL THEN
        RAISE EXCEPTION 'V0450 自检失败：biz_framework_agreement 未建出';
    END IF;
    IF to_regclass('public.biz_framework_agreement_item') IS NULL THEN
        RAISE EXCEPTION 'V0450 自检失败：biz_framework_agreement_item 未建出';
    END IF;

    -- ④.2 列数（单头 29 列 / 明细 13 列）
    SELECT count(*) INTO v_cnt FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_framework_agreement';
    IF v_cnt <> 29 THEN
        RAISE EXCEPTION 'V0450 自检失败：biz_framework_agreement 列数 % ≠ 29', v_cnt;
    END IF;
    SELECT count(*) INTO v_cnt FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_framework_agreement_item';
    IF v_cnt <> 13 THEN
        RAISE EXCEPTION 'V0450 自检失败：biz_framework_agreement_item 列数 % ≠ 13', v_cnt;
    END IF;

    -- ④.3 编号唯一索引必须带 WHERE deleted_at IS NULL（软删后可重建编号）
    SELECT count(*) INTO v_cnt
      FROM pg_indexes
     WHERE schemaname = 'public'
       AND indexname = 'uk_biz_framework_agreement_no'
       AND indexdef LIKE '%WHERE (deleted_at IS NULL)%';
    IF v_cnt <> 1 THEN
        RAISE EXCEPTION 'V0450 自检失败：uk_biz_framework_agreement_no 不是带软删条件的部分索引';
    END IF;
    -- 明细的两个唯一索引 + 三个查询索引
    SELECT count(*) INTO v_cnt
      FROM pg_indexes
     WHERE schemaname = 'public'
       AND indexname IN ('uk_biz_fwai_material','uk_biz_fwai_line');
    IF v_cnt <> 2 THEN
        RAISE EXCEPTION 'V0450 自检失败：明细唯一索引只建出 % / 2 个', v_cnt;
    END IF;
    SELECT count(*) INTO v_cnt
      FROM pg_indexes
     WHERE schemaname = 'public'
       AND indexname IN ('idx_biz_fwa_supplier','idx_biz_fwa_period','idx_biz_fwa_status','idx_biz_fwai_material');
    IF v_cnt <> 4 THEN
        RAISE EXCEPTION 'V0450 自检失败：查询索引只建出 % / 4 个', v_cnt;
    END IF;
    -- 外键必须在，且是 CASCADE（删协议连带删物料范围）
    SELECT count(*) INTO v_cnt
      FROM information_schema.referential_constraints
     WHERE constraint_schema = 'public' AND constraint_name = 'fk_biz_fwai_agreement'
       AND delete_rule = 'CASCADE';
    IF v_cnt <> 1 THEN
        RAISE EXCEPTION 'V0450 自检失败：明细外键 fk_biz_fwai_agreement 不在位或不是 ON DELETE CASCADE';
    END IF;

    -- ④.4 坏数据必须被拒（逐条独立，任一「能插进去」即失败）
    -- 编号形状
    BEGIN
        INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date)
        VALUES ('FA-2026-1', '探针', probe_sup, DATE '2026-01-01', DATE '2026-12-31');
        RAISE EXCEPTION 'V0450 自检失败：不合法的协议编号格式被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    -- 空白名称
    BEGIN
        INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date)
        VALUES ('FWA-20260101-000001', '   ', probe_sup, DATE '2026-01-01', DATE '2026-12-31');
        RAISE EXCEPTION 'V0450 自检失败：空白协议名被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    -- 有效期起止颠倒
    BEGIN
        INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date)
        VALUES ('FWA-20260101-000002', '探针', probe_sup, DATE '2026-12-31', DATE '2026-01-01');
        RAISE EXCEPTION 'V0450 自检失败：有效期起晚于止被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    -- 定价方式越界
    BEGIN
        INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date, price_mode)
        VALUES ('FWA-20260101-000003', '探针', probe_sup, DATE '2026-01-01', DATE '2026-12-31', 'MARKET');
        RAISE EXCEPTION 'V0450 自检失败：定价方式域外的 MARKET 被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    -- 浮动作价却没有调价规则（NULL）
    BEGIN
        INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date, price_mode)
        VALUES ('FWA-20260101-000004', '探针', probe_sup, DATE '2026-01-01', DATE '2026-12-31', 'FLOATING');
        RAISE EXCEPTION 'V0450 自检失败：FLOATING 但没有调价规则被接受（ck_biz_fwa_adjust 失效）';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    -- 浮动作价但调价规则是空白串（3VL/空串陷阱）
    BEGIN
        INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date, price_mode, price_adjust_rule)
        VALUES ('FWA-20260101-000005', '探针', probe_sup, DATE '2026-01-01', DATE '2026-12-31', 'FLOATING', '   ');
        RAISE EXCEPTION 'V0450 自检失败：FLOATING 的调价规则是空白串被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    -- 币种不合 ISO 4217
    BEGIN
        INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date, price_currency)
        VALUES ('FWA-20260101-000006', '探针', probe_sup, DATE '2026-01-01', DATE '2026-12-31', 'rmb');
        RAISE EXCEPTION 'V0450 自检失败：小写币种被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    -- 容差越界（> 1 与 < 0）
    BEGIN
        INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date, price_tolerance)
        VALUES ('FWA-20260101-000007', '探针', probe_sup, DATE '2026-01-01', DATE '2026-12-31', 1.5);
        RAISE EXCEPTION 'V0450 自检失败：容差 1.5 被接受（合法域 0-1）';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date, price_tolerance)
        VALUES ('FWA-20260101-000008', '探针', probe_sup, DATE '2026-01-01', DATE '2026-12-31', -0.1);
        RAISE EXCEPTION 'V0450 自检失败：负容差被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    -- 总量上限写 0（0 不是"不限"，想不限写 NULL）
    BEGIN
        INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date, total_max_qty)
        VALUES ('FWA-20260101-000009', '探针', probe_sup, DATE '2026-01-01', DATE '2026-12-31', 0);
        RAISE EXCEPTION 'V0450 自检失败：总量上限 0 被接受（0 上限应写 NULL = 不限）';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    -- 死状态（本卡刻意不造）
    BEGIN
        INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date, status)
        VALUES ('FWA-20260101-000010', '探针', probe_sup, DATE '2026-01-01', DATE '2026-12-31', 'EXPIRED');
        RAISE EXCEPTION 'V0450 自检失败：状态域外的 EXPIRED 被接受（到期是时间的函数，不落状态）';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date, status)
        VALUES ('FWA-20260101-000011', '探针', probe_sup, DATE '2026-01-01', DATE '2026-12-31', 'TERMINATED');
        RAISE EXCEPTION 'V0450 自检失败：状态域外的 TERMINATED 被接受（无入口的死状态）';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    -- APPROVED 不强制审批实例（显式跳过审批的合法路径）必须插得进；SUBMITTED 没实例必须拒
    BEGIN
        INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date, status)
        VALUES ('FWA-20260101-000012', '探针', probe_sup, DATE '2026-01-01', DATE '2026-12-31', 'SUBMITTED');
        RAISE EXCEPTION 'V0450 自检失败：SUBMITTED 无审批实例被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- ④.5 正样本：默认值对、上限 NULL = 不限、APPROVED 无实例可插、编号存活唯一
    INSERT INTO biz_framework_agreement
        (id, agreement_no, agreement_name, supplier_id, supplier_name, start_date, end_date)
    VALUES (probe, 'FWA-19990101-900001', 'V0450 自检探针', probe_sup, 'V0450 探针供应商',
            DATE '2026-01-01', DATE '2026-12-31');
    IF (SELECT status FROM biz_framework_agreement WHERE id = probe) <> 'DRAFT' THEN
        RAISE EXCEPTION 'V0450 自检失败：status 默认值不是 DRAFT';
    END IF;
    IF (SELECT price_mode FROM biz_framework_agreement WHERE id = probe) <> 'FIXED' THEN
        RAISE EXCEPTION 'V0450 自检失败：price_mode 默认值不是 FIXED';
    END IF;
    IF (SELECT price_currency FROM biz_framework_agreement WHERE id = probe) <> 'CNY' THEN
        RAISE EXCEPTION 'V0450 自检失败：price_currency 默认值不是 CNY';
    END IF;
    IF (SELECT price_includes_tax FROM biz_framework_agreement WHERE id = probe) IS NOT FALSE THEN
        RAISE EXCEPTION 'V0450 自检失败：price_includes_tax 默认值不是 FALSE';
    END IF;
    IF (SELECT price_tolerance FROM biz_framework_agreement WHERE id = probe) <> 0 THEN
        RAISE EXCEPTION 'V0450 自检失败：price_tolerance 默认值不是 0（默认必须不高于协议价）';
    END IF;
    IF (SELECT total_max_qty FROM biz_framework_agreement WHERE id = probe) IS NOT NULL
       OR (SELECT total_max_amount FROM biz_framework_agreement WHERE id = probe) IS NOT NULL THEN
        RAISE EXCEPTION 'V0450 自检失败：总量/总额上限默认必须是 NULL（= 不限）';
    END IF;
    IF (SELECT total_executed_qty FROM biz_framework_agreement WHERE id = probe) <> 0
       OR (SELECT total_executed_amount FROM biz_framework_agreement WHERE id = probe) <> 0 THEN
        RAISE EXCEPTION 'V0450 自检失败：两个已执行合计的默认值不是 0';
    END IF;

    -- 编号存活唯一：同号第二条被拒
    BEGIN
        INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date)
        VALUES ('FWA-19990101-900001', '同号的第二份协议', probe_sup, DATE '2026-01-01', DATE '2026-12-31');
        RAISE EXCEPTION 'V0450 自检失败：同编号的第二份存活协议被接受（uk_biz_framework_agreement_no 失效）';
    EXCEPTION WHEN unique_violation THEN NULL;
    END;
    -- APPROVED 无审批实例是合法路径（采购申请单同口径）
    INSERT INTO biz_framework_agreement (id, agreement_no, agreement_name, supplier_id, start_date, end_date, status)
    VALUES (probe2, 'FWA-19990101-900002', 'V0450 免审探针', probe_sup, DATE '2026-01-01', DATE '2026-12-31', 'APPROVED');
    -- 同供应商的第二份协议必须能插（不同类目/期间，库层不做 supplier 唯一）
    INSERT INTO biz_framework_agreement (agreement_no, agreement_name, supplier_id, start_date, end_date)
    VALUES ('FWA-19990101-900003', '同供应商第二份协议', probe_sup, DATE '2026-01-01', DATE '2026-12-31');

    -- ④.6 明细：默认值 / 物料唯一 / 协议价非负 / 上限 > 0 / 超上限写得进去 / 外键
    INSERT INTO biz_framework_agreement_item
        (id, agreement_id, line_no, material_id, unit_id, agreed_price, tax_rate, max_qty)
    VALUES (item, probe, 1, gen_random_uuid(), gen_random_uuid(), 12.345678, 0.13, 1000);
    IF (SELECT executed_qty FROM biz_framework_agreement_item WHERE id = item) <> 0
       OR (SELECT executed_amount FROM biz_framework_agreement_item WHERE id = item) <> 0 THEN
        RAISE EXCEPTION 'V0450 自检失败：明细两个已执行列的默认值不是 0';
    END IF;

    BEGIN
        INSERT INTO biz_framework_agreement_item (agreement_id, line_no, material_id, unit_id, agreed_price)
        SELECT agreement_id, 2, material_id, unit_id, 10
          FROM biz_framework_agreement_item WHERE id = item;
        RAISE EXCEPTION 'V0450 自检失败：同一协议里「同物料」的第二行被接受（uk_biz_fwai_material 失效：会出现两个协议价）';
    EXCEPTION WHEN unique_violation THEN NULL;
    END;

    BEGIN
        INSERT INTO biz_framework_agreement_item (agreement_id, line_no, material_id, unit_id, agreed_price)
        VALUES (probe, 2, gen_random_uuid(), gen_random_uuid(), -1);
        RAISE EXCEPTION 'V0450 自检失败：负协议价被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    BEGIN
        INSERT INTO biz_framework_agreement_item (agreement_id, line_no, material_id, unit_id, agreed_price, tax_rate)
        VALUES (probe, 2, gen_random_uuid(), gen_random_uuid(), 1, 1.5);
        RAISE EXCEPTION 'V0450 自检失败：税率 1.5 被接受（合法域 0-1）';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    BEGIN
        INSERT INTO biz_framework_agreement_item (agreement_id, line_no, material_id, unit_id, agreed_price, max_qty)
        VALUES (probe, 2, gen_random_uuid(), gen_random_uuid(), 1, 0);
        RAISE EXCEPTION 'V0450 自检失败：物料上限 0 被接受（0 上限应写 NULL = 不限）';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 超上限必须写得进去：加急放量不在库层拦（与 V0446 头注释 4 同口径，拦了就是 500）
    UPDATE biz_framework_agreement_item SET executed_qty = 1200, executed_amount = 14814.8136
     WHERE id = item;
    IF (SELECT executed_qty FROM biz_framework_agreement_item WHERE id = item) <> 1200 THEN
        RAISE EXCEPTION 'V0450 自检失败：已执行量超上限（1200 > 1000）居然写不进去 —— 库层不该拦超限';
    END IF;
    BEGIN
        UPDATE biz_framework_agreement_item SET executed_qty = -1 WHERE id = item;
        RAISE EXCEPTION 'V0450 自检失败：负已执行量被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    BEGIN
        INSERT INTO biz_framework_agreement_item (agreement_id, line_no, material_id, unit_id, agreed_price)
        VALUES (gen_random_uuid(), 1, gen_random_uuid(), gen_random_uuid(), 1);
        RAISE EXCEPTION 'V0450 自检失败：agreement_id 指向不存在的协议被接受（外键失效）';
    EXCEPTION WHEN foreign_key_violation THEN NULL;
    END;

    -- ④.7 删单头连带删明细（ON DELETE CASCADE），探针一行都不留
    DELETE FROM biz_framework_agreement WHERE id IN (probe, probe2);
    DELETE FROM biz_framework_agreement WHERE agreement_no IN
        ('FWA-19990101-900003');
    SELECT count(*) INTO v_cnt FROM biz_framework_agreement_item WHERE id = item;
    IF v_cnt <> 0 THEN
        RAISE EXCEPTION 'V0450 自检失败：删协议后物料范围还留着（ON DELETE CASCADE 失效）';
    END IF;
    SELECT count(*) INTO v_cnt FROM biz_framework_agreement WHERE agreement_no LIKE 'FWA-19990101-9%';
    IF v_cnt <> 0 THEN
        RAISE EXCEPTION 'V0450 自检失败：自检探针没清干净（% 行残留）', v_cnt;
    END IF;

    -- ④.8 取号规则只有一行
    SELECT count(*) INTO v_cnt FROM biz_seq
     WHERE seq_type = 'FWA' AND deleted_at IS NULL AND prefix = 'FWA' AND separator = '-';
    IF v_cnt <> 1 THEN
        RAISE EXCEPTION 'V0450 自检失败：biz_seq(FWA) 种下 % 行（期望 1）', v_cnt;
    END IF;

    -- ④.9 关键列名一个都不能少（迁移改名时这里会先炸，而不是等到 handler 运行期）
    SELECT string_agg(c, ', ') INTO v_missing
      FROM (VALUES ('agreement_no'),('agreement_name'),('supplier_id'),('supplier_name'),
                   ('start_date'),('end_date'),('sign_date'),('price_mode'),
                   ('price_includes_tax'),('price_currency'),('price_tolerance'),('price_adjust_rule'),
                   ('total_max_qty'),('total_max_amount'),('total_executed_qty'),('total_executed_amount'),
                   ('status'),('approval_instance_id'),('deleted_at')) AS v(c)
     WHERE NOT EXISTS (SELECT 1 FROM information_schema.columns
                        WHERE table_schema = 'public' AND table_name = 'biz_framework_agreement'
                          AND column_name = v.c);
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0450 自检失败：biz_framework_agreement 缺列：%', v_missing;
    END IF;
    SELECT string_agg(c, ', ') INTO v_missing
      FROM (VALUES ('agreement_id'),('line_no'),('material_id'),('unit_id'),('agreed_price'),
                   ('tax_rate'),('max_qty'),('executed_qty'),('executed_amount')) AS v(c)
     WHERE NOT EXISTS (SELECT 1 FROM information_schema.columns
                        WHERE table_schema = 'public' AND table_name = 'biz_framework_agreement_item'
                          AND column_name = v.c);
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0450 自检失败：biz_framework_agreement_item 缺列：%', v_missing;
    END IF;

    RAISE NOTICE 'M4-07/V0450: 框架协议两表（有效期 + 价格条款 + 数量上限，供应商 + 物料范围）+ 取号 FWA 自检通过';
END $$;
