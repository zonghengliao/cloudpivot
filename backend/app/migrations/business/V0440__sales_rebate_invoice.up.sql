-- ============================================================================
-- V0440：销售返利 —— 生成红字发票（M3-17）
--
--   ① biz_sales_rebate_settlement 加 5 列（红字发票 / 凭证 / 转结时间）
--      + 2 个索引（`uk_biz_srs_invoice` 一单一张票 / `idx_biz_srs_invoice`）
--      + 1 条 CHECK（`ck_biz_srs_settled_link`：SETTLED 必须有票有凭证有转结时间）
--   ② 科目种子 `2241.02 其他应付款-销售返利`（末级；返利欠客户的落地科目）
--   ③ 放宽 `biz_invoice.ck_biz_invoice_source`：来源类型加 `SALES_REBATE`
--   ④ sys_menu 一个按钮码（挂「返利政策」页节点 …4380 下，sort 70）
--
-- ── 本卡做什么（一句话）──────────────────────────────────────────────────
--   把一张 **DRAFT** 的返利结算单，转成一张 **销项红字发票（OUT_RED）** 并生成
--   红字凭证，结算单置 `SETTLED`。这是卡面「生成应付或红字发票 **二选一**」的
--   **红字发票**那一支（取舍见下）。
--
-- ── 为什么选「红字发票」而不是「应付单」──────────────────────────────────
--   返利在会计上是**销售折让**：冲减收入与销项税，同时形成欠客户的负债。
--     · 本仓有 `biz_invoice`（V0298）已经把红字票做成一等公民：`invoice_type = 'OUT_RED'`
--       + `ck_biz_invoice_sign`（amount < 0 且 tax_amount ≤ 0）+ 退货过账（`sales_return_posting`）
--       已有「程序造 OUT_RED + 收入冲减凭证」的成熟路径可复用；
--     · 本仓**没有**「对客户的应付」表：`biz_payable`（V0148）的列是
--       `supplier_id / invoice_no / due_date`，语义是「应付账款-供应商」——把客户填进
--       supplier_id 是拿一张采购台账记客户返利，对账、账龄、付款审批全会认错对象；
--       新造一张 `biz_rebate_payable` 则等于在 M3-17 里再立一套付款台账（超范围）。
--   ⇒ 选红字发票：**账在 GL 里落地**（贷 `2241.02 其他应付款-销售返利`），票在 `biz_invoice`，
--      不再多一张只此一用的单据表。
--
-- ── 金额口径（税率是百分数，FIX-6 / V0327）──────────────────────────────
--   结算单的 `rebate_amount` 是**含税**金额（它由「本位币含税净销售额 × 比率」算出，
--   与基数同口径），因此红字发票按含税价反算：
--
--     价税合计 total_with_tax = −rebate_amount                      （负）
--     不含税   amount         = −round(rebate_amount / (1 + rate/100), 2)   （负）
--     税额     tax_amount     = total_with_tax − amount             （≤ 0）
--     税率     tax_rate       = 13.0000（百分数：13 = 13%；接口可覆盖，0 ≤ r ≤ 100）
--
--   反算而不是「按不含税开票再单加税」：返利金额是老板/销售口径谈出来的**实际到手**数，
--   开票时把它当含税额拆出去，客户拿到的价税合计正好等于谈定的返利。
--   `ck_biz_invoice_amount`（total = amount + tax）与 `ck_biz_invoice_sign`
--   （OUT_RED 三列全 ≤ 0 且 amount < 0）在库层兜底，拆错一位都写不进去。
--
-- ── 凭证（同一事务，`voucher::post_voucher`，`source_type = 'INVOICE'`）────
--   借 5001    主营业务收入      |amount|        （冲减收入）
--   借 2221.02 应交税费-销项税    |tax|           （冲减销项税；tax = 0 时不出这一行）
--   贷 2241.02 其他应付款-销售返利 |total_with_tax|（欠客户的返利负债）
--
--   贷方**不是** 1122 应收账款：返利通常以现款 / 抵扣下期货款方式结清，不冲某张具体应收；
--   挂「其他应付款-销售返利」才能让财务一眼看出「还欠客户多少返利」。
--   `2241` 在 V0226 已翻成非末级（仅汇总），所以本卡种一个末级子科目 `2241.02`
--   （会计引擎 `account_required` 只允许末级科目记账）。
--
-- ── 状态机（结算单 3 值，RED 是票不是结算状态）──────────────────────────
--     DRAFT ──开红字发票──▶ SETTLED     （只此一条写路径；SETTLED 不可重算、不可作废）
--     DRAFT ──作废────────▶ CANCELLED
--   结算单**不新增状态**：票已经开了就是 `SETTLED`，「票被作废」是票的事
--   （`biz_invoice.status = VOID`），结算单不跟着回退 —— 那属于红冲 / 调账业务，
--   真要重来只能是红冲票 + 重新结算，不是悄悄把结算单改回 DRAFT。
--
-- ── 三条不变量（库层能表达的都表达）──────────────────────────────────────
--   1) `uk_biz_srs_invoice (invoice_id) WHERE invoice_id IS NOT NULL AND deleted_at IS NULL`
--      ⇒ 一张红字发票不可能被两张结算单认领（重复认领 = 返利付两遍）。
--   2) `ck_biz_srs_settled_link`：`status = 'SETTLED'` ⇒ `invoice_id` / `invoice_no` /
--      `settled_at` 三者都非空。防「标记已开票但其实没票」的死状态（约定 11：不许留死单）。
--   3) `ck_biz_invoice_source` 加 `SALES_REBATE`：来源类型白名单不许出现「野值」，
--      报表按 `source_type` 归集返利发票时不会漏。
--
-- ── 幂等 / 回滚 ─────────────────────────────────────────────────────────
--   `ADD COLUMN IF NOT EXISTS` / `CREATE INDEX IF NOT EXISTS` / 约束先查 `pg_constraint`
--   再建 / 菜单 `ON CONFLICT (id) DO NOTHING` / 科目 `WHERE NOT EXISTS`。
--   自检块可反复执行（探针用固定 id 且自带清理）。
--   down：护栏（**已有结算单开过红字发票时拒绝回滚** —— 删列等于把票与结算单的关联
--        从系统里抹掉，票还在 GL 里，对不上账）→ 删菜单 → 还原 ck_biz_invoice_source →
--        删科目（有凭证在用时拒绝）→ 删索引 / 约束 / 5 列。
--
-- ── 号段（开工实测复核）──────────────────────────────────────────────────
--   写入本文件时 `backend/app/migrations/**` 最大号 = V0439（sales_rebate_settlement，
--   M3-16 提交 ce2d1a93）。并行卡会抢号，**提交前**再
--   `ls backend/app/migrations/*/ | grep -oE 'V[0-9]{4}' | sort -u | tail -3` 复核
--   （跨目录全局唯一，重号 = 服务启动直接失败）。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 前置护栏：依赖表 / 父页面节点 / 码未被重复占用 / 没有「已 SETTLED 但没票」的存量行
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n_base   integer;
    n_parent integer;
    n_code   integer;
    n_bad    integer;
BEGIN
    SELECT count(*) INTO n_base
      FROM pg_class c JOIN pg_namespace ns ON ns.oid = c.relnamespace
     WHERE ns.nspname = current_schema() AND c.relkind = 'r'
       AND c.relname IN ('biz_sales_rebate_settlement', 'biz_invoice', 'biz_account',
                         'biz_voucher', 'biz_voucher_entry', 'sys_menu', 'sys_role_menu', 'biz_seq');
    IF n_base <> 8 THEN
        RAISE EXCEPTION 'V0440: 前置表不全（实得 % 张 / 期望 8：返利结算 / 销售发票 / 科目 / 凭证两表 / 菜单两表 / 号段），请先应用 V0149 / V0183 / V0298 / V0439', n_base;
    END IF;

    SELECT count(*) INTO n_parent
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004380'::uuid
       AND menu_type = 2
       AND deleted_at IS NULL;
    IF n_parent <> 1 THEN
        RAISE EXCEPTION 'V0440: 父页面节点「返利政策」(…4380) 不存在或不是页面节点，请先应用 V0438';
    END IF;

    SELECT count(*) INTO n_code
      FROM sys_menu
     WHERE auth_code = 'business:erp:rebate-settlement:invoice'
       AND deleted_at IS NULL;
    IF n_code > 1 THEN
        RAISE EXCEPTION 'V0440: 开红字发票的码已有 % 个承载节点（重放时 ≤ 1）', n_code;
    END IF;

    -- 存量行自洽：M3-16 只写 DRAFT / CANCELLED，正常应为 0（有则说明被人手改过）
    SELECT count(*) INTO n_bad
      FROM biz_sales_rebate_settlement
     WHERE status = 'SETTLED';
    IF n_bad > 0 THEN
        RAISE EXCEPTION 'V0440: 已有 % 张结算单是 SETTLED，但本迁移之前没有任何开票路径 —— 数据被人手改过，请先核对', n_bad;
    END IF;

    RAISE NOTICE 'V0440: 前置检查通过（8 张依赖表在、返利政策页面节点在、开票码当前 % 个承载节点、SETTLED 存量 0 行）', n_code;
END $$;

-- ---------------------------------------------------------------------------
-- ② 结算单：红字发票 / 凭证 / 转结 5 列 + 2 索引 + 1 CHECK
-- ---------------------------------------------------------------------------
ALTER TABLE biz_sales_rebate_settlement
    ADD COLUMN IF NOT EXISTS invoice_id   UUID,           -- 红字发票 id（弱引用 biz_invoice.id）
    ADD COLUMN IF NOT EXISTS invoice_no   VARCHAR(64),    -- 红字发票号快照（列表直接显示，免 JOIN）
    ADD COLUMN IF NOT EXISTS voucher_id   UUID,           -- 红字凭证 id（弱引用 biz_voucher.id）
    ADD COLUMN IF NOT EXISTS settled_at   TIMESTAMPTZ,    -- 转结时间（置 SETTLED 的同一刻）
    ADD COLUMN IF NOT EXISTS settled_by   UUID;           -- 转结人（弱引用 sys_user.id）

COMMENT ON COLUMN biz_sales_rebate_settlement.invoice_id IS '红字发票 id（弱引用 biz_invoice.id；OUT_RED）。SETTLED 必有票，见 ck_biz_srs_settled_link';
COMMENT ON COLUMN biz_sales_rebate_settlement.invoice_no IS '红字发票号快照（IV-yyyyMMdd-000001）；发票被改名/作废后仍能按当时号回溯';
COMMENT ON COLUMN biz_sales_rebate_settlement.voucher_id IS '红字凭证 id（弱引用 biz_voucher.id；冲减收入 + 销项税，贷其他应付款-销售返利）';
COMMENT ON COLUMN biz_sales_rebate_settlement.settled_at IS '转结时间（结算单据此时间已转红字发票；只写一次）';
COMMENT ON COLUMN biz_sales_rebate_settlement.settled_by IS '转结人（弱引用 sys_user.id；审计用，不 join 取实时名 —— 人名历史归审计口径）';

-- 一单一张票（存活行唯一）；同一张票不能被两张结算单认领 = 返利付两遍
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_srs_invoice
    ON biz_sales_rebate_settlement (invoice_id)
 WHERE invoice_id IS NOT NULL AND deleted_at IS NULL;

-- 反查：按发票查结算单（红冲 / 对账用）
CREATE INDEX IF NOT EXISTS idx_biz_srs_invoice
    ON biz_sales_rebate_settlement (invoice_id)
 WHERE invoice_id IS NOT NULL;

-- SETTLED ⇒ 有票、有票号、有转结时间（防「标记已开票但其实没票」的死状态）
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conrelid = 'biz_sales_rebate_settlement'::regclass
           AND conname  = 'ck_biz_srs_settled_link'
    ) THEN
        ALTER TABLE biz_sales_rebate_settlement
            ADD CONSTRAINT ck_biz_srs_settled_link
            CHECK (status <> 'SETTLED'
                   OR (invoice_id IS NOT NULL AND invoice_no IS NOT NULL AND settled_at IS NOT NULL));
    END IF;
END $$;

-- ---------------------------------------------------------------------------
-- ③ 科目种子：`2241.02 其他应付款-销售返利`（末级；2241 在 V0226 已翻成非末级）
-- ---------------------------------------------------------------------------
INSERT INTO biz_account (account_code, account_name, parent_id, level, account_category,
                         balance_direction, is_leaf, status, aux_dimensions, remark)
SELECT '2241.02', '其他应付款-销售返利',
       (SELECT id FROM biz_account WHERE account_code = '2241' AND deleted_at IS NULL),
       2, 'LIABILITY', 'CREDIT', 1, 1, NULL,
       'M3-17：销售返利开红字发票的贷方（欠客户的返利负债）；返利付款时借记本科目'
 WHERE NOT EXISTS (SELECT 1 FROM biz_account WHERE account_code = '2241.02')
   AND EXISTS (SELECT 1 FROM biz_account WHERE account_code = '2241' AND deleted_at IS NULL);

-- ---------------------------------------------------------------------------
-- ④ 放宽 biz_invoice 的来源类型白名单：加 `SALES_REBATE`
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    definition text;
BEGIN
    SELECT pg_get_constraintdef(oid) INTO definition
      FROM pg_constraint
     WHERE conrelid = 'biz_invoice'::regclass
       AND conname  = 'ck_biz_invoice_source';

    IF definition IS NULL THEN
        ALTER TABLE biz_invoice
            ADD CONSTRAINT ck_biz_invoice_source
            CHECK (source_type IS NULL
                OR source_type IN ('SALES_ORDER','SHIPMENT','SALES_OUT','MANUAL',
                                   'SALES_RETURN','SALES_REBATE'));
    ELSIF position('SALES_REBATE' IN definition) = 0 THEN
        ALTER TABLE biz_invoice DROP CONSTRAINT ck_biz_invoice_source;
        ALTER TABLE biz_invoice
            ADD CONSTRAINT ck_biz_invoice_source
            CHECK (source_type IS NULL
                OR source_type IN ('SALES_ORDER','SHIPMENT','SALES_OUT','MANUAL',
                                   'SALES_RETURN','SALES_REBATE'));
    END IF;
END $$;

COMMENT ON COLUMN biz_invoice.source_type IS
    '来源类型：SALES_ORDER / SHIPMENT / SALES_OUT / MANUAL / SALES_RETURN / SALES_REBATE（V0440 加返利）';

-- ---------------------------------------------------------------------------
-- ⑤ sys_menu：开红字发票的按钮码（挂「返利政策」页节点 …4380 下，sort 70）
-- ---------------------------------------------------------------------------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000004393'::uuid,
     'business:erp:rebate-settlement:invoice', 3,
     'business:erp:rebate-settlement:invoice', NULL,
     '20000000-0000-0000-0000-000000004380'::uuid, NULL,
     70, 1, NULL, '开红字发票（试算票面 + 生成 OUT_RED 票与红字凭证，结算单转「已开红字发票」）',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------------------------------------------------------------------------
-- ⑥ 自检（任一不符即 RAISE EXCEPTION 回滚整个迁移）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n          integer;
    v_def      text;
    probe_s    uuid := 'd0440000-0000-4000-8000-000000000001'::uuid;
    probe_i    uuid := 'd0440000-0000-4000-8000-000000000002'::uuid;
    v_cus      uuid := 'd0440000-0000-4000-8000-0000000000c1'::uuid;
    v_policy   uuid := 'd0440000-0000-4000-8000-0000000000b1'::uuid;
    v_inv      uuid := 'd0440000-0000-4000-8000-0000000000a1'::uuid;
    v_account  uuid;
    v_menu     uuid;
BEGIN
    -- ===== 6.1 表列：35 + 5 = 40 列；5 个新列的类型/可空性逐字 =====
    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = current_schema() AND table_name = 'biz_sales_rebate_settlement';
    IF n <> 40 THEN
        RAISE EXCEPTION 'V0440 自检失败：biz_sales_rebate_settlement 应有 40 列（35 + 5），实得 %', n;
    END IF;

    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = current_schema() AND table_name = 'biz_sales_rebate_settlement'
       AND (
             (column_name = 'invoice_id' AND data_type = 'uuid' AND is_nullable = 'YES')
          OR (column_name = 'invoice_no' AND data_type = 'character varying' AND is_nullable = 'YES')
          OR (column_name = 'voucher_id' AND data_type = 'uuid' AND is_nullable = 'YES')
          OR (column_name = 'settled_at' AND data_type = 'timestamp with time zone' AND is_nullable = 'YES')
          OR (column_name = 'settled_by' AND data_type = 'uuid' AND is_nullable = 'YES')
           );
    IF n <> 5 THEN
        RAISE EXCEPTION 'V0440 自检失败：5 个新列的类型/可空性不符（实得 % 列匹配）', n;
    END IF;

    -- ===== 6.2 两个索引在 =====
    SELECT count(*) INTO n
      FROM pg_indexes
     WHERE schemaname = current_schema() AND tablename = 'biz_sales_rebate_settlement'
       AND indexname IN ('uk_biz_srs_invoice', 'idx_biz_srs_invoice');
    IF n <> 2 THEN
        RAISE EXCEPTION 'V0440 自检失败：uk_biz_srs_invoice / idx_biz_srs_invoice 应全在（实得 % 个）', n;
    END IF;

    -- ===== 6.3 ck_biz_srs_settled_link 在 =====
    SELECT count(*) INTO n
      FROM pg_constraint
     WHERE conrelid = 'biz_sales_rebate_settlement'::regclass
       AND conname  = 'ck_biz_srs_settled_link'
       AND contype  = 'c';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0440 自检失败：ck_biz_srs_settled_link 缺失';
    END IF;

    -- ===== 6.4 ck_biz_invoice_source 含 SALES_REBATE =====
    SELECT pg_get_constraintdef(oid) INTO v_def
      FROM pg_constraint
     WHERE conrelid = 'biz_invoice'::regclass AND conname = 'ck_biz_invoice_source';
    IF v_def IS NULL OR position('SALES_REBATE' IN v_def) = 0 THEN
        RAISE EXCEPTION 'V0440 自检失败：ck_biz_invoice_source 未含 SALES_REBATE（%），返利发票会被库层拒绝', coalesce(v_def, 'NULL');
    END IF;

    -- ===== 6.5 科目 2241.02：在、末级、父为 2241、负债/贷方 =====
    SELECT a.id INTO v_account
      FROM biz_account a
      JOIN biz_account p ON p.id = a.parent_id
     WHERE a.account_code = '2241.02' AND a.deleted_at IS NULL
       AND a.is_leaf = 1 AND a.account_category = 'LIABILITY' AND a.balance_direction = 'CREDIT'
       AND p.account_code = '2241';
    IF v_account IS NULL THEN
        RAISE EXCEPTION 'V0440 自检失败：科目 2241.02（其他应付款-销售返利）不存在或不是 2241 下的末级负债科目';
    END IF;

    -- ===== 6.6 菜单行：在、type 3、父 …4380、sort 70、同父 sort 不重复 =====
    SELECT id INTO v_menu
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000004393'::uuid
       AND parent_id = '20000000-0000-0000-0000-000000004380'::uuid
       AND menu_type = 3 AND sort_order = 70 AND status = 1 AND deleted_at IS NULL
       AND auth_code = 'business:erp:rebate-settlement:invoice';
    IF v_menu IS NULL THEN
        RAISE EXCEPTION 'V0440 自检失败：开红字发票的按钮行（…4393）不存在或字段不符';
    END IF;

    SELECT count(*) INTO n
      FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000004380'::uuid
       AND sort_order = 70 AND deleted_at IS NULL;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0440 自检失败：父节点 …4380 下 sort_order=70 有 % 行（必须唯一，重复会让侧栏顺序漂移）', n;
    END IF;

    -- ===== 6.7 行为探针一：SETTLED 没票 → 必须被 ck_biz_srs_settled_link 拒绝 =====
    DELETE FROM biz_sales_rebate_settlement WHERE id = probe_s;
    INSERT INTO biz_sales_rebate_settlement
        (id, settlement_no, customer_id, settle_period, period_code, period_from, period_to,
         policy_id, policy_no, policy_name, rule_type, rate_unit, policy_effective_from,
         policy_snapshot, rebate_amount, status)
    VALUES
        (probe_s, 'RB-19990101-044001', v_cus, 'MONTH', '1999-01',
         DATE '1999-01-01', DATE '1999-02-01',
         v_policy, 'BR-19990101-044001', 'V0440 自检探针', 'BY_AMOUNT', 'PERCENT',
         DATE '1999-01-01', '{"tiers":[]}'::jsonb, 1000, 'DRAFT');

    BEGIN
        UPDATE biz_sales_rebate_settlement SET status = 'SETTLED' WHERE id = probe_s;
        RAISE EXCEPTION 'V0440 自检失败：SETTLED 无票竟然写进去了（ck_biz_srs_settled_link 失效）';
    EXCEPTION
        WHEN check_violation THEN
            NULL;   -- 期望路径
    END;

    -- ===== 6.8 行为探针二：补齐票号与转结时间后 SETTLED 允许（同一事务内回滚）=====
    UPDATE biz_sales_rebate_settlement
       SET status = 'SETTLED', invoice_id = v_inv, invoice_no = 'IV-19990101-044001',
           settled_at = CURRENT_TIMESTAMP
     WHERE id = probe_s;

    -- ===== 6.9 行为探针三：同一张票不能被第二张结算单认领（uk_biz_srs_invoice）=====
    BEGIN
        INSERT INTO biz_sales_rebate_settlement
            (id, settlement_no, customer_id, settle_period, period_code, period_from, period_to,
             policy_id, policy_no, policy_name, rule_type, rate_unit, policy_effective_from,
             policy_snapshot, rebate_amount, status, invoice_id, invoice_no, settled_at)
        VALUES
            (gen_random_uuid(), 'RB-19990101-044003', gen_random_uuid(), 'MONTH', '1999-03',
             DATE '1999-03-01', DATE '1999-04-01',
             v_policy, 'BR-19990101-044002', 'V0440 自检探针', 'BY_AMOUNT', 'PERCENT',
             DATE '1999-01-01', '{"tiers":[]}'::jsonb, 1000, 'SETTLED',
             v_inv, 'IV-19990101-044002', CURRENT_TIMESTAMP);
        RAISE EXCEPTION 'V0440 自检失败：同一张红字发票被第二张结算单认领竟然成功（uk_biz_srs_invoice 失效）';
    EXCEPTION
        WHEN unique_violation THEN
            NULL;   -- 期望路径
    END;

    -- ===== 6.10 行为探针四：biz_invoice 接受 SALES_REBATE、拒绝野值 =====
    DELETE FROM biz_invoice WHERE id = probe_i;
    INSERT INTO biz_invoice
        (id, invoice_no, invoice_type, customer_id, invoice_date,
         amount, tax_amount, total_with_tax, tax_rate, tax_rate_mode, status,
         source_type, source_no, currency_code, exchange_rate,
         base_total_amount, base_total_tax, base_total_with_tax)
    VALUES
        (probe_i, 'IV-19990101-044099', 'OUT_RED', v_cus, DATE '1999-01-31',
         -884.96, -115.04, -1000, 13, 'PERCENT', 'ISSUED',
         'SALES_REBATE', 'RB-19990101-044001', 'CNY', 1,
         -884.96, -115.04, -1000);

    BEGIN
        INSERT INTO biz_invoice
            (id, invoice_no, invoice_type, customer_id, invoice_date,
             amount, tax_amount, total_with_tax, tax_rate, status, source_type)
        VALUES
            (gen_random_uuid(), 'IV-19990101-044098', 'OUT_RED', v_cus, DATE '1999-01-31',
             -100, -13, -113, 13, 'DRAFT', 'SALES_REBATE_BOGUS');
        RAISE EXCEPTION 'V0440 自检失败：来源类型野值竟然写进了 biz_invoice（ck_biz_invoice_source 失效）';
    EXCEPTION
        WHEN check_violation THEN
            NULL;   -- 期望路径
    END;

    -- 探针清理（迁移本身要幂等：重放时不留脏数据）
    DELETE FROM biz_sales_rebate_settlement WHERE id = probe_s;
    DELETE FROM biz_invoice WHERE id = probe_i;

    RAISE NOTICE 'V0440 自检通过：40 列 / 2 索引 / ck_biz_srs_settled_link / 来源白名单含 SALES_REBATE / 科目 2241.02 / 按钮行（sort 70）/ 4 条行为探针';
END $$;
