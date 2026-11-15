-- ============================================================================
-- V0252__menu_expense_icon_bill.up.sql
--
-- 【P2 / t_a43357af】修 V0242 种子菜单「我的报销」的图标：invoice → bill
--
-- 症状
-- ----
-- 侧栏「财务 → 费用报销 → 我的报销」图标渲染空白。常驻防线用例
-- `system::menu::tests::n09_menu_icons_exist_in_tdesign` 确定性报红（串行 / 单跑同样红，
-- 与并发无关）：
--     我的报销 → invoice
--
-- 根因
-- ----
-- `business/V0242__erp_expense_tables.up.sql` 第 263 行种下
--   ('20000000-0000-0000-0000-000000001501', 2, 'business:erp:expense:list',
--    '/business/erp/expense', …, 10, 1, 'invoice', '我的报销（新建 / 提交 / 撤回 / 作废）', …)
-- 而 `invoice` **不在** tdesign-icons-vue-next 的组件集里：
--   frontend/node_modules/tdesign-icons-vue-next/esm/components/invoice.js  → 不存在
-- 同批 V0242 的 money / wallet / folder 都在，只有这一条错。
--
-- 为什么是「新加一条迁移」而不是改 V0242
-- --------------------------------------
-- V0242 已应用到 prod 与所有既有库，`schema_migrations.checksum` 与文件内容绑定，
-- 改一个字节都会让实例启动直接 FATAL（migration version N was modified after it was applied）。
-- 同 V0170（14 条不存在图标的一次性修复）口径：**追加修复**，不动已应用文件。
--
-- 取值
-- ----
-- `bill`（单据 / 账单，语义贴合报销单）。已实测存在：
--   ls frontend/node_modules/tdesign-icons-vue-next/esm/components | grep -x 'bill.js'  → bill.js
--   （同目录还有 bill-filled.js；本仓既有菜单只用线性图标，故取 `bill`）
--
-- 影响面
-- ------
-- 全库普查（干净库 = 193 条迁移全应用后，82 行 icon 非空且未删）：
--   只有这 1 行不在图标集里，其余 81 行（45 种取值）逐条核对均真实存在。
-- 故本迁移只改这 1 行，不做批量重映射。
--
-- 幂等 / 安全
-- -----------
-- · WHERE 带 `icon = 'invoice'`：客户已自行改成别的图标 → 0 行受影响，NOTICE 跳过；
-- · 菜单行不存在（客户库被裁过）→ NOTICE 跳过，不报错；
-- · 回滚见 .down.sql（改回 invoice，仅当当前值是 bill 时）。
-- ============================================================================

DO $$
DECLARE
    c_id    CONSTANT uuid := '20000000-0000-0000-0000-000000001501';
    v_name  text;
    v_icon  text;
    n_hit   int;
BEGIN
    SELECT name, icon INTO v_name, v_icon
      FROM sys_menu
     WHERE id = c_id AND deleted_at IS NULL;

    IF NOT FOUND THEN
        RAISE NOTICE 'V0252：菜单 % 「我的报销」不存在（客户库已裁剪），跳过', c_id;
        RETURN;
    END IF;

    IF v_icon IS DISTINCT FROM 'invoice' THEN
        RAISE NOTICE 'V0252：菜单「%」当前 icon = %，不是 invoice（已修过或客户自定义），跳过',
            v_name, coalesce(v_icon, '<null>');
        RETURN;
    END IF;

    UPDATE sys_menu
       SET icon = 'bill', updated_at = now()
     WHERE id = c_id AND icon = 'invoice';
    GET DIAGNOSTICS n_hit = ROW_COUNT;

    IF n_hit <> 1 THEN
        RAISE EXCEPTION 'V0252 自检失败：期望更新 1 行，实际 %', n_hit;
    END IF;

    -- 自检：落库值必须是真实图标名（与 n09 的判据同源）
    IF (SELECT icon FROM sys_menu WHERE id = c_id) <> 'bill' THEN
        RAISE EXCEPTION 'V0252 自检失败：icon 未落成 bill';
    END IF;

    RAISE NOTICE 'V0252：「%」图标 invoice → bill 已修（1 行）', v_name;
END $$;
