-- ======================================================
-- V0289：销售链 —— 客户主档补「账期天数」`credit_days`（SO-14 出库挂账算到期日）
--
--   ① biz_customer 新增 credit_days（INT NOT NULL DEFAULT 0，0 = 现结）
--   ② 约束 ck_biz_customer_credit_days（>= 0）
--   ③ 末尾 DO 块自检：列存在且 NOT NULL / 默认 0 / 坏数据必被拒
--
-- ── 为什么要加这一列（而不是在挂账时硬编码天数）──────────────
--   卡面口径：`due_date = 过账日 + 客户 credit_days`。V0260 建客户表时只建了
--   `credit_limit`（信用额度），**没有账期天数**；`biz_supplier` 早在 V0137 就有
--   `credit_days INT NOT NULL DEFAULT 0`（`ck_biz_supplier_credit_days CHECK >= 0`）。
--   所以本卡逐列镜像供应商：同类型（INT）、同默认值（0 = 现结）、同约束（>= 0）。
--   不做的事：不给上限（3650 之类的魔法数字）、不建索引（不是查询条件）、
--   不在这里回填历史客户（默认 0 = 现结，是既有客户当前的真实语义）。
--
-- ── 号段 ──────────────────────────────────────────────
--   写入本文件时 backend/migrations/business/ 最大号 = V0288（SO-13 应收权限码卡）。
--   迁移号会被并行卡抢走，**提交前**再 `ls business | tail` 复核一次。
--
-- ── 幂等 ──────────────────────────────────────────────
--   `ADD COLUMN IF NOT EXISTS`；约束走 DO 块判存在后再加（`ADD CONSTRAINT` 没有
--   `IF NOT EXISTS` 语法）。重复执行不报错、不重复加约束。
--
-- ── 为什么 default 是 0 而不是 30 ─────────────────────
--   0 = 现结（到期日 = 过账日），与 `biz_supplier.credit_days` 的口径一致；
--   给 30 会让所有既有客户凭空多出 30 天账期，账龄与逾期口径立刻失真。
-- ======================================================

-- ---------- ① 列 ----------
ALTER TABLE biz_customer ADD COLUMN IF NOT EXISTS credit_days INT NOT NULL DEFAULT 0;

COMMENT ON COLUMN biz_customer.credit_days IS
    '账期天数（INT，>= 0；0 = 现结）：销售出库过账自动挂应收时，due_date = 过账日 + 本列（SO-14）。与 biz_supplier.credit_days 同口径';

-- ---------- ② 约束（ADD CONSTRAINT 无 IF NOT EXISTS ⇒ 先判存在） ----------
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM pg_constraint
         WHERE conname = 'ck_biz_customer_credit_days'
           AND conrelid = 'public.biz_customer'::regclass
    ) THEN
        ALTER TABLE biz_customer
            ADD CONSTRAINT ck_biz_customer_credit_days CHECK (credit_days >= 0);
    END IF;
END $$;

-- ---------- ③ 自检 ----------
DO $$
DECLARE
    n        INTEGER;
    d        TEXT;
    not_null BOOLEAN;
BEGIN
    -- 3.1 列存在 / 类型 INT / NOT NULL / 默认 0
    SELECT count(*), max(is_nullable)
      INTO n, not_null
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_customer'
       AND column_name = 'credit_days'
       AND data_type = 'integer';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0289 自检失败：biz_customer.credit_days 不存在或类型不是 integer';
    END IF;
    IF not_null THEN
        RAISE EXCEPTION 'V0289 自检失败：biz_customer.credit_days 允许 NULL（应为 NOT NULL DEFAULT 0）';
    END IF;
    SELECT column_default INTO d
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_customer'
       AND column_name = 'credit_days';
    IF d IS NULL OR d NOT LIKE '%0%' THEN
        RAISE EXCEPTION 'V0289 自检失败：biz_customer.credit_days 默认值不是 0（实际 %）', d;
    END IF;

    -- 3.2 约束在位
    SELECT count(*) INTO n
      FROM pg_constraint
     WHERE conrelid = 'public.biz_customer'::regclass
       AND conname = 'ck_biz_customer_credit_days';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0289 自检失败：ck_biz_customer_credit_days 不在位';
    END IF;

    -- 3.3 CHECK 真的生效：插一张 credit_days = -1 的客户必须被拒（23514）
    BEGIN
        INSERT INTO biz_customer (code, name, credit_days)
        VALUES ('V0289-PROBE', 'V0289 账期探针', -1);
        RAISE EXCEPTION 'V0289 自检失败：credit_days = -1 没有被 ck_biz_customer_credit_days 拒绝';
    EXCEPTION
        WHEN check_violation THEN NULL;   -- 期望路径：坏数据被库层挡住
    END;

    RAISE NOTICE 'V0289：biz_customer.credit_days（账期天数，0=现结）已就位，坏数据被 CHECK 拒绝';
END $$;
