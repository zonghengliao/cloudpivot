-- ======================================================
-- V0287：销售链 —— 应收账款表 `biz_receivable` + `AR` 取号规则（SO-12）
--
--   ① biz_receivable          应收账款（镜像 biz_payable 的 24 字段）
--   ② biz_seq(seq_type='AR')  单号取号规则：AR-yyyyMMdd-000001（按天滚动）
--
-- ── 号段（开工实测复核）──────────────────────────────────
-- 卡面写「V0264__sales_receivable」，但 V0264 已被 SO-04 的
-- `V0264__sales_order`（销售订单两张表）占用 —— 已应用迁移不可改、指纹校验会炸，
-- **不能复用号段**。写入本文件时 business/ 最大号 = V0286，本卡顺延取 **V0287**。
-- 迁移号会被并行卡抢走，**提交前**再 `ls business | tail` 复核一次。
--
-- ── 口径来源与三处文档的差异（逐条记在这里，避免下游再各写一套）──────────
--   主口径 = 卡面「镜像 biz_payable 的 24 字段」+ `ARCH-7-销售模块设计.md` §3.1.6。
--   字段集合 / 类型 / 可空性 / 默认值**逐列对齐 V0148 的 biz_payable**（含数值精度），
--   只做四处语义改名：
--       payable_no    → ar_no             （应收单号）
--       supplier_id   → customer_id       （供应商 → 客户）
--       paid_amount   → received_amount   （已付 → 已收）
--       unpaid_amount → unsettled_amount  （未付 → 未收）
--   与 ARCH-7 §3.1.6 稿面**不同**的地方（按 payable 对齐，稿面数字更松/更紧各有）：
--     * invoice_no   VARCHAR(50)  ｜ 稿面 64  → 取 payable 口径（同一张表族，别两套宽度）
--     * source_type  VARCHAR(24)  ｜ 稿面 16  → payable 24
--     * currency_code CHAR(3)     ｜ 稿面 8   → payable CHAR(3)（ISO 4217 固定 3 位）
--     * amount/received/unsettled NUMERIC(18,4) ｜ 稿面 (18,2) → payable (18,4)，
--       与 `biz_voucher_entry` / `payable_aging` 的四位小数口径一致，少一位会静默四舍五入
--     * period_code  VARCHAR(7)   ｜ 稿面 8   → payable 7（`yyyy-MM` 正好 7 位）
--     * 金额列 MUST 同时给 `amount` 与 `unsettled_amount`：`ck_biz_ar_unsettled` 是恒等式，
--       只给 amount 会让 unsettled 落到默认 0 而被 CHECK 拒（payable 同一个坑，照搬不改）。
--
-- ── 状态域（`ck_biz_ar_status`，4 值）────────────────────────
--   依据 `ARCH-7` §4.3 状态图：OPEN → PARTIAL → SETTLED，OPEN/PARTIAL → WRITTEN_OFF（坏账）。
--   ⚠ **与 payable 的域值不同**：payable 是 OPEN/PARTIAL_PAID/SETTLED/CANCELLED(+PENDING_CANCEL)。
--     应收的收款侧语义是「部分已收 / 坏账核销」，不是「部分已付 / 作废」；SO-13 落 API 时
--     直接抄 payable 的 `PARTIAL_PAID` 会在本 CHECK 上炸 23514。映射表：
--       payable OPEN          → receivable OPEN
--       payable PARTIAL_PAID  → receivable PARTIAL
--       payable SETTLED       → receivable SETTLED
--       payable CANCELLED     → receivable WRITTEN_OFF（坏账核销，终态）
--   `settled_at` 只在 SETTLED / WRITTEN_OFF 落值（由 SO-13 的核销动作写入，本卡不建触发器）。
--   本卡**不登记** `LEGAL_EDGES`（与 SO-04/SO-08 分工一致）：SO-13 落 API 时登记
--   `DocKind::Receivable` + `RECEIVABLE_EDGES`，届时边表域值必须与本 CHECK 逐字一致。
--
-- ── 约束（卡面点名 `ck_biz_ar_amount`）──────────────────────
--   * ck_biz_ar_amount     : amount > 0 AND received_amount >= 0 AND received_amount <= amount
--                            （卡面逐字；payable 是 amount >= 0，应收**收紧为 > 0**：零元应收没有业务含义，
--                             且会让账龄 / 核销出现除零口径）
--   * ck_biz_ar_unsettled  : unsettled_amount = amount - received_amount（会计恒等式，不是近似）
--   * ck_biz_ar_credit     : credit_days >= 0
--   * ck_biz_ar_settled    : status <> 'SETTLED' OR unsettled_amount = 0
--   * ck_biz_ar_status     : 4 值白名单
--   ⚠ `amount` **不给默认值**：卡面要求 amount > 0，若保留 payable 的 `DEFAULT 0`，
--     任何省略 amount 的插入都会撞 CHECK（默认值必炸）—— 留着就是钓鱼，直接去掉。
--
-- ── 唯一 / 幂等 ─────────────────────────────────────────
--   * uk_biz_ar_no     存活行唯一（`WHERE deleted_at IS NULL`，软删后单号可复用）
--   * uk_biz_ar_source 同一来源单据只能挂一张应收（`source_type, source_id`，存活行唯一，
--                      **MANUAL 除外**：手工挂账没有业务来源，一张源可挂多笔）
--                      ⇒ 过账重试 / 并发不会重复挂账（ARCH-7 §7.2 第 7 步的幂等闸）
--
-- ── 索引（卡面点名 customer_id / status / due_date）──────────────
--   idx_biz_ar_customer (customer_id, status)  卡面 customer_id 打头，兼客户台账按状态筛
--   idx_biz_ar_status   (status)               按状态计数 / 看板
--   idx_biz_ar_due      (due_date, status)     账龄扫描（与 idx_biz_payable_due 同形）
--   idx_biz_ar_period   (period_code)          期间结账
--   idx_biz_ar_voucher  (voucher_id) WHERE NOT NULL  凭证回查（部分索引，空值不占位）
--   ⚠ 索引名统一带 `ar` 全词：PG 索引名是 schema 级唯一，撞名 + IF NOT EXISTS 会被**静默跳过**
--     （SO-04 实测栽过：`uk_biz_so_no` 已被 `biz_stock_out` 占用）。
--
-- ── 外键口径 ────────────────────────────────────────────
--   `customer_id` 是**弱引用**（无外键）：客户走软删（V0260），外键会把「客户软删但历史单据
--   要留」这条业务堵死 —— 与 `biz_sales_order.customer_id` / `biz_shipment.customer_id` 同风格。
--   ARCH-7 §3.1.6 写的「FK → biz_customer(id)」按此口径执行（引用有效性由 handler 建单时校验）。
--   `voucher_id` / `source_id` 同样无外键（凭证由过账事务生成，反向依赖会让事务成环）。
--
-- ── 幂等 ──────────────────────────────────────────────
-- CREATE TABLE / INDEX IF NOT EXISTS；biz_seq 种子 `INSERT … WHERE NOT EXISTS`。
-- 末尾 DO 块自检：表 / 24 列 / 与 biz_payable 逐列同型 / 5 条约束行为 / 8 个索引 /
-- 默认值 / 唯一与软删复用 / 来源幂等（含 MANUAL 例外）/ 取号规则，任一不符 → 迁移失败。
-- ======================================================

-- ---------- ① biz_receivable（应收账款） ----------
CREATE TABLE IF NOT EXISTS biz_receivable (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),      -- 主键
    ar_no            VARCHAR(32)   NOT NULL,                          -- 应收单号：AR-yyyyMMdd-000001（biz_seq 取号，存活行唯一）
    customer_id      UUID          NOT NULL,                          -- 客户（弱引用 biz_customer.id）
    source_type      VARCHAR(24)   NOT NULL DEFAULT 'SALES_OUT',      -- 来源类型：SALES_OUT / SHIPMENT / MANUAL
    source_id        UUID          NOT NULL,                          -- 来源单据 id（MANUAL 时由 model 生成）
    source_no        VARCHAR(32)   NOT NULL,                          -- 来源单据号（手工挂账写 'MANUAL'）
    invoice_no       VARCHAR(50),                                     -- 关联发票号（开票后回填）
    invoice_date     DATE,                                            -- 开票日期
    currency_code    CHAR(3)       NOT NULL DEFAULT 'CNY',            -- 币种（ISO 4217）
    amount           NUMERIC(18,4) NOT NULL,                          -- 应收金额（> 0；无默认值，见文件头）
    received_amount  NUMERIC(18,4) NOT NULL DEFAULT 0,                -- 已收金额（>= 0 且 <= amount）
    unsettled_amount NUMERIC(18,4) NOT NULL DEFAULT 0,                -- 未收金额（恒等 = amount - received_amount）
    credit_days      INT           NOT NULL DEFAULT 0,                -- 账期天数
    due_date         DATE          NOT NULL,                          -- 到期日 = 挂账日 + credit_days
    period_code      VARCHAR(7)    NOT NULL,                          -- 归属期间 yyyy-MM（受会计期间状态约束）
    status           VARCHAR(20)   NOT NULL DEFAULT 'OPEN',           -- 状态机：OPEN/PARTIAL/SETTLED/WRITTEN_OFF
    settled_at       TIMESTAMPTZ,                                     -- 结清 / 核销时间（SO-13 写入）
    voucher_id       UUID,                                            -- 收入确认凭证 id（无外键，避免过账事务成环）
    remark           TEXT,                                            -- 备注
    created_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 审计：创建时间
    updated_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 审计：更新时间（model 层维护）
    create_by        UUID,                                            -- 审计：创建人
    update_by        UUID,                                            -- 审计：更新人
    deleted_at       TIMESTAMPTZ,                                     -- 软删时间（NULL = 存活）

    -- 状态域：ARCH-7 §4.3（**不是** payable 的 PARTIAL_PAID/CANCELLED，见文件头映射表）
    CONSTRAINT ck_biz_ar_status
        CHECK (status IN ('OPEN','PARTIAL','SETTLED','WRITTEN_OFF')),
    -- 卡面逐字：金额三连（amount 收紧为 > 0）
    CONSTRAINT ck_biz_ar_amount
        CHECK (amount > 0 AND received_amount >= 0 AND received_amount <= amount),
    -- 未收金额恒等：程序算错会被数据库拦住（不做生成列，理由同 V0148：
    -- 框架 `#[derive(Model)]` 的 insert 会写结构体全部字段）
    CONSTRAINT ck_biz_ar_unsettled
        CHECK (unsettled_amount = amount - received_amount),
    CONSTRAINT ck_biz_ar_credit
        CHECK (credit_days >= 0),
    -- 已结清 ⇔ 未收为 0（状态与金额不许互相矛盾）
    CONSTRAINT ck_biz_ar_settled
        CHECK (status <> 'SETTLED' OR unsettled_amount = 0)
);

COMMENT ON TABLE  biz_receivable IS '应收账款（镜像 biz_payable 24 字段；status 4 值域见 ARCH-7 §4.3，与 payable 不同）';
COMMENT ON COLUMN biz_receivable.ar_no IS '应收单号：AR-yyyyMMdd-000001（biz_seq seq_type=AR 取号，按 CST 的天滚动）';
COMMENT ON COLUMN biz_receivable.customer_id IS '客户 id（弱引用 biz_customer.id，无外键：客户软删后历史单据仍要可读）';
COMMENT ON COLUMN biz_receivable.source_type IS '来源类型：SALES_OUT（销售出库过账）/ SHIPMENT / MANUAL（手工挂账）';
COMMENT ON COLUMN biz_receivable.amount IS '应收金额（> 0；刻意不给默认值：默认 0 会与 ck_biz_ar_amount 互相矛盾）';
COMMENT ON COLUMN biz_receivable.received_amount IS '已收金额（>= 0 且 <= amount；由收款核销累加）';
COMMENT ON COLUMN biz_receivable.unsettled_amount IS '未收金额 = amount - received_amount（ck_biz_ar_unsettled 恒等兜底）';
COMMENT ON COLUMN biz_receivable.status IS '状态机：OPEN/PARTIAL/SETTLED/WRITTEN_OFF（ARCH-7 §4.3；payable 的 PARTIAL_PAID 对应本表 PARTIAL）';
COMMENT ON COLUMN biz_receivable.settled_at IS '结清 / 坏账核销时间（SETTLED、WRITTEN_OFF 时落值，由 SO-13 写入）';
COMMENT ON COLUMN biz_receivable.deleted_at IS '软删标记；单号唯一索引带 WHERE deleted_at IS NULL ⇒ 软删后单号可复用';

-- 单号唯一：部分唯一索引（存活行唯一），与 uk_biz_payable_no / uk_biz_shipment_no 同口径
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_ar_no
    ON biz_receivable (ar_no) WHERE deleted_at IS NULL;
-- 来源幂等：同一来源单据只许一张应收（MANUAL 手工挂账除外）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_ar_source
    ON biz_receivable (source_type, source_id)
    WHERE deleted_at IS NULL AND source_type <> 'MANUAL';
-- 卡面点名的三个索引
CREATE INDEX IF NOT EXISTS idx_biz_ar_customer ON biz_receivable (customer_id, status);
CREATE INDEX IF NOT EXISTS idx_biz_ar_status   ON biz_receivable (status);
CREATE INDEX IF NOT EXISTS idx_biz_ar_due      ON biz_receivable (due_date, status);
-- 期间结账 / 凭证回查（与 payable 同口径）
CREATE INDEX IF NOT EXISTS idx_biz_ar_period   ON biz_receivable (period_code);
CREATE INDEX IF NOT EXISTS idx_biz_ar_voucher  ON biz_receivable (voucher_id) WHERE voucher_id IS NOT NULL;

-- ---------- ② 取号规则：AR（应收单号） ----------
INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status, remark)
SELECT v.id::uuid, 'AR', '应收单号', 'AR', 'yyyyMMdd', '', 0, 1, '-', 1,
       '应收单号：AR-yyyyMMdd-000001，按天（CST）滚动'
  FROM (VALUES ('d0287000-0000-4000-8000-000000000001')) AS v(id)
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq q WHERE q.seq_type = 'AR' AND q.deleted_at IS NULL);

-- ---------- ③ 自检：结构 + 与 payable 逐列同型 + 域值行为 + 默认值 + 唯一 + 取号 ----------
DO $$
DECLARE
    n          INT;
    d          TEXT;
    bad        TEXT;
    sample     TEXT;
    probe_src  UUID := 'd0287fff-0000-4000-8000-000000000001'::uuid;
    probe_ar   UUID := 'd0287fff-0000-4000-8000-000000000002'::uuid;
BEGIN
    -- 3.1 表在位 + 列数（多一列少一列都算漂移）
    IF to_regclass('public.biz_receivable') IS NULL THEN
        RAISE EXCEPTION 'V0287 自检失败：biz_receivable 未建';
    END IF;
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_receivable';
    IF n <> 24 THEN
        RAISE EXCEPTION 'V0287 自检失败：biz_receivable 列数 = %（期望 24）', n;
    END IF;

    -- 3.2 卡面点名的 24 列必须逐字在位
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_receivable'
       AND column_name IN ('id','ar_no','customer_id','source_type','source_id','source_no',
                           'invoice_no','invoice_date','currency_code','amount','received_amount',
                           'unsettled_amount','credit_days','due_date','period_code','status',
                           'settled_at','voucher_id','remark','created_at','updated_at',
                           'create_by','update_by','deleted_at');
    IF n <> 24 THEN
        RAISE EXCEPTION 'V0287 自检失败：biz_receivable 卡面列只齐了 %/24', n;
    END IF;

    -- 3.3 【硬验收】与 biz_payable 逐列同型：24 对映射列必须全部存在且
    --     data_type / 字符长度 / 数值精度标度 / 可空性 完全一致（paid→received, supplier→customer）
    IF to_regclass('public.biz_payable') IS NULL THEN
        RAISE EXCEPTION 'V0287 自检失败：biz_payable 不存在，无法做「字段对齐」校验（V0148 应先应用）';
    END IF;
    WITH map(ar_col, pay_col) AS (
        VALUES ('id','id'), ('ar_no','payable_no'), ('customer_id','supplier_id'),
               ('source_type','source_type'), ('source_id','source_id'), ('source_no','source_no'),
               ('invoice_no','invoice_no'), ('invoice_date','invoice_date'),
               ('currency_code','currency_code'), ('amount','amount'),
               ('received_amount','paid_amount'), ('unsettled_amount','unpaid_amount'),
               ('credit_days','credit_days'), ('due_date','due_date'),
               ('period_code','period_code'), ('status','status'),
               ('settled_at','settled_at'), ('voucher_id','voucher_id'),
               ('remark','remark'), ('created_at','created_at'), ('updated_at','updated_at'),
               ('create_by','create_by'), ('update_by','update_by'), ('deleted_at','deleted_at')
    )
    SELECT count(*) INTO n
      FROM map m
      JOIN information_schema.columns a
        ON a.table_schema = 'public' AND a.table_name = 'biz_receivable' AND a.column_name = m.ar_col
      JOIN information_schema.columns p
        ON p.table_schema = 'public' AND p.table_name = 'biz_payable' AND p.column_name = m.pay_col;
    IF n <> 24 THEN
        RAISE EXCEPTION 'V0287 自检失败：与 biz_payable 能对上的映射列 = %/24（列名漏了？）', n;
    END IF;

    WITH map(ar_col, pay_col) AS (
        VALUES ('id','id'), ('ar_no','payable_no'), ('customer_id','supplier_id'),
               ('source_type','source_type'), ('source_id','source_id'), ('source_no','source_no'),
               ('invoice_no','invoice_no'), ('invoice_date','invoice_date'),
               ('currency_code','currency_code'), ('amount','amount'),
               ('received_amount','paid_amount'), ('unsettled_amount','unpaid_amount'),
               ('credit_days','credit_days'), ('due_date','due_date'),
               ('period_code','period_code'), ('status','status'),
               ('settled_at','settled_at'), ('voucher_id','voucher_id'),
               ('remark','remark'), ('created_at','created_at'), ('updated_at','updated_at'),
               ('create_by','create_by'), ('update_by','update_by'), ('deleted_at','deleted_at')
    )
    SELECT string_agg(format('%s↔%s[%s/%s vs %s/%s]', m.ar_col, m.pay_col, a.data_type, a.is_nullable,
                             p.data_type, p.is_nullable), '; ')
      INTO bad
      FROM map m
      JOIN information_schema.columns a
        ON a.table_schema = 'public' AND a.table_name = 'biz_receivable' AND a.column_name = m.ar_col
      JOIN information_schema.columns p
        ON p.table_schema = 'public' AND p.table_name = 'biz_payable' AND p.column_name = m.pay_col
     WHERE a.data_type <> p.data_type
        OR coalesce(a.character_maximum_length, 0) <> coalesce(p.character_maximum_length, 0)
        OR coalesce(a.numeric_precision, 0) <> coalesce(p.numeric_precision, 0)
        OR coalesce(a.numeric_scale, 0) <> coalesce(p.numeric_scale, 0)
        OR a.is_nullable <> p.is_nullable;
    IF bad IS NOT NULL THEN
        RAISE EXCEPTION 'V0287 自检失败：下列列与 biz_payable 不同型 → %', bad;
    END IF;

    -- 3.4 五条 CHECK 在位
    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid = 'public.biz_receivable'::regclass
       AND conname IN ('ck_biz_ar_status','ck_biz_ar_amount','ck_biz_ar_unsettled',
                       'ck_biz_ar_credit','ck_biz_ar_settled');
    IF n <> 5 THEN
        RAISE EXCEPTION 'V0287 自检失败：biz_receivable 约束应有 5 条，实际 %', n;
    END IF;

    -- 3.5 状态域 4 值齐全（漏一个下游就会踩 23514）
    FOREACH d IN ARRAY ARRAY['OPEN','PARTIAL','SETTLED','WRITTEN_OFF']
    LOOP
        IF (SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = 'ck_biz_ar_status')
           NOT LIKE '%' || d || '%' THEN
            RAISE EXCEPTION 'V0287 自检失败：ck_biz_ar_status 缺域值 %', d;
        END IF;
    END LOOP;
    -- payable 的 PARTIAL_PAID 不许混进应收域（防 SO-13 抄 payable 后语义漂移）
    IF (SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = 'ck_biz_ar_status')
       LIKE '%PARTIAL_PAID%' THEN
        RAISE EXCEPTION 'V0287 自检失败：ck_biz_ar_status 混入了 payable 的 PARTIAL_PAID';
    END IF;

    -- 3.6 索引：pkey + uk×2 + idx×5 = 8；三个卡面点名的索引必须真实落地
    SELECT count(*) INTO n FROM pg_indexes WHERE schemaname = 'public' AND tablename = 'biz_receivable';
    IF n <> 8 THEN
        RAISE EXCEPTION 'V0287 自检失败：biz_receivable 索引应有 8 个，实际 %（撞名被静默跳过？）', n;
    END IF;
    SELECT count(*) INTO n FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_receivable'
       AND indexname IN ('uk_biz_ar_no','uk_biz_ar_source','idx_biz_ar_customer',
                         'idx_biz_ar_status','idx_biz_ar_due','idx_biz_ar_period','idx_biz_ar_voucher');
    IF n <> 7 THEN
        RAISE EXCEPTION 'V0287 自检失败：点名的 7 个索引只落地了 %/7', n;
    END IF;
    SELECT indexdef INTO d FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_biz_ar_no';
    IF d NOT LIKE '%UNIQUE%' OR d NOT LIKE '%deleted_at IS NULL%' THEN
        RAISE EXCEPTION 'V0287 自检失败：uk_biz_ar_no 不是「存活行唯一」的部分索引（def=%）', d;
    END IF;
    SELECT indexdef INTO d FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_biz_ar_source';
    IF d NOT LIKE '%UNIQUE%' OR d NOT LIKE '%MANUAL%' THEN
        RAISE EXCEPTION 'V0287 自检失败：uk_biz_ar_source 缺 MANUAL 例外或不是部分唯一索引（def=%）', d;
    END IF;

    -- 3.7 默认值：status=OPEN / currency=CNY / source_type=SALES_OUT / 三个 0；
    --     amount 必须**没有**默认值（有默认值 = 钓鱼，见文件头）
    SELECT column_default INTO d FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_receivable' AND column_name = 'status';
    IF d IS NULL OR d NOT LIKE '''OPEN''%' THEN
        RAISE EXCEPTION 'V0287 自检失败：status 默认值不是 OPEN（default=%）', d;
    END IF;
    SELECT column_default INTO d FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_receivable' AND column_name = 'currency_code';
    IF d IS NULL OR d NOT LIKE '''CNY''%' THEN
        RAISE EXCEPTION 'V0287 自检失败：currency_code 默认值不是 CNY（default=%）', d;
    END IF;
    SELECT column_default INTO d FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_receivable' AND column_name = 'source_type';
    IF d IS NULL OR d NOT LIKE '''SALES_OUT''%' THEN
        RAISE EXCEPTION 'V0287 自检失败：source_type 默认值不是 SALES_OUT（default=%）', d;
    END IF;
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_receivable'
       AND column_name IN ('received_amount','unsettled_amount','credit_days')
       AND column_default LIKE '0%';
    IF n <> 3 THEN
        RAISE EXCEPTION 'V0287 自检失败：received/unsettled/credit_days 的 0 默认值只落地了 %/3', n;
    END IF;
    SELECT column_default INTO d FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_receivable' AND column_name = 'amount';
    IF d IS NOT NULL THEN
        RAISE EXCEPTION 'V0287 自检失败：amount 不该有默认值（会与 ck_biz_ar_amount 互相矛盾），实际 %', d;
    END IF;

    -- ================= 3.8 行为探针（每条都是「插坏数据必须被拒」） =================

    -- 3.8a amount = 0 → 拒（卡面 amount > 0）
    BEGIN
        INSERT INTO biz_receivable (ar_no, customer_id, source_type, source_id, source_no,
                                    amount, unsettled_amount, period_code, due_date)
        VALUES ('AR-20261106-000001', gen_random_uuid(), 'MANUAL', gen_random_uuid(), 'MANUAL',
                0, 0, '2026-11', DATE '2026-12-06');
        RAISE EXCEPTION 'V0287 自检失败：amount = 0 未被 ck_biz_ar_amount 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    -- 3.8b received_amount > amount（超收）→ 拒
    BEGIN
        INSERT INTO biz_receivable (ar_no, customer_id, source_type, source_id, source_no,
                                    amount, received_amount, unsettled_amount, period_code, due_date)
        VALUES ('AR-20261106-000002', gen_random_uuid(), 'MANUAL', gen_random_uuid(), 'MANUAL',
                100, 100.0001, -0.0001, '2026-11', DATE '2026-12-06');
        RAISE EXCEPTION 'V0287 自检失败：received_amount > amount 未被 ck_biz_ar_amount 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    -- 3.8c received_amount 为负 → 拒
    BEGIN
        INSERT INTO biz_receivable (ar_no, customer_id, source_type, source_id, source_no,
                                    amount, received_amount, unsettled_amount, period_code, due_date)
        VALUES ('AR-20261106-000003', gen_random_uuid(), 'MANUAL', gen_random_uuid(), 'MANUAL',
                100, -1, 101, '2026-11', DATE '2026-12-06');
        RAISE EXCEPTION 'V0287 自检失败：received_amount = -1 未被 ck_biz_ar_amount 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    -- 3.8d 未收金额与恒等式不符 → 拒（程序算错必须被库拦住）
    BEGIN
        INSERT INTO biz_receivable (ar_no, customer_id, source_type, source_id, source_no,
                                    amount, received_amount, unsettled_amount, period_code, due_date)
        VALUES ('AR-20261106-000004', gen_random_uuid(), 'MANUAL', gen_random_uuid(), 'MANUAL',
                100, 40, 99, '2026-11', DATE '2026-12-06');
        RAISE EXCEPTION 'V0287 自检失败：unsettled_amount 不等于 amount - received_amount 却插入成功';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    -- 3.8e 状态越界值（payable 的 PARTIAL_PAID）→ 拒：抄 payable 会在库层炸
    BEGIN
        INSERT INTO biz_receivable (ar_no, customer_id, source_type, source_id, source_no,
                                    amount, unsettled_amount, period_code, due_date, status)
        VALUES ('AR-20261106-000005', gen_random_uuid(), 'MANUAL', gen_random_uuid(), 'MANUAL',
                100, 100, '2026-11', DATE '2026-12-06', 'PARTIAL_PAID');
        RAISE EXCEPTION 'V0287 自检失败：status = PARTIAL_PAID 未被 ck_biz_ar_status 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    -- 3.8f SETTLED 但未收额非 0 → 拒
    BEGIN
        INSERT INTO biz_receivable (ar_no, customer_id, source_type, source_id, source_no,
                                    amount, received_amount, unsettled_amount, period_code, due_date, status)
        VALUES ('AR-20261106-000006', gen_random_uuid(), 'MANUAL', gen_random_uuid(), 'MANUAL',
                100, 40, 60, '2026-11', DATE '2026-12-06', 'SETTLED');
        RAISE EXCEPTION 'V0287 自检失败：SETTLED 且未收额 = 60 未被 ck_biz_ar_settled 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    -- 3.8g 正常行：默认值 + 恒等式一起过（received 显式 0、unsettled = amount）
    INSERT INTO biz_receivable (id, ar_no, customer_id, source_type, source_id, source_no,
                                amount, unsettled_amount, credit_days, period_code, due_date)
    VALUES (probe_ar, 'AR-20261106-000001', gen_random_uuid(), 'SALES_OUT', probe_src, 'OUT-20261106-000001',
            100.0000, 100.0000, 30, '2026-11', DATE '2026-12-06');
    SELECT status || '|' || received_amount::text || '|' || unsettled_amount::text || '|' || currency_code
      INTO d FROM biz_receivable WHERE id = probe_ar;
    IF d <> 'OPEN|0.0000|100.0000|CNY' THEN
        RAISE EXCEPTION 'V0287 自检失败：默认值落库 = %（期望 OPEN|0.0000|100.0000|CNY）', d;
    END IF;

    -- 3.8h 存活行重号 → 拒
    BEGIN
        INSERT INTO biz_receivable (ar_no, customer_id, source_type, source_id, source_no,
                                    amount, unsettled_amount, period_code, due_date)
        VALUES ('AR-20261106-000001', gen_random_uuid(), 'MANUAL', gen_random_uuid(), 'MANUAL',
                100, 100, '2026-11', DATE '2026-12-06');
        RAISE EXCEPTION 'V0287 自检失败：存活行重号未被 uk_biz_ar_no 拒绝';
    EXCEPTION WHEN unique_violation THEN
        NULL;   -- 期望路径
    END;

    -- 3.8i 同一来源重复挂账（SALES_OUT, source_id）→ 拒（过账重试的幂等闸）
    BEGIN
        INSERT INTO biz_receivable (ar_no, customer_id, source_type, source_id, source_no,
                                    amount, unsettled_amount, period_code, due_date)
        VALUES ('AR-20261106-000007', gen_random_uuid(), 'SALES_OUT', probe_src, 'OUT-20261106-000001',
                100, 100, '2026-11', DATE '2026-12-06');
        RAISE EXCEPTION 'V0287 自检失败：同一 SALES_OUT 来源重复挂账未被 uk_biz_ar_source 拒绝';
    EXCEPTION WHEN unique_violation THEN
        NULL;   -- 期望路径
    END;

    -- 3.8j MANUAL 例外：同 source_id 可挂多笔（手工挂账没有业务来源）
    INSERT INTO biz_receivable (ar_no, customer_id, source_type, source_id, source_no,
                                amount, unsettled_amount, period_code, due_date)
    VALUES ('AR-20261106-000008', gen_random_uuid(), 'MANUAL', probe_src, 'MANUAL',
            50, 50, '2026-11', DATE '2026-12-06');
    INSERT INTO biz_receivable (ar_no, customer_id, source_type, source_id, source_no,
                                amount, unsettled_amount, period_code, due_date)
    VALUES ('AR-20261106-000009', gen_random_uuid(), 'MANUAL', probe_src, 'MANUAL',
            70, 70, '2026-11', DATE '2026-12-06');
    SELECT count(*) INTO n FROM biz_receivable
     WHERE source_type = 'MANUAL' AND source_id = probe_src AND deleted_at IS NULL;
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0287 自检失败：同 source_id 的 MANUAL 应收应为 2 行，实际 %', n;
    END IF;

    -- 3.8k 软删后同号可复用（部分唯一索引语义）
    UPDATE biz_receivable SET deleted_at = now() WHERE ar_no = 'AR-20261106-000008';
    INSERT INTO biz_receivable (ar_no, customer_id, source_type, source_id, source_no,
                                amount, unsettled_amount, period_code, due_date)
    VALUES ('AR-20261106-000008', gen_random_uuid(), 'MANUAL', gen_random_uuid(), 'MANUAL',
            50, 50, '2026-11', DATE '2026-12-06');
    SELECT count(*) INTO n FROM biz_receivable
     WHERE ar_no = 'AR-20261106-000008' AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0287 自检失败：软删后同号重插后存活行 = %（期望 1）', n;
    END IF;

    -- 3.8l 部分收款：OPEN → PARTIAL 的金额形态（库层只管恒等式，状态迁移在 SO-13）
    UPDATE biz_receivable
       SET received_amount = 40.0000, unsettled_amount = 60.0000, status = 'PARTIAL', updated_at = now()
     WHERE id = probe_ar;
    SELECT status || '|' || unsettled_amount::text INTO d FROM biz_receivable WHERE id = probe_ar;
    IF d <> 'PARTIAL|60.0000' THEN
        RAISE EXCEPTION 'V0287 自检失败：部分收款后 = %（期望 PARTIAL|60.0000）', d;
    END IF;

    -- 3.8m 清场：探针行全部消失
    DELETE FROM biz_receivable WHERE ar_no LIKE 'AR-20261106-%';
    SELECT count(*) INTO n FROM biz_receivable WHERE ar_no LIKE 'AR-20261106-%';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0287 自检失败：探针残留 % 行', n;
    END IF;

    -- 3.9 取号规则：AR 行在位且口径正确（前缀 / 日号 / 分隔符 / 步长 / 启用），
    --     并能拼出 AR-yyyyMMdd-000001 形态（6 位补零由 model 的 NO_SEQ_WIDTH 保证，
    --     这里用 lpad 逐步长验证规则参数本身没写错）
    SELECT prefix || '|' || date_pattern || '|' || separator || '|' || step || '|' || status
      INTO d
      FROM biz_seq WHERE seq_type = 'AR' AND deleted_at IS NULL;
    IF d IS NULL THEN
        RAISE EXCEPTION 'V0287 自检失败：biz_seq(seq_type=AR) 取号规则行缺失';
    END IF;
    IF d <> 'AR|yyyyMMdd|-|1|1' THEN
        RAISE EXCEPTION 'V0287 自检失败：AR 取号规则口径不对（% ≠ AR|yyyyMMdd|-|1|1）', d;
    END IF;
    SELECT 'AR-' || to_char(DATE '2026-11-06', 'YYYYMMDD') || '-' || lpad('1', 6, '0') INTO sample;
    IF sample !~ '^AR-[0-9]{8}-[0-9]{6}$' THEN
        RAISE EXCEPTION 'V0287 自检失败：按规则拼出的单号 % 不符合 AR-YYYYMMDD-NNNNNN', sample;
    END IF;

    RAISE NOTICE 'V0287 自检通过：biz_receivable 24 列（与 biz_payable 逐列同型）/ 5 约束 / 8 索引 / AR 取号=%',
                 sample;
END $$;
