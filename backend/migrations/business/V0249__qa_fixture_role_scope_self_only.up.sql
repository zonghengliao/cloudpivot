-- ======================================================
-- V0249：QA 验收夹具角色 · 数据范围收到 4（仅本人）（【P0-FIX-3B】）
--
-- ## 病根（生产实测 2026-11-05 13:44，P0-FIX-3 执行后）
--
--   select role_code, data_scope from sys_role
--    where role_code in ('QA_LIMITED','QA_VIEWER','MUTEX_A','MUTEX_B');
--   → 4 行全是 1（全部数据）
--
--   select count(*) from sys_user_role ur join sys_user u on u.id = ur.user_id
--    where ur.role_id = <上面任一角色> and u.deleted_at is null and u.status = 1;
--   → 0（唯一的持有者是 qa.limited / qa.viewer / qa.mutex，P0-FIX-3 已置 status=0）
--
-- 这 4 个角色不是业务岗位，是**验收夹具**：它们存在的理由是「一组特定的权限码集合」
-- 加上「能看全量」的前置（MUTEX_A/MUTEX_B 用来验角色互斥、QA_VIEWER 用来验只读、
-- QA_LIMITED 用来验权限码裁剪）。V0247 当时**刻意不动**它们，因为持有者还在用，
-- 收紧范围会让既有 QA 用例从「有数据可断言」变成假红。
--
-- 现在前提变了：账号已禁用 ⇒ **角色没有活跃持有者**。此时它们的 data_scope=1 就只剩
-- 一个作用：谁把它们重新挂到某个账号上（或历史上没删干净的绑定被复活），那个人
-- 立刻拿到「全库所有部门」的行级可见性，而且看起来完全正常（不报错、界面无异常）。
-- 这是一颗上了膛的枪，不是配置缺省。
--
-- ## 本迁移做什么（方案 A：收范围，不删角色）
--
-- 对这 4 个角色里**当前没有活跃持有者**的：
--     sys_role.data_scope: 1（全部数据）→ 4（仅本人）
-- 角色行、权限码、菜单授权、互斥配置全部保留（未来还要复用它们做验收，删了要重建）。
--
-- 为什么不选方案 B（软删角色）：
--   · 软删会让 `sys_role_menu` / `sys_role_mutex` / `sys_user_role` 的既有引用变成
--     「指向已删角色」，以后复用要重建角色 + 重挂权限码 + 重配互斥（夹具漂移风险）；
--   · 生产 12 个角色里这 4 个是「已知用途」的，留行 + 收紧范围即可达到安全目标
--     （无持有者时 4 与软删的实际权限效果相同：都拿不到任何别人的行）。
--
-- 为什么是 4（仅本人）而不是 3（本部门）或 5（自定义集合）：
--   · 3 需要「持有者所在部门」才有意义，而验收账号按设计**不进组织树**（dept_id 为空）；
--     部门类范围解析不出部门时会被 `merge_scope_rows` 静默降级成 SELF（S10/S11）——
--     想写 3 却得到 4，配置与行为打架；
--   · 5 必须配 `sys_role_dept` 行，空集合同样降级 SELF；为一个无持有者的夹具维护
--     部门集合是纯负担，还会踩巡检 N1-b；
--   · 4 是**最紧的非删除态**：语义明确、不依赖组织树、不需要集合行，
--     与「夹具只应看见自己造的数据」这个验收语义天然一致。
--
-- 未来若要复用这些角色做验收：把验收账号挂上，并在夹具脚本里**显式**把范围改回 1
-- （要验「能看全量」的前置时），收尾再改回 4 —— 这一步写成脚本里的显式动作，
-- 而不是靠「角色默认就是 1」这种没人记得的隐式前提。
--
-- ## 安全闸（本迁移不会「一刀切」）
--
-- 逐个角色判决：**有活跃持有者（`sys_user_role` ⋈ `sys_user`，且 u.deleted_at is null
-- and u.status = 1）的角色一律跳过并 RAISE NOTICE**。所以在任何环境里跑本迁移都不会
-- 悄悄改变一个在线用户能看到的数据；生产上 4 个角色都是 0 持有者，因此全部生效。
--
-- ## 回滚
--
-- V0249__qa_fixture_role_scope_self_only.down.sql：把这 4 个角色从 4 还原成 1
-- （= 迁移前基线：4 行全为 1）。与 V0247 / 其它种子类迁移同口径：还原「基线」而不是
-- 「上一次的值」；只碰当前值为 4 的行，不动管理界面里别人后来配成的 2/3/5。
-- ======================================================

DO $$
DECLARE
    v_rec     record;
    v_holders bigint;
    v_rows    integer := 0;
    v_kept    integer := 0;
    v_skip    text := '';
    v_missing integer;
BEGIN
    -- ---------- ⓪ 存在性：本库没有这 4 个夹具角色时是整个迁移的空操作 ----------
    SELECT count(*) INTO v_missing
      FROM sys_role
     WHERE deleted_at IS NULL
       AND role_code IN ('QA_LIMITED', 'QA_VIEWER', 'MUTEX_A', 'MUTEX_B');
    IF v_missing = 0 THEN
        RAISE NOTICE 'V0249 本库没有 QA 夹具角色（QA_LIMITED / QA_VIEWER / MUTEX_A / MUTEX_B），空操作';
        RETURN;
    END IF;

    -- ---------- ① 逐个角色判决：有活跃持有者 → 跳过；否则 1 → 4 ----------
    FOR v_rec IN
        SELECT r.id, r.role_code, r.data_scope::text AS old_scope
          FROM sys_role r
         WHERE r.deleted_at IS NULL
           AND r.role_code IN ('QA_LIMITED', 'QA_VIEWER', 'MUTEX_A', 'MUTEX_B')
         ORDER BY r.role_code
    LOOP
        -- 「活跃持有者」= 绑定存在（未过期与否都算，宁可保守）且账号未软删、未禁用。
        -- 与巡检 / 卡面口径一致：`sys_user_role` ⋈ `sys_user`，u.deleted_at is null and u.status = 1。
        SELECT count(*) INTO v_holders
          FROM sys_user_role ur
          JOIN sys_user u ON u.id = ur.user_id
         WHERE ur.role_id = v_rec.id
           AND u.deleted_at IS NULL
           AND u.status = 1;

        IF v_holders > 0 THEN
            -- 有人还在用 ⇒ 收紧会改变在线用户的可见数据，本迁移不做这件事，交给人决策。
            v_skip := v_skip || format('%s(活跃=%s) ', v_rec.role_code, v_holders);
            v_kept := v_kept + 1;
            CONTINUE;
        END IF;

        IF v_rec.old_scope = '4' THEN
            -- 幂等：已经收紧过，什么都不写（up 连跑两次第二次必须 0 行）
            RAISE NOTICE 'V0249  % 已是 data_scope=4，跳过（幂等）', v_rec.role_code;
            CONTINUE;
        END IF;

        UPDATE sys_role
           SET data_scope = 4,
               updated_at = CURRENT_TIMESTAMP
         WHERE id = v_rec.id;
        v_rows := v_rows + 1;
        RAISE NOTICE 'V0249  % data_scope % → 4（仅本人；无活跃持有者，见文件头理由）',
                     v_rec.role_code, v_rec.old_scope;
    END LOOP;

    RAISE NOTICE 'V0249 ① 收紧完成：% 个角色 → 4；跳过（仍有活跃持有者）% 个 %',
                 v_rows, v_kept, CASE WHEN v_skip = '' THEN '(无)' ELSE v_skip END;

    -- ---------- ② 自检：0 持有者的夹具角色必须已经不在「全部数据」上 ----------
    -- 只对「本库存在 + 无活跃持有者」的角色断言，因此空库/已有持有者的环境都不会误报。
    IF EXISTS (
        SELECT 1
          FROM sys_role r
         WHERE r.deleted_at IS NULL
           AND r.role_code IN ('QA_LIMITED', 'QA_VIEWER', 'MUTEX_A', 'MUTEX_B')
           AND r.data_scope <> 4
           AND NOT EXISTS (
               SELECT 1 FROM sys_user_role ur
                 JOIN sys_user u ON u.id = ur.user_id
                WHERE ur.role_id = r.id
                  AND u.deleted_at IS NULL
                  AND u.status = 1)
    ) THEN
        RAISE EXCEPTION 'V0249 自检失败：存在无活跃持有者但仍未收紧到 data_scope=4 的 QA 夹具角色（它们仍能看全部数据）';
    END IF;

    -- 不许顺手改软删状态：夹具角色行必须保留（本迁移只改 data_scope 一列）
    IF EXISTS (
        SELECT 1 FROM sys_role
         WHERE deleted_at IS NOT NULL
           AND role_code IN ('QA_LIMITED', 'QA_VIEWER', 'MUTEX_A', 'MUTEX_B')
    ) THEN
        RAISE WARNING 'V0249 提示：本库存在已软删的 QA 夹具角色行（不是本迁移造成的，本迁移不恢复也不删除它们）';
    END IF;

    -- 超管仍需保持 1（与 V0247 同一兜底：任何迁移都不许把管理员限死）
    IF EXISTS (SELECT 1 FROM sys_role
                WHERE role_code = 'SUPER_ADMIN' AND deleted_at IS NULL AND data_scope <> 1) THEN
        RAISE EXCEPTION 'V0249 自检失败：SUPER_ADMIN 的 data_scope 必须保持 1（不许把管理员限死）';
    END IF;

    RAISE NOTICE 'V0249 自检通过：无持有者的夹具角色已全部为 4 / SUPER_ADMIN 仍为 1';
END $$;
