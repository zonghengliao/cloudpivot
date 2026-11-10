-- ============================================================================
-- V0374：成本核算升级 · 二期三表（标准成本 + 差异）（ARCH-9 §3.2 ⑪⑫⑬）+ 自检
--
-- 卡：t_b8b96fac（COST-B1-4【成本】迁移 V0372）—— 拆卡清单 §8.1 的 B1-4
--     「二期三表（标准成本/差异，先建表，代码后写）」
-- 依据：docs/architecture/ARCH-9-成本核算升级设计.md §3.2 ⑪⑫⑬、§4.9 差异分析
--       （该节 DDL 已在真库跑通并 ROLLBACK，见
--         docs/evidence/2026-11-10-arch9-3-cost-ddl/01_cost_ddl_check.sql 与 .out）
--
-- # 为什么号段是 V0374 而不是卡面写的 V0372
--   V0372（分摊两表）/ V0373（期间两表）已被前置卡 B1-3 用掉，nik 迁移加载器对
--   版本号做跨目录全局唯一校验（重号 → 服务启动直接失败，AGENTS 约定 6）。
--   设计 §7.1 的清单（V0368~V0379）因 B1-1/B1-2/B1-3 各顺延过，整体后移 2 位：
--     B1-4 = V0374（本迁移）、B1-5 = V0375/V0376、B1-6 = V0377、B1-7 = V0378/0379/0381
--
-- 本迁移建 3 张表（**只建表，不写业务代码** —— 二期的计算逻辑另排 S-* 卡）：
--   ⑪ biz_cost_standard       标准成本卡（卡头）：单位标准料/工/费，按生效期版本化
--   ⑫ biz_cost_standard_item  标准成本卡明细：材料标准用量/标准价（量差需要标准 BOM）
--   ⑬ biz_cost_variance       差异明细：六类差异（材料价差/量差、人工率差/效差、费用耗费/产量）
--
-- # ★ 这三张表**不参与存货计价**（设计 D-15）
--   二期只做差异分析，差异**不入账**（1404 一分不动）。理由见 §4.9.3：
--   标准成本 + 差异入 1404 + 期末按差异率分摊 = 计划成本法，推翻移动加权，
--   属另一个决策（ARCH-4 D9）。表在，所以「看得见差异」；账不动，所以零错账风险。
--   将来真要入账，新增 biz_cost_variance_alloc 并走 ADR —— 与本卡的表结构不冲突。
--
-- # 三条会**静默算错账**的库层防线（自检 B3~B9 逐条反证）
--   ① ck_biz_cost_std_sum    ：标准总成本 = 标准料 + 标准工 + 标准费
--      —— 卡头合计与三要素对不上，差异报表的分母就是错的（每个差异都被放大）。
--   ② ck_biz_cost_std_labor  ：标准人工成本 = round(标准工时 × 标准工资率, 6)
--      —— 人工率差/效差公式里的「标准工资率」靠这列；列值和工时×费率脱钩，
--         率差与效差之和就不再等于「实际 − 标准」（§4.9.2 恒等式破掉）。
--   ③ ck_biz_cost_std_item_cal：标准金额 = round(标准用量 × 标准价, 6)
--      —— 材料量差 = (实际用量 − 标准用量) × 标准单价，明细金额算错则量差错。
--   ④ ck_biz_cost_var_sign   ：direction 与 amount 符号一致（不利≥0 / 有利≤0）
--      —— §4.9.4「金额正数=不利、负数=有利」是报表口径；两者不锁死，
--         同一差异会出现「标了不利、金额是负」的自相矛盾行，汇总时正负抵消。
--   注：amount 用 NUMERIC(18,4)、标准单价/费率用 NUMERIC(18,6)，
--       与 §3.2 一致；round(...,6) 在 18,6 列上是精确运算，不引入浮点尾差。
--
-- # 外键与弱引用
--   biz_cost_standard_item.standard_id → biz_cost_standard  ON DELETE CASCADE
--     （明细是卡头的从属组成部分；删卡头必须连带删明细，否则留下无主标准用量）
--   biz_cost_variance.standard_id       → biz_cost_standard  ON DELETE RESTRICT
--   biz_cost_variance.cost_object_id    → biz_cost_object    ON DELETE RESTRICT
--     （差异是**已出的报表结论**，主数据有差异记录就不许删 —— 与 period_item 同口径）
--   material_id / component_material_id / create_by 全部弱引用、不建 FK（设计 §3.4：
--   主数据软删不能锁死历史成本口径）。
--   自检 A5 用 confdeltype 逐条断言：'c' = CASCADE、'r' = RESTRICT。
--
-- # 幂等约定
--   表 / 索引一律 IF NOT EXISTS；注释走 COMMENT ON（重复执行结果不变）；
--   自检探针行按 remark = 'V0374-SELFCHECK' 删干净再用 ⇒ 重复执行零残留。
--
-- # 设计的一处**有意留白**（不是漏写，已具名记录在卡内）
--   biz_cost_variance 没有唯一键、没有 deleted_at、没有 status：
--   差异是「按期间重算 = 整删重灌」的计算产物（同 §4.4.5 分摊结果的口径），
--   幂等由写入方（S-9 卡）保证，不靠库层唯一键 —— 设计 §3.2 未给唯一键，此处不擅自加。
-- ============================================================================

-- ----------------------------------------------------------------------------
-- 0. 前置护栏：biz_cost_object 不在就直说「先回滚/先跑哪张卡」
--    （biz_cost_variance.cost_object_id 有物理外键指向它，缺表则本迁移直接语法错）
-- ----------------------------------------------------------------------------
DO $$
BEGIN
    IF to_regclass('public.biz_cost_object') IS NULL THEN
        RAISE EXCEPTION 'V0374 拒绝执行：biz_cost_object 不存在，请先执行 V0370（COST-B1-1）';
    END IF;
END $$;

-- ----------------------------------------------------------------------------
-- ⑪ biz_cost_standard —— 标准成本卡（卡头）
--    一个物料在一个生效日期上只允许一张**活**卡（唯一键见下）。
--    版本化靠 (material_id, effective_from) 表达「哪天起用哪版」，
--    version_no 只是给人看的版本序号（不进唯一键：同一生效日重复建卡已由唯一键拦掉）。
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_cost_standard (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    material_id        UUID          NOT NULL,
    version_no         INT           NOT NULL DEFAULT 1,
    effective_from     DATE          NOT NULL,
    effective_to       DATE,
    std_material_cost  NUMERIC(18,6) NOT NULL DEFAULT 0,
    std_labor_cost     NUMERIC(18,6) NOT NULL DEFAULT 0,
    std_overhead_cost  NUMERIC(18,6) NOT NULL DEFAULT 0,
    std_total_cost     NUMERIC(18,6) NOT NULL DEFAULT 0,
    std_labor_hours    NUMERIC(18,4) NOT NULL DEFAULT 0,             -- 单位标准工时
    std_labor_rate     NUMERIC(18,6) NOT NULL DEFAULT 0,             -- 标准工资率
    status             SMALLINT      NOT NULL DEFAULT 1,
    remark             TEXT,
    created_at         TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at         TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by          UUID, update_by UUID, deleted_at TIMESTAMPTZ,
    CONSTRAINT ck_biz_cost_std_money CHECK (std_material_cost >= 0 AND std_labor_cost >= 0
                                            AND std_overhead_cost >= 0 AND std_total_cost >= 0),
    CONSTRAINT ck_biz_cost_std_sum   CHECK (std_total_cost = std_material_cost + std_labor_cost + std_overhead_cost),
    CONSTRAINT ck_biz_cost_std_dates CHECK (effective_to IS NULL OR effective_to >= effective_from),
    CONSTRAINT ck_biz_cost_std_ver   CHECK (version_no >= 1),
    CONSTRAINT ck_biz_cost_std_labor CHECK (std_labor_cost = round(std_labor_hours * std_labor_rate, 6))
);

COMMENT ON TABLE  biz_cost_standard IS '标准成本卡（卡头）：单位标准料/工/费，按生效期版本化；只用于差异分析，不参与存货计价（设计 D-15）';
COMMENT ON COLUMN biz_cost_standard.material_id IS '成品/半成品物料（弱引用 biz_material，设计 §3.4 不建 FK）';
COMMENT ON COLUMN biz_cost_standard.version_no IS '版本序号（给人看）：真正决定「哪天起用哪版」的是 effective_from';
COMMENT ON COLUMN biz_cost_standard.effective_from IS '生效起始日：与 material_id 组成唯一键（同物料同生效日只许一张活卡）';
COMMENT ON COLUMN biz_cost_standard.effective_to IS '生效截止日：NULL = 至今有效；必须 >= effective_from';
COMMENT ON COLUMN biz_cost_standard.std_material_cost IS '单位标准材料成本：应等于明细 std_amount 之和（明细见 biz_cost_standard_item）';
COMMENT ON COLUMN biz_cost_standard.std_labor_cost IS '单位标准人工成本：由 ck_biz_cost_std_labor 锁死 = round(标准工时 × 标准工资率, 6)';
COMMENT ON COLUMN biz_cost_standard.std_total_cost IS '单位标准总成本：由 ck_biz_cost_std_sum 锁死 = 料 + 工 + 费（差异报表的分母）';
COMMENT ON COLUMN biz_cost_standard.std_labor_hours IS '单位标准工时（18,4）：人工效率差异 (实际工时 − 标准工时 × 完工数) × 标准工资率';
COMMENT ON COLUMN biz_cost_standard.std_labor_rate IS '标准工资率（18,6）：人工工资率差异 (实际工资率 − 标准工资率) × 实际工时';
COMMENT ON COLUMN biz_cost_standard.status IS '1 启用 / 0 停用（与全库 SMALLINT status 口径一致）；停用不参与新期间取标准，历史差异保留';

-- ★ 同物料同生效日只许一张活卡（软删后可重建 —— 录错卡删掉重来的唯一路径）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_std
    ON biz_cost_standard (material_id, effective_from) WHERE deleted_at IS NULL;

-- ----------------------------------------------------------------------------
-- ⑫ biz_cost_standard_item —— 标准成本卡明细
--    材料量差必须有「标准用量」可依（§4.9.1: (实际用量 − 标准用量) × 标准单价），
--    而标准用量 = 标准单耗 × 完工数 ⇒ 卡头一个人工/费用数字不够，必须有 BOM 级明细。
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_cost_standard_item (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    standard_id           UUID          NOT NULL REFERENCES biz_cost_standard(id) ON DELETE CASCADE,
    line_no               INT           NOT NULL,
    component_material_id UUID          NOT NULL,
    std_qty               NUMERIC(18,6) NOT NULL,
    std_price             NUMERIC(18,6) NOT NULL,
    std_amount            NUMERIC(18,6) NOT NULL,
    created_at            TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT ck_biz_cost_std_item_num CHECK (std_qty > 0 AND std_price >= 0 AND std_amount >= 0),
    CONSTRAINT ck_biz_cost_std_item_cal CHECK (std_amount = round(std_qty * std_price, 6))
);

COMMENT ON TABLE  biz_cost_standard_item IS '标准成本卡明细（标准 BOM）：每个耗用物料的单位标准用量/标准价；材料量差的计算依据';
COMMENT ON COLUMN biz_cost_standard_item.standard_id IS '所属标准成本卡（ON DELETE CASCADE：明细是卡头的组成部分，删卡头连带删明细）';
COMMENT ON COLUMN biz_cost_standard_item.line_no IS '行号：与 standard_id 组成唯一键（同一张卡里行号不重复）';
COMMENT ON COLUMN biz_cost_standard_item.component_material_id IS '耗用物料（弱引用 biz_material）：不建 FK，主数据软删不能锁死历史标准';
COMMENT ON COLUMN biz_cost_standard_item.std_qty IS '单位标准用量（必须 > 0）：量差 = (实际用量 − 标准用量) × 标准单价';
COMMENT ON COLUMN biz_cost_standard_item.std_price IS '标准单价（18,6）：价差 = (实际单价 − 标准单价) × 实际用量';
COMMENT ON COLUMN biz_cost_standard_item.std_amount IS '标准金额：由 ck_biz_cost_std_item_cal 锁死 = round(标准用量 × 标准价, 6)';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_std_item ON biz_cost_standard_item (standard_id, line_no);

-- 明细表没有 deleted_at：明细随卡头版本一起整删重灌（改标准 = 删旧卡建新卡，留痕靠卡头版本）

-- ----------------------------------------------------------------------------
-- ⑬ biz_cost_variance —— 差异明细（六类）
--    §4.9.1 六类：MAT_PRICE / MAT_QTY / LABOR_RATE / LABOR_EFF / OH_SPEND / OH_VOLUME
--    §4.9.2 两因素恒等式（**结算/分析时必须做成断言**）：
--        价差 + 量差 = 实际材料成本 − 标准材料成本
--        率差 + 效差 = 实际人工成本 − 标准人工成本
--      自检 B10 用 §4.9.2 的算例 C 真插两行，验「Σ amount = 实际 − 标准」，
--      即 schema 的符号约定（正=不利）确实让恒等式成立。
--    制造费用用两差异法（耗费 + 产量），不做三差异（§4.9.1 明确取舍）。
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_cost_variance (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    period_code    VARCHAR(7)    NOT NULL,
    cost_object_id UUID          REFERENCES biz_cost_object(id) ON DELETE RESTRICT,
    standard_id    UUID          REFERENCES biz_cost_standard(id) ON DELETE RESTRICT,
    variance_type  VARCHAR(12)   NOT NULL,
    element        VARCHAR(12)   NOT NULL,
    material_id    UUID,
    actual_qty     NUMERIC(18,6), std_qty    NUMERIC(18,6),
    actual_price   NUMERIC(18,6), std_price  NUMERIC(18,6),
    amount         NUMERIC(18,4) NOT NULL,                           -- 正=不利，负=有利
    direction      VARCHAR(12)   NOT NULL,                           -- FAVORABLE / UNFAVORABLE
    remark         VARCHAR(255),
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by      UUID,
    CONSTRAINT ck_biz_cost_var_type CHECK (variance_type IN
        ('MAT_PRICE','MAT_QTY','LABOR_RATE','LABOR_EFF','OH_SPEND','OH_VOLUME')),
    CONSTRAINT ck_biz_cost_var_elem  CHECK (element IN ('MATERIAL','LABOR','OVERHEAD')),
    CONSTRAINT ck_biz_cost_var_dir   CHECK (direction IN ('FAVORABLE','UNFAVORABLE')),
    CONSTRAINT ck_biz_cost_var_period CHECK (period_code ~ '^\d{4}-(0[1-9]|1[0-2])$'),
    CONSTRAINT ck_biz_cost_var_sign  CHECK (
        (direction = 'UNFAVORABLE' AND amount >= 0) OR (direction = 'FAVORABLE' AND amount <= 0))
);

COMMENT ON TABLE  biz_cost_variance IS '差异明细：六类差异（材料价差/量差、人工率差/效差、费用耗费/产量）；只出报表不入账（设计 D-15）';
COMMENT ON COLUMN biz_cost_variance.period_code IS '会计期间 yyyy-MM：按「期间 × 成本对象 × 差异类型」钻取（§4.9.4）';
COMMENT ON COLUMN biz_cost_variance.cost_object_id IS '成本对象（ON DELETE RESTRICT：差异是已出的报表结论，有结论的主数据不许删）';
COMMENT ON COLUMN biz_cost_variance.standard_id IS '取数所依据的标准成本卡（ON DELETE RESTRICT：删卡头会让历史差异失去依据）';
COMMENT ON COLUMN biz_cost_variance.variance_type IS 'MAT_PRICE/MAT_QTY/LABOR_RATE/LABOR_EFF/OH_SPEND/OH_VOLUME（§4.9.1 六类公式）';
COMMENT ON COLUMN biz_cost_variance.element IS '成本要素：MATERIAL/LABOR/OVERHEAD（汇总到 period_item 三要素口径）';
COMMENT ON COLUMN biz_cost_variance.amount IS '差异金额（18,4）：正=不利、负=有利；由 ck_biz_cost_var_sign 与 direction 锁死符号一致';
COMMENT ON COLUMN biz_cost_variance.direction IS 'FAVORABLE 有利 / UNFAVORABLE 不利（与 amount 符号一致，报表直接显示）';

CREATE INDEX IF NOT EXISTS idx_biz_cost_var_period ON biz_cost_variance (period_code, variance_type);
CREATE INDEX IF NOT EXISTS idx_biz_cost_var_obj    ON biz_cost_variance (cost_object_id, period_code);

-- ----------------------------------------------------------------------------
-- ① 自检（写进迁移，部署即校验；失败即整笔回滚 ⇒ 不存在「半建好的表」）
--
--    分两类断言：
--      A. 形态：表 / 唯一索引谓词 / CHECK 数量 / 外键删除动作（CASCADE vs RESTRICT）/ 列精度
--      B. 行为：真插数据，把每条关键约束的**反面**跑一遍；含 §4.9.2 两因素恒等式
--         的活体验证（Σ两因素 = 实际 − 标准）。探针行断言后立即清理，重复执行零残留。
-- ----------------------------------------------------------------------------
DO $$
DECLARE
    v_def  TEXT;
    v_norm TEXT;
    v_n    INT;
    v_fmt  TEXT;
    v_col  TEXT;
    v_want TEXT;
BEGIN
    -- ===== A. 形态 =====

    -- A1 三张表都在
    IF to_regclass('public.biz_cost_standard') IS NULL THEN
        RAISE EXCEPTION 'V0374 自检失败：biz_cost_standard 未建出来';
    END IF;
    IF to_regclass('public.biz_cost_standard_item') IS NULL THEN
        RAISE EXCEPTION 'V0374 自检失败：biz_cost_standard_item 未建出来';
    END IF;
    IF to_regclass('public.biz_cost_variance') IS NULL THEN
        RAISE EXCEPTION 'V0374 自检失败：biz_cost_variance 未建出来';
    END IF;

    -- A2 ★ uk_biz_cost_std：(material_id, effective_from) + 软删
    --    pg_indexes.indexdef 会给部分索引条件多套括号（WHERE ((deleted_at IS NULL))），
    --    所以空格与括号都去掉只比语义。
    SELECT indexdef INTO v_def FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_biz_cost_std';
    IF v_def IS NULL THEN
        RAISE EXCEPTION 'V0374 自检失败：uk_biz_cost_std 不存在';
    END IF;
    IF v_def NOT LIKE 'CREATE UNIQUE%' THEN
        RAISE EXCEPTION 'V0374 自检失败：uk_biz_cost_std 不是 UNIQUE 索引：%', v_def;
    END IF;
    v_norm := regexp_replace(replace(v_def, ' ', ''), '[()'']', '', 'g');
    IF v_norm NOT LIKE '%material_id,effective_from%' THEN
        RAISE EXCEPTION 'V0374 自检失败：uk_biz_cost_std 的列组合不是 (material_id, effective_from)：%', v_def;
    END IF;
    IF v_norm NOT LIKE '%deleted_atISNULL%' THEN
        RAISE EXCEPTION 'V0374 自检失败：uk_biz_cost_std 缺软删条件（卡录错删掉后无法重建）: %', v_def;
    END IF;

    -- A3 uk_biz_cost_std_item：(standard_id, line_no) + UNIQUE
    SELECT indexdef INTO v_def FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_biz_cost_std_item';
    IF v_def IS NULL OR v_def NOT LIKE 'CREATE UNIQUE%' THEN
        RAISE EXCEPTION 'V0374 自检失败：uk_biz_cost_std_item 不存在或不是 UNIQUE：%', coalesce(v_def, '(缺)');
    END IF;
    v_norm := regexp_replace(replace(v_def, ' ', ''), '[()]', '', 'g');
    IF v_norm NOT LIKE '%standard_id,line_no%' THEN
        RAISE EXCEPTION 'V0374 自检失败：uk_biz_cost_std_item 的列组合不是 (standard_id, line_no)：%', v_def;
    END IF;

    -- A4 CHECK 约束按名单点名核对（漏一条 = 一条脏数据入口）
    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE conrelid = 'biz_cost_standard'::regclass AND contype = 'c'
       AND conname IN ('ck_biz_cost_std_money','ck_biz_cost_std_sum','ck_biz_cost_std_dates',
                       'ck_biz_cost_std_ver','ck_biz_cost_std_labor');
    IF v_n <> 5 THEN
        RAISE EXCEPTION 'V0374 自检失败：biz_cost_standard 的 5 条 CHECK 约束只有 % 条落库', v_n;
    END IF;

    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE conrelid = 'biz_cost_standard_item'::regclass AND contype = 'c'
       AND conname IN ('ck_biz_cost_std_item_num','ck_biz_cost_std_item_cal');
    IF v_n <> 2 THEN
        RAISE EXCEPTION 'V0374 自检失败：biz_cost_standard_item 的 2 条 CHECK 约束只有 % 条落库', v_n;
    END IF;

    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE conrelid = 'biz_cost_variance'::regclass AND contype = 'c'
       AND conname IN ('ck_biz_cost_var_type','ck_biz_cost_var_elem','ck_biz_cost_var_dir',
                       'ck_biz_cost_var_period','ck_biz_cost_var_sign');
    IF v_n <> 5 THEN
        RAISE EXCEPTION 'V0374 自检失败：biz_cost_variance 的 5 条 CHECK 约束只有 % 条落库', v_n;
    END IF;

    -- A5 外键：standard 0 条（物料/人员全弱引用）；standard_item 恰 1 条且 CASCADE；
    --    variance 恰 2 条且都是 RESTRICT
    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE contype = 'f' AND conrelid = 'biz_cost_standard'::regclass;
    IF v_n <> 0 THEN
        RAISE EXCEPTION 'V0374 自检失败：biz_cost_standard 上有 % 条外键（物料/人员必须弱引用，设计 §3.4）', v_n;
    END IF;

    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE contype = 'f' AND conrelid = 'biz_cost_standard_item'::regclass;
    IF v_n <> 1 THEN
        RAISE EXCEPTION 'V0374 自检失败：biz_cost_standard_item 的外键应为 1 条（仅 standard_id），实际 %', v_n;
    END IF;

    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE contype = 'f' AND conrelid = 'biz_cost_standard_item'::regclass
       AND confrelid = 'biz_cost_standard'::regclass AND confdeltype = 'c';
    IF v_n <> 1 THEN
        RAISE EXCEPTION 'V0374 自检失败：standard_item.standard_id → biz_cost_standard 必须是 ON DELETE CASCADE（删卡头会留下无主标准用量）';
    END IF;

    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE contype = 'f' AND conrelid = 'biz_cost_variance'::regclass;
    IF v_n <> 2 THEN
        RAISE EXCEPTION 'V0374 自检失败：biz_cost_variance 的外键应为 2 条（cost_object / standard），实际 %', v_n;
    END IF;

    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE contype = 'f' AND conrelid = 'biz_cost_variance'::regclass
       AND confrelid = 'biz_cost_object'::regclass AND confdeltype = 'r';
    IF v_n <> 1 THEN
        RAISE EXCEPTION 'V0374 自检失败：variance.cost_object_id → biz_cost_object 必须是 ON DELETE RESTRICT（删主数据会带走差异结论）';
    END IF;

    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE contype = 'f' AND conrelid = 'biz_cost_variance'::regclass
       AND confrelid = 'biz_cost_standard'::regclass AND confdeltype = 'r';
    IF v_n <> 1 THEN
        RAISE EXCEPTION 'V0374 自检失败：variance.standard_id → biz_cost_standard 必须是 ON DELETE RESTRICT（删卡头会让历史差异失去依据）';
    END IF;

    -- A6 列精度：设计 §3.2 标准价/费率 18,6；差异金额 18,4；标准工时 18,4
    --    （标准成本按 6 位存，是为了让 round(qty × price, 6) 在库层精确成立）
    FOR v_col, v_want IN
        SELECT t.col, t.want FROM (VALUES
            ('biz_cost_standard.std_material_cost',  'numeric(18,6)'),
            ('biz_cost_standard.std_labor_cost',     'numeric(18,6)'),
            ('biz_cost_standard.std_overhead_cost',  'numeric(18,6)'),
            ('biz_cost_standard.std_total_cost',     'numeric(18,6)'),
            ('biz_cost_standard.std_labor_hours',    'numeric(18,4)'),
            ('biz_cost_standard.std_labor_rate',     'numeric(18,6)'),
            ('biz_cost_standard_item.std_qty',       'numeric(18,6)'),
            ('biz_cost_standard_item.std_price',     'numeric(18,6)'),
            ('biz_cost_standard_item.std_amount',    'numeric(18,6)'),
            ('biz_cost_variance.amount',             'numeric(18,4)'),
            ('biz_cost_variance.actual_qty',         'numeric(18,6)'),
            ('biz_cost_variance.actual_price',       'numeric(18,6)')
        ) AS t(col, want)
    LOOP
        SELECT format_type(a.atttypid, a.atttypmod) INTO v_fmt
          FROM pg_attribute a
         WHERE a.attrelid = split_part(v_col, '.', 1)::regclass
           AND a.attname  = split_part(v_col, '.', 2)
           AND NOT a.attisdropped;
        IF v_fmt IS DISTINCT FROM v_want THEN
            RAISE EXCEPTION 'V0374 自检失败：% 的类型是 %，设计要求 %（标准价/费率丢精度会让量差、价差算偏）',
                v_col, coalesce(v_fmt, '(缺列)'), v_want;
        END IF;
    END LOOP;

    -- A7 差异查询要用的两条索引必须落库（报表按期间×类型、按对象×期间钻取，§4.9.4）
    SELECT count(*) INTO v_n FROM pg_indexes
     WHERE schemaname = 'public'
       AND indexname IN ('idx_biz_cost_var_period','idx_biz_cost_var_obj');
    IF v_n <> 2 THEN
        RAISE EXCEPTION 'V0374 自检失败：差异报表索引只有 % 条落库（期望 2：期间×类型 / 对象×期间）', v_n;
    END IF;

    RAISE NOTICE 'V0374 形态断言通过（三表 / 12 条 CHECK / 3 条外键删除动作 / 精度）';
END $$;

-- ----------------------------------------------------------------------------
-- ② 行为自检：把每条关键约束的**反面**真跑一遍
--
--    探针键：物料号用固定 uuid（弱引用不校验父行），remark = 'V0374-SELFCHECK'。
--    ★ B10 是 §4.9.2 恒等式的活体验证 —— 用设计里的算例 C 数字，
--      验 schema 的符号约定能让「价差 + 量差 = 实际 − 标准」成立。
-- ----------------------------------------------------------------------------
DO $$
DECLARE
    v_mat   UUID := '00000000-0000-0000-0000-0000000b04a1';
    v_comp  UUID := '00000000-0000-0000-0000-0000000b04a2';
    v_obj   UUID := '00000000-0000-0000-0000-0000000b04b1';
    v_dept  UUID := '00000000-0000-0000-0000-0000000b04b2';
    v_std   UUID;
    v_std2  UUID;
    v_n     INT;
    v_hit   BOOLEAN;
    v_sum   NUMERIC;
BEGIN
    -- ===== 准备：清掉上次残留的探针行（差异 → 标准卡（级联明细）→ 成本对象） =====
    DELETE FROM biz_cost_variance WHERE remark = 'V0374-SELFCHECK';
    DELETE FROM biz_cost_standard WHERE remark = 'V0374-SELFCHECK';
    DELETE FROM biz_cost_object   WHERE id = v_obj;

    INSERT INTO biz_cost_object (id, object_no, product_id, target_warehouse_id, plan_qty,
                                 workshop_dept_id, workshop_dept_name, start_date)
    VALUES (v_obj, 'SELFCHECK-B4-PD', gen_random_uuid(), gen_random_uuid(), 10,
            v_dept, '自检车间', DATE '2026-01-01');

    -- ===== B1 正面：标准卡（料 100 + 工 60 + 费 40 = 200；工时 2 × 费率 30 = 60）=====
    INSERT INTO biz_cost_standard (material_id, version_no, effective_from,
                                   std_material_cost, std_labor_cost, std_overhead_cost, std_total_cost,
                                   std_labor_hours, std_labor_rate, remark)
    VALUES (v_mat, 1, DATE '2026-01-01', 100, 60, 40, 200, 2, 30, 'V0374-SELFCHECK')
    RETURNING id INTO v_std;

    -- ===== B2 同物料同生效日的第二张活卡必须被拦（否则两张标准同时生效，差异算谁的）=====
    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_standard (material_id, version_no, effective_from, remark)
        VALUES (v_mat, 2, DATE '2026-01-01', 'V0374-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0374 自检失败：同物料同生效日第二张活卡写入成功（uk_biz_cost_std 未生效 ⇒ 标准二义）';
    END IF;

    -- ===== B3 软删后可以重建同生效日的新版（录错卡删掉重来的唯一路径） =====
    INSERT INTO biz_cost_standard (material_id, version_no, effective_from, std_total_cost,
                                   std_material_cost, remark, deleted_at)
    VALUES (v_mat, 2, DATE '2026-01-02', 100, 100, 'V0374-SELFCHECK', now())
    RETURNING id INTO v_std2;
    INSERT INTO biz_cost_standard (material_id, version_no, effective_from, std_total_cost,
                                   std_material_cost, remark)
    VALUES (v_mat, 2, DATE '2026-01-02', 100, 100, 'V0374-SELFCHECK');

    SELECT count(*) INTO v_n FROM biz_cost_standard
     WHERE material_id = v_mat AND effective_from = DATE '2026-01-02' AND deleted_at IS NULL;
    IF v_n <> 1 THEN
        RAISE EXCEPTION 'V0374 自检失败：软删后同物料同生效日重建失败（期望 1 条活卡，实际 %）', v_n;
    END IF;

    -- ===== B4 ck_biz_cost_std_sum：总成本 ≠ 料 + 工 + 费（差异报表分母就是错的） =====
    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_standard (material_id, version_no, effective_from,
                                       std_material_cost, std_labor_cost, std_total_cost, remark)
        VALUES (gen_random_uuid(), 1, DATE '2026-02-01', 100, 60, 200, 'V0374-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0374 自检失败：标准总成本 ≠ 料+工+费 写入成功（ck_biz_cost_std_sum 未生效）';
    END IF;

    -- ===== B5 ck_biz_cost_std_labor：人工成本与 工时×工资率 脱钩（§4.9.2 恒等式必破） =====
    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_standard (material_id, version_no, effective_from,
                                       std_labor_cost, std_total_cost, std_labor_hours, std_labor_rate, remark)
        VALUES (gen_random_uuid(), 1, DATE '2026-02-01', 70, 70, 2, 30, 'V0374-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0374 自检失败：标准人工成本 ≠ round(工时 × 工资率,6) 写入成功（ck_biz_cost_std_labor 未生效 ⇒ 率差/效差之和不再等于实际−标准）';
    END IF;

    -- ===== B6 负标准成本 / 生效日倒退 / 版本号 0 三连反证 =====
    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_standard (material_id, version_no, effective_from, std_material_cost,
                                       std_total_cost, remark)
        VALUES (gen_random_uuid(), 1, DATE '2026-02-01', -1, -1, 'V0374-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0374 自检失败：负标准成本写入成功（ck_biz_cost_std_money 未生效）';
    END IF;

    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_standard (material_id, version_no, effective_from, effective_to, remark)
        VALUES (gen_random_uuid(), 1, DATE '2026-02-01', DATE '2026-01-01', 'V0374-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0374 自检失败：effective_to < effective_from 写入成功（ck_biz_cost_std_dates 未生效）';
    END IF;

    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_standard (material_id, version_no, effective_from, remark)
        VALUES (gen_random_uuid(), 0, DATE '2026-02-01', 'V0374-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0374 自检失败：version_no=0 写入成功（ck_biz_cost_std_ver 未生效）';
    END IF;

    -- ===== B7 明细：金额 = round(用量 × 单价, 6) 正面 + 三条反证 =====
    INSERT INTO biz_cost_standard_item (standard_id, line_no, component_material_id,
                                        std_qty, std_price, std_amount)
    VALUES (v_std, 1, v_comp, 1.1, 12, 13.2);

    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_standard_item (standard_id, line_no, component_material_id,
                                            std_qty, std_price, std_amount)
        VALUES (v_std, 2, v_comp, 1.1, 12, 13.5);
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0374 自检失败：明细金额 ≠ round(用量 × 单价,6) 写入成功（ck_biz_cost_std_item_cal 未生效 ⇒ 材料量差算错）';
    END IF;

    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_standard_item (standard_id, line_no, component_material_id,
                                            std_qty, std_price, std_amount)
        VALUES (v_std, 2, v_comp, 0, 12, 0);
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0374 自检失败：标准用量=0 写入成功（ck_biz_cost_std_item_num 未生效 ⇒ 量差恒等于实际用量）';
    END IF;

    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_standard_item (standard_id, line_no, component_material_id,
                                            std_qty, std_price, std_amount)
        VALUES (v_std, 1, v_comp, 1.1, 12, 13.2);
        v_hit := true;
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0374 自检失败：同卡同行号第二条明细写入成功（uk_biz_cost_std_item 未生效）';
    END IF;

    -- ===== B8 删卡头 ⇒ 明细 CASCADE 跟着走（不许留无主标准用量） =====
    DELETE FROM biz_cost_standard WHERE id = v_std2;
    SELECT count(*) INTO v_n FROM biz_cost_standard_item WHERE standard_id = v_std2;
    IF v_n <> 0 THEN
        RAISE EXCEPTION 'V0374 自检失败：删卡头后明细残留 % 行（standard_id 外键不是 ON DELETE CASCADE）', v_n;
    END IF;

    -- ===== B9 差异符号约定：不利≥0 / 有利≤0（四连反证） =====
    -- 正面：不利 +600、有利 −600 都能落
    INSERT INTO biz_cost_variance (period_code, cost_object_id, standard_id, variance_type, element,
                                   material_id, actual_qty, std_qty, actual_price, std_price,
                                   amount, direction, remark)
    VALUES ('2026-01', v_obj, v_std, 'MAT_PRICE', 'MATERIAL', v_comp, 1200, 880, 12.5, 12,
            600, 'UNFAVORABLE', 'V0374-SELFCHECK');
    INSERT INTO biz_cost_variance (period_code, variance_type, element, amount, direction, remark)
    VALUES ('2026-01', 'LABOR_RATE', 'LABOR', -180, 'FAVORABLE', 'V0374-SELFCHECK');

    -- 反证①：标了「不利」金额却是负（汇总时正负抵消，报表自相矛盾）
    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_variance (period_code, variance_type, element, amount, direction, remark)
        VALUES ('2026-01', 'MAT_QTY', 'MATERIAL', -100, 'UNFAVORABLE', 'V0374-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0374 自检失败：direction=UNFAVORABLE 且 amount<0 写入成功（ck_biz_cost_var_sign 未生效 ⇒ 报表正负抵消）';
    END IF;

    -- 反证②：标了「有利」金额却是正
    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_variance (period_code, variance_type, element, amount, direction, remark)
        VALUES ('2026-01', 'MAT_QTY', 'MATERIAL', 100, 'FAVORABLE', 'V0374-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0374 自检失败：direction=FAVORABLE 且 amount>0 写入成功（ck_biz_cost_var_sign 未生效）';
    END IF;

    -- 反证③：六类之外的差异类型 / 要素值域
    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_variance (period_code, variance_type, element, amount, direction, remark)
        VALUES ('2026-01', 'MAT_MIX', 'MATERIAL', 100, 'UNFAVORABLE', 'V0374-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0374 自检失败：variance_type=MAT_MIX 写入成功（ck_biz_cost_var_type 未生效 ⇒ 报表出现算不出的类型）';
    END IF;

    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_variance (period_code, variance_type, element, amount, direction, remark)
        VALUES ('2026-01', 'MAT_QTY', 'ASSET', 100, 'UNFAVORABLE', 'V0374-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0374 自检失败：element=ASSET 写入成功（ck_biz_cost_var_elem 未生效）';
    END IF;

    -- 反证④：期间格式（差异按月汇总，期间串错就归不进任何一期）
    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_variance (period_code, variance_type, element, amount, direction, remark)
        VALUES ('2026-13', 'MAT_QTY', 'MATERIAL', 100, 'UNFAVORABLE', 'V0374-SELFCHECK');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0374 自检失败：period_code=2026-13 写入成功（ck_biz_cost_var_period 未生效）';
    END IF;

    -- ===== B10 ★ §4.9.2 两因素恒等式（设计算例 C 的真数字） =====
    --    标准 800 件 × 1.1 × 12 = 10560；实际 1200 × 12.5 = 15000
    --    价差 (12.5−12)×1200 = +600；量差 (1200−880)×12 = +3840；合计 4440 = 15000−10560 ✅
    DELETE FROM biz_cost_variance WHERE remark = 'V0374-SELFCHECK';
    INSERT INTO biz_cost_variance (period_code, cost_object_id, standard_id, variance_type, element, material_id,
                                   actual_qty, std_qty, actual_price, std_price,
                                   amount, direction, remark)
    VALUES ('2026-01', v_obj, v_std, 'MAT_PRICE', 'MATERIAL', v_comp, 1200, 880, 12.5, 12,
            600, 'UNFAVORABLE', 'V0374-SELFCHECK'),
           ('2026-01', v_obj, v_std, 'MAT_QTY',   'MATERIAL', v_comp, 1200, 880, 12.5, 12,
            3840, 'UNFAVORABLE', 'V0374-SELFCHECK');

    SELECT sum(amount) INTO v_sum FROM biz_cost_variance
     WHERE remark = 'V0374-SELFCHECK' AND element = 'MATERIAL';
    IF v_sum IS DISTINCT FROM 4440 THEN
        RAISE EXCEPTION 'V0374 自检失败：两因素之和 % ≠ 实际 − 标准 4440（§4.9.2 恒等式在库层不成立）', v_sum;
    END IF;
    IF v_sum IS DISTINCT FROM (15000::numeric - 10560::numeric) THEN
        RAISE EXCEPTION 'V0374 自检失败：价差 + 量差 % ≠ 实际材料成本 15000 − 标准材料成本 10560', v_sum;
    END IF;

    -- ===== B11 外键 RESTRICT：有差异记录时，删成本对象 / 删标准卡都必须被拦 =====
    v_hit := false;
    BEGIN
        DELETE FROM biz_cost_object WHERE id = v_obj;
        v_hit := true;
    EXCEPTION WHEN restrict_violation OR foreign_key_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0374 自检失败：有差异记录时成本对象被删掉（variance→cost_object 不是 ON DELETE RESTRICT）';
    END IF;

    v_hit := false;
    BEGIN
        DELETE FROM biz_cost_standard WHERE id = v_std;
        v_hit := true;
    EXCEPTION WHEN restrict_violation OR foreign_key_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0374 自检失败：有差异记录时标准卡被删掉（variance→standard 不是 ON DELETE RESTRICT，历史差异失去依据）';
    END IF;

    -- ===== 清理探针（重复执行零残留） =====
    -- 明细虽随卡头 CASCADE，这里仍显式删一遍：一来「零残留」要能**逐表**计数，
    -- 二来先删明细再删卡头，避免中间态里 RESTRICT 反证留下的不可预期状态。
    DELETE FROM biz_cost_variance     WHERE remark = 'V0374-SELFCHECK';
    DELETE FROM biz_cost_standard_item
     WHERE standard_id IN (SELECT id FROM biz_cost_standard WHERE remark = 'V0374-SELFCHECK');
    DELETE FROM biz_cost_standard     WHERE remark = 'V0374-SELFCHECK';
    DELETE FROM biz_cost_object       WHERE id = v_obj;

    SELECT count(*) INTO v_n FROM biz_cost_variance WHERE remark = 'V0374-SELFCHECK';
    SELECT v_n + count(*) INTO v_n FROM biz_cost_standard WHERE remark = 'V0374-SELFCHECK';
    SELECT v_n + count(*) INTO v_n FROM biz_cost_object   WHERE id = v_obj;
    -- 两张探针卡（v_std 走显式删、v_std2 已在 B8 被 CASCADE 删）的明细都不许剩
    SELECT v_n + count(*) INTO v_n FROM biz_cost_standard_item WHERE standard_id IN (v_std, v_std2);
    IF v_n <> 0 THEN
        RAISE EXCEPTION 'V0374 自检失败：探针行未清理干净（残留 % 行）', v_n;
    END IF;

    RAISE NOTICE 'V0374 完成：二期三表建成，形态断言 + 11 组行为断言全过（★ 六类差异符号约定 + §4.9.2 两因素恒等式活体验证），探针已清理';
END $$;
