-- ============================================================================
-- V0372：成本核算升级 · 分摊两表（ARCH-9 §3.1 ⑦⑧）+ 自检
--
-- 卡：t_7efea068（COST-B1-3【成本】迁移 V0370/0371）—— 拆卡清单 §8.1 的 B1-3
--     「分摊两表 + 期间两表（含排除 REVERSED 唯一键）」
-- 依据：docs/architecture/ARCH-9-成本核算升级设计.md §3.1 ⑦⑧
--       （该节 DDL 已在真库跑通并 ROLLBACK，见
--         docs/evidence/2026-11-10-arch9-3-cost-ddl/01_cost_ddl_check.sql 与 .out）
--
-- 本卡范围（两个迁移）：
--   V0372（本文件）：⑦ biz_cost_alloc_rule    分摊规则
--                    ⑧ biz_cost_alloc_result  分摊结果（含尾差调整）
--   V0373：          ⑨ biz_cost_period        成本核算批次（期间结算单）★排除 REVERSED 的唯一键
--                    ⑩ biz_cost_period_item   成本计算单
--   二期三表属 B1-4，出库/入库加列属 B1-5，菜单权限/号段字典属 B1-7 —— 一行都不在本文件里。
--
-- # 号段为什么是 V0372/V0373，而不是卡面写的 V0370/V0371
--   卡面标题与设计稿 §8.1 写 B1-3 = V0370/V0371，但这两个号**已经被前置卡用掉**：
--     V0368__receipt_refund_page_and_price_page（收款退款页 + 价格页）
--     V0369__perf_list_order_indexes（N-8 性能基线排序索引）
--     V0370__cost_lot_and_cost_object（B1-1，本卡的前置）
--     V0371__cost_collect_four_tables（B1-2，本卡的前置）
--   nik 迁移加载器对版本号做**跨目录全局唯一**校验
--   （backend/vendor/nik-db/src/migrate/loader.rs::load_from_sources「版本号跨目录全局唯一」），
--   重号会让服务启动直接报错；AGENTS 约定 6 也要求动手前先 `ls backend/migrations/*/` 核对号段。
--   故 B1-3 顺延到 V0372（分摊两表）+ V0373（期间两表）。
--
-- # 唯一键口径（与 V0370/V0371 一致，逐条都有理由）
--   · uk_biz_cost_rule_code：`rule_code` 活行唯一 + `WHERE deleted_at IS NULL`
--     —— 规则停用后再新建同编码不能撞键（规则是主数据，作废保留审计）。
--   · uk_biz_cost_alloc_result：`(period_code, rule_id, cost_object_id, account_code)` 全表唯一，
--     **不带**软删条件 —— 这张表**没有 deleted_at 列**（分摊结果是计算产物，不留软删史）。
--     重算口径见设计 §4.4.5：`DELETE FROM biz_cost_alloc_result WHERE period_code=?` 后重灌，
--     只允许在批次 DRAFT 状态做。所以「同键只能一行」正是想要的语义，不是漏了软删条件。
--
-- # 幂等约定
--   表 / 索引一律 IF NOT EXISTS；注释走 COMMENT ON（重复执行结果不变）；
--   自检先用固定探针键**删一遍再用**，断言后立即清理 ⇒ 重复执行零残留。
-- ============================================================================

-- ----------------------------------------------------------------------------
-- 0. 前置护栏：biz_cost_object 不在就直说「先跑哪张卡」
--    否则 PG 只会抛 `relation "biz_cost_object" does not exist`，看不出依赖关系。
-- ----------------------------------------------------------------------------
DO $$
BEGIN
    IF to_regclass('public.biz_cost_object') IS NULL THEN
        RAISE EXCEPTION 'V0372 拒绝执行：biz_cost_object 不存在，请先执行 V0370（COST-B1-1）';
    END IF;
END $$;

-- ----------------------------------------------------------------------------
-- ⑦ biz_cost_alloc_rule —— 分摊规则
--    一期只做一次分配（车间费用 → 成本对象），biz_type 只有 OVERHEAD；
--    basis 四值 = 四种分配标准（§4.4.2 给了每种的基数定义）。
--    account_code 必须落在 4101.*：配到 5602 的规则永远摊不出成本（风险 R-2）。
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_cost_alloc_rule (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    rule_code        VARCHAR(32)   NOT NULL,
    rule_name        VARCHAR(120)  NOT NULL,
    biz_type         VARCHAR(16)   NOT NULL DEFAULT 'OVERHEAD',      -- 一期 OVERHEAD；三期 STEP_TRANSFER
    source_dept_id   UUID          NOT NULL,                         -- 被摊车间（弱引用 sys_dept，设计 §3.4）
    source_dept_name VARCHAR(120)  NOT NULL,                         -- 车间名快照
    account_code     VARCHAR(30)   NOT NULL,                         -- 被摊科目（4101.01~09）
    basis            VARCHAR(16)   NOT NULL,                         -- LABOR_HOURS/MACHINE_HOURS/OUTPUT_QTY/MATERIAL_COST
    priority         INT           NOT NULL DEFAULT 10,
    effective_from   DATE          NOT NULL,
    effective_to     DATE,
    status           SMALLINT      NOT NULL DEFAULT 1,
    remark           VARCHAR(255),
    created_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by        UUID,
    update_by        UUID,
    deleted_at       TIMESTAMPTZ,
    CONSTRAINT ck_biz_cost_rule_biz     CHECK (biz_type IN ('OVERHEAD')),
    CONSTRAINT ck_biz_cost_rule_basis   CHECK (basis IN ('LABOR_HOURS','MACHINE_HOURS','OUTPUT_QTY','MATERIAL_COST')),
    CONSTRAINT ck_biz_cost_rule_status  CHECK (status IN (0,1)),
    CONSTRAINT ck_biz_cost_rule_dates   CHECK (effective_to IS NULL OR effective_to >= effective_from),
    CONSTRAINT ck_biz_cost_rule_account CHECK (account_code LIKE '4101.%')
);

COMMENT ON TABLE  biz_cost_alloc_rule IS '分摊规则：一期一次分配（车间制造费用 4101.* → 成本对象），按生效期版本化';
COMMENT ON COLUMN biz_cost_alloc_rule.source_dept_id IS '被摊车间（弱引用 sys_dept.id，刻意不建 FK：主数据软删不该锁死历史规则）';
COMMENT ON COLUMN biz_cost_alloc_rule.source_dept_name IS '车间名快照：规则属配置史，车间改名后仍能回溯当时摊的是哪个车间（AGENTS 约定 13）';
COMMENT ON COLUMN biz_cost_alloc_rule.account_code IS '被摊制造费用科目，必须 4101.*（末级）：配成 5602 会摊出 0 元（风险 R-2）';
COMMENT ON COLUMN biz_cost_alloc_rule.basis IS '分配标准：LABOR_HOURS 人工工时 / MACHINE_HOURS 机器工时 / OUTPUT_QTY 完工产量 / MATERIAL_COST 直接材料成本（§4.4.2）';
COMMENT ON COLUMN biz_cost_alloc_rule.priority IS '同一 (车间, 科目, 期间) 命中多条规则时取 priority 最小者（数字小=优先）';
COMMENT ON COLUMN biz_cost_alloc_rule.status IS '1 启用 / 0 停用；停用不改历史分摊结果';

-- 规则编码活行唯一；停用（软删）后可重建同编码
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_rule_code ON biz_cost_alloc_rule (rule_code) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_cost_rule_dept ON biz_cost_alloc_rule (source_dept_id, account_code, status);

-- ----------------------------------------------------------------------------
-- ⑧ biz_cost_alloc_result —— 分摊结果（含尾差调整）
--    一行 = 一条规则摊到一个成本对象的一个科目。
--    pool_amount / basis_total 是**计算当时的快照**：事后改了池金额也不会让历史分摊对不上账。
--    rounding_adj 最大余数法：只有余数最大的那一行非 0，Σalloc_amount = pool_amount（D-9 硬断言）。
--    这张表没有 deleted_at / status：重算=按期间整删重灌（§4.4.5），红冲由批次状态兜。
-- ----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_cost_alloc_result (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    period_code     VARCHAR(7)    NOT NULL,
    rule_id         UUID          NOT NULL REFERENCES biz_cost_alloc_rule(id) ON DELETE RESTRICT,
    cost_object_id  UUID          NOT NULL REFERENCES biz_cost_object(id) ON DELETE RESTRICT,
    dept_id         UUID          NOT NULL,
    dept_name       VARCHAR(120)  NOT NULL,
    account_code    VARCHAR(30)   NOT NULL,
    basis_value     NUMERIC(18,4) NOT NULL,                          -- 本对象基数
    basis_total     NUMERIC(18,4) NOT NULL,                          -- 全车间基数合计（快照）
    pool_amount     NUMERIC(18,4) NOT NULL,                          -- 待分摊总额（快照）
    alloc_amount    NUMERIC(18,4) NOT NULL,                          -- 摊到本对象（含尾差）
    rounding_adj    NUMERIC(18,4) NOT NULL DEFAULT 0,                -- 尾差调整（最大余数法：仅一行非 0）
    created_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by       UUID,
    CONSTRAINT ck_biz_cost_alloc_period CHECK (period_code ~ '^\d{4}-(0[1-9]|1[0-2])$'),
    CONSTRAINT ck_biz_cost_alloc_num    CHECK (basis_value >= 0 AND basis_total >= 0
                                              AND pool_amount >= 0 AND alloc_amount >= 0),
    -- 没有任何基数可摊 ⇒ 只能摊 0（否则就是「凭空分摊」，Σ 守恒断言必挂）
    CONSTRAINT ck_biz_cost_alloc_base   CHECK (basis_total > 0 OR alloc_amount = 0)
);

COMMENT ON TABLE  biz_cost_alloc_result IS '分摊结果：一行=一条规则摊到一个成本对象的一个科目；尾差用最大余数法单行收口（Σ分摊=待分摊总额）';
COMMENT ON COLUMN biz_cost_alloc_result.basis_total IS '全车间基数合计（计算当时快照）：基数为 0 时该规则整体摊 0，事后改基数也不会让历史分摊漂移';
COMMENT ON COLUMN biz_cost_alloc_result.pool_amount IS '待分摊总额（快照）：与 biz_cost_collect 池行的 Σ 同源，差额=归集缺口（B2-9 对账）';
COMMENT ON COLUMN biz_cost_alloc_result.alloc_amount IS '摊到本对象的金额（含尾差）：Σ 等于 pool_amount 是硬断言（设计 D-9）';
COMMENT ON COLUMN biz_cost_alloc_result.rounding_adj IS '尾差调整：最大余数法下只有余数最大的那一行为非 0（算例：1000 元摊 3 个等基数对象 → 333.3333×2 + 333.3334）';
COMMENT ON COLUMN biz_cost_alloc_result.dept_name IS '车间名快照：分摊结果属历史口径';
COMMENT ON COLUMN biz_cost_alloc_result.account_code IS '被摊科目（4101.*）：一行一个科目，便于与凭证分录逐科目对账';

-- 同规则同对象同科目只能一行（重算=按期间整删重灌，见设计 §4.4.5 与文件头说明）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cost_alloc_result
    ON biz_cost_alloc_result (period_code, rule_id, cost_object_id, account_code);
CREATE INDEX IF NOT EXISTS idx_biz_cost_alloc_result_period ON biz_cost_alloc_result (period_code, dept_id);

-- ----------------------------------------------------------------------------
-- ⑨ 自检（写进迁移，部署即校验；失败即整笔回滚 ⇒ 不存在「半建好的表」）
--
--    分两类断言：
--      A. 形态：表 / 唯一索引形状 / CHECK 数量 / 外键动作 / 金额列精度
--         —— 防「唯一键漏软删条件」「FK 写成 CASCADE」「金额掉到 scale=2」这类
--         不会报错、只会以后算错账（或静默丢行）的偏差。
--      B. 行为：真插数据，把每条关键约束的反面跑一遍（基数 0 却摊钱、科目不在 4101、
--         有效期倒挂、同规则同对象重复分摊、有分摊结果时删规则/对象）。
--         探针行断言后立即清理，重复执行零残留。
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

    -- A1 两张表都在
    IF to_regclass('public.biz_cost_alloc_rule') IS NULL THEN
        RAISE EXCEPTION 'V0372 自检失败：biz_cost_alloc_rule 未建出来';
    END IF;
    IF to_regclass('public.biz_cost_alloc_result') IS NULL THEN
        RAISE EXCEPTION 'V0372 自检失败：biz_cost_alloc_result 未建出来';
    END IF;

    -- A2 uk_biz_cost_rule_code：UNIQUE + 软删条件
    SELECT indexdef INTO v_def FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_biz_cost_rule_code';
    IF v_def IS NULL THEN
        RAISE EXCEPTION 'V0372 自检失败：uk_biz_cost_rule_code 不存在';
    END IF;
    IF v_def NOT LIKE 'CREATE UNIQUE%' THEN
        RAISE EXCEPTION 'V0372 自检失败：uk_biz_cost_rule_code 不是 UNIQUE 索引：%', v_def;
    END IF;
    -- 谓词比对先归一化：pg_indexes.indexdef 会给部分索引条件多套一层括号
    -- （`WHERE ((deleted_at IS NULL))`），所以空格和括号都去掉再比，只比「语义形状」。
    v_norm := regexp_replace(replace(v_def, ' ', ''), '[()]', '', 'g');
    IF v_norm NOT LIKE '%WHEREdeleted_atISNULL%' THEN
        RAISE EXCEPTION 'V0372 自检失败：uk_biz_cost_rule_code 缺软删条件（停用规则后再建同编码会撞键）：%', v_def;
    END IF;

    -- A3 uk_biz_cost_alloc_result：UNIQUE + 四列组合（不带软删条件是**有意**的：本表无 deleted_at）
    SELECT indexdef INTO v_def FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_biz_cost_alloc_result';
    IF v_def IS NULL THEN
        RAISE EXCEPTION 'V0372 自检失败：uk_biz_cost_alloc_result 不存在';
    END IF;
    IF v_def NOT LIKE 'CREATE UNIQUE%' THEN
        RAISE EXCEPTION 'V0372 自检失败：uk_biz_cost_alloc_result 不是 UNIQUE 索引：%', v_def;
    END IF;
    v_norm := regexp_replace(replace(v_def, ' ', ''), '[()]', '', 'g');
    IF v_norm NOT LIKE '%period_code,rule_id,cost_object_id,account_code%' THEN
        RAISE EXCEPTION 'V0372 自检失败：uk_biz_cost_alloc_result 的列组合不是 (period_code, rule_id, cost_object_id, account_code)：%', v_def;
    END IF;

    -- A4 CHECK 约束按名单点名核对（漏一个 = 一条脏数据入口）
    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE conrelid = 'biz_cost_alloc_rule'::regclass AND contype = 'c'
       AND conname IN ('ck_biz_cost_rule_biz','ck_biz_cost_rule_basis','ck_biz_cost_rule_status',
                       'ck_biz_cost_rule_dates','ck_biz_cost_rule_account');
    IF v_n <> 5 THEN
        RAISE EXCEPTION 'V0372 自检失败：biz_cost_alloc_rule 的 5 条 CHECK 约束只有 % 条落库', v_n;
    END IF;

    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE conrelid = 'biz_cost_alloc_result'::regclass AND contype = 'c'
       AND conname IN ('ck_biz_cost_alloc_period','ck_biz_cost_alloc_num','ck_biz_cost_alloc_base');
    IF v_n <> 3 THEN
        RAISE EXCEPTION 'V0372 自检失败：biz_cost_alloc_result 的 3 条 CHECK 约束只有 % 条落库', v_n;
    END IF;

    -- A5 外键：恰好 2 条，分别指向 biz_cost_alloc_rule / biz_cost_object，且都必须 ON DELETE RESTRICT
    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE contype = 'f'
       AND conrelid IN ('biz_cost_alloc_rule'::regclass,'biz_cost_alloc_result'::regclass);
    IF v_n <> 2 THEN
        RAISE EXCEPTION 'V0372 自检失败：分摊两表上的外键应为 2 条（result→rule、result→cost_object），实际 %', v_n;
    END IF;
    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE contype = 'f' AND conrelid = 'biz_cost_alloc_result'::regclass
       AND confrelid = 'biz_cost_alloc_rule'::regclass AND confdeltype = 'r';
    IF v_n <> 1 THEN
        RAISE EXCEPTION 'V0372 自检失败：biz_cost_alloc_result.rule_id → biz_cost_alloc_rule 必须是 ON DELETE RESTRICT（删规则会静默带走历史分摊）';
    END IF;
    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE contype = 'f' AND conrelid = 'biz_cost_alloc_result'::regclass
       AND confrelid = 'biz_cost_object'::regclass AND confdeltype = 'r';
    IF v_n <> 1 THEN
        RAISE EXCEPTION 'V0372 自检失败：biz_cost_alloc_result.cost_object_id → biz_cost_object 必须是 ON DELETE RESTRICT';
    END IF;
    -- 反向：不许给主数据列（dept_id 等）加物理外键（设计 §3.4 弱引用约定）
    SELECT count(*) INTO v_n FROM pg_constraint
     WHERE contype = 'f'
       AND conrelid IN ('biz_cost_alloc_rule'::regclass,'biz_cost_alloc_result'::regclass)
       AND confrelid NOT IN ('biz_cost_alloc_rule'::regclass,'biz_cost_object'::regclass);
    IF v_n <> 0 THEN
        RAISE EXCEPTION 'V0372 自检失败：有 % 条外键指向成本模块以外（主数据/单据必须弱引用，设计 §3.4）', v_n;
    END IF;

    -- A6 金额 / 基数精度：设计 §3.1 全篇 NUMERIC(18,4)
    FOR v_col, v_want IN
        SELECT t.col, t.want FROM (VALUES
            ('biz_cost_alloc_result.basis_value',  'numeric(18,4)'),
            ('biz_cost_alloc_result.basis_total',  'numeric(18,4)'),
            ('biz_cost_alloc_result.pool_amount',  'numeric(18,4)'),
            ('biz_cost_alloc_result.alloc_amount', 'numeric(18,4)'),
            ('biz_cost_alloc_result.rounding_adj', 'numeric(18,4)')
        ) AS t(col, want)
    LOOP
        SELECT format_type(a.atttypid, a.atttypmod) INTO v_fmt
          FROM pg_attribute a
         WHERE a.attrelid = split_part(v_col, '.', 1)::regclass
           AND a.attname  = split_part(v_col, '.', 2)
           AND NOT a.attisdropped;
        IF v_fmt IS DISTINCT FROM v_want THEN
            RAISE EXCEPTION 'V0372 自检失败：% 的类型是 %，设计要求 %（尾差 4 位小数，scale 掉到 2 会把尾差放大到分）',
                v_col, coalesce(v_fmt, '(缺列)'), v_want;
        END IF;
    END LOOP;

    RAISE NOTICE 'V0372 形态断言通过';
END $$;

-- ----------------------------------------------------------------------------
-- ⑩ 行为自检：把每条关键约束的**反面**真跑一遍
--
--    为什么非要做：形态对了不代表约束真在拦。`CHECK (basis_total > 0 OR alloc_amount = 0)`
--    这种带布尔组合的约束，写错一个括号就变成恒真（永远不拦），表结构照样「符合设计」。
--    这里每条断言都用一个期望失败的 INSERT / DELETE 反证。
--
--    探针键（固定 UUID，删干净再用）：
--      成本对象 SELFCHECK-B3-AL / 规则 SELFCHECK-B3-R1 / 探针行 remark='V0372-SELFCHECK'
-- ----------------------------------------------------------------------------
DO $$
DECLARE
    v_obj  UUID := '00000000-0000-0000-0000-0000000b0372';
    v_rule UUID := '00000000-0000-0000-0000-0000000b0373';
    v_dept UUID := '00000000-0000-0000-0000-0000000b0374';
    v_n    INT;
    v_hit  BOOLEAN;
BEGIN
    -- ===== 准备：清掉上次残留的探针行，再建父行（成本对象 + 规则） =====
    -- 分摊结果表没有 remark / deleted_at 列（设计 §3.1 ⑧），探针键用 cost_object_id。
    DELETE FROM biz_cost_alloc_result WHERE cost_object_id = v_obj;
    DELETE FROM biz_cost_alloc_rule WHERE rule_code LIKE 'SELFCHECK-B3-%';
    DELETE FROM biz_cost_object WHERE object_no = 'SELFCHECK-B3-AL';

    INSERT INTO biz_cost_object (id, object_no, product_id, target_warehouse_id, plan_qty,
                                 workshop_dept_id, workshop_dept_name, start_date)
    VALUES (v_obj, 'SELFCHECK-B3-AL', gen_random_uuid(), gen_random_uuid(), 10,
            v_dept, '自检车间', DATE '2026-01-01');

    -- ===== B1 规则编码活行唯一 + 软删后可重建 =====
    INSERT INTO biz_cost_alloc_rule (id, rule_code, rule_name, source_dept_id, source_dept_name,
                                     account_code, basis, effective_from)
    VALUES (v_rule, 'SELFCHECK-B3-R1', '自检分摊规则', v_dept, '自检车间',
            '4101.01', 'LABOR_HOURS', DATE '2026-01-01');

    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_alloc_rule (rule_code, rule_name, source_dept_id, source_dept_name,
                                         account_code, basis, effective_from)
        VALUES ('SELFCHECK-B3-R1', '自检重复规则', v_dept, '自检车间',
                '4101.01', 'LABOR_HOURS', DATE '2026-01-01');
        v_hit := true;
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0372 自检失败：同 rule_code 第二条活规则写入成功（uk_biz_cost_rule_code 未生效 ⇒ 取规则随机）';
    END IF;

    -- 软删行不占键：作废旧编码后再建同编码活行
    INSERT INTO biz_cost_alloc_rule (rule_code, rule_name, source_dept_id, source_dept_name,
                                     account_code, basis, effective_from, deleted_at, remark)
    VALUES ('SELFCHECK-B3-R2', '自检软删规则', v_dept, '自检车间',
            '4101.01', 'LABOR_HOURS', DATE '2026-01-01', now(), 'V0372-SELFCHECK');
    INSERT INTO biz_cost_alloc_rule (rule_code, rule_name, source_dept_id, source_dept_name,
                                     account_code, basis, effective_from, remark)
    VALUES ('SELFCHECK-B3-R2', '自检软删后重建', v_dept, '自检车间',
            '4101.01', 'LABOR_HOURS', DATE '2026-01-01', 'V0372-SELFCHECK');

    -- ===== B2 分配标准值域：写错标准 = 分摊口径整个错（不报错才可怕） =====
    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_alloc_rule (rule_code, rule_name, source_dept_id, source_dept_name,
                                         account_code, basis, effective_from)
        VALUES ('SELFCHECK-B3-R9', '自检错标准', v_dept, '自检车间',
                '4101.01', 'SALES_AMOUNT', DATE '2026-01-01');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0372 自检失败：basis=SALES_AMOUNT 写入成功（ck_biz_cost_rule_basis 未生效）';
    END IF;

    -- ===== B3 被摊科目必须 4101.* =====
    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_alloc_rule (rule_code, rule_name, source_dept_id, source_dept_name,
                                         account_code, basis, effective_from)
        VALUES ('SELFCHECK-B3-R3', '自检错科目', v_dept, '自检车间',
                '5602.01', 'LABOR_HOURS', DATE '2026-01-01');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0372 自检失败：被摊科目记 5602.01 写入成功（ck_biz_cost_rule_account 未生效 ⇒ 摊出来的钱进不了成本，风险 R-2）';
    END IF;

    -- ===== B4 生效期不许倒挂 =====
    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_alloc_rule (rule_code, rule_name, source_dept_id, source_dept_name,
                                         account_code, basis, effective_from, effective_to)
        VALUES ('SELFCHECK-B3-R4', '自检倒挂', v_dept, '自检车间',
                '4101.02', 'OUTPUT_QTY', DATE '2026-06-01', DATE '2026-01-01');
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0372 自检失败：effective_to < effective_from 写入成功（ck_biz_cost_rule_dates 未生效 ⇒ 规则永远不生效）';
    END IF;

    -- ===== B5 分摊结果唯一：同规则同对象同科目只能一行 =====
    INSERT INTO biz_cost_alloc_result (period_code, rule_id, cost_object_id, dept_id, dept_name,
                                       account_code, basis_value, basis_total, pool_amount,
                                       alloc_amount, rounding_adj)
    VALUES ('2026-01', v_rule, v_obj, v_dept, '自检车间',
            '4101.01', 60, 100, 1000, 600, 0);

    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_alloc_result (period_code, rule_id, cost_object_id, dept_id, dept_name,
                                           account_code, basis_value, basis_total, pool_amount,
                                           alloc_amount, rounding_adj)
        VALUES ('2026-01', v_rule, v_obj, v_dept, '自检车间',
                '4101.01', 60, 100, 1000, 600, 0);
        v_hit := true;
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0372 自检失败：同 (期间,规则,对象,科目) 第二条分摊结果写入成功（uk_biz_cost_alloc_result 未生效 ⇒ 重算会翻倍进成本）';
    END IF;

    -- ===== B6 没有基数却摊钱 = 凭空分摊（Σ 守恒硬断言的第一道防线） =====
    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_alloc_result (period_code, rule_id, cost_object_id, dept_id, dept_name,
                                           account_code, basis_value, basis_total, pool_amount,
                                           alloc_amount)
        VALUES ('2026-01', v_rule, v_obj, v_dept, '自检车间',
                '4101.02', 0, 0, 1000, 100);
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0372 自检失败：basis_total=0 却摊了 100 元（ck_biz_cost_alloc_base 未生效 ⇒ 分摊总额守恒必挂）';
    END IF;

    -- 正面：basis_total=0 且 alloc_amount=0 允许（该对象无基数，摊 0）
    INSERT INTO biz_cost_alloc_result (period_code, rule_id, cost_object_id, dept_id, dept_name,
                                       account_code, basis_value, basis_total, pool_amount,
                                       alloc_amount)
    VALUES ('2026-01', v_rule, v_obj, v_dept, '自检车间',
            '4101.09', 0, 0, 1000, 0);

    -- ===== B7 金额不许为负（负数分摊=把成本摊成收入） =====
    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_alloc_result (period_code, rule_id, cost_object_id, dept_id, dept_name,
                                           account_code, basis_value, basis_total, pool_amount,
                                           alloc_amount)
        VALUES ('2026-01', v_rule, v_obj, v_dept, '自检车间',
                '4101.02', -1, 100, 1000, 100);
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0372 自检失败：basis_value 为负写入成功（ck_biz_cost_alloc_num 未生效）';
    END IF;

    -- ===== B8 期间格式：yyyy-MM =====
    v_hit := false;
    BEGIN
        INSERT INTO biz_cost_alloc_result (period_code, rule_id, cost_object_id, dept_id, dept_name,
                                           account_code, basis_value, basis_total, pool_amount,
                                           alloc_amount)
        VALUES ('2026-13', v_rule, v_obj, v_dept, '自检车间',
                '4101.02', 1, 100, 1000, 10);
        v_hit := true;
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0372 自检失败：period_code=2026-13 写入成功（ck_biz_cost_alloc_period 未生效）';
    END IF;

    -- ===== B9 外键 ON DELETE RESTRICT：有分摊结果时删规则 / 删成本对象必须被拦 =====
    -- 注意 SQLSTATE：RESTRICT 违反抛的是 **23001 restrict_violation**，不是 23503
    -- foreign_key_violation（23503 只在「子行引用不存在的父行」时抛）。两个都接住：
    -- 以后有人把 RESTRICT 改成 NO ACTION，这里抛的就是 23503。
    v_hit := false;
    BEGIN
        DELETE FROM biz_cost_alloc_rule WHERE id = v_rule;
        v_hit := true;
    EXCEPTION WHEN restrict_violation OR foreign_key_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0372 自检失败：有分摊结果时规则被删掉（外键不是 ON DELETE RESTRICT ⇒ 历史分摊成孤儿）';
    END IF;

    v_hit := false;
    BEGIN
        DELETE FROM biz_cost_object WHERE id = v_obj;
        v_hit := true;
    EXCEPTION WHEN restrict_violation OR foreign_key_violation THEN
        NULL;
    END;
    IF v_hit THEN
        RAISE EXCEPTION 'V0372 自检失败：有分摊结果时成本对象被删掉（外键不是 ON DELETE RESTRICT）';
    END IF;

    -- ===== 清理探针（重复执行零残留） =====
    DELETE FROM biz_cost_alloc_result WHERE cost_object_id = v_obj;
    DELETE FROM biz_cost_alloc_rule WHERE rule_code LIKE 'SELFCHECK-B3-%';
    DELETE FROM biz_cost_object WHERE id = v_obj;

    SELECT count(*) INTO v_n FROM biz_cost_alloc_result WHERE cost_object_id = v_obj;
    SELECT v_n + count(*) INTO v_n FROM biz_cost_alloc_rule WHERE rule_code LIKE 'SELFCHECK-B3-%';
    SELECT v_n + count(*) INTO v_n FROM biz_cost_object WHERE id = v_obj;
    IF v_n <> 0 THEN
        RAISE EXCEPTION 'V0372 自检失败：探针行未清理干净（残留 % 行）', v_n;
    END IF;

    RAISE NOTICE 'V0372 完成：分摊两表建成，形态断言 + 9 组行为断言全过，探针已清理';
END $$;
