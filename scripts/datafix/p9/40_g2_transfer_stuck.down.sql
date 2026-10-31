-- P9 / G2 回滚：把两张调拨改回 PENDING（只改本脚本动过的行）
UPDATE biz_stock_transfer
   SET status = 'PENDING',
       cancelled_at = NULL,
       cancel_reason = NULL,
       updated_at = now()
 WHERE transfer_no IN ('TK-20261031-000002', 'TK-20261031-000004')
   AND status = 'CANCELLED'
   AND cancel_reason LIKE 'P9 数据修复%';
