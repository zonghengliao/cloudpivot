-- P9 / D1：已过账收货单缺凭证 —— 补生成凭证 + 应付单
--
-- 单据：IN-20261031-000003（POSTED，2026-10-31 01:50:33 过账，total 200 / tax 26 / with_tax 226）
--
-- 归因（查清了，不是猜的）：
--   * 该行 voucher_id / payable_id **两个都是 NULL**；
--   * 同批的 IN-000004（02:08:18 过账）两个都填上了，凭证号 PZ-20261031-000001 也正好是 02:08 那一批；
--   * 凭证 + 应付是 P2-C1（t_4fc44801，02:01 完成）才落地的能力 ⇒ 01:50 那次过账时，
--     运行的二进制还没有「过账即生成凭证/应付」的代码。
--   ⇒ 结论：**历史数据缺口（情形 A）**，不是代码静默跳过。代码侧 create_auto 找不到科目映射会
--     直接报错整单回滚（无静默分支），单测 cancel_refuses_when_posted_txn_missing 也钉住了
--     「缺数据必须报错」这一侧。
--
-- 修法：按 P2-C1 的同一口径补一张凭证（借 1405 不含税 / 借 2221.01 进项税 / 贷 2202 价税合计）
--       **和**一张未付应付单（这两件事是同一批代码生成的，少补哪个都会让 B3 失衡）。
--
-- 口径还原（与 voucher::create_auto / payable::create_for_stock_in 逐字对齐）：
--   summary   = '采购入库 {in_no} 价税合计 {total_with_tax}'
--   分录 1..3 = 存货（aux 物料+仓库）/ 进项税额（金额 0 则不出行）/ 应付（aux 供应商）
--   voucher_no / payable_no 走 biz_seq（PZ / AP），取号后同步推进 current_no，避免和线上撞号
--
-- 幂等：已有凭证（source_id 命中）→ NOTICE 跳过。
DO $$
DECLARE
    r            RECORD;
    acct_inv     uuid;
    acct_tax     uuid;
    acct_ap      uuid;
    first_mat    uuid;
    summary      text;
    v_seq        bigint;
    p_seq        bigint;
    v_no         text;
    p_no         text;
    v_id         uuid;
    p_id         uuid;
    sup_days     integer;  /* 供应商账期（不要叫 credit_days：与列名 ambiguous） */
    due          date;
    acct_period  text;  -- 会计期间（变量名不要叫 period_code：会与列名 ambiguous）
    -- 号段周期键（北京时间 yyyyMMdd）。**不要**叫 period_key：会和 biz_seq.period_key 列名撞车。
    today_key    text := to_char(now() AT TIME ZONE 'Asia/Shanghai', 'YYYYMMDD');
BEGIN
    SELECT * INTO r
      FROM biz_stock_in
     WHERE in_no = 'IN-20261031-000003' AND deleted_at IS NULL;

    IF r.id IS NULL THEN
        RAISE NOTICE 'P9-D1: 找不到 IN-20261031-000003，跳过';
        RETURN;
    END IF;
    IF EXISTS (SELECT 1 FROM biz_voucher v WHERE v.source_id = r.id AND v.deleted_at IS NULL) THEN
        RAISE NOTICE 'P9-D1: 该单已有凭证，跳过（幂等）';
        RETURN;
    END IF;
    IF r.status <> 'POSTED' THEN
        RAISE NOTICE 'P9-D1: 该单状态 = %（不是 POSTED），不需要凭证，跳过', r.status;
        RETURN;
    END IF;

    SELECT id INTO acct_inv FROM biz_account WHERE account_code = '1405'    AND deleted_at IS NULL AND status = 1;
    SELECT id INTO acct_tax FROM biz_account WHERE account_code = '2221.01' AND deleted_at IS NULL AND status = 1;
    SELECT id INTO acct_ap  FROM biz_account WHERE account_code = '2202'    AND deleted_at IS NULL AND status = 1;
    IF acct_inv IS NULL OR acct_ap IS NULL THEN
        RAISE EXCEPTION 'P9-D1: 科目 1405 / 2202 缺失，不能补凭证（先把科目主数据补齐）';
    END IF;
    IF r.total_tax > 0 AND acct_tax IS NULL THEN
        RAISE EXCEPTION 'P9-D1: 有进项税 % 但科目 2221.01 缺失，不能补凭证', r.total_tax;
    END IF;
    -- 借贷必须平（库里有 ck_biz_voucher_balance，这里先自己拦一道好读的报错）
    IF abs(r.total_amount + r.total_tax - r.total_with_tax) > 0.0001 THEN
        RAISE EXCEPTION 'P9-D1: 三金额不平（% + % <> %），拒绝补凭证',
            r.total_amount, r.total_tax, r.total_with_tax;
    END IF;

    SELECT p.period_code INTO acct_period
      FROM biz_accounting_period p
     WHERE p.start_date <= r.in_date AND p.end_date >= r.in_date
     ORDER BY p.period_code DESC
     LIMIT 1;
    IF acct_period IS NULL THEN
        RAISE EXCEPTION 'P9-D1: 收货日期 % 没有对应会计期间，拒绝补凭证', r.in_date;
    END IF;

    SELECT i.material_id INTO first_mat
      FROM biz_stock_in_item i
     WHERE i.stock_in_id = r.id
     ORDER BY i.material_id, i.line_no
     LIMIT 1;

    -- ---------- 凭证号（PZ）----------
    UPDATE biz_seq
       SET current_no = CASE WHEN biz_seq.period_key = today_key THEN biz_seq.current_no + 1 ELSE 1 END,
           period_key = today_key,
           updated_at = now()
     WHERE seq_type = 'PZ'
     RETURNING current_no INTO v_seq;
    IF v_seq IS NULL THEN
        RAISE EXCEPTION 'P9-D1: biz_seq 缺少 PZ 号段行，拒绝补凭证（不自己编号）';
    END IF;
    v_no := 'PZ-' || today_key || '-' || lpad(v_seq::text, 6, '0');

    summary := format('采购入库 %s 价税合计 %s', r.in_no, r.total_with_tax);

    INSERT INTO biz_voucher
           (voucher_word, voucher_no, period, voucher_date, voucher_type, source_type,
            source_id, source_no, summary, total_debit, total_credit, entry_count,
            status, is_red, reverse_of, posted_at, posted_by,
            created_at, updated_at, create_by, update_by, remark)
    VALUES ('记', v_no, acct_period, r.in_date, 'AUTO', 'STOCK_IN',
            r.id, r.in_no, summary,
            r.total_amount + r.total_tax, r.total_with_tax, 2 + (CASE WHEN r.total_tax > 0 THEN 1 ELSE 0 END),
            'POSTED', 0, NULL, now(), NULL,
            now(), now(), NULL, NULL,
            'P9 数据修复：P2-C1 之前过账的历史单补生成凭证')
    RETURNING id INTO v_id;

    INSERT INTO biz_voucher_entry
           (voucher_id, line_no, summary, account_id, account_code, account_name,
            debit_amount, credit_amount, aux_supplier_id, aux_material_id, aux_warehouse_id)
    VALUES (v_id, 1, summary || ' 存货', acct_inv, '1405', '库存商品',
            r.total_amount, 0, NULL, first_mat, r.warehouse_id);

    IF r.total_tax > 0 THEN
        INSERT INTO biz_voucher_entry
               (voucher_id, line_no, summary, account_id, account_code, account_name,
                debit_amount, credit_amount, aux_supplier_id, aux_material_id, aux_warehouse_id)
        VALUES (v_id, 2, summary || ' 进项税额', acct_tax, '2221.01', '应交税费-进项税',
                r.total_tax, 0, NULL, NULL, NULL);
    END IF;

    INSERT INTO biz_voucher_entry
           (voucher_id, line_no, summary, account_id, account_code, account_name,
            debit_amount, credit_amount, aux_supplier_id, aux_material_id, aux_warehouse_id)
    VALUES (v_id, CASE WHEN r.total_tax > 0 THEN 3 ELSE 2 END, summary, acct_ap, '2202', '应付账款',
            0, r.total_with_tax, r.supplier_id, NULL, NULL);

    -- ---------- 应付单（AP）----------
    -- 变量不叫 credit_days：会与 biz_supplier.credit_days / biz_payable.credit_days 列名 ambiguous
    SELECT coalesce((SELECT s.credit_days FROM biz_supplier s
                      WHERE s.id = r.supplier_id AND s.deleted_at IS NULL), 0)
      INTO sup_days;
    sup_days := greatest(sup_days, 0);
    due := r.in_date + sup_days;

    UPDATE biz_seq
       SET current_no = CASE WHEN biz_seq.period_key = today_key THEN biz_seq.current_no + 1 ELSE 1 END,
           period_key = today_key,
           updated_at = now()
     WHERE seq_type = 'AP'
     RETURNING current_no INTO p_seq;
    IF p_seq IS NULL THEN
        RAISE EXCEPTION 'P9-D1: biz_seq 缺少 AP 号段行，拒绝补应付单';
    END IF;
    p_no := 'AP-' || today_key || '-' || lpad(p_seq::text, 6, '0');

    IF r.supplier_id IS NULL THEN
        RAISE EXCEPTION 'P9-D1: 该单没有供应商，不能补应付单';
    END IF;

    INSERT INTO biz_payable
           (payable_no, supplier_id, source_type, source_id, source_no, currency_code,
            amount, paid_amount, unpaid_amount, credit_days, due_date, period_code,
            status, voucher_id, created_at, updated_at, create_by, update_by, remark)
    VALUES (p_no, r.supplier_id, 'STOCK_IN', r.id, r.in_no, 'CNY',
            r.total_with_tax, 0, r.total_with_tax, sup_days, due, acct_period,
            'OPEN', v_id, now(), now(), NULL, NULL,
            'P9 数据修复：P2-C1 之前过账的历史单补生成应付单')
    RETURNING id INTO p_id;

    UPDATE biz_stock_in
       SET voucher_id = v_id, payable_id = p_id, updated_at = now()
     WHERE id = r.id;

    RAISE NOTICE 'P9-D1: 已补凭证 %（% 元）与应付单 %，并回写 biz_stock_in.voucher_id/payable_id',
        v_no, r.total_with_tax, p_no;
END $$;
