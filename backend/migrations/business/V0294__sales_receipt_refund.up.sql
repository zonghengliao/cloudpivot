-- ======================================================
-- V0294：销售链 —— 收款退款单 `biz_receipt_refund` + 退款核销明细 `biz_receipt_refund_allocation`
--            （SO-14 遗留卡 t_573bfe5b · 路线 A）
--
-- # 为什么要有这张表（口径决定，见 docs/evidence/20261106-so14-receipt-refund/README.md §一）
--
-- SO-14 遗留卡的边界：出库作废冲销应收只覆盖「一分没收的 OPEN 应收 → CANCELLED」。
-- 已收过款的应收（PARTIAL / SETTLED）作废出库单会被 409 拦住，因为钱已经进账，
-- 冲回必须有一张**反向的资金单据**。本卡选路线 A：新增「收款退款单」，
-- 资金流 借 1122 应收账款 / 贷 1002 银行存款（与 SO-17 收款凭证严格镜像）；
-- 退款核销把应收的 `received_amount` 冲减到 0 ⇒ 应收回到 `OPEN`，此后作废出库单
-- 就能走既有的 `OPEN → CANCELLED`（**CANCELLED 的语义不动**：一分没收）。
--
-- # 为什么不复用 `biz_receipt`（同表按 receipt_type 区分）
--
--   ① V0292 已应用（指纹不可改），同表要放宽 `ck_biz_receipt_amount` /
--      `ck_biz_rcpt_alloc_amount (amount > 0)` 才能表达反向金额 —— 改法是「再发一条迁移
--      DROP/ADD 既有约束」，等于给一张**正在交付中**（SO-17）的表换语义；
--   ② SO-17 的读侧（列表 / 详情 / 核销 / 过账）全部按「收款」假设写死了
--      （`SUM(allocation) = allocated_amount`、`status <> 'CANCELLED' OR allocated = 0`），
--      同表混进退款行会让退款出现在收款台账里、并可能被再次「核销到应收」；
--   ③ 负金额方案（路线 B 红字应收 / 负数核销）会污染 `ck_biz_ar_amount (amount > 0)`、
--      账龄取数、`ck_biz_ar_unsettled` 恒等式与报表 —— 卡面已判不推荐。
--   ⇒ **结构镜像、语义独立**：退款的 allocation.amount 存**正数**（语义「本次冲减多少已收款」），
--      方向由单据类型表达，不做负数（负号只会在 SUM / 对账 / 账龄里制造歧义）。
--
-- # 与 `biz_receipt_allocation` 的关系（复用算法，不复用表）
--
--   核销明细字段镜像（refund_id / receivable_id / amount / remark / created_at / create_by），
--   唯一键同为 `(单据, 应收)` ⇒ 「同一退款对同一应收只有一行，重复核销 = 改金额（UPSERT）」。
--   应收侧回写方向相反：`received_amount -= 本次冲减`（收款是 `+=`）。
--   恒等式随之为：`received_amount = SUM(收款核销) - SUM(退款冲减)`。
--
-- # 状态域（`ck_biz_receipt_refund_status`，4 值，与收款单同一套资金单据台账流）
--
--   DRAFT → CONFIRMED → POSTED（过账 ⇒ 生成退款凭证 借 1122 / 贷 1002）
--   DRAFT → CANCELLED（草稿作废；已核销的退款不许作废，库层 CHECK 兜底）
--   **POSTED 是终态**：钱已付出且凭证已记账，退款单不能作废 —— 要抹掉只能由客户
--   重新回一张收款单（那才是「钱回来了」），或者在凭证层红冲（voucher::reverse_posted）。
--   这条与收款单的「CONFIRMED → CANCELLED 也没有边」是同一套理由：资金单据过账即入账。
--
-- # 退款原因必填（`ck_biz_receipt_refund_reason`）
--
--   退款是**钱往外走**的动作，比收款的审计要求更高：`reason` 非空且去空白后 ≥ 4 字，
--   程序层（`validate_reason`）与库层双闸。收款单的 reason 只在作废时要求，这里是**建单就要**。
--
-- # 外键 / 弱引用口径（与 V0292 逐条对齐）
--
--   * `refund_id`     → biz_receipt_refund(id) **ON DELETE CASCADE**（头没了明细是孤儿）
--   * `receivable_id` → biz_receivable(id)      **ON DELETE RESTRICT**（核销留痕不许被硬删应收吞掉）
--   * `customer_id`   弱引用（无外键）：客户走软删（V0260），外键会把「客户软删但历史退款单要留」堵死
--   * `source_receipt_id` 弱引用（无外键）：指向「退的是哪一笔收款」，收款走软删且弱引用的历史留痕优先
--   * `voucher_id` 无外键（凭证由过账事务生成，反向依赖会让事务成环），与 biz_receivable / biz_receipt 同
--
-- # 索引（7 + 3）
--
--   biz_receipt_refund：pkey + uk_biz_receipt_refund_no（存活行唯一）+ idx_..._customer(customer_id, refund_date)
--                       + idx_..._status + idx_..._date + idx_..._voucher(部分) + idx_..._source_receipt(部分) ⇒ 7
--   biz_receipt_refund_allocation：pkey + uk_biz_rr_alloc_pair + idx_biz_rr_alloc_receivable ⇒ 3
--   ⚠ 索引名统一带 `receipt_refund` / `rr` 全词：PG 索引名 schema 级唯一，撞名 + IF NOT EXISTS 会被静默跳过。
--
-- # 幂等
--
--   CREATE TABLE / INDEX IF NOT EXISTS；biz_seq 种子 `WHERE NOT EXISTS`。
--   末尾 DO 块自检：表/列（18 + 7）/ 5 条 CHECK / 10 个索引 / 默认值 / 外键删除动作 /
--   状态与方式域值 / 20 条行为探针 / 取号规则，任一不符 → 迁移失败。
-- ======================================================

-- ---------- ① biz_receipt_refund（退款单头） ----------
CREATE TABLE IF NOT EXISTS biz_receipt_refund (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),      -- 主键
    refund_no         VARCHAR(32)   NOT NULL,                          -- 退款单号：RR-yyyyMMdd-000001（biz_seq 取号，存活行唯一）
    customer_id       UUID          NOT NULL,                          -- 客户（弱引用 biz_customer.id，无外键）
    source_receipt_id UUID,                                            -- 退的是哪一笔收款（弱引用 biz_receipt.id，可空）
    refund_date       DATE          NOT NULL,                          -- 退款日期（必填；凭证日期取它）
    amount            NUMERIC(18,4) NOT NULL,                          -- 退款金额（> 0；无默认值）
    allocated_amount  NUMERIC(18,4) NOT NULL DEFAULT 0,                -- 已冲减金额（>= 0 且 <= amount，由核销事务维护）
    method            VARCHAR(32),                                     -- 退款方式：BANK_TRANSFER / CASH / CHECK / OTHER
    bank_account      VARCHAR(64),                                     -- 付款账户
    status            VARCHAR(16)   NOT NULL DEFAULT 'DRAFT',          -- 状态机：DRAFT/CONFIRMED/POSTED/CANCELLED
    voucher_id        UUID,                                            -- 退款凭证 id（无外键，避免过账事务成环）
    reason            TEXT          NOT NULL,                          -- 退款原因（必填，去空白后 >= 4 字）
    remark            TEXT,                                            -- 备注
    created_at        TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 审计：创建时间
    updated_at        TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 审计：更新时间（model 层维护）
    create_by         UUID,                                            -- 审计：创建人
    update_by         UUID,                                            -- 审计：更新人
    deleted_at        TIMESTAMPTZ,                                     -- 软删时间（NULL = 存活）

    -- 状态域：与收款单同一套资金单据台账流（ARCH-7 §4.4）
    CONSTRAINT ck_biz_receipt_refund_status
        CHECK (status IN ('DRAFT','CONFIRMED','POSTED','CANCELLED')),
    -- 金额三连：> 0、已冲减 >= 0、已冲减不超过退款金额
    CONSTRAINT ck_biz_receipt_refund_amount
        CHECK (amount > 0 AND allocated_amount >= 0 AND allocated_amount <= amount),
    -- 退款方式白名单（NULL = 未选，合法）
    CONSTRAINT ck_biz_receipt_refund_method
        CHECK (method IS NULL OR method IN ('BANK_TRANSFER','CASH','CHECK','OTHER')),
    -- 作废 ⇔ 一分没核销（作废已核销的退款会让「应收已收」凭空变回来）
    CONSTRAINT ck_biz_receipt_refund_cancelled
        CHECK (status <> 'CANCELLED' OR allocated_amount = 0),
    -- 退款原因必填且至少 4 字（钱往外走，审计要能回答「为什么退」）
    CONSTRAINT ck_biz_receipt_refund_reason
        CHECK (length(btrim(reason)) >= 4)
);

COMMENT ON TABLE  biz_receipt_refund IS
    '收款退款单（销售链 · SO-14 遗留卡；状态 DRAFT/CONFIRMED/POSTED/CANCELLED）：借 1122 应收 / 贷 1002 银行存款';
COMMENT ON COLUMN biz_receipt_refund.refund_no IS '退款单号：RR-yyyyMMdd-000001（biz_seq seq_type=RR 取号，按 CST 的天滚动，存活行唯一）';
COMMENT ON COLUMN biz_receipt_refund.customer_id IS '客户 id（弱引用 biz_customer.id，无外键：客户软删后历史退款单仍要可读）';
COMMENT ON COLUMN biz_receipt_refund.source_receipt_id IS '来源收款单 id（弱引用 biz_receipt.id，可空）：审计「退的是哪一笔收款」，不参与核销计算';
COMMENT ON COLUMN biz_receipt_refund.amount IS '退款金额（> 0；刻意不给默认值：默认 0 会与 ck_biz_receipt_refund_amount 互相矛盾）';
COMMENT ON COLUMN biz_receipt_refund.allocated_amount IS '已冲减金额（>= 0 且 <= amount；= SUM(biz_receipt_refund_allocation.amount)，由核销事务维护）';
COMMENT ON COLUMN biz_receipt_refund.method IS '退款方式：BANK_TRANSFER / CASH / CHECK / OTHER（ck_biz_receipt_refund_method 白名单，NULL = 未选）';
COMMENT ON COLUMN biz_receipt_refund.status IS '状态机：DRAFT/CONFIRMED/POSTED/CANCELLED（POSTED 是终态：钱已付出且凭证已记账）';
COMMENT ON COLUMN biz_receipt_refund.voucher_id IS '退款过账凭证（借 1122 应收账款 / 贷 1002 银行存款；过账时写入）';
COMMENT ON COLUMN biz_receipt_refund.reason IS '退款原因（必填，去空白后 >= 4 字；ck_biz_receipt_refund_reason 兜底）';
COMMENT ON COLUMN biz_receipt_refund.deleted_at IS '软删标记；单号唯一索引带 WHERE deleted_at IS NULL ⇒ 软删后单号可复用';

-- 单号唯一：部分唯一索引（存活行唯一），与 uk_biz_receipt_no / uk_biz_ar_no 同口径
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_receipt_refund_no
    ON biz_receipt_refund (refund_no) WHERE deleted_at IS NULL;
-- 客户台账：按客户 + 日期筛（退款明细页 / 客户对账单）
CREATE INDEX IF NOT EXISTS idx_biz_receipt_refund_customer
    ON biz_receipt_refund (customer_id, refund_date);
CREATE INDEX IF NOT EXISTS idx_biz_receipt_refund_status ON biz_receipt_refund (status);
CREATE INDEX IF NOT EXISTS idx_biz_receipt_refund_date   ON biz_receipt_refund (refund_date);
-- 凭证回查（部分索引，空值不占位）
CREATE INDEX IF NOT EXISTS idx_biz_receipt_refund_voucher
    ON biz_receipt_refund (voucher_id) WHERE voucher_id IS NOT NULL;
-- 按来源收款单回查（「这笔收款被退过几次」）
CREATE INDEX IF NOT EXISTS idx_biz_receipt_refund_source_receipt
    ON biz_receipt_refund (source_receipt_id) WHERE source_receipt_id IS NOT NULL;

-- ---------- ② biz_receipt_refund_allocation（退款核销明细） ----------
CREATE TABLE IF NOT EXISTS biz_receipt_refund_allocation (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),        -- 主键
    refund_id     UUID          NOT NULL
                  REFERENCES biz_receipt_refund (id) ON DELETE CASCADE, -- 退款单（头删 → 明细级联删）
    receivable_id UUID          NOT NULL
                  REFERENCES biz_receivable (id) ON DELETE RESTRICT,    -- 应收单（**RESTRICT**：核销留痕不许被硬删吞掉）
    amount        NUMERIC(18,4) NOT NULL,                            -- 本次冲减的已收款（正数；语义见文件头）
    remark        VARCHAR(500),                                      -- 备注
    created_at    TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,   -- 审计：冲减时间
    create_by     UUID,                                              -- 审计：冲减人

    -- 冲减金额必须为正（方向由单据类型表达，不做负数）
    CONSTRAINT ck_biz_rr_alloc_amount CHECK (amount > 0)
);

COMMENT ON TABLE  biz_receipt_refund_allocation IS
    '退款核销明细（一张退款可冲减多张应收；方向与收款核销相反：received_amount -= amount）';
COMMENT ON COLUMN biz_receipt_refund_allocation.refund_id IS '退款单 id（ON DELETE CASCADE：退款头删除时明细一起走）';
COMMENT ON COLUMN biz_receipt_refund_allocation.receivable_id IS '应收单 id（ON DELETE RESTRICT：核销留痕不许被硬删应收静默吞掉；应收走软删）';
COMMENT ON COLUMN biz_receipt_refund_allocation.amount IS '本次冲减金额（> 0，正数）：应收 received_amount 减去它；同一 (refund_id, receivable_id) 只保留一行';

-- 一条退款对同一笔应收只许一行（重复核销 = UPSERT 改金额，不是插第二行）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_rr_alloc_pair
    ON biz_receipt_refund_allocation (refund_id, receivable_id);
-- 反查：某笔应收被哪些退款冲减过（应收详情「退款记录」/ 对账）
CREATE INDEX IF NOT EXISTS idx_biz_rr_alloc_receivable
    ON biz_receipt_refund_allocation (receivable_id);

-- ---------- ③ 取号规则：RR（退款单号） ----------
INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status, remark)
SELECT v.id::uuid, 'RR', '收款退款单号', 'RR', 'yyyyMMdd', '', 0, 1, '-', 1,
       '收款退款单号：RR-yyyyMMdd-000001，按天（CST）滚动（V0294 · SO-14 遗留卡）'
  FROM (VALUES ('d0294000-0000-4000-8000-000000000001')) AS v(id)
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq q WHERE q.seq_type = 'RR' AND q.deleted_at IS NULL);

-- ---------- ④ 自检：结构 + 域值 + 默认值 + 外键动作 + 20 条行为探针 + 取号 ----------
DO $$
DECLARE
    n         INT;
    d         TEXT;
    s         TEXT;
    sample    TEXT;
    probe_ar  UUID := 'd0294fff-0000-4000-8000-000000000001'::uuid;
    probe_ar2 UUID := 'd0294fff-0000-4000-8000-000000000002'::uuid;
    probe_rr  UUID := 'd0294fff-0000-4000-8000-000000000011'::uuid;
    probe_rr2 UUID := 'd0294fff-0000-4000-8000-000000000012'::uuid;
BEGIN
    -- ===== 4.1 两张表在位 =====
    IF to_regclass('public.biz_receipt_refund') IS NULL THEN
        RAISE EXCEPTION 'V0294 自检失败：biz_receipt_refund 未建';
    END IF;
    IF to_regclass('public.biz_receipt_refund_allocation') IS NULL THEN
        RAISE EXCEPTION 'V0294 自检失败：biz_receipt_refund_allocation 未建';
    END IF;

    -- ===== 4.2 列数 / 列名逐字（多一列少一列都算漂移）=====
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_receipt_refund';
    IF n <> 18 THEN
        RAISE EXCEPTION 'V0294 自检失败：biz_receipt_refund 列数 = %（期望 18）', n;
    END IF;
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_receipt_refund'
       AND column_name IN ('id','refund_no','customer_id','source_receipt_id','refund_date',
                           'amount','allocated_amount','method','bank_account','status',
                           'voucher_id','reason','remark','created_at','updated_at',
                           'create_by','update_by','deleted_at');
    IF n <> 18 THEN
        RAISE EXCEPTION 'V0294 自检失败：biz_receipt_refund 卡面列只齐了 %/18', n;
    END IF;

    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_receipt_refund_allocation';
    IF n <> 7 THEN
        RAISE EXCEPTION 'V0294 自检失败：biz_receipt_refund_allocation 列数 = %（期望 7）', n;
    END IF;
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_receipt_refund_allocation'
       AND column_name IN ('id','refund_id','receivable_id','amount','remark','created_at','create_by');
    IF n <> 7 THEN
        RAISE EXCEPTION 'V0294 自检失败：biz_receipt_refund_allocation 卡面列只齐了 %/7', n;
    END IF;

    -- ===== 4.3 金额列精度（NUMERIC(18,4)，与 receivable / receipt / voucher_entry 同口径）=====
    SELECT count(*) INTO n FROM information_schema.columns
     WHERE table_schema = 'public'
       AND ((table_name = 'biz_receipt_refund' AND column_name IN ('amount','allocated_amount'))
         OR (table_name = 'biz_receipt_refund_allocation' AND column_name = 'amount'))
       AND data_type = 'numeric' AND numeric_precision = 18 AND numeric_scale = 4;
    IF n <> 3 THEN
        RAISE EXCEPTION 'V0294 自检失败：三列金额的 NUMERIC(18,4) 只落地了 %/3', n;
    END IF;

    -- ===== 4.4 五条 CHECK 在位 =====
    SELECT count(*) INTO n FROM pg_constraint
     WHERE conrelid IN ('public.biz_receipt_refund'::regclass,
                        'public.biz_receipt_refund_allocation'::regclass)
       AND conname IN ('ck_biz_receipt_refund_status','ck_biz_receipt_refund_amount',
                       'ck_biz_receipt_refund_method','ck_biz_receipt_refund_cancelled',
                       'ck_biz_receipt_refund_reason','ck_biz_rr_alloc_amount');
    IF n <> 6 THEN
        RAISE EXCEPTION 'V0294 自检失败：本卡 6 条 CHECK 只落地了 %/6', n;
    END IF;
    -- 金额三连（PG 把数值字面量渲染成 `(0)::numeric`，按渲染后形态匹配）
    IF (SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = 'ck_biz_receipt_refund_amount')
       NOT LIKE '%amount > (0)%'
       OR (SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = 'ck_biz_receipt_refund_amount')
          NOT LIKE '%allocated_amount >= (0)%'
       OR (SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = 'ck_biz_receipt_refund_amount')
          NOT LIKE '%allocated_amount <= amount%' THEN
        RAISE EXCEPTION 'V0294 自检失败：ck_biz_receipt_refund_amount 不是金额三连（def=%）',
            (SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = 'ck_biz_receipt_refund_amount');
    END IF;
    IF (SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = 'ck_biz_rr_alloc_amount')
       NOT LIKE '%amount > (0)%' THEN
        RAISE EXCEPTION 'V0294 自检失败：ck_biz_rr_alloc_amount 不是 amount > 0（def=%）',
            (SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = 'ck_biz_rr_alloc_amount');
    END IF;

    -- ===== 4.5 状态域 4 值齐全，且不许混入应收/应付的域值 =====
    FOREACH s IN ARRAY ARRAY['DRAFT','CONFIRMED','POSTED','CANCELLED']
    LOOP
        IF (SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = 'ck_biz_receipt_refund_status')
           NOT LIKE '%' || s || '%' THEN
            RAISE EXCEPTION 'V0294 自检失败：ck_biz_receipt_refund_status 缺域值 %', s;
        END IF;
    END LOOP;
    FOREACH s IN ARRAY ARRAY['BANK_TRANSFER','CASH','CHECK','OTHER']
    LOOP
        IF (SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conname = 'ck_biz_receipt_refund_method')
           NOT LIKE '%' || s || '%' THEN
            RAISE EXCEPTION 'V0294 自检失败：ck_biz_receipt_refund_method 缺域值 %', s;
        END IF;
    END LOOP;

    -- ===== 4.6 索引：7 + 3 = 10，逐名核对（撞名被静默跳过会在这里炸）=====
    SELECT count(*) INTO n FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_receipt_refund';
    IF n <> 7 THEN
        RAISE EXCEPTION 'V0294 自检失败：biz_receipt_refund 索引应有 7 个，实际 %（撞名被静默跳过？）', n;
    END IF;
    SELECT count(*) INTO n FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_receipt_refund'
       AND indexname IN ('uk_biz_receipt_refund_no','idx_biz_receipt_refund_customer',
                         'idx_biz_receipt_refund_status','idx_biz_receipt_refund_date',
                         'idx_biz_receipt_refund_voucher','idx_biz_receipt_refund_source_receipt');
    IF n <> 6 THEN
        RAISE EXCEPTION 'V0294 自检失败：biz_receipt_refund 点名的 6 个二级索引只落地了 %/6', n;
    END IF;
    SELECT count(*) INTO n FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_receipt_refund_allocation';
    IF n <> 3 THEN
        RAISE EXCEPTION 'V0294 自检失败：biz_receipt_refund_allocation 索引应有 3 个，实际 %', n;
    END IF;
    SELECT indexdef INTO d FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_biz_receipt_refund_no';
    IF d NOT LIKE '%UNIQUE%' OR d NOT LIKE '%deleted_at IS NULL%' THEN
        RAISE EXCEPTION 'V0294 自检失败：uk_biz_receipt_refund_no 不是「存活行唯一」的部分索引（def=%）', d;
    END IF;
    SELECT indexdef INTO d FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_biz_rr_alloc_pair';
    IF d NOT LIKE '%UNIQUE%' OR d NOT LIKE '%refund_id, receivable_id%' THEN
        RAISE EXCEPTION 'V0294 自检失败：uk_biz_rr_alloc_pair 不是 (refund_id, receivable_id) 唯一（def=%）', d;
    END IF;

    -- ===== 4.7 默认值：status=DRAFT / allocated_amount=0；amount 必须**没有**默认值 =====
    SELECT column_default INTO d FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_receipt_refund' AND column_name = 'status';
    IF d IS NULL OR d NOT LIKE '''DRAFT''%' THEN
        RAISE EXCEPTION 'V0294 自检失败：status 默认值不是 DRAFT（default=%）', d;
    END IF;
    IF (SELECT column_default FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = 'biz_receipt_refund' AND column_name = 'allocated_amount')
       NOT LIKE '0%' THEN
        RAISE EXCEPTION 'V0294 自检失败：allocated_amount 默认值不是 0';
    END IF;
    IF (SELECT column_default FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = 'biz_receipt_refund' AND column_name = 'amount')
       IS NOT NULL THEN
        RAISE EXCEPTION 'V0294 自检失败：amount 不该有默认值（会与 ck_biz_receipt_refund_amount 互相矛盾）';
    END IF;
    -- reason NOT NULL
    IF (SELECT is_nullable FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = 'biz_receipt_refund' AND column_name = 'reason')
       <> 'NO' THEN
        RAISE EXCEPTION 'V0294 自检失败：reason 必须 NOT NULL（退款原因必填）';
    END IF;

    -- ===== 4.8 外键删除动作：refund_id=CASCADE('c') / receivable_id=RESTRICT('r') =====
    SELECT confdeltype INTO d FROM pg_constraint
     WHERE conname = 'biz_receipt_refund_allocation_refund_id_fkey';
    IF d IS DISTINCT FROM 'c' THEN
        RAISE EXCEPTION 'V0294 自检失败：refund_id 外键的 ON DELETE 不是 CASCADE（confdeltype=%）', d;
    END IF;
    SELECT confdeltype INTO d FROM pg_constraint
     WHERE conname = 'biz_receipt_refund_allocation_receivable_id_fkey';
    IF d IS DISTINCT FROM 'r' THEN
        RAISE EXCEPTION 'V0294 自检失败：receivable_id 外键的 ON DELETE 不是 RESTRICT（confdeltype=%，见文件头）', d;
    END IF;

    -- ================= 4.9 行为探针（坏数据必须被拒）=================

    -- 4.9a amount = 0 → 拒
    BEGIN
        INSERT INTO biz_receipt_refund (refund_no, customer_id, refund_date, amount, reason)
        VALUES ('RR-20261106-900001', gen_random_uuid(), DATE '2026-11-06', 0, '探针：金额为零');
        RAISE EXCEPTION 'V0294 自检失败：amount = 0 未被 ck_biz_receipt_refund_amount 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 4.9b amount 为负 → 拒
    BEGIN
        INSERT INTO biz_receipt_refund (refund_no, customer_id, refund_date, amount, reason)
        VALUES ('RR-20261106-900002', gen_random_uuid(), DATE '2026-11-06', -5, '探针：金额为负');
        RAISE EXCEPTION 'V0294 自检失败：amount = -5 未被 ck_biz_receipt_refund_amount 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 4.9c 已冲减 > 退款金额 → 拒
    BEGIN
        INSERT INTO biz_receipt_refund (refund_no, customer_id, refund_date, amount, allocated_amount, reason)
        VALUES ('RR-20261106-900003', gen_random_uuid(), DATE '2026-11-06', 100, 100.0001, '探针：超冲减');
        RAISE EXCEPTION 'V0294 自检失败：allocated_amount > amount 未被 ck_biz_receipt_refund_amount 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 4.9d 已冲减为负 → 拒
    BEGIN
        INSERT INTO biz_receipt_refund (refund_no, customer_id, refund_date, amount, allocated_amount, reason)
        VALUES ('RR-20261106-900004', gen_random_uuid(), DATE '2026-11-06', 100, -0.0001, '探针：负冲减');
        RAISE EXCEPTION 'V0294 自检失败：allocated_amount = -0.0001 未被 ck_biz_receipt_refund_amount 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 4.9e 状态越界（应收 OPEN / 应付 PAID）→ 拒
    BEGIN
        INSERT INTO biz_receipt_refund (refund_no, customer_id, refund_date, amount, status, reason)
        VALUES ('RR-20261106-900005', gen_random_uuid(), DATE '2026-11-06', 100, 'OPEN', '探针：抄应收域值');
        RAISE EXCEPTION 'V0294 自检失败：status = OPEN 未被 ck_biz_receipt_refund_status 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO biz_receipt_refund (refund_no, customer_id, refund_date, amount, status, reason)
        VALUES ('RR-20261106-900006', gen_random_uuid(), DATE '2026-11-06', 100, 'PAID', '探针：抄应付域值');
        RAISE EXCEPTION 'V0294 自检失败：status = PAID 未被 ck_biz_receipt_refund_status 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 4.9f 退款方式越界 → 拒
    BEGIN
        INSERT INTO biz_receipt_refund (refund_no, customer_id, refund_date, amount, method, reason)
        VALUES ('RR-20261106-900007', gen_random_uuid(), DATE '2026-11-06', 100, 'ALIPAY', '探针：非法方式');
        RAISE EXCEPTION 'V0294 自检失败：method = ALIPAY 未被 ck_biz_receipt_refund_method 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 4.9g 作废且已冲减过钱 → 拒
    BEGIN
        INSERT INTO biz_receipt_refund (refund_no, customer_id, refund_date, amount, allocated_amount, status, reason)
        VALUES ('RR-20261106-900008', gen_random_uuid(), DATE '2026-11-06', 100, 60, 'CANCELLED', '探针：作废已冲减');
        RAISE EXCEPTION 'V0294 自检失败：CANCELLED 且 allocated_amount = 60 未被 ck_biz_receipt_refund_cancelled 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 4.9h 退款原因太短 / 全空白 → 拒
    BEGIN
        INSERT INTO biz_receipt_refund (refund_no, customer_id, refund_date, amount, reason)
        VALUES ('RR-20261106-900009', gen_random_uuid(), DATE '2026-11-06', 100, '退');
        RAISE EXCEPTION 'V0294 自检失败：reason 只有 1 个字未被 ck_biz_receipt_refund_reason 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO biz_receipt_refund (refund_no, customer_id, refund_date, amount, reason)
        VALUES ('RR-20261106-900010', gen_random_uuid(), DATE '2026-11-06', 100, '    ');
        RAISE EXCEPTION 'V0294 自检失败：全空白的 reason 未被 ck_biz_receipt_refund_reason 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- ================= 4.10 正常路径 + 级联/约束行为 =================

    -- 4.10a 正常退款单：默认值一起过（DRAFT / 已冲减 0）
    INSERT INTO biz_receipt_refund (id, refund_no, customer_id, refund_date, amount, method,
                                    bank_account, reason)
    VALUES (probe_rr, 'RR-20261106-900101', gen_random_uuid(), DATE '2026-11-06', 1000.0000,
            'BANK_TRANSFER', '6222 0000 1111 2222', 'V0294 自检探针：客户退货退款');
    SELECT status || '|' || allocated_amount::text || '|' || amount::text INTO d
      FROM biz_receipt_refund WHERE id = probe_rr;
    IF d <> 'DRAFT|0.0000|1000.0000' THEN
        RAISE EXCEPTION 'V0294 自检失败：默认值落库 = %（期望 DRAFT|0.0000|1000.0000）', d;
    END IF;

    -- 4.10b 存活行重号 → 拒
    BEGIN
        INSERT INTO biz_receipt_refund (refund_no, customer_id, refund_date, amount, reason)
        VALUES ('RR-20261106-900101', gen_random_uuid(), DATE '2026-11-06', 10, '探针：重号');
        RAISE EXCEPTION 'V0294 自检失败：存活行重号未被 uk_biz_receipt_refund_no 拒绝';
    EXCEPTION WHEN unique_violation THEN NULL;
    END;

    -- 4.10c 软删后同号可复用（部分唯一索引语义）
    INSERT INTO biz_receipt_refund (refund_no, customer_id, refund_date, amount, reason)
    VALUES ('RR-20261106-900102', gen_random_uuid(), DATE '2026-11-06', 10, '探针：软删复用');
    UPDATE biz_receipt_refund SET deleted_at = now() WHERE refund_no = 'RR-20261106-900102';
    INSERT INTO biz_receipt_refund (refund_no, customer_id, refund_date, amount, reason)
    VALUES ('RR-20261106-900102', gen_random_uuid(), DATE '2026-11-06', 20, '探针：软删复用');
    SELECT count(*) INTO n FROM biz_receipt_refund
     WHERE refund_no = 'RR-20261106-900102' AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0294 自检失败：软删后同号重插后存活行 = %（期望 1）', n;
    END IF;

    -- 4.10d 精度：0.0002 不被截断（NUMERIC(18,4)）
    INSERT INTO biz_receipt_refund (refund_no, customer_id, refund_date, amount, reason)
    VALUES ('RR-20261106-900104', gen_random_uuid(), DATE '2026-11-06', 0.0002, '探针：精度');
    SELECT amount::text INTO d FROM biz_receipt_refund WHERE refund_no = 'RR-20261106-900104';
    IF d <> '0.0002' THEN
        RAISE EXCEPTION 'V0294 自检失败：amount 0.0002 落库变成 %（精度被截断）', d;
    END IF;

    -- 4.10e UPDATE 后超冲减 → 拒；合法值通过
    BEGIN
        UPDATE biz_receipt_refund SET allocated_amount = 1000.0001 WHERE id = probe_rr;
        RAISE EXCEPTION 'V0294 自检失败：UPDATE 把 allocated_amount 改到 > amount 未被拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    UPDATE biz_receipt_refund SET allocated_amount = 600.0000 WHERE id = probe_rr;
    SELECT allocated_amount::text INTO d FROM biz_receipt_refund WHERE id = probe_rr;
    IF d <> '600.0000' THEN
        RAISE EXCEPTION 'V0294 自检失败：allocated_amount 改为 600 落库 = %', d;
    END IF;

    -- 4.10f 备两笔应收（FK 目标）
    INSERT INTO biz_receivable (id, ar_no, customer_id, source_type, source_id, source_no,
                                amount, received_amount, unsettled_amount, credit_days,
                                period_code, due_date)
    VALUES (probe_ar, 'AR-20261106-900101', gen_random_uuid(), 'MANUAL', gen_random_uuid(), 'MANUAL',
            1000.0000, 600.0000, 400.0000, 30, '2026-11', DATE '2026-12-06'),
           (probe_ar2, 'AR-20261106-900102', gen_random_uuid(), 'MANUAL', gen_random_uuid(), 'MANUAL',
            500.0000, 500.0000, 0.0000, 30, '2026-11', DATE '2026-12-06');

    -- 4.10g 正常核销明细
    INSERT INTO biz_receipt_refund_allocation (refund_id, receivable_id, amount, remark)
    VALUES (probe_rr, probe_ar, 600.0000, 'V0294 自检探针');
    SELECT count(*) INTO n FROM biz_receipt_refund_allocation
     WHERE refund_id = probe_rr AND receivable_id = probe_ar AND amount = 600.0000;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0294 自检失败：核销明细未按预期落库';
    END IF;

    -- 4.10h 冲减金额 = 0 → 拒
    INSERT INTO biz_receipt_refund (id, refund_no, customer_id, refund_date, amount, reason)
    VALUES (probe_rr2, 'RR-20261106-900105', gen_random_uuid(), DATE '2026-11-06', 100, '探针：第二张退款');
    BEGIN
        INSERT INTO biz_receipt_refund_allocation (refund_id, receivable_id, amount)
        VALUES (probe_rr2, probe_ar2, 0);
        RAISE EXCEPTION 'V0294 自检失败：核销 amount = 0 未被 ck_biz_rr_alloc_amount 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 4.10i 冲减金额为负 → 拒（方向由单据表达，负数一律不合法）
    BEGIN
        INSERT INTO biz_receipt_refund_allocation (refund_id, receivable_id, amount)
        VALUES (probe_rr2, probe_ar2, -1);
        RAISE EXCEPTION 'V0294 自检失败：核销 amount = -1 未被 ck_biz_rr_alloc_amount 拒绝';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- 4.10j 同一 (退款, 应收) 第二行 → 拒（重复核销会重复计入 SUM）
    BEGIN
        INSERT INTO biz_receipt_refund_allocation (refund_id, receivable_id, amount)
        VALUES (probe_rr, probe_ar, 100);
        RAISE EXCEPTION 'V0294 自检失败：同一退款对同一应收的第二行未被 uk_biz_rr_alloc_pair 拒绝';
    EXCEPTION WHEN unique_violation THEN NULL;
    END;

    -- 4.10k 核销到不存在的应收 → 拒（外键在位）
    BEGIN
        INSERT INTO biz_receipt_refund_allocation (refund_id, receivable_id, amount)
        VALUES (probe_rr, gen_random_uuid(), 10);
        RAISE EXCEPTION 'V0294 自检失败：核销到不存在的应收未被外键拒绝';
    EXCEPTION WHEN foreign_key_violation THEN NULL;
    END;

    -- 4.10l 硬删「被退款冲减过的应收」→ 拒（RESTRICT 抛 restrict_violation 23001，不是 23503）
    BEGIN
        DELETE FROM biz_receivable WHERE id = probe_ar;
        RAISE EXCEPTION 'V0294 自检失败：被冲减的应收被硬删（receivable_id 应是 RESTRICT）';
    EXCEPTION WHEN restrict_violation OR foreign_key_violation THEN NULL;
    END;

    -- 4.10m 删退款头 → 明细级联消失（CASCADE 生效）
    DELETE FROM biz_receipt_refund WHERE id = probe_rr;
    SELECT count(*) INTO n FROM biz_receipt_refund_allocation WHERE refund_id = probe_rr;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0294 自检失败：删退款头后仍残留 % 行核销明细（CASCADE 未生效）', n;
    END IF;

    -- 4.10n 清场：探针行全部消失（先退款（已删）→ 再应收，顺序不能反）
    DELETE FROM biz_receipt_refund WHERE refund_no LIKE 'RR-20261106-9%';
    DELETE FROM biz_receivable WHERE ar_no LIKE 'AR-20261106-9%';
    SELECT count(*) INTO n FROM biz_receipt_refund WHERE refund_no LIKE 'RR-20261106-9%';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0294 自检失败：biz_receipt_refund 探针残留 % 行', n;
    END IF;
    SELECT count(*) INTO n FROM biz_receipt_refund_allocation;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0294 自检失败：biz_receipt_refund_allocation 残留 % 行', n;
    END IF;
    SELECT count(*) INTO n FROM biz_receivable WHERE ar_no LIKE 'AR-20261106-9%';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0294 自检失败：biz_receivable 探针残留 % 行', n;
    END IF;

    -- ===== 4.11 取号规则：RR 行在位、口径正确、能拼出 RR-YYYYMMDD-NNNNNN =====
    SELECT prefix || '|' || date_pattern || '|' || separator || '|' || step || '|' || status
      INTO d
      FROM biz_seq WHERE seq_type = 'RR' AND deleted_at IS NULL;
    IF d IS NULL THEN
        RAISE EXCEPTION 'V0294 自检失败：biz_seq(seq_type=RR) 取号规则行缺失';
    END IF;
    IF d <> 'RR|yyyyMMdd|-|1|1' THEN
        RAISE EXCEPTION 'V0294 自检失败：RR 取号规则口径不对（% ≠ RR|yyyyMMdd|-|1|1）', d;
    END IF;
    sample := 'RR-' || to_char(DATE '2026-11-06', 'YYYYMMDD') || '-' || lpad('1', 6, '0');
    IF sample !~ '^RR-[0-9]{8}-[0-9]{6}$' THEN
        RAISE EXCEPTION 'V0294 自检失败：按规则拼出的单号 % 不符合 RR-YYYYMMDD-NNNNNN', sample;
    END IF;

    RAISE NOTICE 'V0294 自检通过：biz_receipt_refund 18 列 / biz_receipt_refund_allocation 7 列 / 6 约束 / 10 索引 / 外键(CASCADE+RESTRICT) / RR 取号=%',
                 sample;
END $$;
