-- V0226 down：撤掉 ARCH-6 第 0 期的科目与配置改动。
--
-- ⚠️ 顺序与 up 相反：先删子科目 → 父科目才有条件翻回末级。
-- ⚠️ 只在「该父科目已无任何子科目」时翻回 `is_leaf = 1`：若客户在这之后再挂了别的子科目
--    （或历史数据里有），硬翻会造出「有子科目却标末级」的脏结构。
DELETE FROM biz_account WHERE account_code = '2241.01';
DELETE FROM biz_account WHERE account_code LIKE '5602.%';

UPDATE biz_account parent
   SET is_leaf = 1, updated_at = now()
 WHERE parent.account_code IN ('2241', '5602')
   AND NOT EXISTS (
       SELECT 1 FROM biz_account child
        WHERE child.parent_id = parent.id AND child.deleted_at IS NULL
   );

UPDATE biz_account
   SET aux_dimensions = NULL, updated_at = now()
 WHERE account_code IN ('5601', '5602', '5603', '5711')
   AND aux_dimensions = 'DEPT';

DELETE FROM sys_config
 WHERE config_key IN ('erp.budget.control_mode', 'erp.budget.enforce_on_po',
                      'erp.expense.pay_remind_days');
