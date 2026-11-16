-- ============================================================================
-- V0414__cashflow_worksheet（M1-13 · 现金流底稿：表结构 biz_cashflow_worksheet · 2026-11-16）
--
-- 卡：t_31c91cd5（M1-13【M1·财务】现金流底稿：表结构 biz_cashflow_worksheet）
-- 拆卡依据 `docs/planning/20261113-新卡清单.md` §4 M1 财务 的 M1-13 行；
-- 下游 M1-14（录入 + 勾稽校验·直接法）接着用这张表。
--
-- 【本卡只做一件事】建**一张**表：现金流量表编制底稿的「行」。
--   一行 = 项目 × 期间 × 金额。不种数据（底稿是逐期间编制的，种子行没有期间可挂）、
--   不种权限码（还没有接口，码要跟接口一起种，见 AGENTS.md 约定 7）、不动任何存量表。
--
-- ── 为什么另建一张表（而不是给 biz_cashflow_item 加列）────────────────────────
--   `biz_cashflow_item`（V0185）是**分类配置**：一行 = 一个报表明细行 + 它接受哪些
--   对方科目（`account_pattern`）。它回答「这笔钱该归到哪一行」，是**规则**；
--   底稿回答「这一行这个期间是多少钱」，是**金额**。
--   一张表同时装规则与逐期间金额，配置行数会被期间数乘起来（12 行 × N 期），
--   而且「停用一行配置」与「这一期这一行为 0」在语义上会变成同一件事 —— 分不清。
--
-- ── 为什么行内冗余 item_name / activity / direction（三个快照列）──────────────
--   底稿是**历史编制依据**：配置行改名（「支付的各项税费」→「支付的税费」）、
--   停用（`is_active = 0`）、甚至整行被替换之后，已经编好的底稿与从中出的报表
--   必须还按当时的项目名/活动/方向呈现（AGENTS.md 约定 13 的「审计/历史类存快照」）。
--   另外 M1-16「由底稿生成现金流量表」直接读底稿即可出表，不必每行 join 配置表
--   （少一次快照漂移的机会，也不会因为配置被停用而少一行）。
--
-- ── 三种行（`line_type`）──────────────────────────────────────────────────
--   FLOW     明细行：`item_code` / `activity` / `direction` 三要素**必填**，
--            `amount` = 该行**本期发生额**（正数，方向由 `direction` 表达，见下）。
--   OPENING  期初现金及现金等价物余额（每期间最多一行），`amount` = 期初**余额**。
--   CLOSING  期末现金及现金等价物余额（每期间最多一行），`amount` = 期末**余额**。
--   余额行为什么放进同一张表：M1-14 的勾稽式是「期初 + 流入 − 流出 = 期末」，
--   把两个基准余额与明细行放在同一张表里，这条校验就是一次单表聚合，
--   不需要另建一张「底稿头」去跟明细行做跨表一致性维护（也就没有两处真相）。
--   `ck_biz_cashflow_ws_flow_shape` 用一条断言把两类行的字段形状分开：
--   FLOW 行三要素齐全且 `item_code` 非空；余额行三要素**必须为空**。
--
-- ── 金额口径（会静默算错账的三件事，都写进约束）────────────────────────────
--   ① **精度 `NUMERIC(18,4)`** —— 不使用 `NUMERIC(18,2)`：底稿金额来自凭证分录
--      （`biz_voucher_entry` / `biz_voucher.total_*` 是 `NUMERIC(20,4)`），
--      写窄到 2 位会在分摊/按比例拆分时造出「底稿合计 ≠ 凭证合计」的尾差，
--      而这种差**不报错**，只会让报表与总账差几分钱，查起来极痛。
--      与全仓 43 张表的 `amount NUMERIC(18,4)` 同口径。
--   ② **FLOW 行金额必须 ≥ 0**（`ck_biz_cashflow_ws_flow_amount`）——
--      方向由 `direction`（INFLOW / OUTFLOW）表达，用负数表达流出会让
--      「经营活动流入合计」出现负流入，两栏互相抵消，报表口径当场失真。
--   ③ **余额行允许负数** —— 银行透支、受限资金被占用等场景下期末现金可为负，
--      拦成 ≥ 0 会让真实账况录不进来（宁可录得进、让勾稽去报差）。
--
--   底稿**只出本位币**：外币凭证在记账时已折为本位币（V0409/M1-04），
--   现金流量表也只按本位币编制，所以本表**没有币种列**（有币种列反而会引出
--   「底稿按原币还是本位币」的二义，M1-09 的报表币种参数只管资产负债表/利润表）。
--
-- ── 唯一性 = 写入语义（M1-14 的 upsert 契约）────────────────────────────────
--   `uk_biz_cashflow_ws_item`    (period_code, item_code) WHERE item_code IS NOT NULL
--       ⇒ 一个期间里一个项目只有一行：录入就是 upsert，重复提交不会攒出两行同项目金额
--         （两行的后果是报表把同一笔现金流算两遍，且谁都不会发现）。
--   `uk_biz_cashflow_ws_balance` (period_code, line_type) WHERE line_type <> 'FLOW'
--       ⇒ 一个期间最多一行期初、一行期末：两个「期末余额」等于两个真相。
--   两条都用**部分**唯一索引：余额行的 `item_code` 是 NULL，若用普通唯一约束，
--   PG 允许多个 NULL，挡住「两个期末」这件事就落空了。
--
-- ── 为什么不做软删（没有 deleted_at）────────────────────────────────────────
--   底稿行不是单据：它是「某期间某项目多少钱」的可重算工作底稿，M1-16 出表时整期重算。
--   留墓碑行会造出「这行已删、报表还算着」的双真相（这正是 V0410 头注释反对的那种
--   双真相）。删除一行 = 该项目本期记为 0；真要追「谁在哪天改过」，走操作日志
--   （`audit_logs` / `sys_change_log`），而不是在底稿表里留半条记录。
--
-- ── 为什么 period_code / item_code 不加物理外键 ─────────────────────────────
--   ① `period_code`：全仓没有任何表对 `biz_accounting_period` 建外键（120 条外键里
--      指向期间表的是 0 条），期间是「弱引用」——外键会把期间表变成底稿的删除依赖，
--      而反结账/重开期间这类运维动作不该被底稿卡住。期间是否存在由写入口校验。
--   ② `item_code`：`biz_cashflow_item` 是配置表，项目停用/换版时删配置行是正常运维；
--      外键会让「删一条不再用的分类配置」直接失败，历史底稿跟着失去可读性。
--      快照三列（item_name/activity/direction）保证底稿脱离配置仍可读，
--      而写入时的「项目必须存在且启用」由 M1-14 的写入口校验。
--
-- ── 幂等 / 回滚 ───────────────────────────────────────────────────────────
--   `CREATE TABLE IF NOT EXISTS` + 全部 `CREATE INDEX IF NOT EXISTS` +
--   自检块可反复执行（探针行挂在 1900-01 这个永不使用的期间上，块尾自清场）。
--   down 幂等（`DROP TABLE IF EXISTS`），并加护栏：表里已有底稿行时**拒绝回滚**
--   （底稿是编制结果，删表等于丢工作底稿；先由 M1-14 导出/作废再回滚）。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 前置护栏：ERP 报表链必须已经在（缺表说明迁移没跑到 V0185）
-- ---------------------------------------------------------------------------
DO $$
BEGIN
    IF to_regclass('public.biz_cashflow_item') IS NULL THEN
        RAISE EXCEPTION 'V0414: biz_cashflow_item 不存在，请先应用 V0185（现金流量表分类配置）';
    END IF;
    IF to_regclass('public.biz_accounting_period') IS NULL THEN
        RAISE EXCEPTION 'V0414: biz_accounting_period 不存在，请先应用会计期间迁移';
    END IF;
    IF to_regclass('public.biz_cashflow_worksheet') IS NOT NULL THEN
        RAISE NOTICE 'V0414: biz_cashflow_worksheet 已存在，本迁移将按幂等路径重放（重建索引/复验约束）';
    END IF;
    RAISE NOTICE 'V0414: 前置表齐备，开始建现金流底稿表';
END $$;

-- ---------------------------------------------------------------------------
-- ② 底稿行表
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS biz_cashflow_worksheet (
    id          UUID          PRIMARY KEY DEFAULT gen_random_uuid(),
    period_code VARCHAR(7)    NOT NULL,                 -- 期间 'YYYY-MM'（与 biz_accounting_period.period_code 同型同长度）
    line_type   VARCHAR(8)    NOT NULL DEFAULT 'FLOW',  -- FLOW 明细行 / OPENING 期初余额 / CLOSING 期末余额
    item_code   VARCHAR(32),                            -- 现金流量项目编码（FLOW 行必填，对应 biz_cashflow_item.item_code）
    item_name   VARCHAR(64)   NOT NULL,                 -- 项目名称快照（配置改名不影响历史底稿）
    activity    VARCHAR(12),                            -- 活动快照：OPERATING / INVESTING / FINANCING（FLOW 行必填）
    direction   VARCHAR(8),                             -- 方向快照：INFLOW / OUTFLOW（FLOW 行必填）
    amount      NUMERIC(18,4) NOT NULL DEFAULT 0,       -- FLOW=本期发生额(≥0) / OPENING,CLOSING=余额(可负)
    sort_order  INT           NOT NULL DEFAULT 0,       -- 显示顺序（明细行照抄配置的 sort_order；期初 1、期末 9999）
    remark      VARCHAR(255),                           -- 编制说明（为什么手工调整、取数口径备注）
    create_by   UUID,
    created_at  TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    update_by   UUID,
    updated_at  TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT ck_biz_cashflow_ws_line_type CHECK (line_type IN ('FLOW', 'OPENING', 'CLOSING')),
    -- 期间格式：写错成 '2026-1' / '2026-13' 的行永远聚合不进任何一期（静默丢数）
    CONSTRAINT ck_biz_cashflow_ws_period CHECK (period_code ~ '^[0-9]{4}-(0[1-9]|1[0-2])$'),
    CONSTRAINT ck_biz_cashflow_ws_activity
        CHECK (activity IS NULL OR activity IN ('OPERATING', 'INVESTING', 'FINANCING')),
    CONSTRAINT ck_biz_cashflow_ws_direction
        CHECK (direction IS NULL OR direction IN ('INFLOW', 'OUTFLOW')),
    -- 两类行的字段形状互斥：FLOW 行三要素齐全；余额行三要素必须为空
    CONSTRAINT ck_biz_cashflow_ws_flow_shape CHECK (
        (line_type = 'FLOW' AND item_code IS NOT NULL AND activity IS NOT NULL AND direction IS NOT NULL)
        OR (line_type <> 'FLOW' AND item_code IS NULL AND activity IS NULL AND direction IS NULL)
    ),
    -- 明细行金额非负（方向由 direction 表达）；余额行不拦（透支/受限资金可为负）
    CONSTRAINT ck_biz_cashflow_ws_flow_amount CHECK (line_type <> 'FLOW' OR amount >= 0),
    CONSTRAINT ck_biz_cashflow_ws_name CHECK (length(btrim(item_name)) > 0)
);

-- 一期间一项目一行（FLOW 行 upsert 键）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cashflow_ws_item
    ON biz_cashflow_worksheet (period_code, item_code)
    WHERE item_code IS NOT NULL;

-- 每期间最多一行期初、一行期末（余额行的 item_code 为 NULL，必须用部分唯一索引才挡得住）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_cashflow_ws_balance
    ON biz_cashflow_worksheet (period_code, line_type)
    WHERE line_type <> 'FLOW';

-- 底稿列表（按期间取整张、按 sort_order 排）
CREATE INDEX IF NOT EXISTS idx_biz_cashflow_ws_period
    ON biz_cashflow_worksheet (period_code, sort_order);

-- 跨期对比一行项目的历史（M1-16 报表「上期金额」、底稿复核）
CREATE INDEX IF NOT EXISTS idx_biz_cashflow_ws_item_code
    ON biz_cashflow_worksheet (item_code)
    WHERE item_code IS NOT NULL;

COMMENT ON TABLE biz_cashflow_worksheet IS
    '现金流量表编制底稿行（直接法）：一行 = 项目 × 期间 × 金额；FLOW 明细行 + 每期间一行期初/期末余额';
COMMENT ON COLUMN biz_cashflow_worksheet.period_code IS
    '期间 YYYY-MM（弱引用 biz_accounting_period.period_code，写入口校验已开账）';
COMMENT ON COLUMN biz_cashflow_worksheet.line_type IS
    'FLOW 明细行 / OPENING 期初现金余额 / CLOSING 期末现金余额';
COMMENT ON COLUMN biz_cashflow_worksheet.item_code IS
    '现金流量项目编码（FLOW 行必填，弱引用 biz_cashflow_item.item_code，不快照停用态）';
COMMENT ON COLUMN biz_cashflow_worksheet.item_name IS
    '项目名称快照：配置改名/停用后历史底稿仍按当时名称呈现';
COMMENT ON COLUMN biz_cashflow_worksheet.activity IS
    '活动快照 OPERATING/INVESTING/FINANCING（FLOW 行必填，余额行为空）';
COMMENT ON COLUMN biz_cashflow_worksheet.direction IS
    '方向快照 INFLOW/OUTFLOW（FLOW 行必填，余额行为空）';
COMMENT ON COLUMN biz_cashflow_worksheet.amount IS
    'FLOW=本期发生额（≥0，方向由 direction 表达）/ OPENING,CLOSING=期初期末余额（可负）';
COMMENT ON COLUMN biz_cashflow_worksheet.sort_order IS
    '显示顺序：明细行照抄配置 sort_order，期初用 1、期末用 9999';

-- ---------------------------------------------------------------------------
-- ③ 自检 + 行为探针（任一不符即 RAISE EXCEPTION 回滚整个迁移）
--   探针期间固定 1900-01（永不使用的期间），块尾一律 DELETE 干净 ⇒ 重放零残留。
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n integer;
BEGIN
    -- ===== 结构：表在、列数与关键列类型/精度正确 =====
    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_cashflow_worksheet';
    IF n <> 14 THEN
        RAISE EXCEPTION 'V0414 自检失败：biz_cashflow_worksheet 应有 14 列，实际 % 列', n;
    END IF;

    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_cashflow_worksheet'
       AND (   (column_name = 'period_code' AND character_maximum_length = 7  AND is_nullable = 'NO')
            OR (column_name = 'line_type'   AND character_maximum_length = 8  AND is_nullable = 'NO')
            OR (column_name = 'item_code'   AND character_maximum_length = 32 AND is_nullable = 'YES')
            OR (column_name = 'item_name'   AND character_maximum_length = 64 AND is_nullable = 'NO')
            OR (column_name = 'activity'    AND character_maximum_length = 12 AND is_nullable = 'YES')
            OR (column_name = 'direction'   AND character_maximum_length = 8  AND is_nullable = 'YES')
            OR (column_name = 'amount'      AND numeric_precision = 18 AND numeric_scale = 4 AND is_nullable = 'NO'));
    IF n <> 7 THEN
        RAISE EXCEPTION 'V0414 自检失败：七个关键列的类型/精度/可空性不对（实际符合 % 列）—— amount 必须是 NUMERIC(18,4)（写窄到 2 位会让底稿合计与凭证合计差尾差）', n;
    END IF;

    -- ===== 约束在位数：7 条 CHECK =====
    SELECT count(*) INTO n
      FROM pg_constraint
     WHERE conrelid = 'public.biz_cashflow_worksheet'::regclass
       AND contype = 'c'
       AND conname IN ('ck_biz_cashflow_ws_line_type', 'ck_biz_cashflow_ws_period',
                       'ck_biz_cashflow_ws_activity',  'ck_biz_cashflow_ws_direction',
                       'ck_biz_cashflow_ws_flow_shape','ck_biz_cashflow_ws_flow_amount',
                       'ck_biz_cashflow_ws_name');
    IF n <> 7 THEN
        RAISE EXCEPTION 'V0414 自检失败：7 条 CHECK 只建成 % 条', n;
    END IF;

    -- ===== 索引在位数：4 条（2 唯一部分索引 + 2 普通索引）=====
    SELECT count(*) INTO n
      FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_cashflow_worksheet'
       AND indexname IN ('uk_biz_cashflow_ws_item', 'uk_biz_cashflow_ws_balance',
                         'idx_biz_cashflow_ws_period', 'idx_biz_cashflow_ws_item_code');
    IF n <> 4 THEN
        RAISE EXCEPTION 'V0414 自检失败：4 条索引只建成 % 条', n;
    END IF;
    -- 两条唯一索引必须是**部分**唯一索引（否则余额行的 NULL 让「两个期末」混得过去）
    SELECT count(*) INTO n
      FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_cashflow_worksheet'
       AND indexname IN ('uk_biz_cashflow_ws_item', 'uk_biz_cashflow_ws_balance')
       AND indexdef LIKE '%UNIQUE INDEX%' AND indexdef LIKE '%WHERE%';
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0414 自检失败：两条唯一索引必须是 UNIQUE ... WHERE ... 的部分索引（实际 % 条）', n;
    END IF;

    -- ===== 行为探针 ①：FLOW 明细行可写 =====
    INSERT INTO biz_cashflow_worksheet
        (period_code, line_type, item_code, item_name, activity, direction, amount, sort_order, remark)
    VALUES ('1900-01', 'FLOW', 'V0414PROBE', '__探针__', 'OPERATING', 'INFLOW', 100.0000, 101, 'V0414 探针行，勿留');

    -- ===== 行为探针 ②：同期间同项目再来一行必须被唯一索引拒绝（upsert 语义）=====
    BEGIN
        INSERT INTO biz_cashflow_worksheet
            (period_code, line_type, item_code, item_name, activity, direction, amount, sort_order)
        VALUES ('1900-01', 'FLOW', 'V0414PROBE', '__探针__', 'OPERATING', 'INFLOW', 1.0000, 101);
        RAISE EXCEPTION 'V0414 自检失败：同期间同项目的第二行竟然写进去了（uk_biz_cashflow_ws_item 没生效 → 报表会把同一笔现金流算两遍）';
    EXCEPTION
        WHEN unique_violation THEN NULL;
    END;

    -- ===== 行为探针 ③：FLOW 行金额为负必须被拒 =====
    BEGIN
        INSERT INTO biz_cashflow_worksheet
            (period_code, line_type, item_code, item_name, activity, direction, amount)
        VALUES ('1900-01', 'FLOW', 'V0414PROBENEG', '__探针__', 'OPERATING', 'OUTFLOW', -1.0000);
        RAISE EXCEPTION 'V0414 自检失败：负数流入行竟然写进去了（ck_biz_cashflow_ws_flow_amount 没生效）';
    EXCEPTION
        WHEN check_violation THEN NULL;
    END;

    -- ===== 行为探针 ④：FLOW 行缺活动/方向必须被拒 =====
    BEGIN
        INSERT INTO biz_cashflow_worksheet
            (period_code, line_type, item_code, item_name, amount)
        VALUES ('1900-01', 'FLOW', 'V0414PROBESHAPE', '__探针__', 10.0000);
        RAISE EXCEPTION 'V0414 自检失败：缺 activity/direction 的明细行竟然写进去了（ck_biz_cashflow_ws_flow_shape 没生效）';
    EXCEPTION
        WHEN check_violation THEN NULL;
    END;

    -- ===== 行为探针 ⑤：余额行（item_code/activity/direction 全空）可写 =====
    INSERT INTO biz_cashflow_worksheet
        (period_code, line_type, item_code, item_name, activity, direction, amount, sort_order)
    VALUES ('1900-01', 'OPENING', NULL, '期初现金及现金等价物余额', NULL, NULL, 500.0000, 1);

    -- ===== 行为探针 ⑥：同期间第二个期初余额必须被拒（两个期末 = 两个真相）=====
    BEGIN
        INSERT INTO biz_cashflow_worksheet
            (period_code, line_type, item_code, item_name, activity, direction, amount, sort_order)
        VALUES ('1900-01', 'OPENING', NULL, '期初现金及现金等价物余额', NULL, NULL, 1.0000, 1);
        RAISE EXCEPTION 'V0414 自检失败：同期间第二个期初余额竟然写进去了（uk_biz_cashflow_ws_balance 没生效）';
    EXCEPTION
        WHEN unique_violation THEN NULL;
    END;

    -- ===== 行为探针 ⑦：非法 line_type / 非法期间 / 空项目名必须被拒 =====
    BEGIN
        INSERT INTO biz_cashflow_worksheet (period_code, line_type, item_name, amount)
        VALUES ('1900-01', 'BOGUS', '__探针__', 1.0000);
        RAISE EXCEPTION 'V0414 自检失败：非法 line_type 竟然写进去了（ck_biz_cashflow_ws_line_type 没生效）';
    EXCEPTION
        WHEN check_violation THEN NULL;
    END;

    BEGIN
        INSERT INTO biz_cashflow_worksheet (period_code, line_type, item_name, amount)
        VALUES ('2026-13', 'OPENING', '__探针__', 1.0000);
        RAISE EXCEPTION 'V0414 自检失败：非法期间 2026-13 竟然写进去了（ck_biz_cashflow_ws_period 没生效 → 该行永远聚合不进任何一期）';
    EXCEPTION
        WHEN check_violation THEN NULL;
    END;

    BEGIN
        INSERT INTO biz_cashflow_worksheet (period_code, line_type, item_name, amount)
        VALUES ('1900-01', 'CLOSING', '   ', 1.0000);
        RAISE EXCEPTION 'V0414 自检失败：空项目名竟然写进去了（ck_biz_cashflow_ws_name 没生效）';
    EXCEPTION
        WHEN check_violation THEN NULL;
    END;

    -- ===== 探针清场：重放本迁移不许往真库里攒垃圾行 =====
    DELETE FROM biz_cashflow_worksheet WHERE period_code = '1900-01';
    SELECT count(*) INTO n FROM biz_cashflow_worksheet WHERE period_code = '1900-01';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0414 自检失败：探针行残留 % 行（period_code=1900-01）', n;
    END IF;

    RAISE NOTICE 'V0414 自检通过：biz_cashflow_worksheet 建表 + 7 条 CHECK + 4 条索引 + 7 组行为探针（明细行非负/唯一 upsert/每期一份期初/期间格式）';
END $$;
