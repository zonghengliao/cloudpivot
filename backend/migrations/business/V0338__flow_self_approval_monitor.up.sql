-- ============================================================
-- V0338：ALLOW 自审的配套约束（P3-05）
--
-- 背景：`biz_flow_node.self_approval = 'ALLOW'` 允许「发起人本人」签自己的单
-- （一人公司刚需），但放开之后必须**可审计、可限额**，否则它就是「谁发起谁自己批」
-- 的合法后门。代码侧（`flow/self_approval.rs`）已经做了三件事：
--   ① 金额超上限 → 降级为 SKIP（走 P3-02 升级链 / P3-03 兜底人），仍无人可用则 422 + 65044；
--   ② 「允许自审」/「真的自审了」/「超上限被拦」三类事件落 `operation_logs`；
--   ③ `GET /business/flow/stats/self-approval` 月度报表（分子 = 自审单数，
--      分母 = 同期已结算通过单数，金额一律 `::text`）。
--
-- 本迁移补两件**配置类**的东西（不建表、不加列、不改任何业务表）：
--
--   ① `sys_config` 一条：自审金额上限（默认 `0` = **不限额**）。
--      为什么默认 0 而不是 1 万：ARCH-8 §8 待确认问题 2 的两条建议里选了「不限额」——
--      阈值是客户自己的内控松紧，代码替客户拍数字会在升级后静默改变已有 ALLOW 节点的行为。
--      写成 `NUMBER`（白名单四值之一，见 `support::config::model::config_type`），
--      配置页会渲染成数字输入框。
--
--   ② `sys_menu` 一行：`/flow/stats`「自审监控」页面。
--      **必须建**：卡面要求「流程统计页加自审监控 tab」，而此前前端根本没有流程统计页
--      （三个 `stats/*` 接口只有后端、零前端调用方）。权限码复用
--      `business:flow:instance:read`（与其余统计接口同口径，不新造 `business:flow:stats:read`
--      这行死码）。path / component 与 `frontend/src/router/index.ts` 逐字一致（n08 门禁）。
--
-- 无「拦截计数表」：拦下的每一次都落在 `operation_logs`（`FLOW_SELF_APPROVAL_BLOCKED`），
-- 报表直接数日志 —— 多一张计数表就多一份「与真实留痕不一致」的风险。
-- ============================================================

-- ---------- ① 自审金额上限参数 ----------
INSERT INTO sys_config (
    id, config_key, config_value, config_type, description,
    category, config_level, is_modifiable, is_encrypted, is_restart, status,
    sort_order, ref_value, select_value
) VALUES (
    'e2b00000-0000-4000-8000-000000000005',
    'erp.separation_of_duties.self_approval_max_amount',
    '0',
    'NUMBER',
    '【自审金额上限】节点允许自审（ALLOW）时，单据金额超过该值就不许自审，'
    || '改走升级链 / 兜底人；0 = 不限额',
    'erp_internal_control', 1, TRUE, FALSE, FALSE, 1,
    50, NULL, NULL
)
ON CONFLICT (config_key) WHERE deleted_at IS NULL DO NOTHING;

-- ---------- ② 自审监控页面菜单 ----------
-- 号段：V0336 用到 …1240/1241/1242，这里顺延 …1243（插前先探一次占用）。
-- title 用短名（≤12 字，MENU-FIX 规范 §1.6.1），说明写在 description 里。
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001243'::uuid, '自审监控', 2,
     'business:flow:instance:read', '/flow/stats',
     '20000000-0000-0000-0000-000000000860'::uuid, 'business/flow/stats/index',
     50, 1, 'chart', '自审监控', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ③ 授权：谁能进审批中心（business:flow:instance:read）谁就能看自审监控 ----------
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT rm.role_id, m.id
  FROM sys_role_menu rm
  JOIN sys_menu base ON base.id = rm.menu_id AND base.auth_code = 'business:flow:instance:read'
  CROSS JOIN sys_menu m
 WHERE m.id = '20000000-0000-0000-0000-000000001243'::uuid
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------- ③.5 顺手修一个既有的同父 sort 撞车（MENU-FIX 不变量） ----------
-- 实测（全新库首启）：审批流目录下 `/flow/linkage`（联动规则）与 `/flow/delegate`
-- （审批委托，V0336 种的 …1240）**都是 sort_order=40** —— V0336 当时没核兄弟行。
-- 撞车不会报错，只会让侧栏两条菜单的先后取决于 id 的排序，运维以为「菜单顺序会自己变」。
--
-- 这里把**审批委托**挪到 45（联动规则 40 → 委托 45 → 自审监控 50），带守卫：
-- 只有「该行仍是 40 且确实存在同父同 sort 的另一行」时才动，客户自己排过序的库整段跳过。
UPDATE sys_menu d
   SET sort_order = 45,
       updated_at = now()
 WHERE d.id = '20000000-0000-0000-0000-000000001240'::uuid
   AND d.sort_order = 40
   AND d.deleted_at IS NULL
   AND EXISTS (
        SELECT 1 FROM sys_menu o
         WHERE o.parent_id = d.parent_id
           AND o.sort_order = d.sort_order
           AND o.id <> d.id
           AND o.deleted_at IS NULL
   );

-- ---------- ④ 自检（空库首启与有数据重放都必须过） ----------
DO $$
DECLARE
    v_cfg int;
    v_bad int;
    v_menu int;
BEGIN
    -- 参数存在、启用、可修改、类型 NUMBER、默认值 '0'（= 不限额）
    SELECT count(*) INTO v_cfg
      FROM sys_config
     WHERE config_key = 'erp.separation_of_duties.self_approval_max_amount'
       AND status = 1
       AND deleted_at IS NULL
       AND config_type = 'NUMBER'
       AND is_modifiable IS TRUE
       AND config_value = '0'
       AND category = 'erp_internal_control';
    IF v_cfg <> 1 THEN
        RAISE EXCEPTION 'V0338 自检失败：自审金额上限参数缺失 / 类型或默认值不合口径（命中 % 行）', v_cfg;
    END IF;

    -- 兄弟节点 sort 不许撞车（MENU-FIX 不变量）
    SELECT count(*) INTO v_bad
      FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000000860'::uuid
       AND deleted_at IS NULL
       AND sort_order = 50
       AND id <> '20000000-0000-0000-0000-000000001243'::uuid;
    IF v_bad <> 0 THEN
        RAISE EXCEPTION 'V0338 自检失败：审批流下 sort_order=50 已被占用（% 行），请顺延', v_bad;
    END IF;

    -- 页面行的 path / component / 父目录与前端路由逐字一致（n08 门禁前置条件）
    SELECT count(*) INTO v_menu
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000001243'::uuid
       AND path = '/flow/stats'
       AND component = 'business/flow/stats/index'
       AND parent_id = '20000000-0000-0000-0000-000000000860'::uuid
       AND auth_code = 'business:flow:instance:read'
       AND deleted_at IS NULL;
    IF v_menu <> 1 THEN
        RAISE EXCEPTION 'V0338 自检失败：自审监控页面菜单的 path/component/父目录/权限码与前端路由不一致';
    END IF;
END $$;
