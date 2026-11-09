\pset pager off
\timing on
\set QUIET on
\echo ================ A. 任务列表 count（100k） ================
EXPLAIN (ANALYZE, BUFFERS, TIMING)
SELECT COUNT(*) FROM (
  SELECT t.id, t.parent_id, t.task_no, t.title, t.status, t.priority, t.progress, t.need_review,
         t.owner_id, COALESCE(NULLIF(u.real_name, ''), u.username, '') AS owner_name,
         t.dept_id, t.source, t.start_date, t.due_date, t.created_at, t.updated_at,
         (SELECT count(*) FROM biz_task c WHERE c.parent_id = t.id AND c.deleted_at IS NULL) AS child_count,
         (SELECT count(*) FROM biz_task c WHERE c.parent_id = t.id AND c.deleted_at IS NULL AND c.status = 'DONE') AS child_done_count,
         t.project_id, COALESCE(pj.project_no, '') AS project_code,
         COALESCE(pj.project_name, '') AS project_name,
         t.doc_type, t.doc_id, t.flow_instance_id, t.flow_node_key, t.flow_round
    FROM biz_task t
    LEFT JOIN sys_user u ON u.id = t.owner_id
    LEFT JOIN biz_project pj ON pj.id = t.project_id AND pj.deleted_at IS NULL
   WHERE t.deleted_at IS NULL AND ( (1 = 1 AND 1 = 1) OR t.id = ANY('{}'::text::uuid[]) )
) AS _scope_count;

\echo ================ B. 任务列表 第1页 / 第1000页 ================
EXPLAIN (ANALYZE, BUFFERS, TIMING)
SELECT t.id, t.parent_id, t.task_no, t.title, t.status, t.priority, t.progress, t.need_review,
       t.owner_id, COALESCE(NULLIF(u.real_name, ''), u.username, '') AS owner_name,
       t.dept_id, t.source, t.start_date, t.due_date, t.created_at, t.updated_at,
       (SELECT count(*) FROM biz_task c WHERE c.parent_id = t.id AND c.deleted_at IS NULL) AS child_count,
       (SELECT count(*) FROM biz_task c WHERE c.parent_id = t.id AND c.deleted_at IS NULL AND c.status = 'DONE') AS child_done_count,
       t.project_id, COALESCE(pj.project_no, '') AS project_code,
       COALESCE(pj.project_name, '') AS project_name,
       t.doc_type, t.doc_id, t.flow_instance_id, t.flow_node_key, t.flow_round
  FROM biz_task t
  LEFT JOIN sys_user u ON u.id = t.owner_id
  LEFT JOIN biz_project pj ON pj.id = t.project_id AND pj.deleted_at IS NULL
 WHERE t.deleted_at IS NULL AND ( (1 = 1 AND 1 = 1) OR t.id = ANY('{}'::text::uuid[]) )
 ORDER BY t.due_date ASC NULLS LAST, t.created_at DESC, t.id DESC
 LIMIT 20 OFFSET 0;

EXPLAIN (ANALYZE, BUFFERS, TIMING)
SELECT t.id, t.task_no, t.title, t.due_date, t.created_at
  FROM biz_task t
 WHERE t.deleted_at IS NULL AND ( (1 = 1 AND 1 = 1) OR t.id = ANY('{}'::text::uuid[]) )
 ORDER BY t.due_date ASC NULLS LAST, t.created_at DESC, t.id DESC
 LIMIT 20 OFFSET 19980;

\echo ================ C. 凭证列表 count / 第1000页 ================
EXPLAIN (ANALYZE, BUFFERS, TIMING)
SELECT COUNT(*) FROM (
  SELECT id, voucher_word, voucher_no, period, voucher_date, voucher_type, source_type, source_no,
         summary, total_debit, total_credit, entry_count, status
    FROM biz_voucher WHERE deleted_at IS NULL
) AS _scope_count;

EXPLAIN (ANALYZE, BUFFERS, TIMING)
SELECT id, voucher_word, voucher_no, period, voucher_date, voucher_type, summary, total_debit, status
  FROM biz_voucher WHERE deleted_at IS NULL
 ORDER BY voucher_date DESC, voucher_word DESC, voucher_no DESC
 LIMIT 20 OFFSET 19980;

\echo ================ D. 试算平衡 2026-11（全表聚合） ================
EXPLAIN (ANALYZE, BUFFERS, TIMING)
WITH opening AS (
   SELECT e.account_id, SUM(e.debit_amount) AS d, SUM(e.credit_amount) AS c
     FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id
    WHERE v.status = 'POSTED' AND v.deleted_at IS NULL AND v.voucher_date < DATE '2026-11-01'
    GROUP BY e.account_id
), current AS (
   SELECT e.account_id, SUM(e.debit_amount) AS d, SUM(e.credit_amount) AS c
     FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id
    WHERE v.status = 'POSTED' AND v.deleted_at IS NULL
      AND v.voucher_date BETWEEN DATE '2026-11-01' AND DATE '2026-11-30'
    GROUP BY e.account_id
)
SELECT a.id, a.account_code, a.account_name,
       COALESCE(o.d, 0)::numeric(20, 4) AS opening_debit_total,
       COALESCE(o.c, 0)::numeric(20, 4) AS opening_credit_total,
       COALESCE(cu.d, 0)::numeric(20, 4) AS period_debit,
       COALESCE(cu.c, 0)::numeric(20, 4) AS period_credit
  FROM biz_account a
  LEFT JOIN opening o ON o.account_id = a.id
  LEFT JOIN current cu ON cu.account_id = a.id
 WHERE a.deleted_at IS NULL
 ORDER BY a.account_code;

\echo ================ E. 资产负债表 load_accounts（全表聚合 x2） ================
EXPLAIN (ANALYZE, BUFFERS, TIMING)
SELECT a.id, a.account_code, a.account_name, a.account_category, a.balance_direction, a.is_leaf, a.level,
       COALESCE(o.amount, 0) AS net_before, COALESCE(p.amount, 0) AS net_period
  FROM biz_account a
  LEFT JOIN (
       SELECT e.account_id, SUM(e.debit_amount - e.credit_amount) AS amount
         FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id
        WHERE v.status = 'POSTED' AND v.deleted_at IS NULL
          AND (v.voucher_date < DATE '2026-11-01'
               OR (v.voucher_type = 'OPENING'
                   AND (v.voucher_date BETWEEN DATE '2026-11-01' AND DATE '2026-11-30'
                        OR v.period = NULLIF('2026-11'::text, ''))))
        GROUP BY e.account_id
  ) o ON o.account_id = a.id
  LEFT JOIN (
       SELECT e.account_id, SUM(e.debit_amount - e.credit_amount) AS amount
         FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id
        WHERE v.status = 'POSTED' AND v.deleted_at IS NULL
          AND v.voucher_type NOT IN ('OPENING', 'YEAR_END')
          AND v.voucher_date >= DATE '2026-11-01' AND v.voucher_date <= DATE '2026-11-30'
        GROUP BY e.account_id
  ) p ON p.account_id = a.id
 WHERE a.deleted_at IS NULL
 ORDER BY a.account_code;

\echo ================ F. 资产负债表 分录合计 / 不平凭证 ================
EXPLAIN (ANALYZE, BUFFERS, TIMING)
SELECT COALESCE(SUM(e.debit_amount), 0) AS debit, COALESCE(SUM(e.credit_amount), 0) AS credit
  FROM biz_voucher_entry e JOIN biz_voucher v ON v.id = e.voucher_id
 WHERE v.status = 'POSTED' AND v.deleted_at IS NULL
   AND v.voucher_date >= DATE '2026-11-01' AND v.voucher_date <= DATE '2026-11-30';

EXPLAIN (ANALYZE, BUFFERS, TIMING)
SELECT v.voucher_no, v.voucher_date, v.total_debit, v.total_credit,
       COALESCE(SUM(e.debit_amount), 0) AS entry_debit,
       COALESCE(SUM(e.credit_amount), 0) AS entry_credit
  FROM biz_voucher v LEFT JOIN biz_voucher_entry e ON e.voucher_id = v.id
 WHERE v.status = 'POSTED' AND v.deleted_at IS NULL
   AND v.voucher_date >= DATE '2026-11-01' AND v.voucher_date <= DATE '2026-11-30'
 GROUP BY v.id, v.voucher_no, v.voucher_date, v.total_debit, v.total_credit
HAVING COALESCE(SUM(e.debit_amount), 0) <> v.total_debit
    OR COALESCE(SUM(e.credit_amount), 0) <> v.total_credit
 ORDER BY v.voucher_date, v.voucher_no
 LIMIT 20;

\echo ================ G. 库存流水 count / 第1页 ================
EXPLAIN (ANALYZE, BUFFERS, TIMING)
SELECT COUNT(*) FROM (SELECT id, material_id, warehouse_id, txn_type, doc_no, qty, created_at
                        FROM biz_inventory_txn WHERE 1 = 1) AS _scope_count;

EXPLAIN (ANALYZE, BUFFERS, TIMING)
SELECT id, material_id, warehouse_id, txn_type, doc_no, qty, created_at
  FROM biz_inventory_txn WHERE 1 = 1
 ORDER BY created_at DESC, id DESC LIMIT 20 OFFSET 0;

\echo ================ H. 操作日志 count / 第1000页 ================
EXPLAIN (ANALYZE, BUFFERS, TIMING)
SELECT COUNT(*) FROM (SELECT id FROM operation_logs WHERE 1 = 1) AS _scope_count;

EXPLAIN (ANALYZE, BUFFERS, TIMING)
SELECT id, request_id, username, operation, status_code, created_at
  FROM operation_logs WHERE 1 = 1
 ORDER BY created_at DESC LIMIT 20 OFFSET 19980;
