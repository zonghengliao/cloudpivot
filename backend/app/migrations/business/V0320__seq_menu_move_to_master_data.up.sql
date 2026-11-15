-- ============================================================================
-- V0320：单号序列归位「基础数据」（MENU-01B，卡 t_a578f919）
--
-- ## 为什么
--
-- 用户明确指示：「单号规则属于配置。」
--
-- 单号序列（编码规则配置）是**主数据/配置类**页面，却一直挂在「财务管理」目录下：
--
--   /business/erp/seq  parent = 20000000-…0e02（财务管理 /business/erp/finance-mgmt）
--                      sort_order = 50
--
-- 它是纯配置维护页（前缀 / 日期段 / 流水位数 / 步长），与会计期间、总账、凭证
-- 这些「财务业务」不同类；同类的物料管理、计量单位都在基础数据下。故归位：
--
--   /business/erp/seq  parent = 20000000-…0e03（基础数据 /business/erp/master-data）
--                      sort_order = 30
--
-- ## 历史（为什么要一条新迁移而不是改旧的）
--
--   ① V0141 把单号序列插到 …946 下；
--   ② V0154（erp 业务线重组）把它显式 UPDATE 到 …0e02 / sort 50；
--   ③ V0319（菜单结构基线）只碰 parent / 名称 / 可见性 / 排序的 13 项差异，
--      不涉及本条菜单的归属。
--   本迁移是第 ④ 步。V0319 已应用（145 dev 有记录），按规矩**不改历史迁移**，
--   归属变更只能新增一条。
--
-- ## 基础数据处的落点
--
--   10 物料管理 /business/erp/material
--   20 计量单位 /business/erp/unit
--   30 单号序列 /business/erp/seq     ← 本迁移
--
-- ## 财务管理剔除 seq 后无需调 sort
--
--   10 会计科目 / 20 会计期间 / 21 期初余额 / 22 年度结转 / 25 预算管理 /
--   27 费用报销 / 29 固定资产 / 30 记账凭证 / 40 总账 / 45-47 报表 / 48 报表项维护
--   —— seq 原本在 50（末尾），挪走不留空洞。
--
-- ## 只改两列
--
--   只 UPDATE parent_id + sort_order（+ updated_at）。不 DELETE、不碰
--   sys_role_menu（位置变了，权限不变）、不动 name/title/icon/is_visible/auth_code。
--   单号序列节点 id 不变（…0952），故其自身子节点（若有）与授权关系全部保持。
--
-- ## 幂等
--
--   UPDATE 带 `IS DISTINCT FROM` 守卫：已是目标值时为 0 行。
--   回滚：V0320__seq_menu_move_to_master_data.down.sql（退回 …0e02 / sort 50）。
-- ============================================================================

-- ---------- ① 前置断言：目标父级必须存在且是「基础数据」 ----------
DO $$
DECLARE
    v_parent_name text;
    v_parent_path text;
BEGIN
    SELECT name, path INTO v_parent_name, v_parent_path
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000000e03'::uuid
       AND deleted_at IS NULL;

    IF NOT FOUND THEN
        RAISE EXCEPTION 'V0320 前置失败：父级 …0e03（基础数据）不存在或已删除，无法归位';
    END IF;

    IF v_parent_path IS DISTINCT FROM '/business/erp/master-data' THEN
        RAISE EXCEPTION 'V0320 前置失败：父级 …0e03 的 path = %（期望 /business/erp/master-data）',
            v_parent_path;
    END IF;

    RAISE NOTICE 'V0320 前置通过：父级 = %（%）', v_parent_name, v_parent_path;
END $$;

-- ---------- ② 归位：财务管理 → 基础数据，sort 50 → 30 ----------
-- menu_type = 2 锁住「页面」节点，避免误伤同 path 的按钮行（当前库里只有这 1 行）。
UPDATE sys_menu
   SET parent_id  = '20000000-0000-0000-0000-000000000e03'::uuid,  -- 基础数据
       sort_order = 30,
       updated_at = now()
 WHERE deleted_at IS NULL
   AND path = '/business/erp/seq'
   AND menu_type = 2
   AND (parent_id IS DISTINCT FROM '20000000-0000-0000-0000-000000000e03'::uuid
        OR sort_order IS DISTINCT FROM 30);

-- ---------- ③ 自检 ----------
DO $$
DECLARE
    bad     int;
    v_rows  int;
    v_name  text;
BEGIN
    -- ③-1 单号序列页面必须存在且唯一（0 行会让上面的 UPDATE 静默什么都不做）
    SELECT count(*) INTO v_rows
      FROM sys_menu
     WHERE deleted_at IS NULL
       AND path = '/business/erp/seq'
       AND menu_type = 2;
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'V0320 自检失败：/business/erp/seq 页面节点数 = %（期望 1）', v_rows;
    END IF;

    -- ③-2 必须挂在基础数据下、排序 30
    SELECT count(*) INTO bad
      FROM sys_menu
     WHERE deleted_at IS NULL
       AND path = '/business/erp/seq'
       AND menu_type = 2
       AND (parent_id IS DISTINCT FROM '20000000-0000-0000-0000-000000000e03'::uuid
            OR sort_order <> 30);
    IF bad > 0 THEN
        RAISE EXCEPTION 'V0320 自检失败：% 条单号序列未归位到基础数据', bad;
    END IF;

    -- ③-3 财务管理和基础数据下的「单号序列」不能同时存在（防半改）
    SELECT count(*) INTO bad
      FROM sys_menu
     WHERE deleted_at IS NULL
       AND path = '/business/erp/seq'
       AND parent_id = '20000000-0000-0000-0000-000000000e02'::uuid;
    IF bad > 0 THEN
        RAISE EXCEPTION 'V0320 自检失败：% 条单号序列仍挂在财务管理下', bad;
    END IF;

    -- ③-4 基础数据下 30 这个位次只留给单号序列（防撞号导致侧栏顺序不确定）
    SELECT count(*) INTO v_rows
      FROM sys_menu
     WHERE deleted_at IS NULL
       AND parent_id = '20000000-0000-0000-0000-000000000e03'::uuid
       AND sort_order = 30;
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'V0320 自检失败：基础数据下 sort_order=30 的菜单数 = %（期望 1）', v_rows;
    END IF;

    SELECT title INTO v_name
      FROM sys_menu
     WHERE deleted_at IS NULL AND path = '/business/erp/seq' AND menu_type = 2;

    RAISE NOTICE 'V0320 自检通过：% 已归位到「基础数据」sort=30', v_name;
END $$;
