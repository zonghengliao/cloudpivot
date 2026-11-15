-- ============================================================================
-- V0410__period_fx_revaluation（down）
--
-- 只删本卡种的两类行：菜单按钮节点 + 它的角色授权。
-- **不删**任何凭证 / 分录 / 期间 / 汇率数据（调汇凭证是真实记账依据，回滚代码不等于抹账）。
--
-- 护栏：库里已存在 `FX_REVAL_ACCOUNT` 凭证时拒绝执行 —— 说明这个能力已经被真实使用过，
-- 删权限节点会让「谁有权调汇」失去追溯。要先红冲那些凭证（业务动作），再回滚本迁移。
-- ============================================================================
DO $$
DECLARE
    n integer;
BEGIN
    IF to_regclass('public.biz_voucher') IS NOT NULL THEN
        SELECT count(*) INTO n
          FROM biz_voucher
         WHERE source_type = 'FX_REVAL_ACCOUNT'
           AND deleted_at IS NULL;
        IF n > 0 THEN
            RAISE EXCEPTION 'V0410 down 拒绝执行：库里已有 % 张期末调汇凭证（source_type = FX_REVAL_ACCOUNT）；'
                            '先按业务口径红冲它们，再回滚（回滚代码不该抹掉记账依据）', n;
        END IF;
    END IF;
END $$;

DELETE FROM sys_role_menu
 WHERE menu_id = '20000000-0000-0000-0000-000000001730'::uuid;

DELETE FROM sys_menu
 WHERE id = '20000000-0000-0000-0000-000000001730'::uuid
   AND auth_code = 'business:erp:period:fx-reval';

DO $$
DECLARE
    n integer;
BEGIN
    SELECT count(*) INTO n
      FROM sys_menu
     WHERE auth_code = 'business:erp:period:fx-reval';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0410 down 失败：权限节点残留 % 行', n;
    END IF;
    SELECT count(*) INTO n
      FROM sys_role_menu
     WHERE menu_id = '20000000-0000-0000-0000-000000001730'::uuid;
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0410 down 失败：授权行残留 % 行', n;
    END IF;
    RAISE NOTICE 'V0410 down 完成：权限节点与授权行已清空（凭证 / 期间数据未动）';
END $$;
