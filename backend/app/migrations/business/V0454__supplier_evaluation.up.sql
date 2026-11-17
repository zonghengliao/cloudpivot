-- ============================================================================
-- V0454：供应商评估表结构（采购链 M4-12）—— 质量 / 交期 / 价格 / 服务 四维打分 + 权重
--
--   ① biz_supplier_evaluation   供应商评估单（一行 = 一家供应商在一个评估期间的一份四维评估结论）
--   ② biz_seq：SEV（SEV-yyyyMMdd-000001，按 CST 天滚动）
--   ③ 索引（编号存活唯一 / 同供应商同期间唯一 / 期间扫 / 等级筛）+ 末尾 DO 块自检
--
-- ── 本卡只交付表结构（卡面「本卡只做这一件事，不夹带」）───────────────────
--   不做：评估打分编排（读订单/收货算分）、评级阈值（A/B/C/D 的分界）、准入与黑名单流程、
--   CRUD 接口、权限码、菜单、路由、前端。
--   这些分别是 M4-13（评估打分 + 评级）、M4-14（准入/黑名单）、M4-15（单测）、
--   M4-16（前端页面）。本迁移**不写一行 sys_menu**（不会顶破 menu_invariants 的 400–540
--   上限），权限码一个不种，**也不给 biz_supplier 加列** —— 供应商主数据不动。
--
-- ── 号段核对（动手前 `ls backend/app/migrations/*/ | grep -oE 'V[0-9]{4}' | sort -u | tail`）
--   写入本文件时全局最大号 = V0453（V0453__framework_agreement_execution_view_fix），V0454 空闲。
--   biz_seq 已用 seq_type 里没有 SEV；biz_seq 行 id 取 …4482（V0452 的菜单 id 到 …4481，
--   与之连号但不交叠）。表名/约束前缀 sev 在全库迁移里 grep 无占用。
--
-- ── 表口径（卡面逐条落地，差异与理由都写在这里）──────────────────────────
--   1) 卡面「质量/交期/价格/服务四维」⇒ **四列分数 + 四列权重**，一维一对，都在单头。
--      `quality_score` / `delivery_score` / `price_score` / `service_score`：各 0–100。
--      `quality_weight` / `delivery_weight` / `price_weight` / `service_weight`：各 0–1，
--      四者之和**必须精确等于 1**（`ck_biz_sev_weights`）。
--      为什么权重必填且和恒为 1：权重是「这次评估怎么算总分」的全部依据，留空或留个
--      0.9 就等于「总分口径不明」；而 NULL 权重会让下面的生成列永远算不出值。
--      现实里 90% 的用法是 25/25/25/25 或 40/30/20/10 —— 前者是默认值，
--      后者只要四列一起写就行，不需要额外配置表。
--
--      为什么**不**做明细表（一行 = 一个细分指标，如来料合格率 / 准时交付率）：
--      卡面给的是「四维」这一层，细分指标（分子分母、取数口径、抽样规则）属于打分
--      编排（M4-13）的业务设计，现在建一张没有写入方的空表就是死结构。
--      真要拆四维明细时**追加**迁移建 `biz_supplier_evaluation_item` 并把四维列降级为
--      汇总（本表的四列届时就是汇总缓存），不预造。
--
--   2) 卡面「打分 + 权重」⇒ 总分 `total_score` 是**生成列**
--      （`GENERATED ALWAYS AS (round(Σ weight × score, 2)) STORED`），不是普通列。
--      为什么：加权是确定性的算术（四列乘再除以 100 的等价形式），把它写成一个普通列，
--      就必然出现「客户端传了总分」「改了一维分数忘了重算」两条漂移路径 —— 与本仓
--      V0420（在途量）/ V0211（盘盈亏）同一条理由：**能由库算的就不许人写**。
--      生成列的直接好处：`INSERT/UPDATE` 里显式写 `total_score` 会被 PG 直接拒
--      （SQLSTATE 428C9），任何写入路径（含以后补的 handler、手工 SQL、导入）都不可能写错。
--      `round(..., 2)` 与 model 侧纯函数 [`weighted_total`] 逐位对齐（真库用例对拍，
--      含 .005 中点）；PG 的 `round(numeric, 2)` 与 `rust_decimal` 的 mid-away-from-zero
--      同为「四舍五入」，不存在两种舍入口径。
--
--   3) 评级 `grade`（A/B/C/D）是**普通列、可空**，不是生成列：
--      分数→等级的分界（如 ≥90 = A）是**政策**，不是算术 —— 写进 DDL 就成了「改阈值
--      必须发迁移」，而阈值每年都会调。所以本卡只钉住**值域**（`ck_biz_sev_grade`：
--      NULL 或 A/B/C/D），阈值与赋值统一由 M4-13 实现。
--      `NULL = 未评级`：分数先落、评级后算（M4-13 之前以及任何人工重算之间）是正常状态，
--      不造 `UNRATED` 这种字符串值（那就成了第二个「空」）。
--
--   4) 卡面没写但必须有：**评估期间**与**评估人**。
--      · `period_start` / `period_end`（`ck_biz_sev_period` 保证起 ≤ 止）：供应商评估
--        评的是「一段时间内的表现」，没有期间就没有可比性。期间用起止日而不是
--        `period_year/period_month`：月度 / 季度 / 半年度 / 年度都是同一种结构
--        （2026-01-01..2026-03-31 就是 Q1），不必为「季度」再加一列。
--      · `eval_date`（评估日，默认当日）+ `ck_biz_sev_eval_date`：`eval_date >= period_start`
--        —— 期间还没开始就评完，只能是录错了。
--      · `evaluator_id` + `evaluator_name`：**姓名存快照**（约定 13 审计/历史类：评估人
--        离职/改名后，这份评估上写的还是当时那个人）。展示「现在叫什么」时 join `sys_user`。
--      · `supplier_name` 同理是快照（评估结论针对的是当时那家供应商）。
--
--   5) 「同供应商同期间只有一份评估」：`uk_biz_sev_supplier_period (supplier_id,
--      period_start, period_end) WHERE deleted_at IS NULL`。
--      理由：一份期间内的评估结论是**唯一的对账口径**（供方准入、黑名单、下单优先级都
--      按它查）。允许同期间多份，就必须再定义一个「取哪一份」的规则，那是没人拍板的口径。
--      需要多评估人（采购评一次、质量评一次再平均）时，正确做法是**追加**迁移给唯一键
--      加评估人维度（或建评估人明细表），而不是先把唯一键放开。
--      编号唯一 `uk_biz_sev_no` 与其它单据同口径：**存活行**唯一，软删后可复用同号。
--
--   6) 状态：**本卡不设 status 列**。
--      评估单的流转（拟稿→已评级→已发布）没有任何一张卡定义入口：M4-13 是打分与评级，
--      M4-14 是准入/黑名单（另一张表的事）。造 `DRAFT/PUBLISHED` 就成了永远停在 DRAFT 的
--      死状态（本仓 V0450 头注释 6 同一条理由）。真需要流转时**追加**迁移加列 + 扩 CHECK。
--
--   7) 数据权限：`biz_supplier_evaluation` 与采购计划 / 框架协议同口径 **SCOPE-EXEMPT** ——
--      供应商评估是公司级采购文件（采购要看全公司、财务要复核），本卡**不**登记
--      `shared::scope::SCOPED_TABLES`（本表也没有 dept 归属列）。可见性交给 M4-16 的权限码。
--
-- ── 幂等与回滚 ───────────────────────────────────────────────────────────
--   CREATE TABLE / INDEX IF NOT EXISTS；biz_seq 用 WHERE NOT EXISTS。
--   末尾 DO 块自检：表 / 列数 25 / 索引 4 / 生成列属性与表达式 / 坏数据逐条被拒 /
--   正样本与默认值 / 生成列算得对且拒写 / 唯一键（编号、供应商+期间）/ 探针清理。
--   回滚见 V0454__supplier_evaluation.down.sql：**有数据就拒绝回滚**（评估结论不可丢），
--   再删表与取号行。
-- ============================================================================

-- ---------- ① biz_supplier_evaluation（评估单：四维打分 + 权重） ----------
CREATE TABLE IF NOT EXISTS biz_supplier_evaluation (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),        -- 主键
    evaluation_no    VARCHAR(32)   NOT NULL,                            -- 评估单号：SEV-yyyyMMdd-000001（biz_seq 取号，存活行唯一）
    supplier_id      UUID          NOT NULL,                            -- 供应商（弱引用 biz_supplier.id）
    supplier_name    VARCHAR(200),                                      -- 供应商名快照（评估结论针对当时那家供应商）
    period_start     DATE          NOT NULL,                            -- 评估期间起（含）
    period_end       DATE          NOT NULL,                            -- 评估期间止（含）；ck_biz_sev_period 保证起 ≤ 止
    eval_date        DATE          NOT NULL DEFAULT CURRENT_DATE,       -- 评估日；ck_biz_sev_eval_date 保证不早于期间起点
    evaluator_id     UUID,                                              -- 评估人（弱引用 sys_user.id）
    evaluator_name   VARCHAR(64),                                       -- 评估人姓名快照（审计/历史类，约定 13）
    quality_weight   NUMERIC(7,4)  NOT NULL DEFAULT 0.2500,             -- 质量权重（0–1；四维之和必须 = 1）
    delivery_weight  NUMERIC(7,4)  NOT NULL DEFAULT 0.2500,             -- 交期权重（0–1）
    price_weight     NUMERIC(7,4)  NOT NULL DEFAULT 0.2500,             -- 价格权重（0–1）
    service_weight   NUMERIC(7,4)  NOT NULL DEFAULT 0.2500,             -- 服务权重（0–1）
    quality_score    NUMERIC(6,2)  NOT NULL DEFAULT 0,                  -- 质量得分（0–100）
    delivery_score   NUMERIC(6,2)  NOT NULL DEFAULT 0,                  -- 交期得分（0–100）
    price_score      NUMERIC(6,2)  NOT NULL DEFAULT 0,                  -- 价格得分（0–100）
    service_score    NUMERIC(6,2)  NOT NULL DEFAULT 0,                  -- 服务得分（0–100）
    total_score      NUMERIC(6,2)  GENERATED ALWAYS AS (                -- 加权总分（生成列：客户端/手工都写不进）
                         round(quality_weight  * quality_score
                             + delivery_weight * delivery_score
                             + price_weight    * price_score
                             + service_weight  * service_score, 2)
                     ) STORED,
    grade            VARCHAR(2),                                        -- 等级：A/B/C/D（**NULL = 未评级**；阈值属 M4-13）
    remark           TEXT,                                              -- 备注（扣分事由 / 改进要求）
    created_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- 审计：创建时间
    updated_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- 审计：更新时间
    create_by        UUID,                                              -- 审计：创建人
    update_by        UUID,                                              -- 审计：更新人
    deleted_at       TIMESTAMPTZ,                                       -- 软删时间（NULL = 存活）

    CONSTRAINT ck_biz_sev_no_fmt
        CHECK (evaluation_no ~ '^SEV-[0-9]{8}-[0-9]{6}$'),
    CONSTRAINT ck_biz_sev_period
        CHECK (period_start <= period_end),
    -- 期间还没开始就评完 = 录错了（评估日必须落在期间内或之后）
    CONSTRAINT ck_biz_sev_eval_date
        CHECK (eval_date >= period_start),
    -- 四维权重：各自 0–1，且和**精确等于 1**（权重口径唯一，见头注释 1）
    CONSTRAINT ck_biz_sev_weights
        CHECK (quality_weight  BETWEEN 0 AND 1
           AND delivery_weight BETWEEN 0 AND 1
           AND price_weight    BETWEEN 0 AND 1
           AND service_weight  BETWEEN 0 AND 1
           AND quality_weight + delivery_weight + price_weight + service_weight = 1),
    -- 四维分数：各 0–100（百分制；0 分与 100 分都是合法极端值）
    CONSTRAINT ck_biz_sev_scores
        CHECK (quality_score  BETWEEN 0 AND 100
           AND delivery_score BETWEEN 0 AND 100
           AND price_score    BETWEEN 0 AND 100
           AND service_score  BETWEEN 0 AND 100),
    -- 等级值域：NULL（未评级）或 A/B/C/D（阈值与赋值属 M4-13，本卡只钉域）
    CONSTRAINT ck_biz_sev_grade
        CHECK (grade IS NULL OR grade IN ('A','B','C','D'))
);

COMMENT ON TABLE  biz_supplier_evaluation IS '供应商评估单（M4-12 表结构）：一家供应商在一个评估期间的一份四维（质量/交期/价格/服务）打分 + 权重结论；总分是生成列，等级由 M4-13 按阈值写';
COMMENT ON COLUMN biz_supplier_evaluation.evaluation_no IS '评估单号：SEV-yyyyMMdd-000001（biz_seq.seq_type=SEV，按北京时间天滚动）；存活行唯一，软删后可复用';
COMMENT ON COLUMN biz_supplier_evaluation.supplier_id IS '供应商（弱引用 biz_supplier.id）；同供应商同期间只能有一份评估（uk_biz_sev_supplier_period）';
COMMENT ON COLUMN biz_supplier_evaluation.supplier_name IS '供应商名快照（评估结论的历史事实）；展示「现名」时 join biz_supplier，两列并存不是冗余（约定 13）';
COMMENT ON COLUMN biz_supplier_evaluation.period_start IS '评估期间起（含）：月度/季度/半年度/年度都用起止日表达，不为季度另加列';
COMMENT ON COLUMN biz_supplier_evaluation.period_end IS '评估期间止（含）；ck_biz_sev_period 保证 period_start <= period_end';
COMMENT ON COLUMN biz_supplier_evaluation.eval_date IS '评估日（默认当日）；ck_biz_sev_eval_date 保证不早于 period_start（期间没开始就评完只能是录错）';
COMMENT ON COLUMN biz_supplier_evaluation.evaluator_id IS '评估人（弱引用 sys_user.id）；姓名走 evaluator_name 快照（审计/历史类，约定 13）';
COMMENT ON COLUMN biz_supplier_evaluation.evaluator_name IS '评估人姓名快照：离职/改名后这份评估上写的仍是当时那个人';
COMMENT ON COLUMN biz_supplier_evaluation.quality_weight IS '质量权重（0–1）；四维之和必须 = 1（ck_biz_sev_weights），默认四等分 0.25';
COMMENT ON COLUMN biz_supplier_evaluation.delivery_weight IS '交期权重（0–1）；四维之和必须 = 1';
COMMENT ON COLUMN biz_supplier_evaluation.price_weight IS '价格权重（0–1）；四维之和必须 = 1';
COMMENT ON COLUMN biz_supplier_evaluation.service_weight IS '服务权重（0–1）；四维之和必须 = 1';
COMMENT ON COLUMN biz_supplier_evaluation.quality_score IS '质量得分（0–100）';
COMMENT ON COLUMN biz_supplier_evaluation.delivery_score IS '交期得分（0–100）';
COMMENT ON COLUMN biz_supplier_evaluation.price_score IS '价格得分（0–100）';
COMMENT ON COLUMN biz_supplier_evaluation.service_score IS '服务得分（0–100）';
COMMENT ON COLUMN biz_supplier_evaluation.total_score IS '加权总分 = round(Σ 权重 × 分数, 2)：**生成列**（STORED），任何写入路径显式写它都会被 PG 拒（428C9）';
COMMENT ON COLUMN biz_supplier_evaluation.grade IS '等级 A/B/C/D；**NULL = 未评级**（不造 UNRATED 字符串）。分界阈值属 M4-13';
COMMENT ON COLUMN biz_supplier_evaluation.remark IS '备注：扣分事由 / 改进要求 / 复审约定';

-- 编号存活唯一（软删后可重建同号，与其它单据同口径）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_sev_no
    ON biz_supplier_evaluation (evaluation_no) WHERE deleted_at IS NULL;
-- 同供应商同期间唯一：一份期间内只有一个评估结论（准入/黑名单/下单优先级都按它查）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_sev_supplier_period
    ON biz_supplier_evaluation (supplier_id, period_start, period_end) WHERE deleted_at IS NULL;
-- 按期间扫（「本季度全部评估」「季度评估进度」）
CREATE INDEX IF NOT EXISTS idx_biz_sev_period
    ON biz_supplier_evaluation (period_end DESC, supplier_id) WHERE deleted_at IS NULL;
-- 按等级筛（M4-14 准入 / 黑名单：C/D 级供应商清单）
CREATE INDEX IF NOT EXISTS idx_biz_sev_grade
    ON biz_supplier_evaluation (grade, period_end DESC)
 WHERE deleted_at IS NULL AND grade IS NOT NULL;

-- ---------- ② 取号规则 SEV ----------
INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status, remark)
SELECT '20000000-0000-0000-0000-000000004482'::uuid, 'SEV', '供应商评估单号', 'SEV', 'yyyyMMdd', '', 0, 1, '-', 1,
       '供应商评估单号：SEV-yyyyMMdd-000001，按天（CST）滚动'
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'SEV' AND deleted_at IS NULL);

-- ---------- ③ 自检 ----------
DO $$
DECLARE
    v_cnt     integer;
    v_missing text;
    v_generated text;
    v_total   numeric;
    probe     uuid := gen_random_uuid();
    probe2    uuid := gen_random_uuid();
    probe_sup uuid := gen_random_uuid();
    probe_sup2 uuid := gen_random_uuid();
BEGIN
    -- ③.1 表必须建出来
    IF to_regclass('public.biz_supplier_evaluation') IS NULL THEN
        RAISE EXCEPTION 'V0454 自检失败：biz_supplier_evaluation 未建出';
    END IF;

    -- ③.2 列数 25
    SELECT count(*) INTO v_cnt FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_supplier_evaluation';
    IF v_cnt <> 25 THEN
        RAISE EXCEPTION 'V0454 自检失败：biz_supplier_evaluation 列数 % ≠ 25', v_cnt;
    END IF;

    -- ③.3 total_score 必须是 STORED 生成列（普通列就有人能写脏）
    SELECT is_generated INTO v_generated FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_supplier_evaluation'
       AND column_name = 'total_score';
    IF v_generated IS DISTINCT FROM 'ALWAYS' THEN
        RAISE EXCEPTION 'V0454 自检失败：total_score 不是 GENERATED ALWAYS 列（实际 %）', COALESCE(v_generated, 'NULL');
    END IF;
    SELECT generation_expression INTO v_generated FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_supplier_evaluation'
       AND column_name = 'total_score';
    IF v_generated IS NULL OR v_generated NOT LIKE '%quality_weight%' OR v_generated NOT LIKE '%round%' THEN
        RAISE EXCEPTION 'V0454 自检失败：total_score 的生成表达式不是四维加权 + round：%', v_generated;
    END IF;

    -- ③.4 四个索引都在，且两个唯一索引都带软删条件
    SELECT count(*) INTO v_cnt
      FROM pg_indexes
     WHERE schemaname = 'public'
       AND indexname IN ('uk_biz_sev_no','uk_biz_sev_supplier_period','idx_biz_sev_period','idx_biz_sev_grade');
    IF v_cnt <> 4 THEN
        RAISE EXCEPTION 'V0454 自检失败：索引只建出 % / 4 个', v_cnt;
    END IF;
    SELECT count(*) INTO v_cnt
      FROM pg_indexes
     WHERE schemaname = 'public'
       AND indexname IN ('uk_biz_sev_no','uk_biz_sev_supplier_period')
       AND indexdef LIKE '%WHERE (deleted_at IS NULL)%';
    IF v_cnt <> 2 THEN
        RAISE EXCEPTION 'V0454 自检失败：两个唯一索引不是带软删条件的部分索引（% / 2）', v_cnt;
    END IF;

    -- ③.5 坏数据必须被拒（逐条独立，任一「能插进去」即失败）
    -- 编号形状
    BEGIN
        INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end)
        VALUES ('SEV-2026-1', probe_sup, DATE '2026-01-01', DATE '2026-03-31');
        RAISE EXCEPTION 'V0454 自检失败：不合法的评估单号格式被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    -- 期间起止颠倒
    BEGIN
        INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end)
        VALUES ('SEV-19990101-900001', probe_sup, DATE '2026-03-31', DATE '2026-01-01');
        RAISE EXCEPTION 'V0454 自检失败：评估期间起晚于止被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    -- 评估日早于期间起点
    BEGIN
        INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end, eval_date)
        VALUES ('SEV-19990101-900002', probe_sup, DATE '2026-01-01', DATE '2026-03-31', DATE '2025-12-01');
        RAISE EXCEPTION 'V0454 自检失败：评估日早于评估期间起点被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    -- 权重之和 ≠ 1（1.1）
    BEGIN
        INSERT INTO biz_supplier_evaluation
            (evaluation_no, supplier_id, period_start, period_end,
             quality_weight, delivery_weight, price_weight, service_weight)
        VALUES ('SEV-19990101-900003', probe_sup, DATE '2026-01-01', DATE '2026-03-31',
                0.4, 0.3, 0.2, 0.2);
        RAISE EXCEPTION 'V0454 自检失败：四维权重之和 1.1 被接受（ck_biz_sev_weights 失效）';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    -- 权重之和 ≠ 1（0.9：只填了三项以为第四项会默认）
    BEGIN
        INSERT INTO biz_supplier_evaluation
            (evaluation_no, supplier_id, period_start, period_end,
             quality_weight, delivery_weight, price_weight)
        VALUES ('SEV-19990101-900004', probe_sup, DATE '2026-01-01', DATE '2026-03-31',
                0.4, 0.3, 0.2);
        RAISE EXCEPTION 'V0454 自检失败：只写三个权重（和 0.9，第四项取默认 0.25 后和 1.15）被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    -- 单个权重越界
    BEGIN
        INSERT INTO biz_supplier_evaluation
            (evaluation_no, supplier_id, period_start, period_end,
             quality_weight, delivery_weight, price_weight, service_weight)
        VALUES ('SEV-19990101-900005', probe_sup, DATE '2026-01-01', DATE '2026-03-31',
                1.5, 0, 0, -0.5);
        RAISE EXCEPTION 'V0454 自检失败：权重 1.5 / -0.5 被接受（合法域 0–1）';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    -- 分数越界（> 100 与 < 0）
    BEGIN
        INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end, quality_score)
        VALUES ('SEV-19990101-900006', probe_sup, DATE '2026-01-01', DATE '2026-03-31', 100.01);
        RAISE EXCEPTION 'V0454 自检失败：质量得分 100.01 被接受（百分制上限 100）';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end, service_score)
        VALUES ('SEV-19990101-900007', probe_sup, DATE '2026-01-01', DATE '2026-03-31', -1);
        RAISE EXCEPTION 'V0454 自检失败：负的服务得分被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    -- 等级域外（含「UNRATED」这种自造字符串）
    BEGIN
        INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end, grade)
        VALUES ('SEV-19990101-900008', probe_sup, DATE '2026-01-01', DATE '2026-03-31', 'E');
        RAISE EXCEPTION 'V0454 自检失败：等级域外的 E 被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end, grade)
        VALUES ('SEV-19990101-900009', probe_sup, DATE '2026-01-01', DATE '2026-03-31', 'UNRATED');
        RAISE EXCEPTION 'V0454 自检失败：自造的 UNRATED 等级被接受（未评级只许写 NULL）';
    -- ⚠ 这条会先撞列宽（VARCHAR(2) → 22001 string_data_right_truncation），
    --   走不到 CHECK。两个异常都要接：只接 check_violation 会让迁移自己炸在自检里。
    EXCEPTION WHEN check_violation OR string_data_right_truncation THEN NULL;
    END;

    -- ③.6 正样本 + 默认值 + 生成列算术
    INSERT INTO biz_supplier_evaluation
        (id, evaluation_no, supplier_id, supplier_name, period_start, period_end, eval_date)
    VALUES (probe, 'SEV-19990101-900010', probe_sup, 'V0454 探针供应商',
            DATE '2026-01-01', DATE '2026-03-31', DATE '2026-04-02');
    IF (SELECT quality_weight FROM biz_supplier_evaluation WHERE id = probe) <> 0.25
       OR (SELECT delivery_weight FROM biz_supplier_evaluation WHERE id = probe) <> 0.25
       OR (SELECT price_weight FROM biz_supplier_evaluation WHERE id = probe) <> 0.25
       OR (SELECT service_weight FROM biz_supplier_evaluation WHERE id = probe) <> 0.25 THEN
        RAISE EXCEPTION 'V0454 自检失败：四维权重默认值不是四等分 0.25';
    END IF;
    IF (SELECT grade FROM biz_supplier_evaluation WHERE id = probe) IS NOT NULL THEN
        RAISE EXCEPTION 'V0454 自检失败：grade 默认必须是 NULL（未评级），不能预填等级';
    END IF;
    IF (SELECT total_score FROM biz_supplier_evaluation WHERE id = probe) <> 0 THEN
        RAISE EXCEPTION 'V0454 自检失败：四维全 0 分时总分不是 0';
    END IF;
    -- 0.4/0.3/0.2/0.1 × 80/90/70/60 = 32+27+14+6 = 79.00
    UPDATE biz_supplier_evaluation
       SET quality_weight = 0.4, delivery_weight = 0.3, price_weight = 0.2, service_weight = 0.1,
           quality_score = 80, delivery_score = 90, price_score = 70, service_score = 60
     WHERE id = probe;
    SELECT total_score INTO v_total FROM biz_supplier_evaluation WHERE id = probe;
    IF v_total <> 79.00 THEN
        RAISE EXCEPTION 'V0454 自检失败：加权总分算成 %，期望 79.00', v_total;
    END IF;
    -- 改一维分数，总分必须自动跟（生成列的意义）
    UPDATE biz_supplier_evaluation SET delivery_score = 100 WHERE id = probe;
    SELECT total_score INTO v_total FROM biz_supplier_evaluation WHERE id = probe;
    IF v_total <> 82.00 THEN
        RAISE EXCEPTION 'V0454 自检失败：改分数后总分没跟着重算（得到 %，期望 82.00）', v_total;
    END IF;
    -- 显式写生成列必须被 PG 拒（428C9）
    BEGIN
        INSERT INTO biz_supplier_evaluation
            (evaluation_no, supplier_id, period_start, period_end, total_score)
        VALUES ('SEV-19990101-900011', probe_sup, DATE '2026-01-01', DATE '2026-03-31', 99);
        RAISE EXCEPTION 'V0454 自检失败：显式写 total_score 被接受（生成列失效，会与加权值漂移）';
    EXCEPTION WHEN others THEN
        IF SQLSTATE <> '428C9' THEN
            RAISE;
        END IF;
    END;
    BEGIN
        UPDATE biz_supplier_evaluation SET total_score = 1 WHERE id = probe;
        RAISE EXCEPTION 'V0454 自检失败：UPDATE 显式写 total_score 被接受';
    EXCEPTION WHEN others THEN
        IF SQLSTATE <> '428C9' THEN
            RAISE;
        END IF;
    END;

    -- ③.7 唯一键：编号存活唯一 / 同供应商同期间唯一 / 换期间或换供应商可插
    BEGIN
        INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end)
        VALUES ('SEV-19990101-900010', probe_sup2, DATE '2025-01-01', DATE '2025-03-31');
        RAISE EXCEPTION 'V0454 自检失败：同编号的第二份存活评估被接受（uk_biz_sev_no 失效）';
    EXCEPTION WHEN unique_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end)
        VALUES ('SEV-19990101-900012', probe_sup, DATE '2026-01-01', DATE '2026-03-31');
        RAISE EXCEPTION 'V0454 自检失败：同供应商同期间的第二个评估被接受（uk_biz_sev_supplier_period 失效）';
    EXCEPTION WHEN unique_violation THEN NULL;
    END;
    INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end)
    VALUES ('SEV-19990101-900013', probe_sup, DATE '2026-04-01', DATE '2026-06-30');
    INSERT INTO biz_supplier_evaluation (id, evaluation_no, supplier_id, period_start, period_end)
    VALUES (probe2, 'SEV-19990101-900014', probe_sup2, DATE '2026-01-01', DATE '2026-03-31');

    -- ③.8 软删后同号可复用（部分唯一索引的意义）
    UPDATE biz_supplier_evaluation SET deleted_at = now() WHERE id = probe2;
    INSERT INTO biz_supplier_evaluation (evaluation_no, supplier_id, period_start, period_end)
    VALUES ('SEV-19990101-900014', probe_sup2, DATE '2026-01-01', DATE '2026-03-31');

    -- ③.9 探针清干净
    DELETE FROM biz_supplier_evaluation WHERE evaluation_no LIKE 'SEV-19990101-9%';
    SELECT count(*) INTO v_cnt FROM biz_supplier_evaluation WHERE evaluation_no LIKE 'SEV-19990101-9%';
    IF v_cnt <> 0 THEN
        RAISE EXCEPTION 'V0454 自检失败：自检探针没清干净（% 行残留）', v_cnt;
    END IF;

    -- ③.10 取号规则只有一行
    SELECT count(*) INTO v_cnt FROM biz_seq
     WHERE seq_type = 'SEV' AND deleted_at IS NULL AND prefix = 'SEV' AND separator = '-';
    IF v_cnt <> 1 THEN
        RAISE EXCEPTION 'V0454 自检失败：biz_seq(SEV) 种下 % 行（期望 1）', v_cnt;
    END IF;

    -- ③.11 关键列名一个都不能少（迁移改名时这里先炸，而不是等到 handler 运行期）
    SELECT string_agg(c, ', ') INTO v_missing
      FROM (VALUES ('evaluation_no'),('supplier_id'),('supplier_name'),
                   ('period_start'),('period_end'),('eval_date'),
                   ('evaluator_id'),('evaluator_name'),
                   ('quality_weight'),('delivery_weight'),('price_weight'),('service_weight'),
                   ('quality_score'),('delivery_score'),('price_score'),('service_score'),
                   ('total_score'),('grade'),('remark'),('deleted_at')) AS v(c)
     WHERE NOT EXISTS (SELECT 1 FROM information_schema.columns
                        WHERE table_schema = 'public' AND table_name = 'biz_supplier_evaluation'
                          AND column_name = v.c);
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0454 自检失败：biz_supplier_evaluation 缺列：%', v_missing;
    END IF;

    RAISE NOTICE 'M4-12/V0454: 供应商评估表（质量/交期/价格/服务 四维打分 + 权重 + 生成列总分）+ 取号 SEV 自检通过';
END $$;
