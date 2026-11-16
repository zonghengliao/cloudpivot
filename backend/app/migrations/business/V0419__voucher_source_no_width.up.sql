-- ============================================================================
-- V0419__voucher_source_no_width（M1-FIX · 外币手工凭证 source_no 超 varchar(32) → 建单 500 · 2026-11-16）
--
-- 卡：t_112c3502（M1-FIX【M1·财务】外币手工凭证 source_no 超 varchar(32) → 建单 500）
-- 上游：M1-04（V0406 单头币种/汇率 + V0409 分录原币）写的手工凭证 `source_no` 文案；
--       M1-11 在 145 真机验证外币列展示时踩到，PG 18 日志原文
--       `ERROR: value too long for type character varying(32)`。
--
-- 【现场】外币**手工**凭证的 `source_no` 是拼出来的：
--           手工凭证 {原币合计} {币种}（本位币 {本位币合计}）
--         固定部分 15 字 + 两个金额串。金额列是 NUMERIC(20,4)，但**常见金额就已经越界**：
--           原币 100.0000 @8.5 → 850.0000   → 31 字  ✅
--           原币 500.0000 @8.5 → 4250.0000  → 32 字  ✅（恰好顶满）
--           原币 1000.0000@8.5 → 8500.0000  → 33 字  ❌ 22001 → 500 + 10004「服务器内部错误」
--         即：**原币合计 ≥ 1000 的外币手工凭证一张都建不出来**，前端只看到「服务器内部错误」。
--         M1-04 的单测用 100 / 7.2（31 字）夹具，所以红不了 —— 边界值没覆盖。
--
-- 【为什么放宽列宽（而不是把文案写短）】
--   ① `source_no` 的历史用途**不含格式解析**：列表「业务单号 / 来源单号」列原样显示、
--      关键词搜索 `source_no LIKE %x%`、以及 `source_doc_no = COALESCE(显式传入, source_no)`
--      的兜底（V0357）。前端对 `source_type = MANUAL` 走纯文本、不建回链
--      （`views/business/erp/shared/source-route.ts` 的 `Manual` 分支），所以文案本身不是契约。
--   ② 但文案里那两段金额是 M1-04 **刻意**加的：外币单只写「手工凭证 100」会被读成 100 元人民币
--      （清单页按单号认金额的用法直接错）。缩文案就得把「本位币合计」搬到 remark / 摘要，
--      那等于把「一眼看清双口径」这个设计意图再拆掉一次。
--   ③ 放宽的代价是**零**：PG 的 `varchar(n) → varchar(m)`（m > n）是元数据变更，不重写表、
--      不重建索引、不锁长表；而且列宽本身仍是有界护栏（不是 TEXT）。
--   ⇒ 选「放宽列宽」，文案一字不改（前端 / e2e / 证据文档里的旧格式断言全部继续有效）。
--
-- 【为什么是 64 —— 有依据的上限，不是拍脑袋】
--   预算 = 15（`手工凭证 ` + ` ` + 币种 3 位 + `（本位币 ` + `）`）
--        + 金额串 ×2，每个金额串 ≤ 21 字（NUMERIC(20,4) 的极限：16 位整数 + '.' + 4 位小数）
--        ⇒ **最坏 57 字**，VARCHAR(64) 留 7 字余量；而 32 字连「两位数千元级」外币单都装不下。
--   （`1000000000.0000 @8.5` 实测 45 字；`1234567890123456.1234` 这种 16 位整数原币已是
--     金额列自身的极限，本位币再乘汇率就超 NUMERIC(20,4) 了 —— 那种输入由金额列拦，与本列无关。）
--
-- 【同一次 INSERT 的另一列：source_doc_no】
--   `post_voucher_fx` 的 INSERT 里 `source_doc_no = COALESCE(显式传入, source_no)`（V0357 加的列，
--   建列时就是 VARCHAR(64)）。**必须两列同量级**：只放宽 source_no 的话，落库会把同一个串
--   塞进 source_doc_no —— 现在恰好也是 64 所以没事，但这是「靠巧合对齐」。本迁移因此把
--   「两列都 ≥ 64」写成自检断言，谁把 source_doc_no 改窄都会在这里当场翻红。
--   ⇒ 本迁移**只动 source_no**（source_doc_no 本来就是 64，无需改），但**断言两列**。
--
-- 【幂等 / 回滚】
--   - 只放宽、不收窄：已是 ≥ 64 就跳过（重复执行不报错，也不会把别人的更宽值改窄）。
--   - 不改任何存量行（0 行数据改写）⇒ 重放天然幂等。
--   - 文件内不写 BEGIN/COMMIT（迁移器自己开事务）。
--   - 回滚：`V0419__voucher_source_no_width.down.sql` —— 有 > 32 字的 source_no 就**拒绝**收窄
--     （收窄会把业务单号截断/报错），要回滚得先把那些外币凭证红冲 / 作废。
-- ============================================================================

-- ============================================================
-- §1 放宽 `biz_voucher.source_no`：VARCHAR(32) → VARCHAR(64)（幂等，只放宽不收窄）
-- ============================================================
DO $$
DECLARE
    v_len integer;
BEGIN
    SELECT character_maximum_length INTO v_len
      FROM information_schema.columns
     WHERE table_schema = 'public'
       AND table_name   = 'biz_voucher'
       AND column_name  = 'source_no';

    IF v_len IS NULL THEN
        RAISE EXCEPTION 'V0419 前置失败：biz_voucher.source_no 不存在（V0149 没应用？）';
    END IF;

    IF v_len < 64 THEN
        -- 元数据变更：不重写表、不重建索引（PG 对 varchar 加长一直是 no-rewrite）
        ALTER TABLE biz_voucher ALTER COLUMN source_no TYPE VARCHAR(64);
        RAISE NOTICE 'V0419: biz_voucher.source_no 由 VARCHAR(%) 放宽到 VARCHAR(64)', v_len;
    ELSE
        RAISE NOTICE 'V0419: biz_voucher.source_no 已是 VARCHAR(%)，跳过（幂等）', v_len;
    END IF;
END $$;

COMMENT ON COLUMN biz_voucher.source_no IS
    '记账依据单据号（手工外币凭证是 「手工凭证 {原币合计} {币种}（本位币 {本位币合计}）」）。'
    'V0419 起 VARCHAR(64)：文案最坏 57 字（15 固定 + 2×21 金额），32 字装不下 ≥1000 的外币单。';

-- ============================================================
-- §2 自检一：两列宽度都必须 ≥ 64（source_doc_no 由 V0357 建为 64，本迁移只断言不改）
-- ============================================================
DO $$
DECLARE
    n integer := 0;
    v_extra text;
BEGIN
    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = 'public'
       AND table_name   = 'biz_voucher'
       AND column_name IN ('source_no', 'source_doc_no')
       AND character_maximum_length >= 64;

    IF n <> 2 THEN
        SELECT string_agg(column_name || '=' || coalesce(character_maximum_length::text, 'unbounded'), ', ')
          INTO v_extra
          FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = 'biz_voucher'
           AND column_name IN ('source_no', 'source_doc_no');
        RAISE EXCEPTION
            'V0419 自检失败：source_no / source_doc_no 必须都 ≥ VARCHAR(64)（同一次 INSERT 里 '
            'source_doc_no = COALESCE(显式传入, source_no)，一宽一窄就会在落库时炸），实际 % （命中 %/2 列）',
            coalesce(v_extra, '<无>'), n;
    END IF;

    RAISE NOTICE 'V0419 自检一通过：source_no / source_doc_no 都 ≥ VARCHAR(64)';
END $$;

-- ============================================================
-- §3 自检二：行为探针（33 字 / 57 字能写进去；65 字仍被列宽拦住）
--    探针行写完立刻清场，重复执行零残留。
-- ============================================================
DO $$
DECLARE
    v_voucher uuid;
    n integer := 0;
BEGIN
    -- P1：修复前的失败形态（33 字 = 「手工凭证 1000.0000 USD（本位币 8500.0000）」）现在必须能写
    INSERT INTO biz_voucher (voucher_word, voucher_no, period, voucher_date, voucher_type,
                             source_type, source_no, source_doc_no, summary,
                             total_debit, total_credit, entry_count, status)
    VALUES ('记', 'V0419PROBE', '1999-01', DATE '1999-01-01', 'MANUAL',
            'MANUAL', repeat('手', 33), repeat('手', 33), '__V0419_PROBE__', 1, 1, 2, 'DRAFT')
    RETURNING id INTO v_voucher;

    -- P2：预算最坏情况（57 字）也必须能写
    UPDATE biz_voucher
       SET source_no = repeat('凭', 57), source_doc_no = repeat('凭', 57)
     WHERE id = v_voucher;

    -- P3：列宽仍然有界 —— 65 字必须被拒（SQLSTATE 22001），证明我们放宽的是「到 64」，不是改成 TEXT
    BEGIN
        UPDATE biz_voucher SET source_no = repeat('超', 65) WHERE id = v_voucher;
        RAISE EXCEPTION 'V0419 自检失败：65 字写入成功 —— source_no 被改成了无界类型？';
    EXCEPTION WHEN string_data_right_truncation THEN
        NULL;  -- 22001：正是我们要的「列宽护栏还在」
    END;

    -- 探针清场 + 残留归零
    DELETE FROM biz_voucher WHERE id = v_voucher;

    SELECT count(*) INTO n FROM biz_voucher WHERE voucher_no = 'V0419PROBE';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0419 自检失败：凭证探针行残留 % 行', n;
    END IF;

    RAISE NOTICE 'V0419 自检二通过：source_no 能写 33 / 57 字，65 字仍被列宽拦（22001），探针零残留';
END $$;
