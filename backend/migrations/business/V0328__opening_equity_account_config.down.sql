-- ======================================================
-- V0328 down：删掉「库存期初对方科目」这条系统参数
--
-- 删掉之后代码读不到该键，`opening::inventory_equity_account_code()` 回退常量 3103，
-- 行为与 V0328 之前逐字一致。
-- 幂等：`DELETE ... WHERE config_key = ...` 重复执行删 0 行。
-- ======================================================

DELETE FROM sys_config WHERE config_key = 'inventory_opening_equity_account';
