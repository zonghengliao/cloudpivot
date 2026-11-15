-- ======================================================
-- V0407 down：拆掉汇率表的「生效区间」+ 区间唯一（M1-02）
--
-- 与 up 严格对称，顺序反过来：先拆约束、再拆列。
--
-- ── 为什么**没有**数据护栏 ────────────────────────────
--   `effective_to` 是**派生列**（恒等于「同组下一条 effective_date - 1」，末行 NULL），
--   不含任何人工录入的业务信息：重放 up 即可逐行重建出同样的值。丢掉它不丢真相
--   ——「这条汇率从哪天起生效」始终在 `effective_date` 里。
--   对照 V0406 的 down 为什么加护栏：那里 `currency_code` / `exchange_rate` 是人工录的
--   业务信息，回滚就真丢了，所以拒绝。
--
-- ── 拆的顺序（有依赖，不能换）───────────────────────
--   1) 排除约束 `ex_biz_exchange_rate_period` 依赖 `daterange(effective_date, effective_to)`
--      这个表达式索引 ⇒ 必须先 DROP CONSTRAINT，否则 DROP COLUMN 报「被索引依赖」。
--   2) CHECK `ck_biz_exchange_rate_dates` 同理先拆（也可以靠 DROP COLUMN 级联，但显式更清楚）。
--   3) 最后 DROP COLUMN。
--
-- ── 不删 btree_gist ───────────────────────────────
--   扩展是库级对象，可能被别的区间约束（V0336 `ex_biz_flow_delegate_no_overlap` 等）用着。
--   down 只回收本迁移自己建的对象。
--
-- 幂等：全部 IF EXISTS / IF NOT EXISTS，重复执行零报错。
-- ======================================================

ALTER TABLE biz_exchange_rate
    DROP CONSTRAINT IF EXISTS ex_biz_exchange_rate_period;

ALTER TABLE biz_exchange_rate
    DROP CONSTRAINT IF EXISTS ck_biz_exchange_rate_dates;

ALTER TABLE biz_exchange_rate
    DROP COLUMN IF EXISTS effective_to;
