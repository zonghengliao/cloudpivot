-- ======================================================
-- V0242 down：撤掉报销四表 + 种子 + 权限 + 菜单
--
-- 顺序：授权 → 菜单 → 子表 → 主表 → 主数据。
-- `biz_expense_item` / `biz_expense_payment` 对主表有外键，必须先删；
-- 主表对凭证 / 审批实例只是弱引用（无外键），删表不会连带删凭证 ——
-- 演练库以外**不要**在有业务数据时跑这个 down（凭证会变成孤儿）。
-- ======================================================

-- ---------- ① 授权 ----------
DELETE FROM sys_role_menu
 WHERE menu_id IN (SELECT id FROM sys_menu
                    WHERE id::text LIKE '20000000-0000-0000-0000-0000000015%');

-- ---------- ② 菜单 / 权限码 ----------
DELETE FROM sys_menu WHERE id::text LIKE '20000000-0000-0000-0000-0000000015%';

-- ---------- ③ 业务表（子 → 主 → 主数据） ----------
DROP TABLE IF EXISTS biz_expense_payment;
DROP TABLE IF EXISTS biz_expense_item;
DROP TABLE IF EXISTS biz_expense_report;
DROP TABLE IF EXISTS biz_expense_type;
