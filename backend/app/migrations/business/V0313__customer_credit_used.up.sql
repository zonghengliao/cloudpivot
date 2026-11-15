-- SO-38 / ARCH-7 §15.5：客户信用额度「已用」缓存列 + 变动台账 + 兜底重算任务行
--
-- 背景（v1.0 的漏洞）：
--   v1.0 只写了「下单时校验 credit_used + 本单金额 <= credit_limit」，**没写什么时候释放**。
--   只占用不释放 ⇒ 客户额度被永久占死。本迁移补上「已用」的落库位与变动台账。
--
-- 口径（与 erp_invariants.py 的 S9 逐字同源，改一处必须改三处：本注释 / credit.rs / S9）：
--   credit_used = SUM(有效订单含税)                      -- biz_sales_order.status IN (APPROVED, PARTIAL_OUT, OUTBOUND)
--               − SUM(应收已核销额)                      -- biz_receivable.received_amount
--               − SUM(已过账退货含税)                    -- biz_sales_return.status = 'POSTED'
--               下限 0（不出负占用：负占用会让下一张单白拿额度）
--   容差 0.01（numeric(18,4) 存 4 位，口径口径比到分）。
--
-- 为什么 DRAFT / SUBMITTED 不进「有效订单」：
--   ARCH-7 §15.5.2「占用时机（唯一）：销售订单 APPROVED 时」。
--   提交待审的单不进本计数器，但**提交守卫**仍把它们算作管道占用（`credit::pending_credit`），
--   否则「连开 10 张草稿一起提交」能绕过额度 —— 守卫比计数器严格，方向是安全的。
--
-- 为什么要有缓存列（而不是纯现算）：
--   现算口径仍在（`credit.rs::usage_of`），但客户列表 / 下单校验要按 `credit_used` 过滤与排序，
--   每条都现算三张表的聚合撑不住；缓存列 + 每日重算 + S9 巡检 = 用「可对账」换性能。

-- ---------- ① 已用额度列 ----------
ALTER TABLE biz_customer
  ADD COLUMN IF NOT EXISTS credit_used numeric(18,4) NOT NULL DEFAULT 0;

DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_customer_credit_used') THEN
    ALTER TABLE biz_customer
      ADD CONSTRAINT ck_biz_customer_credit_used CHECK (credit_used >= 0);
  END IF;
END $$;

-- 索引：ARCH-7 §15.5.3「credit_used 字段已有，需补索引」。
-- 部分索引（只索引「确实占着额度」的客户）—— 绝大部分客户是 0，全量索引白占空间。
CREATE INDEX IF NOT EXISTS idx_biz_customer_credit_used
  ON biz_customer (credit_used)
  WHERE deleted_at IS NULL AND credit_used > 0;

-- ---------- ② 额度变动台账（重算修正 / 四个出口的留痕） ----------
-- 不做分区：「每客户每天最多几笔」，量级远小于 sys_change_log；
-- 用 biz_status_log 不合适（那是状态机的边日志，额度变动不是状态迁移）。
CREATE TABLE IF NOT EXISTS biz_credit_log (
  id            uuid PRIMARY KEY,
  customer_id   uuid NOT NULL,
  before_used   numeric(18,4) NOT NULL,
  after_used    numeric(18,4) NOT NULL,
  delta         numeric(18,4) NOT NULL,
  -- 变动原因（见 credit.rs 的 CAUSE_*）：ORDER_OCCUPY / ORDER_RELEASE /
  -- AR_SETTLED / AR_REVERSED / RETURN_POSTED / RECOMPUTE
  cause         varchar(24) NOT NULL,
  -- 触发单据号（订单号 / 应收单号 / 退货单号；重算时空串）
  ref_no        varchar(40) NOT NULL DEFAULT '',
  -- 触发来源：API / JOB（每日重算）/ SYSTEM
  trigger_source varchar(16) NOT NULL DEFAULT 'API',
  created_at    timestamptz NOT NULL DEFAULT now(),
  CONSTRAINT ck_biz_credit_log_delta CHECK (delta = after_used - before_used)
);

CREATE INDEX IF NOT EXISTS idx_biz_credit_log_customer
  ON biz_credit_log (customer_id, created_at DESC);

-- ---------- ③ 存量数据一次性算平 ----------
-- 老库的 credit_used 是 000000 默认 0，直接上线等于「所有客户都没有占额度」
-- （比「占死」更危险：能超额度下单）。这里按 §15.5.2 公式一次性回填。
-- 缺表（未上销售模块的库）就跳过，不因为「表不存在」把整条迁移打红。
DO $$
DECLARE
  has_all boolean;
BEGIN
  SELECT to_regclass('public.biz_sales_order') IS NOT NULL
     AND to_regclass('public.biz_receivable') IS NOT NULL
     AND to_regclass('public.biz_sales_return') IS NOT NULL
    INTO has_all;
  IF NOT has_all THEN
    RAISE NOTICE 'SO-38: 缺销售链表，跳过 credit_used 回填（新库首次启动时本来就是 0）';
    RETURN;
  END IF;

  UPDATE biz_customer c
     SET credit_used = GREATEST(
           0,
           COALESCE((SELECT SUM(o.total_with_tax) FROM biz_sales_order o
                      WHERE o.customer_id = c.id AND o.deleted_at IS NULL
                        AND o.status IN ('APPROVED', 'PARTIAL_OUT', 'OUTBOUND')), 0)
         - COALESCE((SELECT SUM(r.received_amount) FROM biz_receivable r
                      WHERE r.customer_id = c.id AND r.deleted_at IS NULL), 0)
         - COALESCE((SELECT SUM(t.total_with_tax) FROM biz_sales_return t
                      WHERE t.customer_id = c.id AND t.deleted_at IS NULL
                        AND t.status = 'POSTED'), 0)
         ),
         updated_at = now()
   WHERE c.deleted_at IS NULL;
END $$;

-- ---------- ④ sys_job 种子：兜底重算（默认停用，仅供「定时任务」页查看 / 手动执行一次） ----------
-- 与 V0210 的四条提醒任务同范式：真正的调度由代码内置注册（`credit::register_builtin`），
-- 本行 status = 0 只为「页面可见 + 可手动补跑」。改成 1 会同一 cron 注册两遍（白跑）。
INSERT INTO sys_job (job_name, job_group, cron_expr, invoke_target, params, status, remark)
SELECT v.job_name, 'ERP', v.cron_expr, v.invoke_target, NULL, 0, v.remark
  FROM (VALUES
        ('erp_credit_recompute', '0 10 2 * * *', 'erp_credit_recompute',
         '客户信用额度兜底重算（内置任务已注册，本行仅供查看/手动执行）：每天 02:10 按「有效订单含税 − 已核销应收 − 已过账退货」重算 credit_used，漂移即修正并记 biz_credit_log（ARCH-7 §15.5）')
       ) AS v(job_name, cron_expr, invoke_target, remark)
 WHERE NOT EXISTS (SELECT 1 FROM sys_job j WHERE j.job_name = v.job_name AND j.deleted_at IS NULL);
