-- 回滚：清掉 title（依赖 V0154 的 down 一起回滚）
BEGIN;
UPDATE sys_menu SET title = NULL, updated_at = now() WHERE path LIKE '%erp%';
COMMIT;
