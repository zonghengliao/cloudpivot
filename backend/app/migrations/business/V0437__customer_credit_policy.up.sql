-- ============================================================================
-- V0437__customer_credit_policy（M3-09 · 客户信用：额度字段 + 校验配置 · 2026-11-17）
--
-- 卡：t_464b7265（M3-09【M3·销售】客户信用：额度字段 + 校验配置）
-- 前置：V0260（biz_customer 主档 + credit_limit）/ V0289（credit_days）/
--       V0313（credit_used 缓存列 + biz_credit_log 台账）
--
-- ── 本迁移只做一件事 ────────────────────────────────────────────────────────
-- 给 `biz_customer` 补「超限策略」列 `credit_policy`：
--
--     BLOCK  超限拦截：提交销售订单时 409（**默认值 = 存量行为**，老库升级后行为不变）
--     WARN   超限警告：允许提交，同时写一条 delta = 0 的警示台账 + WARN 日志，
--            提交响应里带 `credit_warning` 文案供前端提示
--     PASS   超限放行：允许提交，不留痕、不提示（VIP 客户的显式例外）
--
-- 策略由 `credit::decide()` 读取并裁决；额度口径本身（`credit_limit` /
-- `credit_used` / 公式）**一个字都不动** —— 那是 V0260 + V0313 + S9 的地盘。
--
-- ── 为什么默认 BLOCK（而不是 WARN / PASS）──────────────────────────────────
-- M3-09 之前代码里是**硬编拦截**（`ensure_submittable` 直接 409）。默认值必须
-- 让老库升级后行为完全不变：升级即静默放行等于「把内控关了」——这是最不能接受的
-- 静默变更。要放宽的客户由业务在客户档案里显式改成 WARN / PASS。
--
-- ── 为什么策略挂在客户上（而不是全局配置）──────────────────────────────────
-- 「哪个客户可以超限发货」是客户维度的商务决策（老客户 / 战略客户 / 预付款客户），
-- 全局开关表达不出来。供应商侧的 `biz_purchase_order.over_credit_limit` 是「按单
-- 冻结快照」，客户侧这里是「按客户常态策略」，两者互补不冲突。
--
-- ── 幂等 ──────────────────────────────────────────────────────────────────
-- `ADD COLUMN IF NOT EXISTS` + 约束先查 `pg_constraint` 再加；
-- 存量行由 `DEFAULT 'BLOCK'` 自动填满（`NOT NULL` 不需要回填 UPDATE）；
-- 重复执行影响 0 行、不产生新约束。末尾 DO 块自检：列形状 / 约束域值（行为断言，
-- 不拿文本比）/ 默认值 / 存量行全部在域内，任一不符 → 迁移失败。
-- ============================================================================

-- ---------- ① 加列 ----------
ALTER TABLE biz_customer
    ADD COLUMN IF NOT EXISTS credit_policy VARCHAR(10) NOT NULL DEFAULT 'BLOCK';

-- ---------- ② 约束（幂等：先查后加） ----------
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_customer_credit_policy') THEN
        ALTER TABLE biz_customer
            ADD CONSTRAINT ck_biz_customer_credit_policy
            CHECK (credit_policy IN ('BLOCK', 'WARN', 'PASS'));
    END IF;
END $$;

COMMENT ON COLUMN biz_customer.credit_policy IS
    '超限策略：BLOCK=超限拦截（默认，提交 409）/ WARN=超限警告（放行 + 警示台账 + 响应带 credit_warning）/ PASS=超限放行（放行不留痕）；credit_limit = 0（不控额度）时本列不生效';

-- ---------- ③ 自检：列形状 + 域值行为 + 默认值 + 存量行 ----------
DO $$
DECLARE
    n         INT;
    v_type    TEXT;
    v_len     INT;
    v_null    TEXT;
    v_default TEXT;
    v_def     TEXT;
BEGIN
    -- 3.1 列形状：character varying(10) NOT NULL DEFAULT 'BLOCK'
    SELECT data_type, character_maximum_length, is_nullable, column_default
      INTO v_type, v_len, v_null, v_default
      FROM information_schema.columns
     WHERE table_schema = 'public'
       AND table_name = 'biz_customer'
       AND column_name = 'credit_policy';
    IF v_type IS NULL THEN
        RAISE EXCEPTION 'V0437 自检失败：biz_customer.credit_policy 列不存在';
    END IF;
    IF v_type <> 'character varying' OR v_len <> 10 THEN
        RAISE EXCEPTION 'V0437 自检失败：credit_policy 类型应为 varchar(10)，实际 %(%)', v_type, v_len;
    END IF;
    IF v_null <> 'NO' THEN
        RAISE EXCEPTION 'V0437 自检失败：credit_policy 必须 NOT NULL（实际 is_nullable=%）', v_null;
    END IF;
    IF v_default IS NULL OR v_default NOT LIKE '''BLOCK''%' THEN
        RAISE EXCEPTION 'V0437 自检失败：credit_policy 默认值应为 ''BLOCK''（实际 %）', v_default;
    END IF;

    -- 3.2 约束在位，且域值文本含三个字面量
    SELECT pg_get_constraintdef(oid) INTO v_def
      FROM pg_constraint WHERE conname = 'ck_biz_customer_credit_policy';
    IF v_def IS NULL THEN
        RAISE EXCEPTION 'V0437 自检失败：ck_biz_customer_credit_policy 缺失';
    END IF;
    IF v_def NOT LIKE '%BLOCK%' OR v_def NOT LIKE '%WARN%' OR v_def NOT LIKE '%PASS%' THEN
        RAISE EXCEPTION 'V0437 自检失败：ck_biz_customer_credit_policy 域值不是 BLOCK/WARN/PASS（def=%）', v_def;
    END IF;

    -- 3.3 行为断言：越界策略必须被拒（探针插入在子事务里回滚，不留行）
    BEGIN
        INSERT INTO biz_customer (code, name, credit_policy)
        VALUES ('__V0437_PROBE_POLICY__', '自检探针（应被拒）', 'BOGUS');
        RAISE EXCEPTION 'V0437 自检失败：credit_policy = BOGUS 未被 ck_biz_customer_credit_policy 拒绝';
    EXCEPTION WHEN check_violation THEN
        NULL;   -- 期望路径
    END;

    -- 3.4 行为断言：省略 credit_policy 时必须落默认值 BLOCK（老代码 INSERT 不带该列）
    BEGIN
        INSERT INTO biz_customer (code, name)
        VALUES ('__V0437_PROBE_DEFAULT__', '自检探针（默认值）');
        SELECT count(*) INTO n
          FROM biz_customer
         WHERE code = '__V0437_PROBE_DEFAULT__' AND credit_policy = 'BLOCK';
        IF n <> 1 THEN
            RAISE EXCEPTION 'V0437 自检失败：省略 credit_policy 的 INSERT 没有落默认值 BLOCK';
        END IF;
        RAISE EXCEPTION 'V0437 自检探针回滚';   -- 借异常回滚本子事务，不留行
    EXCEPTION WHEN raise_exception THEN
        IF SQLERRM <> 'V0437 自检探针回滚' THEN
            RAISE;
        END IF;
    END;

    -- 3.5 存量行全部在域内（默认值已覆盖历史行；不是 0 行说明有人手工写坏）
    SELECT count(*) INTO n FROM biz_customer WHERE credit_policy NOT IN ('BLOCK', 'WARN', 'PASS');
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0437 自检失败：% 行 credit_policy 不在域内', n;
    END IF;

    -- 3.6 探针残留必须为 0
    SELECT count(*) INTO n FROM biz_customer WHERE code LIKE '__V0437_PROBE%';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0437 自检失败：探针行残留 % 条', n;
    END IF;

    RAISE NOTICE 'V0437：biz_customer.credit_policy 就位（BLOCK/WARN/PASS，默认 BLOCK）✓';
END $$;
