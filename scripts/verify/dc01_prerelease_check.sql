-- ============================================================
-- DC-01（t_4130a770）**上线前排查**：只读，不改任何数据
--
-- 背景：ARCH-3 §5 DC-01 把「审批流未配置 → 静默降级为直接生效」改成**默认拒绝**
--       （409 + `APPROVAL_FLOW_NOT_CONFIGURED` / 66033）。上线后，
--       **没有已发布流程的单据类型会被挡住**，所以必须先确认：
--       ① 8 个流程都是 PUBLISHED（否则当天业务直接停摆）；
--       ② 就算某个流程没发布，该类型当前也**没有在途单据**（在途 = 上线后正要做那个动作的单）。
--
-- 判据：`verdict = 'BLOCKER'` 的行 = 上线当天会被挡住的业务；有一条就不能上，
--       要么先把流程发布，要么先给对应角色开 `business:erp:skip-approval`。
--
-- 用法（生产库只读）：
--   psql -h 127.0.0.1 -U <user> -d cloudpivot -f scripts/verify/dc01_prerelease_check.sql
--
-- ⚠️ 卡面点名的「附录 D.4 的 SQL」在《ARCH-3-模块勾稽关系审计.md》里是 **R-P0-3 / DC-03**
--    （已取消采购单仍能收货）的排查语句，不是本卡的。本卡按 DC-01 的语义另写一份
--    （即本文件），两份都应在上线前跑一遍。
-- ============================================================

\timing off
SET statement_timeout = '15s';

WITH flows (biz_type, flow_code) AS (
    VALUES ('PO_APPROVE', 'po_approve'),
           ('STOCK_IN_POST', 'stock_in_post'),
           ('STOCK_IN_CANCEL', 'stock_in_cancel'),
           ('PAYABLE_PAY', 'payable_pay'),
           ('PAYABLE_CANCEL', 'payable_cancel'),
           ('VOUCHER_REVERSE', 'voucher_reverse'),
           ('STOCK_TRANSFER', 'stock_transfer'),
           ('TASK_ACCEPT', 'task_accept')
),
published AS (
    SELECT f.biz_type,
           f.flow_code,
           EXISTS (
               SELECT 1
                 FROM biz_flow_def d
                WHERE d.deleted_at IS NULL
                  AND d.status = 'PUBLISHED'
                  AND d.current_version_id IS NOT NULL
                  AND (d.biz_type = f.biz_type OR lower(d.flow_code) = f.flow_code)
           ) AS is_published,
           (SELECT d.current_version_no
              FROM biz_flow_def d
             WHERE d.deleted_at IS NULL
               AND d.status = 'PUBLISHED'
               AND d.current_version_id IS NOT NULL
               AND (d.biz_type = f.biz_type OR lower(d.flow_code) = f.flow_code)
             ORDER BY (d.biz_type = f.biz_type) DESC, d.is_default DESC,
                      d.current_version_no DESC, d.updated_at DESC
             LIMIT 1) AS version_no
      FROM flows f
),
inflight (biz_type, action_label, n) AS (
    -- ① 采购单提交审批：草稿
    SELECT 'PO_APPROVE', '采购单提交审批（DRAFT）', count(*)
      FROM biz_purchase_order WHERE deleted_at IS NULL AND status = 'DRAFT'
    UNION ALL
    -- ② 收货单过账：草稿（过账审批中 PENDING_POST 已送审，不受影响）
    SELECT 'STOCK_IN_POST', '收货单过账（DRAFT）', count(*)
      FROM biz_stock_in WHERE deleted_at IS NULL AND status = 'DRAFT'
    UNION ALL
    -- ③ 收货单作废：已过账（作废审批中 PENDING_CANCEL 已送审）
    SELECT 'STOCK_IN_CANCEL', '收货单作废（POSTED）', count(*)
      FROM biz_stock_in WHERE deleted_at IS NULL AND status = 'POSTED'
    UNION ALL
    -- ④ 应付付款：未付清
    SELECT 'PAYABLE_PAY', '应付付款（OPEN / PARTIAL_PAID）', count(*)
      FROM biz_payable WHERE deleted_at IS NULL AND status IN ('OPEN', 'PARTIAL_PAID')
    UNION ALL
    -- ⑤ 应付作废：未付款
    SELECT 'PAYABLE_CANCEL', '应付作废（OPEN）', count(*)
      FROM biz_payable WHERE deleted_at IS NULL AND status = 'OPEN'
    UNION ALL
    -- ⑥ 凭证红冲：已记账且未红冲的蓝字凭证
    SELECT 'VOUCHER_REVERSE', '凭证红冲（POSTED 且未红冲）', count(*)
      FROM biz_voucher
     WHERE deleted_at IS NULL AND status = 'POSTED' AND is_red = 0 AND reversed_by IS NULL
    UNION ALL
    -- ⑦ 调拨提交（本卡不改调拨的降级行为，一并体检配置）
    SELECT 'STOCK_TRANSFER', '调拨提交（DRAFT）', count(*)
      FROM biz_stock_transfer WHERE deleted_at IS NULL AND status = 'DRAFT'
    UNION ALL
    -- ⑧ 任务验收（流程未发布时验收会被流程闸挡住）
    SELECT 'TASK_ACCEPT', '任务验收（REVIEW）', count(*)
      FROM biz_task WHERE deleted_at IS NULL AND status = 'REVIEW'
)
SELECT p.biz_type,
       p.flow_code,
       p.is_published,
       p.version_no,
       i.action_label,
       i.n AS in_flight,
       CASE
           WHEN p.is_published THEN 'OK'
           WHEN i.n = 0 THEN 'OK（未配流程但无在途单）'
           ELSE 'BLOCKER（未配流程且有在途单）'
       END AS verdict
  FROM published p
  JOIN inflight i ON i.biz_type = p.biz_type
 ORDER BY (CASE WHEN p.is_published THEN 1 ELSE 0 END), p.biz_type;
