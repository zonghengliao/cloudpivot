-- ============================================================================
-- V0411__fx_gain_loss_account.down.sql（M1-06 回滚）
--
-- 顺序反过来：先撤授权、再删权限节点、最后删参数行。
--
-- 【参数行为什么有条件删】
--   只有值仍然是缺省 `5603` 时才删（= 本迁移种下之后没人动过）。
--   值已被人工改成别的科目（例如 5602.09 管理费用-其他）说明那是**真实账务意图**：
--   本卡之前的行为就是硬编码 5603，回滚代码不该顺手把人的配置抹掉。
--   这种情况下保留参数行并打 NOTICE —— 回滚后的代码不再读这个键，留着无害；
--   将来重新 up 时 `WHERE NOT EXISTS` 也不会覆盖它。
--
-- 【幂等】
--   三条 DELETE 都可以反复执行；末尾断言让「删了但没删干净」直接报错。
-- ============================================================================

DO $$
DECLARE
    n integer;
BEGIN
    IF to_regclass('public.sys_menu') IS NULL THEN
        RAISE EXCEPTION 'V0411 down: sys_menu 不存在，库状态异常';
    END IF;

    -- ① 授权
    DELETE FROM sys_role_menu
     WHERE menu_id = '20000000-0000-0000-0000-000000001740'::uuid;

    -- ② 权限节点
    DELETE FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000001740'::uuid;

    SELECT count(*) INTO n
      FROM sys_menu
     WHERE auth_code = 'business:erp:fx-settings:update'
       AND deleted_at IS NULL;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0411 down 失败：business:erp:fx-settings:update 仍残留 % 行菜单', n;
    END IF;

    -- ③ 参数行（只在值仍是缺省 5603 时删）
    IF to_regclass('public.sys_config') IS NOT NULL THEN
        DELETE FROM sys_config
         WHERE config_key = 'erp.fx.gain_loss_account'
           AND config_value = '5603';
        IF EXISTS (SELECT 1 FROM sys_config
                    WHERE config_key = 'erp.fx.gain_loss_account'
                      AND deleted_at IS NULL) THEN
            RAISE NOTICE 'V0411 down：erp.fx.gain_loss_account 已被人工改成非 5603 的值，保留该参数行（回滚后的代码不再读它）';
        END IF;
    END IF;

    RAISE NOTICE 'V0411 down 完成：授权 / 权限节点已删，参数行按是否改过决定去留';
END $$;
