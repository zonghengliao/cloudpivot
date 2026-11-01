-- ======================================================
-- V0181：流程配置化 · 权限码与菜单（P-FLOW-1A 取本迁移的 tasknode 部分）
--
-- 依据：`docs/architecture/流程全配置化-设计.md` §2.0 / §3.8（commit 09b87fe）
-- 回滚：`V0181__flow_config_permissions.down.sql`（按 auth_code 删，零残留）
--
-- ── 本迁移落了什么、没落什么（必须说清，否则后续卡会以为码已经齐了）──────
-- 设计 §3.8 为「流程配置化」一口气列了 7 个码：
--   business:flow:tasknode:read / :write
--   business:flow:statemachine:read / :write / :admin
--   business:flow:linkage:read / :write
--
-- **本卡（P-FLOW-1A）只种 `business:flow:tasknode:read`，其余 6 个不种**，理由是可验证的：
--
--   1. 后端有一条常驻不变量用例（`system::menu::permission_audit_tests` 的 N14 / N18）：
--      **`sys_menu` 里每个权限码必须在后端源码里有落地**，否则就是「管理员能勾、
--      勾完永不生效」的死码（BUG-SYS-05 的原形态，FIX-06 刚把它钉成常驻用例）。
--      现在把 statemachine / linkage 的 5 个码种进来，它们对应的接口要等 P-FLOW-3 / P-FLOW-4，
--      中间这段时间里这 5 个码就是**确定会红的死码**。
--   2. 设计 §2.0 把 V0181 标成「阶段 1–4 共用」，是**编号规划**，不是「阶段 1 必须一次种完」。
--      未实现的码随各自的接口一起种，是同一个设计里更稳的读法。
--   3. `business:flow:tasknode:write` 在 §3.8 里原文写着「包含在 `business:flow:update` 里，
--      此处不重复授」—— 它不是一个独立可授的码，因此也不种（种了就是第二个死码）。
--
-- ── 为什么 tasknode:read 是「真」权限码而不是摆设 ────────────────
-- 它挂在 `POST /business/flow/node/task/preview`（模板预演）上：
--   · 设计 §3.3 原本写 `business:flow:read`，但 §3.8 又专门为「查看流程任务配置」造了
--     `business:flow:tasknode:read`。两个码落到同一个接口只有一个能生效；
--   · 选 tasknode:read 的实际理由：用 flow:read 会让 §3.8 的这个码**没有任何接口**，
--     直接命中上面说的死码用例。而 tasknode:read 的语义（「能看任务配置就能预演」）
--     与预演接口完全吻合，且当前没有任何前端调用方依赖 flow:read 这一路径（1C 才建页面）。
--   · 结论写进代码注释（`flow/handler.rs` 预演接口的 `#[nik::permission]`）与交付报告。
--
-- ── 菜单 UUID 段 ────────────────────────────────────────────
-- 全库枚举确认 …0780–…0785 是流程定义的页面 + 5 个按钮（V0121），…0800–…0804 是实例侧
-- （V0124），…0860–…0862 是审批流目录（V0128）。本迁移取 **…0786**（紧跟在流程定义按钮之后）。
-- ======================================================

-- ---------- ① 一个按钮级权限点（挂「流程定义」页面 …0780 下）----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES (
    '20000000-0000-0000-0000-000000000786'::uuid,
    'business:flow:tasknode:read',
    3,
    'business:flow:tasknode:read',
    NULL,
    '20000000-0000-0000-0000-000000000780'::uuid,
    NULL,
    60,
    1,
    NULL,
    '查看流程任务节点的配置（含模板预演）',
    FALSE,
    'business'
)
ON CONFLICT (id) DO NOTHING;

-- ---------- ② 授权给所有未删除的角色中的超管角色 ----------
-- 为什么只给超管：这个码是**配置面**的只读权限（能看流程定义的人本来就能预演），
-- 先随 SUPER_ADMIN 下发；普通业务角色要不要看，等 1C 的前端入口定了再按需授
-- （现在给所有角色授一个「还没有页面」的码，等于给全员加一条无用的授权）。
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.id
  FROM sys_role r
  CROSS JOIN sys_menu m
 WHERE r.deleted_at IS NULL
   AND r.role_code = 'SUPER_ADMIN'
   AND m.id = '20000000-0000-0000-0000-000000000786'::uuid
   AND NOT EXISTS (
       SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
   );

-- ---------- ③ 自检 ----------
DO $$
DECLARE
    found int;
    wrong int;
BEGIN
    SELECT count(*) INTO found
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000000786'::uuid
       AND deleted_at IS NULL;
    IF found <> 1 THEN
        RAISE EXCEPTION 'V0181 自检失败：business:flow:tasknode:read 的菜单行不存在';
    END IF;

    SELECT count(*) INTO wrong
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000000786'::uuid
       AND (menu_type <> 3
            OR auth_code <> 'business:flow:tasknode:read'
            OR parent_id IS DISTINCT FROM '20000000-0000-0000-0000-000000000780'::uuid
            OR module_code <> 'business'
            OR status <> 1);
    IF wrong > 0 THEN
        RAISE EXCEPTION 'V0181 自检失败：按钮节点的类型 / auth_code / 父节点 / 模块 / 状态不对';
    END IF;

    RAISE NOTICE 'V0181 自检通过：1 个按钮权限点（business:flow:tasknode:read）';
END
$$;
