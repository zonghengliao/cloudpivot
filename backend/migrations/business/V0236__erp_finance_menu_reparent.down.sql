-- ============================================================================
-- V0236__erp_finance_menu_reparent.down.sql
--
-- 回滚【P2-2B-FIX / t_d1a5324f】：把 up 挪走的两条菜单节点还原成 V0189 / V0190 种下的原样：
--     期初余额 /business/erp/opening   → parent = …946（采购管理），sort_order = 25（V0189:132-133）
--     年度结转 /business/erp/year-end  → parent = …946（采购管理），sort_order = 26（V0190:86-87）
--
-- 说明：
--   · 只翻回 parent_id / sort_order 两列；节点本身（id / title / auth_code / component / 角色授权）
--     up 期间一个字没动，所以回滚后侧边栏与改前完全一致；
--   · 采购管理目录按 **path** 定位（不按 title 硬编码，运维改过中文名也能回滚）；
--   · 幂等：重复执行结果一致；自检断言的是「回滚后的稳定状态」。
--   · 按 path 找不到采购管理目录时 **不静默跳过**（否则两条节点会变成 parent_id 悬空/或停在财务组，
--     两种都比报错更难查）—— 直接 RAISE。
-- ============================================================================

-- ---------- ① 前置自检 ----------
DO $$
DECLARE
    n_purchase int;
    n_opening  int;
    n_yearend  int;
BEGIN
    SELECT count(*) INTO n_purchase FROM sys_menu WHERE path = '/business/erp/purchase-mgmt' AND deleted_at IS NULL;
    SELECT count(*) INTO n_opening  FROM sys_menu WHERE path = '/business/erp/opening'       AND deleted_at IS NULL;
    SELECT count(*) INTO n_yearend  FROM sys_menu WHERE path = '/business/erp/year-end'      AND deleted_at IS NULL;

    IF n_purchase <> 1 THEN
        RAISE EXCEPTION 'V0236 down 前置自检失败：/business/erp/purchase-mgmt 在用节点数 = %（必须为 1）', n_purchase;
    END IF;
    IF n_opening <> 1 THEN
        RAISE EXCEPTION 'V0236 down 前置自检失败：/business/erp/opening 在用节点数 = %（必须为 1）', n_opening;
    END IF;
    IF n_yearend <> 1 THEN
        RAISE EXCEPTION 'V0236 down 前置自检失败：/business/erp/year-end 在用节点数 = %（必须为 1）', n_yearend;
    END IF;
END $$;

-- ---------- ② 还原两条节点 ----------
UPDATE sys_menu m
   SET parent_id  = p.id,
       sort_order = 25,
       updated_at = now()
  FROM sys_menu p
 WHERE p.path = '/business/erp/purchase-mgmt' AND p.deleted_at IS NULL
   AND m.path = '/business/erp/opening'       AND m.deleted_at IS NULL;

UPDATE sys_menu m
   SET parent_id  = p.id,
       sort_order = 26,
       updated_at = now()
  FROM sys_menu p
 WHERE p.path = '/business/erp/purchase-mgmt' AND p.deleted_at IS NULL
   AND m.path = '/business/erp/year-end'      AND m.deleted_at IS NULL;

-- ---------- ③ 回滚后自检 ----------
DO $$
DECLARE
    v_open_path text;
    v_ye_path   text;
    v_open_ord  int;
    v_ye_ord    int;
BEGIN
    SELECT p.path, m.sort_order INTO v_open_path, v_open_ord
      FROM sys_menu m LEFT JOIN sys_menu p ON p.id = m.parent_id
     WHERE m.path = '/business/erp/opening' AND m.deleted_at IS NULL;
    SELECT p.path, m.sort_order INTO v_ye_path, v_ye_ord
      FROM sys_menu m LEFT JOIN sys_menu p ON p.id = m.parent_id
     WHERE m.path = '/business/erp/year-end' AND m.deleted_at IS NULL;

    IF v_open_path IS DISTINCT FROM '/business/erp/purchase-mgmt' OR v_open_ord <> 25 THEN
        RAISE EXCEPTION 'V0236 down 自检失败：期初余额 = % / sort %（应为 /business/erp/purchase-mgmt / 25）',
            coalesce(v_open_path, 'NULL'), v_open_ord;
    END IF;
    IF v_ye_path IS DISTINCT FROM '/business/erp/purchase-mgmt' OR v_ye_ord <> 26 THEN
        RAISE EXCEPTION 'V0236 down 自检失败：年度结转 = % / sort %（应为 /business/erp/purchase-mgmt / 26）',
            coalesce(v_ye_path, 'NULL'), v_ye_ord;
    END IF;
END $$;
