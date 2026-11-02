-- ============================================================
-- V0197：单据追溯只读视图 v_biz_doc_edge / v_biz_doc_ref（ARCH-2 汇总定稿 · 第五处关注点）
--
-- 口径来源：docs/architecture/合同与勾稽关系设计.md §6.6
-- 设计来源：docs/evidence/2026-11-02-arch2-contract/V0193__erp_contract.up.sql §5（草稿）
--
-- 为什么是视图而不是表：每条边都来自真实列（source_order_id / contract_id / source_type+source_id）。
-- 建 biz_doc_link 之类的「关系表」= 第二真相：边写两处，删单/改单后必然不一致。
-- 前置：V0193（biz_contract）+ V0194（biz_purchase_order.contract_id）已落地；本迁移不写任何数据。
-- ============================================================

CREATE OR REPLACE VIEW v_biz_doc_edge AS
    SELECT 'STOCK_IN'::varchar(32)        AS child_type,
           s.id                           AS child_id,
           'PURCHASE_ORDER'::varchar(32)  AS parent_type,
           s.source_order_id              AS parent_id,
           'source_order_id'::varchar(32) AS via_column
      FROM biz_stock_in s
     WHERE s.deleted_at IS NULL AND s.source_order_id IS NOT NULL
    UNION ALL
    SELECT 'PURCHASE_ORDER', o.id, 'CONTRACT', o.contract_id, 'contract_id'
      FROM biz_purchase_order o
     WHERE o.deleted_at IS NULL AND o.contract_id IS NOT NULL
    UNION ALL
    SELECT 'PAYABLE', p.id, p.source_type, p.source_id, 'source_type/source_id'
      FROM biz_payable p
     WHERE p.deleted_at IS NULL
    UNION ALL
    SELECT 'VOUCHER', v.id, v.source_type, v.source_id, 'source_type/source_id'
      FROM biz_voucher v
     WHERE v.deleted_at IS NULL AND v.source_id IS NOT NULL;

COMMENT ON VIEW v_biz_doc_edge IS
  '单据父子边（只读视图）：每条边都来自真实列，不新增存储。追溯链用 recursive CTE 走它；**禁止**再建 biz_doc_link 表（那是第二真相）';

CREATE OR REPLACE VIEW v_biz_doc_ref AS
    SELECT 'CONTRACT'::varchar(32) AS doc_type, c.id, c.contract_no AS doc_no, c.contract_name AS doc_title, c.status
      FROM biz_contract c WHERE c.deleted_at IS NULL
    UNION ALL
    SELECT 'PURCHASE_ORDER', o.id, o.order_no, o.order_no, o.status
      FROM biz_purchase_order o WHERE o.deleted_at IS NULL
    UNION ALL
    SELECT 'STOCK_IN', s.id, s.in_no, s.in_no, s.status
      FROM biz_stock_in s WHERE s.deleted_at IS NULL
    UNION ALL
    SELECT 'PAYABLE', p.id, p.payable_no, p.payable_no, p.status
      FROM biz_payable p WHERE p.deleted_at IS NULL
    UNION ALL
    SELECT 'VOUCHER', v.id, v.voucher_no, v.summary, v.status
      FROM biz_voucher v WHERE v.deleted_at IS NULL
    UNION ALL
    SELECT 'STOCK_TRANSFER', t.id, t.transfer_no, t.transfer_no, t.status
      FROM biz_stock_transfer t WHERE t.deleted_at IS NULL;

COMMENT ON VIEW v_biz_doc_ref IS
  '单据号索引（只读视图）：doc_type+id → 单号/标题/状态。追溯链每一跳靠它取可读信息，前端不需要 N 次请求';
