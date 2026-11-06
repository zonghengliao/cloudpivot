-- ======================================================
-- V0298：销售链 —— 销售发票 `biz_invoice` + `IV` 取号规则（SO-19）
--
--   ① biz_invoice             销售发票（21 列 = ARCH-7 §3.1.8 的 16 个业务字段 + 审计 5 件套）
--   ② biz_seq(seq_type='IV')  单据号取号规则：IV-yyyyMMdd-000001（按 CST 天滚动）
--
-- ── 号段（开工实测复核，别信卡面）──────────────────────────
--   卡面写「V0266__sales_invoice」，但 V0266 已被销售订单菜单卡占用（已应用迁移不可改、
--   指纹校验会炸，**不能复用号段**）。写入本文件时 business/ 最大号 = V0294 ⇒ 本卡取 **V0298**。
--   迁移号会被并行卡抢走，**提交前**再 `ls business | tail` 复核一次。
--
--   ⚠ 号段最终值（DEPLOY-SYNC 卡 t_c3087afe，2026-11-06 22:0x 合仓时定）：本条落 `V0298`。
--     原因：并行的 SO-14 遗留分支已占 `V0294/V0295/V0296/V0297`（收款退款），
--     发票三件套由 `V0295/V0296/V0297` 顺延为 `V0298/V0299/V0300`（`git mv` + 文件内
--     `V0295` 字样同步，SQL 一行未改）。两条分支此前**都未进生产库**（prod `schema_migrations`
--     头 = `V0293`），所以顺延不涉及已应用迁移的改号。
--
-- ── 口径来源与 ARCH-7 稿面的差异（逐条记理由，别当漏项）──────────────
--   主口径 = `docs/architecture/ARCH-7-销售模块设计.md` §3.1.8 + §4.5 + §7.2。
--   与稿面**不同**的地方：
--     * 金额三列 NUMERIC(18,4) ｜ 稿面 (18,2)
--       → 与 `biz_voucher_entry` / `biz_receivable.amount` / `biz_receipt.amount` 的四位
--         小数口径一致；少两位会在「金额 = 不含税 + 税额」的恒等上静默四舍五入
--         （V0287 / V0292 对 receivable / receipt 做过同一决策）。
--     * `invoice_no` VARCHAR(64)（照稿面）—— **不复用** `biz_receivable.invoice_no` 的
--       VARCHAR(50)：那张表里的 invoice_no 只是「关联发票号」的拷贝列，本表才是号码的
--       权威来源（owner）。宽度按稿面，不跟着拷贝列走。
--     * `source_type` VARCHAR(24) ｜ 稿面 (16)
--       → 与 `biz_receivable.source_type`（V0287，24）同宽，便于「发票 ← 应收」直传不截断。
--     * 审计 5 件套（created_at / updated_at / create_by / update_by / deleted_at）+
--       软删 + 存活行唯一索引，与 biz_receivable / biz_receipt / biz_sales_order 逐列同形。
--
-- ── 单号口径（卡面：「单据号 IV-YYYYMMDD-NNNNNN（发票号可手填）」）──────
--   本表**只有一列号** `invoice_no`（不额外加 `doc_no`：卡面字段集与 ARCH-7 §3.1.8 都只给
--   一列，加第二列会让下游取值口径分叉）。两种来源，二选一，**同一列**：
--     a) 自动取号：`IV-yyyyMMdd-000001`，来自 `biz_seq(seq_type='IV')`，六位补零由
--        model 层 `NO_SEQ_WIDTH` 决定（与 AR / RC 同机制）。
--     b) 手填税控发票号：用户直接给（增值税发票号码 / 全电票 20 位号码等）。
--   ⚠ 给下游（SO-20）的硬口径：**只在 invoice_no 为空时才取号**。先取号再让手填覆盖
--     会白烧号码（同一天的单号出现空洞，审计问「缺号去哪了」回答不了）。
--   `uk_biz_invoice_no` 是存活行唯一的部分索引 ⇒ 软删后同号可复用；手填号码撞号 → 23505。
--
-- ── 状态域（`ck_biz_invoice_status`，4 值）──────────────────
--   依据 ARCH-7 §4.5 状态图：
--     DRAFT → ISSUED → POSTED
--     DRAFT / ISSUED → VOID（作废；过账后作废见下）
--   ⚠ **与应收（OPEN/PARTIAL/SETTLED/WRITTEN_OFF/CANCELLED）、收款
--     （DRAFT/CONFIRMED/POSTED/CANCELLED）都不是一套域值**：
--       抄 receivable 的 `OPEN` 或 receipt 的 `CONFIRMED` 都会在本 CHECK 上炸 23514。
--   ⚠ `VOID` 不是 `CANCELLED`：本仓收款用 CANCELLED、发票用 VOID（稿面逐字），
--     两词并存是稿面既定，不要为了「统一」把其中一个改名 —— 已有单据数据不可改。
--
-- ── 红字发票（`ck_biz_invoice_sign`）─────────────────────
--   ARCH-7 §15.2.3 逐字：「生成红字发票（biz_invoice，**amount 为负**，type=OUT_RED）」。
--   所以本表金额**允许负数**，且正负与类型强绑定：
--     OUT      → amount > 0  且 tax_amount >= 0   （销项正票）
--     OUT_RED  → amount < 0  且 tax_amount <= 0   （红冲票，税额同为负）
--   ⚠ 与 `biz_voucher`（V0149）的红冲口径**不同**：凭证是「借贷互换、金额恒为正 + is_red」，
--     发票是**单据**（不是凭证），按稿面走负金额。两者不要互相抄：把红字发票写成正金额会被
--     本 CHECK 拒（23514），把红字凭证写成负金额会被 voucher 侧的自检拒。
--   `ck_biz_invoice_amount`（卡面点名的恒等式）保持纯恒等，正负规则单独一条约束，
--   这样报错能指认是「算错了」还是「符号搞错了」。
--
-- ── 约束清单（6 条）───────────────────────────────────
--   * ck_biz_invoice_amount  : total_with_tax = amount + tax_amount     ← 卡面点名
--   * ck_biz_invoice_sign    : 正票/红冲票的金额符号（见上）
--   * ck_biz_invoice_status  : 4 值白名单
--   * ck_biz_invoice_type    : invoice_type IN ('OUT','OUT_RED')
--   * ck_biz_invoice_tax_rate: tax_rate >= 0 AND tax_rate <= 1（0~100%）
--   * ck_biz_invoice_source  : source_type 白名单或 NULL
--   ⚠ 三个金额列（amount / tax_amount / total_with_tax）**都不给默认值**：
--     有默认值 + 恒等式 CHECK = 钓鱼（省略任一列都必炸，且报错指向恒等式，看不出是漏字段）。
--   ⚠ `tax_rate` 给默认 0.13（稿面逐字）；显式给值优先。
--
--   **本卡不建触发器**（本仓迁移零 TRIGGER，与 V0287/V0290/V0292 同口径）：
--   「开票金额 ≤ 应收金额」（卡面子卡 SO-20 的要求）是**跨表**条件，CHECK 表达不了，
--   由 SO-20 的 handler 在事务里 `SELECT … FOR UPDATE` 锁应收后判定。
--
-- ── 唯一 / 索引 ─────────────────────────────────────────
--   * uk_biz_invoice_no      存活行唯一（WHERE deleted_at IS NULL）
--   * idx_biz_invoice_customer(customer_id, invoice_date)  卡面点名 customer_id，兼客户台账按日筛
--   * idx_biz_invoice_status  (status)                     卡面点名
--   * idx_biz_invoice_receivable(receivable_id) WHERE NOT NULL  卡面点名
--       ⚠ **非唯一**：一张应收可以分次开票，作废后还会再开红冲票 ⇒ 不能学
--         `uk_biz_rcpt_alloc_pair` 那样做唯一对。
--   * idx_biz_invoice_date    (invoice_date)               开票台账 / 期间闸按日期扫
--   * idx_biz_invoice_voucher (voucher_id) WHERE NOT NULL  凭证回查（部分索引，空值不占位）
--   ⇒ pkey + 1 唯一 + 5 普通/部分 = 7 个
--   ⚠ 索引名一律带 `invoice` 全词：PG 索引名是 schema 级唯一，撞名 + IF NOT EXISTS 会被
--     **静默跳过**（V0287 文件头记过 SO-04 栽的那次）。本卡开工 `grep -rn "biz_invoice"` 全仓 0 命中。
--
-- ── 外键口径 ────────────────────────────────────────────
--   * `receivable_id` → biz_receivable(id) **ON DELETE RESTRICT**，可空（手工/无应收开票）：
--       发票是「这张应收开了哪张票」的对账留痕。级联删会静默吞掉留痕（票据没了、应收还在），
--       与 V0292 对 `allocation.receivable_id` 的决策同口径。应收本身走软删，实务不触发。
--   * `customer_id` 是**弱引用**（无外键）：客户走软删（V0260），外键会把「客户软删但历史
--       发票要留」堵死 —— 与 biz_receivable / biz_receipt / biz_sales_order 同风格。
--       ARCH-7 §3.1.8 写的「FK → biz_customer(id)」按此口径执行（引用有效性由 SO-20 建单时校验）。
--   * `voucher_id` 无外键（凭证由开票/作废事务生成，反向依赖会让事务成环），与 receivable 同。
--
-- ── 幂等 ──────────────────────────────────────────────
--   CREATE TABLE / INDEX IF NOT EXISTS；biz_seq 种子 `INSERT … WHERE NOT EXISTS`。
--   末尾 DO 块自检：表 / 21 列 / 卡面列逐字 / 6 条 CHECK / 7 个索引 / 默认值 / 外键动作 /
--   14 条行为探针（坏数据必须被拒 + 正常路径 + 软删复用 + 精度 + RESTRICT）/ 取号规则，
--   任一不符 → 迁移失败。
-- ======================================================

-- ---------- ① biz_invoice（销售发票） ----------
CREATE TABLE IF NOT EXISTS biz_invoice (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),      -- 主键
    invoice_no     VARCHAR(64)   NOT NULL,                          -- 发票号 / 单据号：IV-yyyyMMdd-000001 或手填税控号（存活行唯一）
    invoice_type   VARCHAR(16)   NOT NULL DEFAULT 'OUT',            -- 票种：OUT 销项 / OUT_RED 销项红字（金额为负）
    invoice_code   VARCHAR(32),                                     -- 发票代码（税控，预留接金税系统；无则 NULL）
    customer_id    UUID          NOT NULL,                          -- 客户（弱引用 biz_customer.id，无外键）
    invoice_date   DATE          NOT NULL,                          -- 开票日期（凭证日期 / 期间闸都取它）
    amount         NUMERIC(18,4) NOT NULL,                          -- 不含税金额（无默认值，见文件头）
    tax_amount     NUMERIC(18,4) NOT NULL,                          -- 税额（无默认值）
    total_with_tax NUMERIC(18,4) NOT NULL,                          -- 价税合计 = amount + tax_amount（无默认值）
    tax_rate       NUMERIC(6,4)  NOT NULL DEFAULT 0.1300,           -- 税率（0~1；稿面默认 0.13）
    status         VARCHAR(16)   NOT NULL DEFAULT 'DRAFT',          -- 状态机：DRAFT/ISSUED/POSTED/VOID（ARCH-7 §4.5）
    receivable_id  UUID,                                            -- 关联应收（FK → biz_receivable.id，ON DELETE RESTRICT；手工开票可空）
    source_type    VARCHAR(24),                                     -- 来源类型：SALES_ORDER / SHIPMENT / SALES_OUT / MANUAL
    source_no      VARCHAR(32),                                     -- 来源单据号
    voucher_id     UUID,                                            -- 收入确认 / 红冲凭证 id（无外键，避免开票事务成环）
    remark         TEXT,                                            -- 备注
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 审计：创建时间
    updated_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 审计：更新时间（model 层维护）
    create_by      UUID,                                            -- 审计：创建人
    update_by      UUID,                                            -- 审计：更新人
    deleted_at     TIMESTAMPTZ,                                     -- 软删时间（NULL = 存活）

    -- 卡面点名的金额恒等式（纯恒等，符号规则另立一条，报错能区分「算错」与「符号错」）
    CONSTRAINT ck_biz_invoice_amount
        CHECK (total_with_tax = amount + tax_amount),
    -- 正票 / 红冲票的金额符号（ARCH-7 §15.2.3：红字发票 amount 为负）
    CONSTRAINT ck_biz_invoice_sign
        CHECK ((invoice_type = 'OUT'     AND amount > 0 AND tax_amount >= 0)
            OR (invoice_type = 'OUT_RED' AND amount < 0 AND tax_amount <= 0)),
    -- 状态域：ARCH-7 §4.5（**不是** receivable 的 OPEN、也不是 receipt 的 CONFIRMED）
    CONSTRAINT ck_biz_invoice_status
        CHECK (status IN ('DRAFT','ISSUED','POSTED','VOID')),
    CONSTRAINT ck_biz_invoice_type
        CHECK (invoice_type IN ('OUT','OUT_RED')),
    -- 税率 0~100%（0.1300 = 13%）；负税率会让凭证的销项税方向反掉
    CONSTRAINT ck_biz_invoice_tax_rate
        CHECK (tax_rate >= 0 AND tax_rate <= 1),
    -- 来源类型白名单（允许 NULL：「手工开票 / 直接挂应收开票」没有上游单据）
    CONSTRAINT ck_biz_invoice_source
        CHECK (source_type IS NULL
            OR source_type IN ('SALES_ORDER','SHIPMENT','SALES_OUT','MANUAL')),

    -- 关联应收：RESTRICT（不是 CASCADE，见文件头外键口径）
    CONSTRAINT biz_invoice_receivable_id_fkey
        FOREIGN KEY (receivable_id) REFERENCES biz_receivable (id) ON DELETE RESTRICT
);

COMMENT ON TABLE  biz_invoice IS '销售发票（ARCH-7 §3.1.8；状态 4 值见 §4.5，红字票金额为负见 §15.2.3）';
COMMENT ON COLUMN biz_invoice.invoice_no IS '发票号/单据号：自动 IV-yyyyMMdd-000001（biz_seq seq_type=IV）或手填税控发票号；存活行唯一';
COMMENT ON COLUMN biz_invoice.invoice_type IS '票种：OUT 销项正票（金额>0）/ OUT_RED 销项红字（金额<0，冲减）';
COMMENT ON COLUMN biz_invoice.invoice_code IS '发票代码（税控预留字段，本仓不接金税系统，可空）';
COMMENT ON COLUMN biz_invoice.customer_id IS '客户 id（弱引用 biz_customer.id，无外键：客户软删后历史发票仍要可读）';
COMMENT ON COLUMN biz_invoice.invoice_date IS '开票日期（必填；收入凭证日期与会计期间闸都取它）';
COMMENT ON COLUMN biz_invoice.amount IS '不含税金额（OUT > 0 / OUT_RED < 0；刻意不给默认值）';
COMMENT ON COLUMN biz_invoice.tax_amount IS '税额（OUT >= 0 / OUT_RED <= 0）';
COMMENT ON COLUMN biz_invoice.total_with_tax IS '价税合计 = amount + tax_amount（ck_biz_invoice_amount 恒等兜底）';
COMMENT ON COLUMN biz_invoice.tax_rate IS '税率 0~1（0.1300 = 13%；默认 0.13 取自 ARCH-7 §3.1.8）';
COMMENT ON COLUMN biz_invoice.status IS '状态机：DRAFT/ISSUED/POSTED/VOID（ARCH-7 §4.5；VOID ≠ 收款的 CANCELLED）';
COMMENT ON COLUMN biz_invoice.receivable_id IS '关联应收 id（FK，ON DELETE RESTRICT；手工开票可空）';
COMMENT ON COLUMN biz_invoice.source_type IS '来源类型：SALES_ORDER / SHIPMENT / SALES_OUT / MANUAL（ARCH 稿面的 SO 归一为 SALES_ORDER）';
COMMENT ON COLUMN biz_invoice.voucher_id IS '收入确认凭证 / 红冲凭证 id（弱引用，无外键）';
COMMENT ON COLUMN biz_invoice.deleted_at IS '软删标记；发票号唯一索引带 WHERE deleted_at IS NULL ⇒ 软删后同号可复用';

-- 发票号唯一：部分唯一索引（存活行唯一），与 uk_biz_ar_no / uk_biz_receipt_no 同口径
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_invoice_no
    ON biz_invoice (invoice_no) WHERE deleted_at IS NULL;
-- 卡面点名的三个索引
CREATE INDEX IF NOT EXISTS idx_biz_invoice_customer  ON biz_invoice (customer_id, invoice_date);
CREATE INDEX IF NOT EXISTS idx_biz_invoice_status    ON biz_invoice (status);
CREATE INDEX IF NOT EXISTS idx_biz_invoice_receivable ON biz_invoice (receivable_id)
    WHERE receivable_id IS NOT NULL;
-- 开票台账 / 期间闸 / 凭证回查
CREATE INDEX IF NOT EXISTS idx_biz_invoice_date    ON biz_invoice (invoice_date);
CREATE INDEX IF NOT EXISTS idx_biz_invoice_voucher ON biz_invoice (voucher_id)
    WHERE voucher_id IS NOT NULL;

-- ---------- ② 取号规则：IV（销售发票单据号） ----------
INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status, remark)
SELECT v.id::uuid, 'IV', '销售发票号', 'IV', 'yyyyMMdd', '', 0, 1, '-', 1,
       '销售发票单据号：IV-yyyyMMdd-000001，按天（CST）滚动；手填税控发票号时不取号'
  FROM (VALUES ('d0295000-0000-4000-8000-000000000001')) AS v(id)
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq q WHERE q.seq_type = 'IV' AND q.deleted_at IS NULL);

-- ---------- ③ 自检：结构 + 约束 + 索引 + 默认值 + 外键 + 行为探针 + 取号 ----------
DO $$
DECLARE
    n        INT;
    d        TEXT;
    sample   TEXT;
    cust     UUID := gen_random_uuid();
    probe_iv UUID := 'd0295fff-0000-4000-8000-000000000001'::uuid;
    probe_ar UUID := 'd0295fff-0000-4000-8000-000000000002'::uuid;
    probe_fk UUID := 'd0295fff-0000-4000-8000-000000000003'::uuid;
BEGIN
    -- 3.1 表在位 + 列数（多一列少一列都算漂移）
    IF to_regclass('public.biz_invoice') IS NULL THEN
        RAISE EXCEPTION 'V0298 自检失败：biz_invoice 未建';
    END IF;
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_invoice';
    IF n <> 21 THEN
        RAISE EXCEPTION 'V0298 自检失败：biz_invoice 列数 = %（期望 21 = 16 业务 + 5 审计）', n;
    END IF;

    -- 3.2 卡面点名的 16 个业务字段 + 审计 5 件套必须逐字在位
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_invoice'
       AND column_name IN ('id','invoice_no','invoice_type','invoice_code','customer_id',
                           'invoice_date','amount','tax_amount','total_with_tax','tax_rate',
                           'status','receivable_id','source_type','source_no','voucher_id','remark',
                           'created_at','updated_at','create_by','update_by','deleted_at');
    IF n <> 21 THEN
        RAISE EXCEPTION 'V0298 自检失败：biz_invoice 卡面列只齐了 %/21', n;
    END IF;

    -- 3.3 金额三列必须 NUMERIC(18,4)（与 voucher_entry / receivable / receipt 同精度）
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_invoice'
       AND column_name IN ('amount','tax_amount','total_with_tax')
       AND numeric_precision = 18 AND numeric_scale = 4;
    IF n <> 3 THEN
        RAISE EXCEPTION 'V0298 自检失败：金额三列 NUMERIC(18,4) 只落地 %/3', n;
    END IF;
    -- 税率 NUMERIC(6,4)
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_invoice'
       AND column_name = 'tax_rate' AND numeric_precision = 6 AND numeric_scale = 4;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0298 自检失败：tax_rate 不是 NUMERIC(6,4)';
    END IF;

    -- 3.4 六条 CHECK 在位
    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid = 'public.biz_invoice'::regclass
       AND conname IN ('ck_biz_invoice_amount','ck_biz_invoice_sign','ck_biz_invoice_status',
                       'ck_biz_invoice_type','ck_biz_invoice_tax_rate','ck_biz_invoice_source');
    IF n <> 6 THEN
        RAISE EXCEPTION 'V0298 自检失败：biz_invoice 点名约束应有 6 条，实际 %', n;
    END IF;

    -- 3.5 状态域 4 值齐全（漏一个下游就会踩 23514）
    FOREACH d IN ARRAY ARRAY['DRAFT','ISSUED','POSTED','VOID']
    LOOP
        IF (SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = 'ck_biz_invoice_status')
           NOT LIKE '%''' || d || '''%' THEN
            RAISE EXCEPTION 'V0298 自检失败：ck_biz_invoice_status 缺域值 %', d;
        END IF;
    END LOOP;
    -- 隔壁模块的域值不许混进发票域（防下游抄 receivable / receipt 后语义漂移）
    IF (SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = 'ck_biz_invoice_status')
       LIKE '%CANCELLED%' OR (SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = 'ck_biz_invoice_status')
       LIKE '%OPEN%' THEN
        RAISE EXCEPTION 'V0298 自检失败：ck_biz_invoice_status 混入了收款/应收的域值（CANCELLED / OPEN）';
    END IF;

    -- 3.6 索引：pkey + uk×1 + idx×5 = 7；点名索引必须真实落地（撞名会被静默跳过）
    SELECT count(*) INTO n FROM pg_indexes WHERE schemaname = 'public' AND tablename = 'biz_invoice';
    IF n <> 7 THEN
        RAISE EXCEPTION 'V0298 自检失败：biz_invoice 索引应有 7 个，实际 %（撞名被静默跳过？）', n;
    END IF;
    SELECT count(*) INTO n FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_invoice'
       AND indexname IN ('uk_biz_invoice_no','idx_biz_invoice_customer','idx_biz_invoice_status',
                         'idx_biz_invoice_receivable','idx_biz_invoice_date','idx_biz_invoice_voucher');
    IF n <> 6 THEN
        RAISE EXCEPTION 'V0298 自检失败：点名的 6 个索引只落地了 %/6', n;
    END IF;
    SELECT indexdef INTO d FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_biz_invoice_no';
    IF d NOT LIKE '%UNIQUE%' OR d NOT LIKE '%deleted_at IS NULL%' THEN
        RAISE EXCEPTION 'V0298 自检失败：uk_biz_invoice_no 不是「存活行唯一」的部分索引（def=%）', d;
    END IF;
    -- receivable_id 索引不许是唯一（一张应收可分次开票 + 红冲）
    SELECT indexdef INTO d FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'idx_biz_invoice_receivable';
    IF d LIKE '%UNIQUE%' THEN
        RAISE EXCEPTION 'V0298 自检失败：idx_biz_invoice_receivable 不该是唯一索引（def=%）', d;
    END IF;

    -- 3.7 默认值：invoice_type=OUT / status=DRAFT / tax_rate=0.1300；
    --     三个金额列**必须没有**默认值（有默认值 = 钓鱼，见文件头）
    SELECT column_default INTO d FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_invoice' AND column_name = 'invoice_type';
    IF d IS NULL OR d NOT LIKE '''OUT''%' THEN
        RAISE EXCEPTION 'V0298 自检失败：invoice_type 默认值不是 OUT（default=%）', d;
    END IF;
    SELECT column_default INTO d FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_invoice' AND column_name = 'status';
    IF d IS NULL OR d NOT LIKE '''DRAFT''%' THEN
        RAISE EXCEPTION 'V0298 自检失败：status 默认值不是 DRAFT（default=%）', d;
    END IF;
    SELECT column_default INTO d FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_invoice' AND column_name = 'tax_rate';
    IF d IS NULL OR d::numeric <> 0.13 THEN
        RAISE EXCEPTION 'V0298 自检失败：tax_rate 默认值不是 0.13（default=%）', d;
    END IF;
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_invoice'
       AND column_name IN ('amount','tax_amount','total_with_tax')
       AND column_default IS NOT NULL;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0298 自检失败：金额列不该有默认值（会与 ck_biz_invoice_amount 互相矛盾），实际 % 列有', n;
    END IF;

    -- 3.8 外键：receivable_id → biz_receivable(id) ON DELETE RESTRICT（confdeltype='r'）
    SELECT confdeltype INTO d FROM pg_constraint
     WHERE conname = 'biz_invoice_receivable_id_fkey'
       AND conrelid = 'public.biz_invoice'::regclass;
    IF d IS NULL THEN
        RAISE EXCEPTION 'V0298 自检失败：biz_invoice_receivable_id_fkey 不存在';
    END IF;
    IF d <> 'r' THEN
        RAISE EXCEPTION 'V0298 自检失败：receivable_id 外键删除动作 = %（期望 r = RESTRICT）', d;
    END IF;
    -- customer_id 必须是弱引用：整张表只许这一个外键
    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid = 'public.biz_invoice'::regclass AND contype = 'f';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0298 自检失败：biz_invoice 外键数 = %（期望 1：只有 receivable_id；customer_id 必须弱引用）', n;
    END IF;

    -- ================= 3.9 行为探针（坏数据必须被拒） =================

    -- 3.9a 恒等式不成立（1000 + 130 ≠ 1131）→ 拒
    BEGIN
        INSERT INTO biz_invoice (invoice_no, customer_id, invoice_date, amount, tax_amount, total_with_tax)
        VALUES ('IV-PROBE-0001', cust, DATE '2026-11-06', 1000, 130, 1131);
        RAISE EXCEPTION 'V0298 自检失败：total_with_tax ≠ amount + tax_amount 却插入成功';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 3.9b 正票 amount = 0 → 拒（符号约束）
    BEGIN
        INSERT INTO biz_invoice (invoice_no, customer_id, invoice_date, amount, tax_amount, total_with_tax)
        VALUES ('IV-PROBE-0002', cust, DATE '2026-11-06', 0, 0, 0);
        RAISE EXCEPTION 'V0298 自检失败：OUT 且 amount = 0 未被 ck_biz_invoice_sign 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 3.9c 正票 amount 为负 → 拒（红冲必须走 OUT_RED）
    BEGIN
        INSERT INTO biz_invoice (invoice_no, customer_id, invoice_date, amount, tax_amount, total_with_tax)
        VALUES ('IV-PROBE-0003', cust, DATE '2026-11-06', -1000, -130, -1130);
        RAISE EXCEPTION 'V0298 自检失败：OUT 且 amount < 0 未被 ck_biz_invoice_sign 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 3.9d 红字票金额为正 → 拒（ARCH-7 §15.2.3 要求 amount 为负）
    BEGIN
        INSERT INTO biz_invoice (invoice_no, invoice_type, customer_id, invoice_date,
                                 amount, tax_amount, total_with_tax)
        VALUES ('IV-PROBE-0004', 'OUT_RED', cust, DATE '2026-11-06', 1000, 130, 1130);
        RAISE EXCEPTION 'V0298 自检失败：OUT_RED 且 amount > 0 未被 ck_biz_invoice_sign 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 3.9e 状态越界：应收的 OPEN → 拒（抄 receivable 会在库层炸）
    BEGIN
        INSERT INTO biz_invoice (invoice_no, customer_id, invoice_date, amount, tax_amount, total_with_tax, status)
        VALUES ('IV-PROBE-0005', cust, DATE '2026-11-06', 1000, 130, 1130, 'OPEN');
        RAISE EXCEPTION 'V0298 自检失败：status = OPEN 未被 ck_biz_invoice_status 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 3.9f 状态越界：收款的 CONFIRMED / CANCELLED → 拒（发票用 VOID）
    BEGIN
        INSERT INTO biz_invoice (invoice_no, customer_id, invoice_date, amount, tax_amount, total_with_tax, status)
        VALUES ('IV-PROBE-0006', cust, DATE '2026-11-06', 1000, 130, 1130, 'CONFIRMED');
        RAISE EXCEPTION 'V0298 自检失败：status = CONFIRMED 未被 ck_biz_invoice_status 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO biz_invoice (invoice_no, customer_id, invoice_date, amount, tax_amount, total_with_tax, status)
        VALUES ('IV-PROBE-0007', cust, DATE '2026-11-06', 1000, 130, 1130, 'CANCELLED');
        RAISE EXCEPTION 'V0298 自检失败：status = CANCELLED 未被 ck_biz_invoice_status 拒绝（发票作废用 VOID）';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 3.9g 票种越界：进项 IN（本表是销售发票）→ 拒
    BEGIN
        INSERT INTO biz_invoice (invoice_no, invoice_type, customer_id, invoice_date,
                                 amount, tax_amount, total_with_tax)
        VALUES ('IV-PROBE-0008', 'IN', cust, DATE '2026-11-06', 1000, 130, 1130);
        RAISE EXCEPTION 'V0298 自检失败：invoice_type = IN 未被 ck_biz_invoice_type 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 3.9h 来源类型：ARCH 稿面字面量 'SO' → 拒（归一到 SALES_ORDER，见 COMMENT）
    BEGIN
        INSERT INTO biz_invoice (invoice_no, customer_id, invoice_date, amount, tax_amount,
                                 total_with_tax, source_type)
        VALUES ('IV-PROBE-0009', cust, DATE '2026-11-06', 1000, 130, 1130, 'SO');
        RAISE EXCEPTION 'V0298 自检失败：source_type = SO 未被 ck_biz_invoice_source 拒绝（应写 SALES_ORDER）';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 3.9i 税率越界：-0.01 / 1.5 → 拒
    BEGIN
        INSERT INTO biz_invoice (invoice_no, customer_id, invoice_date, amount, tax_amount,
                                 total_with_tax, tax_rate)
        VALUES ('IV-PROBE-0010', cust, DATE '2026-11-06', 1000, 130, 1130, -0.01);
        RAISE EXCEPTION 'V0298 自检失败：tax_rate = -0.01 未被 ck_biz_invoice_tax_rate 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO biz_invoice (invoice_no, customer_id, invoice_date, amount, tax_amount,
                                 total_with_tax, tax_rate)
        VALUES ('IV-PROBE-0011', cust, DATE '2026-11-06', 1000, 130, 1130, 1.5);
        RAISE EXCEPTION 'V0298 自检失败：tax_rate = 1.5 未被 ck_biz_invoice_tax_rate 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 3.9j 正常行：默认值一起落（DRAFT / OUT / 0.1300）
    INSERT INTO biz_invoice (id, invoice_no, customer_id, invoice_date,
                             amount, tax_amount, total_with_tax)
    VALUES (probe_iv, 'IV-PROBE-1000', cust, DATE '2026-11-06', 1000.0000, 130.0000, 1130.0000);
    SELECT status || '|' || invoice_type || '|' || tax_rate::text || '|' || total_with_tax::text
      INTO d FROM biz_invoice WHERE id = probe_iv;
    IF d <> 'DRAFT|OUT|0.1300|1130.0000' THEN
        RAISE EXCEPTION 'V0298 自检失败：默认值落库 = %（期望 DRAFT|OUT|0.1300|1130.0000）', d;
    END IF;

    -- 3.9k 存活行重号 → 拒
    BEGIN
        INSERT INTO biz_invoice (invoice_no, customer_id, invoice_date, amount, tax_amount, total_with_tax)
        VALUES ('IV-PROBE-1000', cust, DATE '2026-11-06', 1000, 130, 1130);
        RAISE EXCEPTION 'V0298 自检失败：存活行重号未被 uk_biz_invoice_no 拒绝';
    EXCEPTION WHEN unique_violation THEN NULL;
    END;

    -- 3.9l 金额精度不被截断（NUMERIC(18,4)）：0.0002 必须原样留下
    INSERT INTO biz_invoice (invoice_no, customer_id, invoice_date, amount, tax_amount, total_with_tax)
    VALUES ('IV-PROBE-1001', cust, DATE '2026-11-06', 0.0002, 0.0000, 0.0002);
    SELECT amount::text INTO d FROM biz_invoice WHERE invoice_no = 'IV-PROBE-1001';
    IF d <> '0.0002' THEN
        RAISE EXCEPTION 'V0298 自检失败：金额精度被截断（0.0002 → %）', d;
    END IF;

    -- 3.9m 红字票正常路径：金额为负、恒等式成立 → 通过
    INSERT INTO biz_invoice (invoice_no, invoice_type, customer_id, invoice_date,
                             amount, tax_amount, total_with_tax)
    VALUES ('IV-PROBE-1002', 'OUT_RED', cust, DATE '2026-11-06', -1000.0000, -130.0000, -1130.0000);
    SELECT count(*) INTO n FROM biz_invoice WHERE invoice_no = 'IV-PROBE-1002'
       AND total_with_tax = -1130.0000;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0298 自检失败：红字票（负金额）正常路径插入失败';
    END IF;

    -- 3.9n 软删后同号可复用（部分唯一索引语义）
    UPDATE biz_invoice SET deleted_at = now() WHERE invoice_no = 'IV-PROBE-1001';
    INSERT INTO biz_invoice (invoice_no, customer_id, invoice_date, amount, tax_amount, total_with_tax)
    VALUES ('IV-PROBE-1001', cust, DATE '2026-11-06', 0.0002, 0.0000, 0.0002);
    SELECT count(*) INTO n FROM biz_invoice
     WHERE invoice_no = 'IV-PROBE-1001' AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0298 自检失败：软删后同号重插后存活行 = %（期望 1）', n;
    END IF;

    -- 3.9o 外键：挂到不存在的应收 → 拒（foreign_key_violation）
    BEGIN
        INSERT INTO biz_invoice (invoice_no, customer_id, invoice_date, amount, tax_amount,
                                 total_with_tax, receivable_id)
        VALUES ('IV-PROBE-1003', cust, DATE '2026-11-06', 1000, 130, 1130, gen_random_uuid());
        RAISE EXCEPTION 'V0298 自检失败：receivable_id 指向不存在的应收却插入成功';
    EXCEPTION WHEN foreign_key_violation THEN NULL;
    END;

    -- 3.9p 硬删「已被发票引用」的应收 → 拒（RESTRICT = SQLSTATE 23001，不是 23503）
    INSERT INTO biz_receivable (id, ar_no, customer_id, source_type, source_id, source_no,
                                amount, unsettled_amount, credit_days, period_code, due_date)
    VALUES (probe_ar, 'AR-PROBE-V0298-0001', cust, 'MANUAL', gen_random_uuid(), 'MANUAL',
            1130.0000, 1130.0000, 0, '2026-11', DATE '2026-11-06');
    INSERT INTO biz_invoice (id, invoice_no, customer_id, invoice_date, amount, tax_amount,
                             total_with_tax, receivable_id)
    VALUES (probe_fk, 'IV-PROBE-1004', cust, DATE '2026-11-06', 1000, 130, 1130, probe_ar);
    BEGIN
        DELETE FROM biz_receivable WHERE id = probe_ar;
        RAISE EXCEPTION 'V0298 自检失败：硬删已被发票引用的应收未被 RESTRICT 拒绝';
    EXCEPTION WHEN restrict_violation OR foreign_key_violation THEN NULL;
    END;

    -- 3.9q 清场：探针行全部消失（发票 + 探针应收）
    DELETE FROM biz_invoice WHERE invoice_no LIKE 'IV-PROBE-%';
    DELETE FROM biz_receivable WHERE ar_no LIKE 'AR-PROBE-V0298-%';
    SELECT count(*) INTO n FROM biz_invoice WHERE invoice_no LIKE 'IV-PROBE-%';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0298 自检失败：发票探针残留 % 行', n;
    END IF;
    SELECT count(*) INTO n FROM biz_receivable WHERE ar_no LIKE 'AR-PROBE-V0298-%';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0298 自检失败：应收探针残留 % 行', n;
    END IF;

    -- 3.10 取号规则：IV 行在位且口径正确，并能拼出 IV-yyyyMMdd-000001 形态
    SELECT prefix || '|' || date_pattern || '|' || separator || '|' || step || '|' || status
      INTO d FROM biz_seq WHERE seq_type = 'IV' AND deleted_at IS NULL;
    IF d IS NULL THEN
        RAISE EXCEPTION 'V0298 自检失败：biz_seq(seq_type=IV) 取号规则行缺失';
    END IF;
    IF d <> 'IV|yyyyMMdd|-|1|1' THEN
        RAISE EXCEPTION 'V0298 自检失败：IV 取号规则口径不对（% ≠ IV|yyyyMMdd|-|1|1）', d;
    END IF;
    SELECT 'IV-' || to_char(DATE '2026-11-06', 'YYYYMMDD') || '-' || lpad('1', 6, '0') INTO sample;
    IF sample !~ '^IV-[0-9]{8}-[0-9]{6}$' THEN
        RAISE EXCEPTION 'V0298 自检失败：按规则拼出的单号 % 不符合 IV-YYYYMMDD-NNNNNN', sample;
    END IF;

    RAISE NOTICE 'V0298 自检通过：biz_invoice 21 列 / 6 约束 / 7 索引 / 外键 RESTRICT / IV 取号=%',
                 sample;
END $$;
