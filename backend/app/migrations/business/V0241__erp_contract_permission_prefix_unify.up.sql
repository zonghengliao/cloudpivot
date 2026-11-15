-- ============================================================================
-- V0241__erp_contract_permission_prefix_unify.up.sql
--
-- 【P2-4】权限码命名不统一：合同用 `erp:contract:*`，ERP 其余 313 个用 `business:erp:*`
--
-- # 一、问题
--   `V0197` 按 `ARCH-2 附录 B #7` 的逐字契约给合同域种了 5 个 `erp:contract:*`
--   权限码（+ 1 条菜单节点取其 list 码），而 `V0141` 起的 ERP 其余码统一是
--   `business:erp:{资源}:{动作}`。两套前缀并存 ⇒ 前端判权要记两套规则、运维查授权
--   要搜两个前缀、新人也容易照抄错一套。评审实测：合同 6 条、其余 313 条。
--
-- # 二、本文件做什么（只改 `sys_menu.auth_code` / 按钮 `name` / `sys_user.token_version`，
--        **不碰授权表 `sys_role_menu`**）
--
--   ① 把 6 条菜单行的 `auth_code` **就地改名**成 `business:erp:*`：
--        erp:contract:list   → business:erp:contract:list     （menu_type=2 菜单节点）
--        erp:contract:create → business:erp:contract:create
--        erp:contract:update → business:erp:contract:update
--        erp:contract:delete → business:erp:contract:delete
--        erp:contract:status → business:erp:contract:status
--      ⚠ **为什么不删除旧行 + 插入新行**：`sys_role_menu` 是按 `menu_id` 挂的。删旧行
--      就等于把「已配好的合同授权」全部清空（admin / laoliu / 任何自建角色都得重新勾，
--      且勾漏了就是线上 403）。就地改名 ⇒ 菜单 id 不变 ⇒ 授权表**一行不动**，
--      「已配权限不丢失」是结构上保证的，不靠回填脚本。
--
--   ② 按钮行的 `name` 跟随（`V0141` 起的约定：按钮 name 与 auth_code 同值，
--      管理面「角色授权」树按 name 显示）。菜单行 name 是中文标题（「合同管理」），不动。
--
--   ③ 兜底补种：万一某个环境把 6 行中的某行**物理删掉**了（版本化迁移不会做，但人工
--      运维可能），①的 UPDATE 影响 0 行 → 权限码直接消失。这里按 `V0197` 的固定 UUID
--      再 INSERT 一次（`ON CONFLICT (id) DO NOTHING`），保证 5 个码 + 1 条菜单一定在。
--
--   ④ 在途令牌升级：给「持有这 6 个权限点的人」推进 `sys_user.token_version`。
--      **为什么必须推**：框架的接口级 403 只认**令牌里的权限快照**
--      （`claims.permissions.contains(perm)`，BUG-SYS-04 记录的已知边界）。部署前签发的
--      令牌里只有旧码，路由注解已换成新码 → 该令牌会在剩余有效期内被 403。别名扩展发生在
--      **装载侧**（登录 / refresh / `/me`），管不到已签发的令牌；在不动 vendored 框架的
--      前提下，唯一干净的做法是让旧令牌立刻失效：401 + `20008 您的权限已变更，请重新登录`。
--      注意本仓库的 refresh **也**比对 `token_version`（`auth/handler.rs` 的刷新分支），所以
--      这一步等价于「持合同的账号被登出一次、重新登录后拿到双码令牌」—— 与「改角色权限」
--      完全同一口径（`user::model::bump_token_version_of_role_holders_in`）。代价是一次
--      重新登录，收益是**绝不会有人被静默 403 卡在「无权限」上**（403 会被当成功能坏了，
--      20008 会明确引导重登）。
--      不推版本号的替代方案是：部署前签发的令牌在剩余有效期内（prod access 2h）访问合同接口
--      一律 403 —— 那正是本卡要消灭的「换个前缀就有人用不了」。
--
--   ⑤ 自检（异常即中止迁移，不留半成品）：
--      - 5 个规范码全部存在（未软删且 status=1）；
--      - `sys_menu` 里 **0 条** `erp:contract:*` 残留（旧码只允许活在代码的兼容别名表里）；
--      - `sys_role_menu` 指向这 6 个菜单 id 的行数 **改名前 = 改名后**（授权零丢失）。
--      并把改名前 / 改名后的行数、升级令牌的人数打进迁移日志，便于运维核对。
--
-- # 三、旧码为什么还要"能用"（兼容期双码）
--   已签发的 JWT 里是**登录那一刻的权限快照**，只有旧码；路由注解换成新码后，老令牌
--   会 403 到自然过期。所以旧码在**装载侧**保留：`backend/src/shared/permission_alias.rs`
--   在 `auth::model::load_permissions` 末尾把 `erp:contract:*` 与 `business:erp:contract:*`
--   双向互扩（新令牌 / `/me` / 前端判权 / 行级 `has_permission` 因此新旧码都命中），
--   在途令牌则由上面 ④ 一次性升级。库里**不再**保留旧码行：双码都进菜单会让管理面出现
--   两条同义节点，勾一条只生效另一条 —— 那正是本卡要消灭的双真值源。兼容期结束后删
--   别名表即可，本迁移无需再动。
--
-- # 四、幂等
--   UPDATE 带旧码条件（第二次执行影响 0 行）；INSERT 用固定 UUID + `ON CONFLICT (id)
--   DO NOTHING`（不覆盖运维改过的标题 / 排序 / is_visible）。
-- ============================================================================

DO $$
DECLARE
    v_ids          uuid[] := ARRAY[
        '20000000-0000-0000-0000-000000001100',
        '20000000-0000-0000-0000-000000001101',
        '20000000-0000-0000-0000-000000001102',
        '20000000-0000-0000-0000-000000001103',
        '20000000-0000-0000-0000-000000001104'
    ]::uuid[];
    n_grant_before bigint;
    n_grant_after  bigint;
    n_legacy       integer;
    n_new          integer;
    n_missing      integer;
    n_bumped       bigint;
    v_missing      text;
BEGIN
    -- ---------- 改名前的授权行数（自检基准） ----------
    SELECT count(*) INTO n_grant_before FROM sys_role_menu WHERE menu_id = ANY (v_ids);

    -- ---------- ① auth_code 就地改名 ----------
    UPDATE sys_menu
       SET auth_code = 'business:erp:' || substring(auth_code FROM length('erp:') + 1)
     WHERE auth_code LIKE 'erp:contract:%';

    -- ---------- ② 按钮 name 跟随 auth_code（V0141 起的约定） ----------
    UPDATE sys_menu
       SET name = auth_code
     WHERE menu_type = 3
       AND auth_code LIKE 'business:erp:contract:%'
       AND name LIKE 'erp:contract:%';

    -- ---------- ③ 兜底补种（某行被人工物理删除时仍能收敛到「5 码 + 1 菜单」） ----------
    INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                          sort_order, status, icon, title, is_visible, module_code)
    VALUES
        ('20000000-0000-0000-0000-000000001100'::uuid, '合同管理', 2,
         'business:erp:contract:list', '/business/erp/contract', NULL,
         'business/erp/contract/index', 75, 1, 'file', '合同管理（采购/销售/服务/租赁）',
         FALSE, 'business'),
        ('20000000-0000-0000-0000-000000001101'::uuid, 'business:erp:contract:create', 3,
         'business:erp:contract:create', NULL, '20000000-0000-0000-0000-000000001100'::uuid,
         NULL, 10, 1, NULL, '新建合同', FALSE, 'business'),
        ('20000000-0000-0000-0000-000000001102'::uuid, 'business:erp:contract:update', 3,
         'business:erp:contract:update', NULL, '20000000-0000-0000-0000-000000001100'::uuid,
         NULL, 20, 1, NULL, '编辑合同', FALSE, 'business'),
        ('20000000-0000-0000-0000-000000001103'::uuid, 'business:erp:contract:delete', 3,
         'business:erp:contract:delete', NULL, '20000000-0000-0000-0000-000000001100'::uuid,
         NULL, 30, 1, NULL, '删除合同', FALSE, 'business'),
        ('20000000-0000-0000-0000-000000001104'::uuid, 'business:erp:contract:status', 3,
         'business:erp:contract:status', NULL, '20000000-0000-0000-0000-000000001100'::uuid,
         NULL, 40, 1, NULL, '合同状态流转', FALSE, 'business')
    ON CONFLICT (id) DO NOTHING;

    -- ---------- ④ 在途令牌升级：让持有这 6 个权限点的人换一批带双码的令牌 ----------
    -- 框架的接口级 403 只认令牌快照（BUG-SYS-04 的已知边界），别名扩展在装载侧、
    -- 管不到已签发的令牌。推进 token_version ⇒ 旧令牌 401 + 20008 ⇒ 前端自动 refresh
    -- ⇒ 新令牌带双码 ⇒ 原请求重放成功（无需重输密码，也不会看到误导性的「无权限」）。
    UPDATE sys_user u
       SET token_version = token_version + 1,
           updated_at = now()
     WHERE u.deleted_at IS NULL
       AND u.id IN (
           SELECT ur.user_id
             FROM sys_user_role ur
             JOIN sys_role_menu rm ON rm.role_id = ur.role_id
            WHERE rm.menu_id = ANY (v_ids)
       );
    GET DIAGNOSTICS n_bumped = ROW_COUNT;

    -- ---------- ⑤ 自检 ----------
    -- ⑤-1 5 个规范码必须全部在（且启用、未软删）
    SELECT count(DISTINCT auth_code) INTO n_new
      FROM sys_menu
     WHERE auth_code LIKE 'business:erp:contract:%'
       AND deleted_at IS NULL
       AND status = 1;
    IF n_new <> 5 THEN
        RAISE EXCEPTION 'V0241 自检失败：business:erp:contract:* 应有 5 个启用码，实际 %', n_new;
    END IF;

    SELECT count(*) INTO n_missing
      FROM (VALUES ('business:erp:contract:list'), ('business:erp:contract:create'),
                   ('business:erp:contract:update'), ('business:erp:contract:delete'),
                   ('business:erp:contract:status')) AS t(code)
     WHERE NOT EXISTS (
           SELECT 1 FROM sys_menu m
            WHERE m.auth_code = t.code AND m.deleted_at IS NULL AND m.status = 1);
    IF n_missing <> 0 THEN
        SELECT string_agg(code, ', ') INTO v_missing
          FROM (VALUES ('business:erp:contract:list'), ('business:erp:contract:create'),
                       ('business:erp:contract:update'), ('business:erp:contract:delete'),
                       ('business:erp:contract:status')) AS t(code)
         WHERE NOT EXISTS (
               SELECT 1 FROM sys_menu m
                WHERE m.auth_code = t.code AND m.deleted_at IS NULL AND m.status = 1);
        RAISE EXCEPTION 'V0241 自检失败：缺权限码 %', v_missing;
    END IF;

    -- ⑤-2 旧码必须 0 残留（含已软删的行：任何残留都会让管理面出现同义节点）
    SELECT count(*) INTO n_legacy FROM sys_menu WHERE auth_code LIKE 'erp:contract:%';
    IF n_legacy <> 0 THEN
        RAISE EXCEPTION 'V0241 自检失败：sys_menu 仍有 % 条 erp:contract:* 残留（应为 0）', n_legacy;
    END IF;

    -- ⑤-3 授权零丢失：改名前后指向这 6 个菜单的 sys_role_menu 行数必须相等
    SELECT count(*) INTO n_grant_after FROM sys_role_menu WHERE menu_id = ANY (v_ids);
    IF n_grant_after <> n_grant_before THEN
        RAISE EXCEPTION
            'V0241 自检失败：改名前后授权行数不一致（前 % / 后 %）—— 授权表本不应被触碰，请立即排查',
            n_grant_before, n_grant_after;
    END IF;

    RAISE NOTICE 'V0241 自检通过：5 个 business:erp:contract:* 就位，旧码 0 残留，授权行 % → %（零丢失），在途令牌升级 % 人',
        n_grant_before, n_grant_after, n_bumped;
END $$;
