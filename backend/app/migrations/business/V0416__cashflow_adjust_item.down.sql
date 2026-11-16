-- ============================================================================
-- V0416 down：删掉间接法调整项配置（表 + 2 个权限码 + 授权）
--
-- 顺序不能反：`sys_role_menu.menu_id` 有外键指向 `sys_menu`，先删菜单行会失败。
--
-- 护栏（与 V0414 / V0415 的 down 同一口径）：表里存在**非标准模板行**（管理员通过接口
-- 加过的调整项）时拒绝回滚 —— 回滚 = 这些配置永久消失，而「其他调整项」是报表的一部分，
-- 悄悄少一行比报错更危险。要真回滚请先由 DBA 导出这些行（或把它们停用后确认不再需要）。
--
-- 幂等：表/菜单/授权都不存在时按 `NOTICE` 跳过，不报错。
-- ============================================================================
DO $$
DECLARE
    n integer;
BEGIN
    -- ===== 护栏：有非种子行就拒绝 =====
    IF to_regclass('public.biz_cashflow_adjust_item') IS NOT NULL THEN
        EXECUTE $sql$
            SELECT count(*) FROM biz_cashflow_adjust_item
             WHERE item_code NOT IN (
                   'ADJ_ASSET_IMPAIRMENT', 'ADJ_DEPRECIATION', 'ADJ_AMORT_INTANGIBLE',
                   'ADJ_AMORT_LONGTERM', 'ADJ_DISPOSAL_LOSS', 'ADJ_FINANCE_COST',
                   'ADJ_INVEST_LOSS', 'ADJ_DEFERRED_TAX', 'ADJ_INVENTORY_DECREASE',
                   'ADJ_OPER_RECEIVABLE_DECREASE', 'ADJ_OPER_PAYABLE_INCREASE', 'ADJ_OTHER')
        $sql$ INTO n;
        IF n > 0 THEN
            RAISE EXCEPTION 'V0416 down 拒绝执行：间接法调整项里有 % 行是标准模板之外的自建配置，回滚会把它们永久丢掉（请先导出 / 停用并确认不再需要）', n;
        END IF;
    ELSE
        RAISE NOTICE 'V0416 down：biz_cashflow_adjust_item 不存在，按幂等路径跳过';
    END IF;
END $$;

-- ---------------------------------------------------------------------------
-- ① 授权（先删，避开 sys_menu 的外键）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n integer;
BEGIN
    IF to_regclass('public.sys_role_menu') IS NOT NULL THEN
        DELETE FROM sys_role_menu
         WHERE menu_id IN ('20000000-0000-0000-0000-000000001790'::uuid,
                           '20000000-0000-0000-0000-000000001800'::uuid);
        GET DIAGNOSTICS n = ROW_COUNT;
        RAISE NOTICE 'V0416 down：删除调整项按钮授权 % 行', n;
    END IF;
END $$;

-- ---------------------------------------------------------------------------
-- ② 菜单节点
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n integer;
BEGIN
    IF to_regclass('public.sys_menu') IS NOT NULL THEN
        DELETE FROM sys_menu
         WHERE id IN ('20000000-0000-0000-0000-000000001790'::uuid,
                      '20000000-0000-0000-0000-000000001800'::uuid);
        GET DIAGNOSTICS n = ROW_COUNT;
        RAISE NOTICE 'V0416 down：删除调整项按钮节点 % 行', n;
    END IF;
END $$;

-- ---------------------------------------------------------------------------
-- ③ 表（索引 / 约束随表带走）
-- ---------------------------------------------------------------------------
DROP TABLE IF EXISTS biz_cashflow_adjust_item;

-- ---------------------------------------------------------------------------
-- ④ 回滚自检：表 / 节点 / 授权都清干净了
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n integer;
BEGIN
    IF to_regclass('public.biz_cashflow_adjust_item') IS NOT NULL THEN
        RAISE EXCEPTION 'V0416 down 自检失败：biz_cashflow_adjust_item 仍然存在';
    END IF;
    SELECT count(*) INTO n FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_cashflow_adjust_item';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0416 down 自检失败：仍有 % 条索引挂在 biz_cashflow_adjust_item 上', n;
    END IF;
    IF to_regclass('public.sys_menu') IS NOT NULL THEN
        SELECT count(*) INTO n FROM sys_menu
         WHERE id IN ('20000000-0000-0000-0000-000000001790'::uuid,
                      '20000000-0000-0000-0000-000000001800'::uuid);
        IF n <> 0 THEN
            RAISE EXCEPTION 'V0416 down 自检失败：仍有 % 行调整项按钮节点', n;
        END IF;
        SELECT count(*) INTO n FROM sys_role_menu
         WHERE menu_id IN ('20000000-0000-0000-0000-000000001790'::uuid,
                           '20000000-0000-0000-0000-000000001800'::uuid);
        IF n <> 0 THEN
            RAISE EXCEPTION 'V0416 down 自检失败：仍有 % 行授权指向已删菜单', n;
        END IF;
    END IF;
    RAISE NOTICE 'V0416 down 完成：调整项配置表 / 2 个权限码 / 授权 已清空';
END $$;
