-- ======================================================
-- V0189 down：撤掉期初余额录入（ERP-R5）
--
-- # 边界（必读）
--
-- 本 down 只撤**结构**与**权限节点**，并且有一道**安全阀**：
--
--   · 库里已经存在 `voucher_type='OPENING'` 的期初凭证时 → `RAISE EXCEPTION` 中止。
--     理由：把白名单收回 `AUTO|MANUAL` 会让这些凭证变成「非法行」；而 `DROP TABLE
--     biz_opening_balance` 又会把它们的取数依据抹掉 —— 账还在、凭据没了，是最坏的形态。
--     正确做法是先在业务上作废期初（下一张卡做「期初凭证作废」），再跑 down。
--   · 期初**数据行**（biz_opening_balance / biz_opening_inventory）随表一起 drop：
--     它们没有独立于表的意义（表没了就是没录入过）。
--   · `biz_inventory` / `biz_inventory_location` / `biz_inventory_txn` 里由库存期初
--     写入的三本账**一行不动** —— 那属于业务数据，回滚迁移不等于反做业务。
--     同理期初凭证本身也不删（它在上面的安全阀里已经要求「要么没有、要么先作废」）。
--
-- 幂等：全部 `IF EXISTS`。
-- ======================================================

DO $$
DECLARE
    n_opening INT;
BEGIN
    SELECT count(*) INTO n_opening
      FROM biz_voucher
     WHERE voucher_type = 'OPENING' AND deleted_at IS NULL;
    IF n_opening > 0 THEN
        RAISE EXCEPTION
            'V0189 down 中止：库里还有 % 张期初凭证（voucher_type=OPENING）。\n'
            '请先在业务上作废期初（或手工删掉这些凭证）再回滚本迁移 —— '
            '否则收回 voucher_type 白名单会让它们在库层面变成非法行。', n_opening;
    END IF;
END $$;

-- ① 权限节点（先删授权行，再删节点）
DELETE FROM sys_role_menu
 WHERE menu_id IN ('20000000-0000-0000-0000-000000001022'::uuid,
                   '20000000-0000-0000-0000-000000001023'::uuid,
                   '20000000-0000-0000-0000-000000001024'::uuid);

DELETE FROM sys_menu
 WHERE id IN ('20000000-0000-0000-0000-000000001022'::uuid,
              '20000000-0000-0000-0000-000000001023'::uuid,
              '20000000-0000-0000-0000-000000001024'::uuid);

-- ② 期初凭证唯一索引
DROP INDEX IF EXISTS uk_biz_voucher_opening_period;

-- ③ 凭证类型白名单收回 AUTO|MANUAL（安全阀已保证没有 OPENING 行）
ALTER TABLE biz_voucher DROP CONSTRAINT IF EXISTS ck_biz_voucher_type;
ALTER TABLE biz_voucher
    ADD CONSTRAINT ck_biz_voucher_type
    CHECK (voucher_type IN ('AUTO', 'MANUAL'));

-- ④ 两张期初表
DROP TABLE IF EXISTS biz_opening_inventory;
DROP TABLE IF EXISTS biz_opening_balance;
