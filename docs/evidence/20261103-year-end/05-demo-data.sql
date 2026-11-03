-- ======================================================
-- YEAR-END 真机演示数据（卡 t_f530126c）
--
-- 场景：**2040 年度已有 3 张挡路单据**，且 2041 年度已开账（否则还要多一个
-- 「下一年度未开账」阻塞项，遮住明细表）。
--
--   ① 2040 全年 12 个月 = CLOSED（年结前置：本年必须全部结账）
--   ② 2041 全年 12 个月 = OPEN  （年结要往 2041-01 写期初）
--   ③ 挡路单据三张（三种「该谁处理」都覆盖到）：
--        PC-YE-PO-2040-001  SUBMITTED 采购单 → 去审批 → 处理人是「采购经理」角色（2 人 → 只给岗位）
--        PC-YE-IN-2040-001  DRAFT     收货单 → 去过账 → 处理人是「仓管」角色（3 人 → 只给岗位）
--        PC-YE-TR-2040-001  PENDING   调拨单 → 去审批 → **没有角色持有 transfer:status** → 待分配
--
-- 为什么用 2040/2041：2026 是生产年度、2031–2038 被别的卡的夹具占用；
-- 2040/2041 全库无主（`grep -r 2040 backend/src` 只有本文件与它的证据脚本）。
--
-- 幂等：先按前缀删一遍再插，可重复执行。
-- 复原（处理掉单据后再变回「三张都挡路」）：重跑本文件即可。
-- ======================================================

BEGIN;

-- ---------- ① 清场（只删本卡演示数据）----------
DELETE FROM sys_notice
 WHERE biz_type IN ('year_end_remind', 'period_remind')
   AND biz_id IN (SELECT id::text FROM biz_purchase_order WHERE order_no LIKE 'PC-YE-%'
                  UNION SELECT id::text FROM biz_stock_in WHERE in_no LIKE 'PC-YE-%'
                  UNION SELECT id::text FROM biz_stock_transfer WHERE transfer_no LIKE 'PC-YE-%');
DELETE FROM biz_purchase_order WHERE order_no LIKE 'PC-YE-%';
DELETE FROM biz_stock_in WHERE in_no LIKE 'PC-YE-%';
DELETE FROM biz_stock_transfer WHERE transfer_no LIKE 'PC-YE-%';
DELETE FROM biz_status_log WHERE entity_type = 'ACCOUNTING_PERIOD'
   AND (entity_no LIKE '2040-%' OR entity_no LIKE '2041-%');
DELETE FROM biz_status_log WHERE entity_type = 'ACCOUNTING_YEAR' AND entity_no IN ('2040', '2041');
DELETE FROM biz_opening_balance WHERE period_code LIKE '2040-%' OR period_code LIKE '2041-%';
DELETE FROM biz_voucher WHERE period LIKE '2040-%' OR period LIKE '2041-%';
DELETE FROM biz_accounting_period WHERE period_code LIKE '2040-%' OR period_code LIKE '2041-%';

-- ---------- ② 24 个期间（2040 已结账 / 2041 已开账）----------
INSERT INTO biz_accounting_period (id, period_year, period_month, period_code, start_date, end_date, status, closed_at, remark)
SELECT gen_random_uuid(),
       2040,
       m,
       '2040-' || lpad(m::text, 2, '0'),
       make_date(2040, m, 1),
       (make_date(2040, m, 1) + interval '1 month - 1 day')::date,
       'CLOSED',
       timestamptz '2041-01-05 10:00:00+08',
       'YEAR-END 演示：本年已逐月结账'
  FROM generate_series(1, 12) AS m;

INSERT INTO biz_accounting_period (id, period_year, period_month, period_code, start_date, end_date, status, remark)
SELECT gen_random_uuid(),
       2041,
       m,
       '2041-' || lpad(m::text, 2, '0'),
       make_date(2041, m, 1),
       (make_date(2041, m, 1) + interval '1 month - 1 day')::date,
       'OPEN',
       'YEAR-END 演示：下一年度已开账'
  FROM generate_series(1, 12) AS m;

-- ---------- ③ 三张挡路单据 ----------
-- 采购单：SUBMITTED（待审批）→ 去审批
--   金额要满足 ck_biz_po_tax_sum（total_with_tax = total_amount + total_tax）
INSERT INTO biz_purchase_order (id, order_no, supplier_id, order_date, warehouse_id, status, create_by,
                                total_amount, total_tax, total_with_tax)
SELECT gen_random_uuid(), 'PC-YE-PO-2040-001', s.id, date '2040-11-18', w.id, 'SUBMITTED',
       u.id, 7787.61, 1012.39, 8800.00
  FROM (SELECT id FROM biz_supplier WHERE deleted_at IS NULL ORDER BY id LIMIT 1) s,
       (SELECT id FROM biz_warehouse WHERE deleted_at IS NULL ORDER BY id LIMIT 1) w,
       (SELECT id FROM sys_user WHERE username = 'admin' LIMIT 1) u;

-- 收货单：DRAFT（未过账）→ 去过账（无源入库，见 V0199 的三条 CHECK）
INSERT INTO biz_stock_in (id, in_no, in_type, warehouse_id, in_date, status, create_by, is_sourceless,
                          total_amount, total_tax, total_with_tax)
SELECT gen_random_uuid(), 'PC-YE-IN-2040-001', 'OTHER', w.id, date '2040-12-06', 'DRAFT',
       u.id, TRUE, 495.58, 64.42, 560.00
  FROM (SELECT id FROM biz_warehouse WHERE deleted_at IS NULL ORDER BY id LIMIT 1) w,
       (SELECT id FROM sys_user WHERE username = 'admin' LIMIT 1) u;

-- 调拨单：PENDING（待审批）→ 去审批（需要两个仓库；ck 要求 submitted_at 非空）
INSERT INTO biz_stock_transfer (id, transfer_no, from_warehouse_id, to_warehouse_id, status, transfer_date,
                                create_by, submitted_at, total_amount)
SELECT gen_random_uuid(), 'PC-YE-TR-2040-001', w1.id, w2.id, 'PENDING', date '2040-12-20',
       u.id, timestamptz '2040-12-20 09:30:00+08', 1200.00
  FROM (SELECT id FROM biz_warehouse WHERE deleted_at IS NULL ORDER BY id LIMIT 1 OFFSET 0) w1,
       (SELECT id FROM biz_warehouse WHERE deleted_at IS NULL ORDER BY id LIMIT 1 OFFSET 1) w2,
       (SELECT id FROM sys_user WHERE username = 'admin' LIMIT 1) u;

COMMIT;

-- ---------- ④ 自检 ----------
DO $$
DECLARE
    n_closed int;
    n_open   int;
    n_docs   int;
BEGIN
    SELECT count(*) INTO n_closed FROM biz_accounting_period
     WHERE period_code LIKE '2040-%' AND status = 'CLOSED';
    SELECT count(*) INTO n_open FROM biz_accounting_period
     WHERE period_code LIKE '2041-%' AND status = 'OPEN';
    SELECT (SELECT count(*) FROM biz_purchase_order WHERE order_no LIKE 'PC-YE-PO-%')
         + (SELECT count(*) FROM biz_stock_in WHERE in_no LIKE 'PC-YE-IN-%')
         + (SELECT count(*) FROM biz_stock_transfer WHERE transfer_no LIKE 'PC-YE-TR-%')
      INTO n_docs;
    IF n_closed <> 12 OR n_open <> 12 OR n_docs <> 3 THEN
        RAISE EXCEPTION '演示数据不完整：2040 已结账 % 个月 / 2041 已开账 % 个月 / 挡路单据 % 张（期望 12/12/3）',
            n_closed, n_open, n_docs;
    END IF;
    RAISE NOTICE 'YEAR-END 演示数据就绪：2040 已结账 12 个月、2041 已开账 12 个月、挡路单据 3 张';
END $$;
