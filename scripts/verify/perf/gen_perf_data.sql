-- =============================================================================
-- N-8 性能基线数据集生成器（可再生、幂等递增）
--
-- 用法（在目标库上执行，psql 需要 :target 变量）：
--   psql -v target=1000  -f scripts/verify/perf/gen_perf_data.sql   -- 1 千级
--   psql -v target=10000 -f scripts/verify/perf/gen_perf_data.sql   -- 1 万级
--   psql -v target=100000 -f scripts/verify/perf/gen_perf_data.sql  -- 10 万级
--
-- 语义：把 5 张关键表补到**每表 target 行**（只插缺的部分，不删已有行），
--       所以同一个库可以 1千 → 1万 → 10万 逐级长大，用于测「数据量↑ 的退化倍数」。
--
-- 行的身份靠前缀标记（N8PERF / n8perf-），只统计/递增自己的行：
--   biz_task.task_no            LIKE 'N8PERF-%'
--   biz_voucher.summary         LIKE 'N8PERF %'
--   biz_voucher_entry.summary   LIKE 'N8PERF %'
--   biz_inventory_txn.doc_no    LIKE 'N8PERF-%'
--   operation_logs.request_id   LIKE 'n8perf-%'
--
-- 可复现：凭证/分录主键由 n 直接推出（固定前缀 + 12 位序号），重跑不会重复；
--         其余表用 gen_random_uuid()，但受「已有行数」约束，重跑是 no-op。
--
-- 清理（只在专用性能库上执行，见 docs/运维规程-性能基线.md）：
--   DELETE FROM biz_voucher_entry WHERE summary LIKE 'N8PERF %';
--   DELETE FROM biz_voucher       WHERE summary LIKE 'N8PERF %';
--   DELETE FROM biz_task          WHERE task_no LIKE 'N8PERF-%';
--   DELETE FROM biz_inventory_txn WHERE doc_no  LIKE 'N8PERF-%';
--   DELETE FROM operation_logs    WHERE request_id LIKE 'n8perf-%';
-- =============================================================================
\set ON_ERROR_STOP on
\pset pager off

-- 目标行数（每张表）。未传 target 时按 1000 处理。
\if :{?target}
\else
  \set target 1000
\endif

\echo ============ [gen] 开始生成 ============
\echo target = :target （每表行数）

-- 叶子科目池（分录按 n 轮转取，保证 SUM 聚合有 66 个分组而不是 1 个）
DROP TABLE IF EXISTS n8_acct;
CREATE TEMP TABLE n8_acct AS
SELECT row_number() OVER (ORDER BY account_code) AS rn, id, account_code, account_name
  FROM biz_account
 WHERE is_leaf = 1 AND status = 1 AND deleted_at IS NULL;

-- 员工/部门/物料/仓库池（取真实主数据，避免外键悬空）
DROP TABLE IF EXISTS n8_pool;
CREATE TEMP TABLE n8_pool AS
SELECT (SELECT id FROM sys_user    WHERE deleted_at IS NULL ORDER BY created_at, id LIMIT 1) AS user_id,
       (SELECT id FROM sys_dept    WHERE deleted_at IS NULL ORDER BY sort_order, id LIMIT 1) AS dept_id,
       (SELECT id FROM biz_material WHERE deleted_at IS NULL ORDER BY id LIMIT 1) AS material_id,
       (SELECT id FROM biz_warehouse WHERE deleted_at IS NULL ORDER BY id LIMIT 1) AS warehouse_id;

-- 叶子科目池已就绪

-- ---------------------------------------------------------------------------
-- ① biz_task：任务列表（分页 + 可见性谓词 + due_date 排序）
-- ---------------------------------------------------------------------------
WITH base AS (SELECT count(*)::int AS c FROM biz_task WHERE task_no LIKE 'N8PERF-%')
INSERT INTO biz_task
  (task_no, title, description, status, priority, progress,
   owner_id, creator_id, dept_id, start_date, due_date,
   created_at, updated_at, source, exec_mode)
SELECT
  'N8PERF-' || lpad(n::text, 7, '0'),
  '性能基线任务 #' || n,
  'N-8 10 万行性能基线数据集（可整体删除，见 docs/运维规程-性能基线.md）',
  (ARRAY['TODO','DOING','REVIEW','DONE','CANCELLED'])[1 + ((n - 1) % 5)],
  (ARRAY['P0','P1','P2','P3'])[1 + ((n - 1) % 4)],
  ((n * 7) % 101)::smallint,
  p.user_id, p.user_id, p.dept_id,
  DATE '2025-01-01' + ((n % 700))::int,
  -- 让 1/6 的任务没有截止日（列表排序里 NULLS LAST 的分支也要压一压）
  CASE WHEN n % 6 = 0 THEN NULL ELSE DATE '2025-01-01' + ((n % 700) + (n % 30))::int END,
  now() - ((n % 730) || ' days')::interval - ((n % 24) || ' hours')::interval,
  now() - ((n % 730) || ' days')::interval,
  'SELF', 'HUMAN'
FROM base, n8_pool p, generate_series(base.c + 1, :target) n;

-- ---------------------------------------------------------------------------
-- ② biz_voucher：凭证列表 / 试算平衡 / 资产负债表
--    主键由 n 推出：00000000-0000-4000-a000-<12 位序号>
--    每张 3 条分录：借 60 + 借 40 = 贷 100（借贷平衡，报表勾稽才不会 500）
-- ---------------------------------------------------------------------------
WITH base AS (SELECT count(*)::int AS c FROM biz_voucher WHERE summary LIKE 'N8PERF %'),
     gen AS (
       SELECT n,
              DATE '2024-01-01' + (n % 1080)::int AS d
         FROM base, generate_series(base.c + 1, :target) n
     )
INSERT INTO biz_voucher
  (id, voucher_word, voucher_no, period, voucher_date, voucher_type, source_type,
   summary, total_debit, total_credit, entry_count, status, posted_at, created_at, updated_at)
SELECT
  ('00000000-0000-4000-a000-' || lpad(n::text, 12, '0'))::uuid,
  '记',
  lpad(n::text, 8, '0'),
  to_char(d, 'YYYY-MM'),
  d,
  'MANUAL',
  'MANUAL',
  'N8PERF 凭证 #' || n,
  100, 100, 3,
  -- 九成已过账（试算平衡/报表的重查询要真的聚合到绝大多数行），一成草稿
  CASE WHEN n % 10 = 0 THEN 'DRAFT' ELSE 'POSTED' END,
  d::timestamp AT TIME ZONE 'Asia/Shanghai',
  d::timestamp AT TIME ZONE 'Asia/Shanghai',
  d::timestamp AT TIME ZONE 'Asia/Shanghai'
FROM gen;

WITH base AS (SELECT count(*)::int AS c FROM biz_voucher_entry WHERE summary LIKE 'N8PERF %'),
     gen AS (
       SELECT n, DATE '2024-01-01' + (n % 1080)::int AS d
         FROM base, generate_series((base.c / 3) + 1, :target) n
     ),
     -- 每行 3 条分录；科目按 (n, line) 轮转。JOIN 的 ON 不能引用同级 FROM 里的
     -- 其它表（PG 语义），所以先在 x 里把 rn 算好，再 JOIN 科目池。
     x AS (
       SELECT g.n, g.d, l.line,
              1 + (((g.n::bigint * 7) + l.line * 13) % (SELECT count(*)::int FROM n8_acct)) AS rn
         FROM gen g, generate_series(1, 3) AS l(line)
     )
INSERT INTO biz_voucher_entry
  (id, voucher_id, line_no, summary, account_id, account_code, account_name,
   debit_amount, credit_amount, created_at)
SELECT
  gen_random_uuid(),
  ('00000000-0000-4000-a000-' || lpad(x.n::text, 12, '0'))::uuid,
  x.line,
  'N8PERF 分录',
  a.id, a.account_code, a.account_name,
  CASE x.line WHEN 1 THEN 60 WHEN 2 THEN 40 ELSE 0 END,
  CASE x.line WHEN 3 THEN 100 ELSE 0 END,
  x.d::timestamp AT TIME ZONE 'Asia/Shanghai'
FROM x JOIN n8_acct a ON a.rn = x.rn;

-- ---------------------------------------------------------------------------
-- ③ biz_inventory_txn：库存流水分页（按 created_at DESC + period/material 过滤）
-- ---------------------------------------------------------------------------
WITH base AS (SELECT count(*)::int AS c FROM biz_inventory_txn WHERE doc_no LIKE 'N8PERF-%')
INSERT INTO biz_inventory_txn
  (id, material_id, warehouse_id, txn_type, biz_type, doc_type, doc_id, doc_no,
   line_no, qty, unit_cost, amount, before_qty, before_amount, before_cost,
   after_qty, after_amount, after_cost, period_code, operator_id, remark, created_at)
SELECT
  gen_random_uuid(),
  p.material_id, p.warehouse_id,
  (ARRAY['IN','OUT'])[1 + ((n - 1) % 2)],  -- ck_biz_inv_txn_type 只允许 IN/OUT
  'PERF',
  (ARRAY['PURCHASE_RECEIPT','SALES_DELIVERY','STOCK_ADJUST'])[1 + ((n - 1) % 3)],
  gen_random_uuid(),
  'N8PERF-TXN-' || lpad(n::text, 9, '0'),
  1,
  ((n % 50) + 1)::numeric(20, 4),
  10::numeric(20, 4),
  (((n % 50) + 1) * 10)::numeric(20, 4),
  (n % 500)::numeric(20, 4), ((n % 500) * 10)::numeric(20, 4), 10::numeric(20, 4),
  ((n % 500) + (n % 50) + 1)::numeric(20, 4),
  (((n % 500) + (n % 50) + 1) * 10)::numeric(20, 4), 10::numeric(20, 4),
  to_char(DATE '2024-01-01' + (n % 1080)::int, 'YYYY-MM'),
  p.user_id,
  'N8PERF 库存流水',
  (DATE '2024-01-01' + (n % 1080)::int)::timestamp AT TIME ZONE 'Asia/Shanghai'
FROM base, n8_pool p, generate_series(base.c + 1, :target) n;

-- ---------------------------------------------------------------------------
-- ④ operation_logs：日志分页（created_at DESC + user/request_id 过滤）
-- ---------------------------------------------------------------------------
WITH base AS (SELECT count(*)::int AS c FROM operation_logs WHERE request_id LIKE 'n8perf-%')
INSERT INTO operation_logs
  (request_id, user_id, username, operation, target_type, target_id, detail,
   ip, user_agent, duration_ms, request_path, request_method, status_code, created_at)
SELECT
  'n8perf-' || lpad(n::text, 9, '0'),
  p.user_id,
  'admin',
  (ARRAY['PERF_READ','PERF_CREATE','PERF_UPDATE'])[1 + ((n - 1) % 3)],
  'perf',
  n::text,
  jsonb_build_object('n', n, 'note', 'N8 性能基线'),
  '127.0.0.1'::inet,
  'k6/performance-baseline',
  (n % 900)::int,
  '/api/business/perf/baseline',
  'GET',
  200,
  now() - ((n % 90) || ' days')::interval - ((n % 1440) || ' minutes')::interval
FROM base, n8_pool p, generate_series(base.c + 1, :target) n;

-- ---------------------------------------------------------------------------
-- ⑤ 统计信息（不 ANALYZE 查询计划就是旧基数，测出来的时间不可信）
-- ---------------------------------------------------------------------------
ANALYZE biz_task;
ANALYZE biz_voucher;
ANALYZE biz_voucher_entry;
ANALYZE biz_inventory_txn;
ANALYZE operation_logs;

\echo `[gen] 行数：`
SELECT 'biz_task' AS t, count(*) FROM biz_task WHERE task_no LIKE 'N8PERF-%'
UNION ALL SELECT 'biz_voucher', count(*) FROM biz_voucher WHERE summary LIKE 'N8PERF %'
UNION ALL SELECT 'biz_voucher_entry', count(*) FROM biz_voucher_entry WHERE summary LIKE 'N8PERF %'
UNION ALL SELECT 'biz_inventory_txn', count(*) FROM biz_inventory_txn WHERE doc_no LIKE 'N8PERF-%'
UNION ALL SELECT 'operation_logs', count(*) FROM operation_logs WHERE request_id LIKE 'n8perf-%';
