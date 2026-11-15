-- ======================================================
-- V0329 down：撤掉「负数重分类」配置
--
-- 只删本迁移加的列（取值随之消失），不动其它报表项配置：回滚后「应交税费」恢复按
-- 科目余额方向取数（借方余额就显示负数），即 V0183 以来的行为。
-- 幂等：`DROP COLUMN IF EXISTS`。
-- ======================================================

ALTER TABLE biz_report_item DROP COLUMN IF EXISTS reclass_negative_to;

UPDATE biz_report_item
   SET remark = replace(
           remark,
           '；FIX-6：留抵税额（应交税费期末借方余额）重分类到本行，列报为「待抵扣进项税额」',
           ''
       )
 WHERE report_code = 'BALANCE_SHEET'
   AND item_code = 'OTHER_CURRENT_ASSETS'
   AND coalesce(remark, '') LIKE '%留抵税额%';
