-- ======================================================
-- V0292：销售链 —— 收款单 `biz_receipt` + 核销明细 `biz_receipt_allocation`（SO-16）
--
--   ① biz_receipt             收款单头（16 列 = ARCH-7 §3.1.7 的 11 个业务字段 + 审计 5 件套）
--   ② biz_receipt_allocation  收款核销明细（一张收款可核销多张应收）
--   ③ biz_seq(seq_type='RC')  单号取号规则：RC-yyyyMMdd-000001（按 CST 天滚动）
--
-- ── 号段（开工实测复核，别信卡面）──────────────────────────
--   卡面写「V0265__sales_receipt」，但 V0265 已被 SO-04 占用（已应用迁移不可改、
--   指纹校验会炸，**不能复用号段**）；同期并行的「出库作废红冲」卡占 V0290，
--   SO-15 占 V0291。写入本文件时 business/ 最大号 = V0291 ⇒ 本卡取 **V0292**。
--   迁移号会被并行卡抢走，**提交前**再 `ls business | tail` 复核一次。
--
-- ── 口径来源与 ARCH-7 稿面的差异（逐条记在这里，避免下游再各写一套）────────
--   主口径 = `docs/architecture/ARCH-7-销售模块设计.md` §3.1.7 + §4.4。
--   与稿面**不同**的地方（都是「照应付/应收既成口径」的收敛，不是随手改）：
--     * amount / allocated_amount  NUMERIC(18,4) ｜ 稿面 (18,2)
--       → 与 `biz_voucher_entry` / `biz_receivable.amount` / `biz_payment.amount` 的
--         四位小数口径一致；少两位会静默四舍五入（V0287 对 receivable 做过同样决策）。
--     * 卡面「审计」= 五个列（created_at / updated_at / create_by / update_by / deleted_at），
--       与 `biz_receivable` / `biz_payment` 逐列同形（含软删 + 存活行唯一索引）。
--     * 核销明细在稿面 5 字段（id/receipt_id/receivable_id/amount/remark）之外补
--       `created_at` / `create_by`：核销是**动钱的动作**，审计要能回答「谁在什么时候
--       把这张收款核到了那笔应收上」。只补这两列（核销行随收款级联删除，
--       `updated_at` / `deleted_at` 没有语义，不加）。
--     * 金额列 MUST 显式写 `allocated_amount`：它有 `DEFAULT 0`，而 `ck_biz_receipt_amount`
--       要求 `allocated_amount <= amount`，只写 amount 时默认 0 是合法值（不像 receivable
--       的恒等式那样必炸）；**但**反过来「只写 allocated_amount 不写 amount」会撞 NOT NULL。
--
-- ── 状态域（`ck_biz_receipt_status`，4 值）──────────────────
--   依据 ARCH-7 §4.4 状态图：
--     DRAFT → CONFIRMED → POSTED（过账 ⇒ 生成收款凭证 借 1002 银行存款 / 贷 1122 应收账款）
--     DRAFT → CANCELLED（草稿作废；已核销的收款不许作废，见下）
--   ⚠ **与应收（OPEN/PARTIAL/SETTLED/WRITTEN_OFF/CANCELLED）和应付（OPEN/PARTIAL_PAID/…）
--     都不是一套域值**：收款单是「资金单据」，走的是「登记 → 确认 → 过账」台账流。
--     SO-17 落 API 时抄 payable 的 `PAID` / receivable 的 `OPEN` 都会在本 CHECK 上炸 23514。
--
--   `ck_biz_receipt_cancelled`：`status <> 'CANCELLED' OR allocated_amount = 0`
--   —— 作废一张已经核销过钱的收款会让「应收已收款」凭空多出来（核销明细还在、收款头作废），
--   钱与账对不上。要先冲核销明细（删 allocation + 回滚 received_amount）再作废，
--   handler 侧给 409，库层这条兜底（与 V0290 `ck_biz_ar_cancelled` 同思路）。
--
-- ── 约束（卡面点名 `amount > 0 AND allocated_amount >= 0 AND allocated_amount <= amount`）──
--   * ck_biz_receipt_amount : 卡面逐字（三连；含「已核销不许超过收款金额」）
--                              ⚠ `amount` **不给默认值**：卡面要求 amount > 0，
--                              留 `DEFAULT 0` 就是钓鱼（任何省略 amount 的插入必炸）。
--   * ck_biz_receipt_method : method 白名单（稿面 BANK_TRANSFER / CASH / CHECK / OTHER），
--                             允许 NULL（「还没选」≠ 「选了个非法值」）
--   * ck_biz_rcpt_alloc_amount : 核销明细 amount > 0（卡面逐字）
--
--   **本卡不建触发器**（与 V0287/V0290 同口径）：`allocated_amount` 与
--   `SUM(allocation.amount)` 的一致性由 SO-17 的核销事务维护（同一事务里 UPDATE 头 + INSERT 明细），
--   `ck_biz_receipt_amount` 兜住上限。明细总和不超头的约束是跨表聚合，CHECK 表达不了，
--   硬上触发器会把「核销 / 反核销」两边的写法都锁死。
--
-- ── 唯一 / 幂等 ─────────────────────────────────────────
--   * uk_biz_receipt_no        存活行唯一（`WHERE deleted_at IS NULL`，软删后单号可复用）
--   * uk_biz_rcpt_alloc_pair   `(receipt_id, receivable_id)` **一条收款项对同一笔应收只有一行**
--                              ⇒ 重复核销只可能是「改金额」（UPSERT），不会出现两条明细重复计入
--                              `SUM(amount)` 让「核销总额 ≤ 收款金额」的校验形同虚设。
--                              SO-17 写核销请用
--                              `INSERT … ON CONFLICT (receipt_id, receivable_id) DO UPDATE SET amount = …`。
--                              （反核销 = DELETE 该行 + 回滚应收的 received_amount。）
--
-- ── 外键口径（**唯一一处与卡面「外键 ON DELETE CASCADE」不一致，理由在此**）─────
--   * `receipt_id`  → biz_receipt(id)  **ON DELETE CASCADE**  ← 稿面逐字，卡面逐字：
--       收款头没了，它的核销明细就是孤儿行，必须一起走。
--   * `receivable_id` → biz_receivable(id) **ON DELETE RESTRICT**（**不是** CASCADE）：
--       核销明细是「这笔钱抵了哪笔应收」的唯一留痕。若级联删，硬删应收会**静默吞掉**
--       核销明细，而 `biz_receipt.allocated_amount` 仍是原值 ⇒ 收款头声称「已核销 600」
--       却找不到抵给了谁，恒等与审计同时崩（且没有任何报错）。
--       应收本身走**软删**（`deleted_at`，V0287），实务上永远不触发 RESTRICT；
--       一旦有人真想物理删一笔被核销过的应收，库层让他先解核销 —— 这才是想要的语义。
--   * `customer_id` 是**弱引用**（无外键）：客户走软删（V0260），外键会把
--       「客户软删但历史收款单要留」这条业务堵死 —— 与 `biz_receivable.customer_id`
--       / `biz_sales_order.customer_id` / `biz_shipment.customer_id` 同风格。
--       引用有效性由 SO-17 建单时校验。ARCH-7 §3.1.7 写的「FK → biz_customer(id)」按此口径执行。
--   * `voucher_id` 无外键（凭证由过账事务生成，反向依赖会让事务成环），与 `biz_receivable` 同。
--
-- ── 索引 ──────────────────────────────────────────────
--   biz_receipt：pkey + uk_biz_receipt_no + idx_biz_receipt_customer(customer_id, receipt_date)
--                + idx_biz_receipt_status(status) + idx_biz_receipt_date(receipt_date)
--                + idx_biz_receipt_voucher(voucher_id) WHERE NOT NULL        ⇒ 6 个
--   biz_receipt_allocation：pkey + uk_biz_rcpt_alloc_pair
--                + idx_biz_rcpt_alloc_receivable(receivable_id)              ⇒ 3 个
--   ⚠ 索引名统一带 `receipt` / `rcpt` 全词：PG 索引名是 schema 级唯一，撞名 + IF NOT EXISTS
--     会被**静默跳过**（SO-04 实测栽过）。本卡开工 `grep -rn "biz_receipt\|biz_rcpt"` 全仓 0 命中。
--
-- ── 幂等 ──────────────────────────────────────────────
--   CREATE TABLE / INDEX IF NOT EXISTS；biz_seq 种子 `INSERT … WHERE NOT EXISTS`。
--   末尾 DO 块自检：表 / 列（16 + 7）/ 5 条 CHECK / 9 个索引 / 默认值 / 外键删除动作 /
--   状态与方式域值 / 20 条行为探针（坏数据必须被拒 + 正常路径 + CASCADE/RESTRICT + 精度）/
--   取号规则，任一不符 → 迁移失败。
-- ======================================================

-- ---------- ① biz_receipt（收款单头） ----------
CREATE TABLE IF NOT EXISTS biz_receipt (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),       -- 主键
    receipt_no       VARCHAR(32)   NOT NULL,                           -- 收款单号：RC-yyyyMMdd-000001（biz_seq 取号，存活行唯一）
    customer_id      UUID          NOT NULL,                           -- 客户（弱引用 biz_customer.id，无外键）
    receipt_date     DATE          NOT NULL,                           -- 收款日期（必填；凭证日期取它）
    amount           NUMERIC(18,4) NOT NULL,                           -- 收款金额（> 0；无默认值，见文件头）
    allocated_amount NUMERIC(18,4) NOT NULL DEFAULT 0,                 -- 已核销金额（>= 0 且 <= amount，由核销事务维护）
    method           VARCHAR(32),                                      -- 收款方式：BANK_TRANSFER / CASH / CHECK / OTHER
    bank_account     VARCHAR(64),                                      -- 银行账号 / 收款账户
    status           VARCHAR(16)   NOT NULL DEFAULT 'DRAFT',           -- 状态机：DRAFT/CONFIRMED/POSTED/CANCELLED（ARCH-7 §4.4）
    voucher_id       UUID,                                             -- 收款凭证 id（无外键，避免过账事务成环）
    remark           TEXT,                                             -- 备注
    created_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 审计：创建时间
    updated_at       TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 审计：更新时间（model 层维护）
    create_by        UUID,                                             -- 审计：创建人
    update_by        UUID,                                             -- 审计：更新人
    deleted_at       TIMESTAMPTZ,                                      -- 软删时间（NULL = 存活）

    -- 状态域：ARCH-7 §4.4（资金单据的台账流，**不是**应收/应付那一套域值，见文件头）
    CONSTRAINT ck_biz_receipt_status
        CHECK (status IN ('DRAFT','CONFIRMED','POSTED','CANCELLED')),
    -- 卡面逐字：金额三连（含「已核销不许超过收款金额」）
    CONSTRAINT ck_biz_receipt_amount
        CHECK (amount > 0 AND allocated_amount >= 0 AND allocated_amount <= amount),
    -- 收款方式白名单（NULL = 未选，合法）
    CONSTRAINT ck_biz_receipt_method
        CHECK (method IS NULL OR method IN ('BANK_TRANSFER','CASH','CHECK','OTHER')),
    -- 作废 ⇔ 一分没核销（作废已核销的收款会让「应收已收」凭空多出来）
    CONSTRAINT ck_biz_receipt_cancelled
        CHECK (status <> 'CANCELLED' OR allocated_amount = 0)
);

COMMENT ON TABLE  biz_receipt IS '收款单头（销售链 · SO-16；状态 DRAFT/CONFIRMED/POSTED/CANCELLED，ARCH-7 §4.4）';
COMMENT ON COLUMN biz_receipt.receipt_no IS '收款单号：RC-yyyyMMdd-000001（biz_seq seq_type=RC 取号，按 CST 的天滚动）';
COMMENT ON COLUMN biz_receipt.customer_id IS '客户 id（弱引用 biz_customer.id，无外键：客户软删后历史收款单仍要可读）';
COMMENT ON COLUMN biz_receipt.amount IS '收款金额（> 0；刻意不给默认值：默认 0 会与 ck_biz_receipt_amount 互相矛盾）';
COMMENT ON COLUMN biz_receipt.allocated_amount IS '已核销金额（>= 0 且 <= amount；= SUM(biz_receipt_allocation.amount)，由核销事务维护）';
COMMENT ON COLUMN biz_receipt.method IS '收款方式：BANK_TRANSFER / CASH / CHECK / OTHER（ck_biz_receipt_method 白名单，NULL = 未选）';
COMMENT ON COLUMN biz_receipt.status IS '状态机：DRAFT/CONFIRMED/POSTED/CANCELLED（ARCH-7 §4.4；与应收/应付域值不同）';
COMMENT ON COLUMN biz_receipt.voucher_id IS '收款过账凭证（借 1002 银行存款 / 贷 1122 应收账款；由 SO-17 过账时写入）';
COMMENT ON COLUMN biz_receipt.deleted_at IS '软删标记；单号唯一索引带 WHERE deleted_at IS NULL ⇒ 软删后单号可复用';

-- 单号唯一：部分唯一索引（存活行唯一），与 uk_biz_ar_no / uk_biz_payment_no 同口径
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_receipt_no
    ON biz_receipt (receipt_no) WHERE deleted_at IS NULL;
-- 客户台账：按客户 + 日期筛（收款明细页 / 客户对账单）
CREATE INDEX IF NOT EXISTS idx_biz_receipt_customer ON biz_receipt (customer_id, receipt_date);
CREATE INDEX IF NOT EXISTS idx_biz_receipt_status   ON biz_receipt (status);
CREATE INDEX IF NOT EXISTS idx_biz_receipt_date     ON biz_receipt (receipt_date);
-- 凭证回查（部分索引，空值不占位）
CREATE INDEX IF NOT EXISTS idx_biz_receipt_voucher  ON biz_receipt (voucher_id) WHERE voucher_id IS NOT NULL;

-- ---------- ② biz_receipt_allocation（收款核销明细） ----------
CREATE TABLE IF NOT EXISTS biz_receipt_allocation (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),       -- 主键
    receipt_id    UUID          NOT NULL
                  REFERENCES biz_receipt (id) ON DELETE CASCADE,    -- 收款单（头删 → 明细级联删，稿面逐字）
    receivable_id UUID          NOT NULL
                  REFERENCES biz_receivable (id) ON DELETE RESTRICT,-- 应收单（**RESTRICT**，见文件头「外键口径」）
    amount        NUMERIC(18,4) NOT NULL,                           -- 本次核销金额（> 0）
    remark        VARCHAR(500),                                     -- 备注
    created_at    TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 审计：核销时间
    create_by     UUID,                                             -- 审计：核销人

    -- 卡面逐字：核销金额必须为正
    CONSTRAINT ck_biz_rcpt_alloc_amount CHECK (amount > 0)
);

COMMENT ON TABLE  biz_receipt_allocation IS '收款核销明细（一张收款可核销多张应收；收款单删除时级联）';
COMMENT ON COLUMN biz_receipt_allocation.receipt_id IS '收款单 id（ON DELETE CASCADE：收款头删除时明细一起走）';
COMMENT ON COLUMN biz_receipt_allocation.receivable_id IS '应收单 id（ON DELETE RESTRICT：核销留痕不许被硬删应收静默吞掉；应收走软删）';
COMMENT ON COLUMN biz_receipt_allocation.amount IS '本次核销金额（> 0；同一 (receipt_id, receivable_id) 只保留一行，重复核销 = 改金额）';

-- 一条收款项对同一笔应收只许一行（重复核销 = UPSERT 改金额，不是插第二行）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_rcpt_alloc_pair
    ON biz_receipt_allocation (receipt_id, receivable_id);
-- 反查：某笔应收被哪些收款核销过（详情抽屉「核销记录」/ 账龄回查）
CREATE INDEX IF NOT EXISTS idx_biz_rcpt_alloc_receivable
    ON biz_receipt_allocation (receivable_id);

-- ---------- ③ 取号规则：RC（收款单号） ----------
INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status, remark)
SELECT v.id::uuid, 'RC', '收款单号', 'RC', 'yyyyMMdd', '', 0, 1, '-', 1,
       '收款单号：RC-yyyyMMdd-000001，按天（CST）滚动（V0292 · SO-16）'
  FROM (VALUES ('d0292000-0000-4000-8000-000000000001')) AS v(id)
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq q WHERE q.seq_type = 'RC' AND q.deleted_at IS NULL);

-- ---------- ④ 自检：结构 + 域值 + 默认值 + 外键动作 + 20 条行为探针 + 取号 ----------
DO $$
DECLARE
    n         INT;
    d         TEXT;
    s         TEXT;
    sample    TEXT;
    probe_ar  UUID := 'd0292fff-0000-4000-8000-000000000001'::uuid;
    probe_rc  UUID := 'd0292fff-0000-4000-8000-000000000002'::uuid;
    probe_rc2 UUID := 'd0292fff-0000-4000-8000-000000000003'::uuid;
BEGIN
    -- ===== 4.1 两张表在位 =====
    IF to_regclass('public.biz_receipt') IS NULL THEN
        RAISE EXCEPTION 'V0292 自检失败：biz_receipt 未建';
    END IF;
    IF to_regclass('public.biz_receipt_allocation') IS NULL THEN
        RAISE EXCEPTION 'V0292 自检失败：biz_receipt_allocation 未建';
    END IF;

    -- ===== 4.2 列数 / 列名逐字（多一列少一列都算漂移）=====
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_receipt';
    IF n <> 16 THEN
        RAISE EXCEPTION 'V0292 自检失败：biz_receipt 列数 = %（期望 16）', n;
    END IF;
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_receipt'
       AND column_name IN ('id','receipt_no','customer_id','receipt_date','amount',
                           'allocated_amount','method','bank_account','status','voucher_id',
                           'remark','created_at','updated_at','create_by','update_by','deleted_at');
    IF n <> 16 THEN
        RAISE EXCEPTION 'V0292 自检失败：biz_receipt 卡面列只齐了 %/16', n;
    END IF;

    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_receipt_allocation';
    IF n <> 7 THEN
        RAISE EXCEPTION 'V0292 自检失败：biz_receipt_allocation 列数 = %（期望 7）', n;
    END IF;
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_receipt_allocation'
       AND column_name IN ('id','receipt_id','receivable_id','amount','remark','created_at','create_by');
    IF n <> 7 THEN
        RAISE EXCEPTION 'V0292 自检失败：biz_receipt_allocation 卡面列只齐了 %/7', n;
    END IF;

    -- ===== 4.3 金额列精度（NUMERIC(18,4)，与 receivable / voucher_entry 同口径）=====
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public'
       AND ((table_name = 'biz_receipt' AND column_name IN ('amount','allocated_amount'))
         OR (table_name = 'biz_receipt_allocation' AND column_name = 'amount'))
       AND data_type = 'numeric' AND numeric_precision = 18 AND numeric_scale = 4;
    IF n <> 3 THEN
        RAISE EXCEPTION 'V0292 自检失败：三列金额的 NUMERIC(18,4) 只落地了 %/3', n;
    END IF;

    -- ===== 4.4 五条 CHECK 在位 =====
    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid IN ('public.biz_receipt'::regclass, 'public.biz_receipt_allocation'::regclass)
       AND conname IN ('ck_biz_receipt_status','ck_biz_receipt_amount','ck_biz_receipt_method',
                       'ck_biz_receipt_cancelled','ck_biz_rcpt_alloc_amount');
    IF n <> 5 THEN
        RAISE EXCEPTION 'V0292 自检失败：本卡 5 条 CHECK 只落地了 %/5', n;
    END IF;
    -- 卡面点名的两条必须逐字常见（PG 的约束定义会把数值字面量渲染成 `(0)::numeric`，
    -- 所以下面按「渲染后的形态」匹配，而不是按源码字面量）
    IF (SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = 'ck_biz_receipt_amount')
       NOT LIKE '%amount > (0)%'
       OR (SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = 'ck_biz_receipt_amount')
          NOT LIKE '%allocated_amount >= (0)%'
       OR (SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = 'ck_biz_receipt_amount')
          NOT LIKE '%allocated_amount <= amount%' THEN
        RAISE EXCEPTION 'V0292 自检失败：ck_biz_receipt_amount 不是卡面的三连口径（def=%）',
            (SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = 'ck_biz_receipt_amount');
    END IF;
    IF (SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = 'ck_biz_rcpt_alloc_amount')
       NOT LIKE '%amount > (0)%' THEN
        RAISE EXCEPTION 'V0292 自检失败：ck_biz_rcpt_alloc_amount 不是 amount > 0（def=%）',
            (SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = 'ck_biz_rcpt_alloc_amount');
    END IF;

    -- ===== 4.5 状态域 4 值齐全，且不许混入应收/应付的域值 =====
    FOREACH s IN ARRAY ARRAY['DRAFT','CONFIRMED','POSTED','CANCELLED']
    LOOP
        IF (SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = 'ck_biz_receipt_status')
           NOT LIKE '%' || s || '%' THEN
            RAISE EXCEPTION 'V0292 自检失败：ck_biz_receipt_status 缺域值 %', s;
        END IF;
    END LOOP;
    -- 方式域 4 值齐全
    FOREACH s IN ARRAY ARRAY['BANK_TRANSFER','CASH','CHECK','OTHER']
    LOOP
        IF (SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = 'ck_biz_receipt_method')
           NOT LIKE '%' || s || '%' THEN
            RAISE EXCEPTION 'V0292 自检失败：ck_biz_receipt_method 缺域值 %', s;
        END IF;
    END LOOP;

    -- ===== 4.6 索引：6 + 3 = 9，逐名核对（撞名被静默跳过会在这里炸）=====
    SELECT count(*) INTO n FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_receipt';
    IF n <> 6 THEN
        RAISE EXCEPTION 'V0292 自检失败：biz_receipt 索引应有 6 个，实际 %（撞名被静默跳过？）', n;
    END IF;
    SELECT count(*) INTO n FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_receipt'
       AND indexname IN ('uk_biz_receipt_no','idx_biz_receipt_customer','idx_biz_receipt_status',
                         'idx_biz_receipt_date','idx_biz_receipt_voucher');
    IF n <> 5 THEN
        RAISE EXCEPTION 'V0292 自检失败：biz_receipt 点名的 5 个索引只落地了 %/5', n;
    END IF;
    SELECT count(*) INTO n FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_receipt_allocation';
    IF n <> 3 THEN
        RAISE EXCEPTION 'V0292 自检失败：biz_receipt_allocation 索引应有 3 个，实际 %', n;
    END IF;
    SELECT indexdef INTO d FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_biz_receipt_no';
    IF d NOT LIKE '%UNIQUE%' OR d NOT LIKE '%deleted_at IS NULL%' THEN
        RAISE EXCEPTION 'V0292 自检失败：uk_biz_receipt_no 不是「存活行唯一」的部分索引（def=%）', d;
    END IF;
    SELECT indexdef INTO d FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_biz_rcpt_alloc_pair';
    IF d NOT LIKE '%UNIQUE%' OR d NOT LIKE '%receipt_id, receivable_id%' THEN
        RAISE EXCEPTION 'V0292 自检失败：uk_biz_rcpt_alloc_pair 不是 (receipt_id, receivable_id) 唯一（def=%）', d;
    END IF;

    -- ===== 4.7 默认值：status=DRAFT / allocated_amount=0；amount 必须**没有**默认值 =====
    SELECT column_default INTO d FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_receipt' AND column_name = 'status';
    IF d IS NULL OR d NOT LIKE '''DRAFT''%' THEN
        RAISE EXCEPTION 'V0292 自检失败：status 默认值不是 DRAFT（default=%）', d;
    END IF;
    IF (SELECT column_default FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = 'biz_receipt' AND column_name = 'allocated_amount')
       NOT LIKE '0%' THEN
        RAISE EXCEPTION 'V0292 自检失败：allocated_amount 默认值不是 0';
    END IF;
    IF (SELECT column_default FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = 'biz_receipt' AND column_name = 'amount')
       IS NOT NULL THEN
        RAISE EXCEPTION 'V0292 自检失败：amount 不该有默认值（会与 ck_biz_receipt_amount 互相矛盾）';
    END IF;

    -- ===== 4.8 外键删除动作：receipt_id=CASCADE('c') / receivable_id=RESTRICT('r') =====
    SELECT confdeltype INTO d FROM pg_constraint
     WHERE conname = 'biz_receipt_allocation_receipt_id_fkey';
    IF d IS DISTINCT FROM 'c' THEN
        RAISE EXCEPTION 'V0292 自检失败：receipt_id 外键的 ON DELETE 不是 CASCADE（confdeltype=%）', d;
    END IF;
    SELECT confdeltype INTO d FROM pg_constraint
     WHERE conname = 'biz_receipt_allocation_receivable_id_fkey';
    IF d IS DISTINCT FROM 'r' THEN
        RAISE EXCEPTION 'V0292 自检失败：receivable_id 外键的 ON DELETE 不是 RESTRICT（confdeltype=%，见文件头「外键口径」）', d;
    END IF;

    -- ================= 4.9 行为探针（坏数据必须被拒）=================

    -- 4.9a amount = 0 → 拒（卡面 amount > 0）
    BEGIN
        INSERT INTO biz_receipt (receipt_no, customer_id, receipt_date, amount)
        VALUES ('RC-20261106-900001', gen_random_uuid(), DATE '2026-11-06', 0);
        RAISE EXCEPTION 'V0292 自检失败：amount = 0 未被 ck_biz_receipt_amount 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 4.9b amount 为负 → 拒
    BEGIN
        INSERT INTO biz_receipt (receipt_no, customer_id, receipt_date, amount)
        VALUES ('RC-20261106-900002', gen_random_uuid(), DATE '2026-11-06', -5);
        RAISE EXCEPTION 'V0292 自检失败：amount = -5 未被 ck_biz_receipt_amount 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 4.9c 已核销金额 > 收款金额 → 拒
    BEGIN
        INSERT INTO biz_receipt (receipt_no, customer_id, receipt_date, amount, allocated_amount)
        VALUES ('RC-20261106-900003', gen_random_uuid(), DATE '2026-11-06', 100, 100.0001);
        RAISE EXCEPTION 'V0292 自检失败：allocated_amount > amount 未被 ck_biz_receipt_amount 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 4.9d 已核销金额为负 → 拒
    BEGIN
        INSERT INTO biz_receipt (receipt_no, customer_id, receipt_date, amount, allocated_amount)
        VALUES ('RC-20261106-900004', gen_random_uuid(), DATE '2026-11-06', 100, -0.0001);
        RAISE EXCEPTION 'V0292 自检失败：allocated_amount = -0.0001 未被 ck_biz_receipt_amount 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 4.9e 状态越界值（应收 OPEN / 应付 PAID 的域值）→ 拒：抄隔壁模块的域值在库层就炸
    BEGIN
        INSERT INTO biz_receipt (receipt_no, customer_id, receipt_date, amount, status)
        VALUES ('RC-20261106-900005', gen_random_uuid(), DATE '2026-11-06', 100, 'OPEN');
        RAISE EXCEPTION 'V0292 自检失败：status = OPEN 未被 ck_biz_receipt_status 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO biz_receipt (receipt_no, customer_id, receipt_date, amount, status)
        VALUES ('RC-20261106-900006', gen_random_uuid(), DATE '2026-11-06', 100, 'PAID');
        RAISE EXCEPTION 'V0292 自检失败：status = PAID 未被 ck_biz_receipt_status 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 4.9f 收款方式越界 → 拒
    BEGIN
        INSERT INTO biz_receipt (receipt_no, customer_id, receipt_date, amount, method)
        VALUES ('RC-20261106-900007', gen_random_uuid(), DATE '2026-11-06', 100, 'ALIPAY');
        RAISE EXCEPTION 'V0292 自检失败：method = ALIPAY 未被 ck_biz_receipt_method 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 4.9g 作废且已核销过钱 → 拒（作废会把「应收已收」凭空多出来）
    BEGIN
        INSERT INTO biz_receipt (receipt_no, customer_id, receipt_date, amount, allocated_amount, status)
        VALUES ('RC-20261106-900008', gen_random_uuid(), DATE '2026-11-06', 100, 60, 'CANCELLED');
        RAISE EXCEPTION 'V0292 自检失败：CANCELLED 且 allocated_amount = 60 未被 ck_biz_receipt_cancelled 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- ================= 4.10 正常路径 + 级联/约束行为 =================

    -- 4.10a 正常收款单：默认值一起过（DRAFT / 已核销 0）
    INSERT INTO biz_receipt (id, receipt_no, customer_id, receipt_date, amount, method, bank_account)
    VALUES (probe_rc, 'RC-20261106-900101', gen_random_uuid(), DATE '2026-11-06', 1000.0000,
            'BANK_TRANSFER', '6222 0000 1111 2222');
    SELECT status || '|' || allocated_amount::text || '|' || amount::text INTO d
      FROM biz_receipt WHERE id = probe_rc;
    IF d <> 'DRAFT|0.0000|1000.0000' THEN
        RAISE EXCEPTION 'V0292 自检失败：默认值落库 = %（期望 DRAFT|0.0000|1000.0000）', d;
    END IF;

    -- 4.10b 存活行重号 → 拒
    BEGIN
        INSERT INTO biz_receipt (receipt_no, customer_id, receipt_date, amount)
        VALUES ('RC-20261106-900101', gen_random_uuid(), DATE '2026-11-06', 10);
        RAISE EXCEPTION 'V0292 自检失败：存活行重号未被 uk_biz_receipt_no 拒绝';
    EXCEPTION WHEN unique_violation THEN NULL;
    END;

    -- 4.10c 软删后同号可复用（部分唯一索引语义；用独立的号，不动 probe_rc）
    INSERT INTO biz_receipt (receipt_no, customer_id, receipt_date, amount)
    VALUES ('RC-20261106-900102', gen_random_uuid(), DATE '2026-11-06', 10);
    UPDATE biz_receipt SET deleted_at = now() WHERE receipt_no = 'RC-20261106-900102';
    INSERT INTO biz_receipt (receipt_no, customer_id, receipt_date, amount)
    VALUES ('RC-20261106-900102', gen_random_uuid(), DATE '2026-11-06', 20);
    SELECT count(*) INTO n FROM biz_receipt
     WHERE receipt_no = 'RC-20261106-900102' AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0292 自检失败：软删后同号重插后存活行 = %（期望 1）', n;
    END IF;

    -- 4.10d 精度：0.0002 不被截断（NUMERIC(18,4)）
    INSERT INTO biz_receipt (receipt_no, customer_id, receipt_date, amount)
    VALUES ('RC-20261106-900104', gen_random_uuid(), DATE '2026-11-06', 0.0002);
    SELECT amount::text INTO d FROM biz_receipt WHERE receipt_no = 'RC-20261106-900104';
    IF d <> '0.0002' THEN
        RAISE EXCEPTION 'V0292 自检失败：amount 0.0002 落库变成 %（精度被截断）', d;
    END IF;

    -- 4.10e UPDATE 后超核销 → 拒（核销事务写错会被库拦住）
    BEGIN
        UPDATE biz_receipt SET allocated_amount = 1000.0001 WHERE id = probe_rc;
        RAISE EXCEPTION 'V0292 自检失败：UPDATE 把 allocated_amount 改到 > amount 未被拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    UPDATE biz_receipt SET allocated_amount = 600.0000 WHERE id = probe_rc;   -- 合法
    SELECT allocated_amount::text INTO d FROM biz_receipt WHERE id = probe_rc;
    IF d <> '600.0000' THEN
        RAISE EXCEPTION 'V0292 自检失败：allocated_amount 改为 600 落库 = %', d;
    END IF;

    -- 4.10f 备一笔应收（FK 目标），核销明细才能插
    INSERT INTO biz_receivable (id, ar_no, customer_id, source_type, source_id, source_no,
                                amount, received_amount, unsettled_amount, credit_days,
                                period_code, due_date)
    VALUES (probe_ar, 'AR-20261106-900101', gen_random_uuid(), 'MANUAL', gen_random_uuid(), 'MANUAL',
            1000.0000, 0, 1000.0000, 30, '2026-11', DATE '2026-12-06');

    -- 4.10g 正常核销明细
    INSERT INTO biz_receipt_allocation (receipt_id, receivable_id, amount, remark)
    VALUES (probe_rc, probe_ar, 600.0000, 'V0292 自检探针');
    SELECT count(*) INTO n FROM biz_receipt_allocation
     WHERE receipt_id = probe_rc AND receivable_id = probe_ar AND amount = 600.0000;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0292 自检失败：核销明细未按预期落库';
    END IF;

    -- 4.10h 核销金额 = 0 → 拒（用第二张收款单，避免唯一对先命中把 CHECK 探针糊过去）
    INSERT INTO biz_receipt (id, receipt_no, customer_id, receipt_date, amount)
    VALUES (probe_rc2, 'RC-20261106-900105', gen_random_uuid(), DATE '2026-11-06', 100);
    BEGIN
        INSERT INTO biz_receipt_allocation (receipt_id, receivable_id, amount)
        VALUES (probe_rc2, probe_ar, 0);
        RAISE EXCEPTION 'V0292 自检失败：核销 amount = 0 未被 ck_biz_rcpt_alloc_amount 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 4.10i 核销金额为负 → 拒
    BEGIN
        INSERT INTO biz_receipt_allocation (receipt_id, receivable_id, amount)
        VALUES (probe_rc2, probe_ar, -1);
        RAISE EXCEPTION 'V0292 自检失败：核销 amount = -1 未被 ck_biz_rcpt_alloc_amount 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 4.10j 同一 (receipt, receivable) 第二行 → 拒（重复核销会重复计入 SUM）
    BEGIN
        INSERT INTO biz_receipt_allocation (receipt_id, receivable_id, amount)
        VALUES (probe_rc, probe_ar, 100);
        RAISE EXCEPTION 'V0292 自检失败：同一收款对同一应收的第二行未被 uk_biz_rcpt_alloc_pair 拒绝';
    EXCEPTION WHEN unique_violation THEN NULL;
    END;

    -- 4.10k 核销到不存在的应收 → 拒（外键在位）
    BEGIN
        INSERT INTO biz_receipt_allocation (receipt_id, receivable_id, amount)
        VALUES (probe_rc, gen_random_uuid(), 10);
        RAISE EXCEPTION 'V0292 自检失败：核销到不存在的应收未被外键拒绝';
    EXCEPTION WHEN foreign_key_violation THEN NULL;
    END;

    -- 4.10l 硬删「被核销过的应收」→ 拒（RESTRICT：核销留痕不许被静默吞掉）
    --   ⚠ 坑：ON DELETE **RESTRICT** 抛的是 `restrict_violation`（SQLSTATE **23001**），
    --   不是 `foreign_key_violation`（23503，那是 NO ACTION / 插入端外键）。只捕 23503
    --   会让这条探针把整个 DO 块炸掉（实测踩过），所以两个都捕。
    BEGIN
        DELETE FROM biz_receivable WHERE id = probe_ar;
        RAISE EXCEPTION 'V0292 自检失败：被核销的应收被硬删（receivable_id 应是 RESTRICT）';
    EXCEPTION WHEN restrict_violation OR foreign_key_violation THEN NULL;
    END;

    -- 4.10m 删收款头 → 核销明细级联消失（CASCADE 生效）
    DELETE FROM biz_receipt WHERE id = probe_rc;
    SELECT count(*) INTO n FROM biz_receipt_allocation WHERE receipt_id = probe_rc;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0292 自检失败：删收款头后仍残留 % 行核销明细（CASCADE 未生效）', n;
    END IF;

    -- 4.10n 清场：探针行全部消失（先收款（已删）→ 再应收，顺序不能反）
    DELETE FROM biz_receipt WHERE receipt_no LIKE 'RC-20261106-9%';
    DELETE FROM biz_receivable WHERE ar_no LIKE 'AR-20261106-9%';
    SELECT count(*) INTO n FROM biz_receipt WHERE receipt_no LIKE 'RC-20261106-9%';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0292 自检失败：biz_receipt 探针残留 % 行', n;
    END IF;
    SELECT count(*) INTO n FROM biz_receipt_allocation;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0292 自检失败：biz_receipt_allocation 残留 % 行', n;
    END IF;
    SELECT count(*) INTO n FROM biz_receivable WHERE ar_no LIKE 'AR-20261106-9%';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0292 自检失败：biz_receivable 探针残留 % 行', n;
    END IF;

    -- ===== 4.11 取号规则：RC 行在位、口径正确、能拼出 RC-YYYYMMDD-NNNNNN =====
    SELECT prefix || '|' || date_pattern || '|' || separator || '|' || step || '|' || status
      INTO d
      FROM biz_seq WHERE seq_type = 'RC' AND deleted_at IS NULL;
    IF d IS NULL THEN
        RAISE EXCEPTION 'V0292 自检失败：biz_seq(seq_type=RC) 取号规则行缺失';
    END IF;
    IF d <> 'RC|yyyyMMdd|-|1|1' THEN
        RAISE EXCEPTION 'V0292 自检失败：RC 取号规则口径不对（% ≠ RC|yyyyMMdd|-|1|1）', d;
    END IF;
    sample := 'RC-' || to_char(DATE '2026-11-06', 'YYYYMMDD') || '-' || lpad('1', 6, '0');
    IF sample !~ '^RC-[0-9]{8}-[0-9]{6}$' THEN
        RAISE EXCEPTION 'V0292 自检失败：按规则拼出的单号 % 不符合 RC-YYYYMMDD-NNNNNN', sample;
    END IF;

    RAISE NOTICE 'V0292 自检通过：biz_receipt 16 列 / biz_receipt_allocation 7 列 / 5 约束 / 9 索引 / 外键(CASCADE+RESTRICT) / RC 取号=%',
                 sample;
END $$;
