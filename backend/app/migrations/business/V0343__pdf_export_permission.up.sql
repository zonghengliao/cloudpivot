-- ======================================================
-- V0343：新增权限点 business:pdf:render:export（PDF 导出，仅超管）
--
-- 背景：PDF-01 落地 PDF 导出基础设施（常驻 Chromium + CDP 渲染引擎），
--       三个接口（/business/pdf/health | /sample | /render）由
--       src/business/pdf/handler.rs 的 #[nik::permission] 校验，
--       权限码必须与 sys_menu.auth_code 逐字一致。
--
-- 权限码：business:pdf:render:export
-- 授权范围：仅 SUPER_ADMIN。
--   超管鉴权本身由 role_code 短路（src/system/auth/model.rs::is_super_admin，
--   权限列表下发 *:*:*），**不依赖下面这张授权表**；写 sys_role_menu 只是为了让
--   「角色授权」页的权限树能完整展示本权限点。
--
-- ⚠️ 空库首启时下面的 INSERT 实际插入 0 行（不是写错，与 V0020 同口径）：
--   `sys_role` 的 SUPER_ADMIN 是 **seed 迁移**（`migrations/seed/S__*.sql`）种的，
--   而 seed 在全部 V* 之后才跑（V0342 的注释里也记过这件事）。所以：
--     · 全新库：V0343 先跑 → 找不到 SUPER_ADMIN → 0 行（无害，鉴权短路不受影响）；
--     · 已有数据的库升级：SUPER_ADMIN 已在 → 正常插 1 行，权限树可见。
--   已核对：V0020 的 business:monitor:server-status 在空库首启后同样是 0 行。
--
-- 为什么只给超管：
--   · /business/pdf/render 接受任意 HTML（等于一个「让服务器渲染任意页面」的口子）；
--   · 渲染是重资源操作（实测 500 行单据单次峰值约 190MiB 内存，3 并发 578MiB）。
--   真正给业务角色用的单据导出（发票 / 入库单 / 对账单）由 PDF-02 起的卡各挂
--   自己的权限码，不共用本码。
--
-- 为什么 menu_type = 3（按钮/权限点）而不是页面菜单：
--   本次不新增前端路由，PDF 导出是「单据页面上的一个按钮」的能力，没有独立页面；
--   写成按钮型既能让角色授权页展示它，也不会改变菜单树结构（顶级菜单数量不变）。
--
-- 幂等：固定 UUID + ON CONFLICT DO NOTHING，重复执行不报错、不覆盖人工改名。
-- ======================================================

INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES ('20000000-0000-0000-0000-00000000f001'::uuid,
        'business:pdf:render:export', 3, 'business:pdf:render:export',
        NULL, '10000000-0000-0000-0000-000000000050'::uuid, NULL,
        60, 1, NULL, 'PDF 导出', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP
FROM sys_role r
CROSS JOIN (VALUES ('20000000-0000-0000-0000-00000000f001'::uuid)) AS m(id)
WHERE r.role_code = 'SUPER_ADMIN'
ON CONFLICT DO NOTHING;
