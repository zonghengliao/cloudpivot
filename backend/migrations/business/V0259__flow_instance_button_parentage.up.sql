-- ============================================================================
-- V0259：流程实例侧 5 个按钮的父页归位 —— 消灭「孤儿按钮」（角色有按钮码、父页码没授）
--
-- 依据：kanban 卡 t_243bc2aa（P3，源自 t_227464dd 交付时实测发现的既有缺陷）
--
-- # 一、问题（生产 cloudpivot@172.18.10.144 只读实测，非推测）
--   两个角色持有按钮码，但按钮所挂的父页 `business:flow:read`（/flow/def 流程定义）没授
--   ⇒ 在「角色管理」权限树里，这些按钮挂在一个该角色永远看不见的页面下（孤儿按钮）：
--
--     select r.role_code, m.auth_code as button_code, p.path, p.auth_code as parent_code
--       from sys_role_menu rm
--       join sys_role r on r.id = rm.role_id
--       join sys_menu m on m.id = rm.menu_id and m.menu_type = 3 and m.deleted_at is null
--       join sys_menu p on p.id = m.parent_id and p.deleted_at is null
--      where coalesce(p.auth_code,'') <> ''
--        and not exists (select 1 from sys_role_menu rm2 join sys_menu m2 on m2.id = rm2.menu_id
--                         where rm2.role_id = r.id and m2.auth_code = p.auth_code);
--
--   实测 8 对（卡面只列了 approve 那 2 对；同一个父页下 4 个码全部越界）：
--     FLOW_CONFIG_APPROVER × business:flow:approve / instance:create / instance:read / instance:withdraw
--     FINANCE_DIRECTOR     × business:flow:approve / instance:create / instance:read / instance:withdraw
--
-- # 二、决策：**不补授 business:flow:read**，而是把这 5 个按钮挂回它们真正的父页
--   卡面给了两条路（补授父页码 / 改父页），本迁移选后者，依据三条实测：
--   ① 按钮真正出现在哪个页面：`frontend/src/views/business/flow/task/index.vue:345`
--      `hasPermission('business:flow:approve')` —— 审批按钮长在「审批中心」（/flow/task，
--      页码 `business:flow:instance:read`）上，不在「流程定义」页；后端也是
--      `POST /business/flow/task/{id}/approve|reject|transfer|add-sign`（handler.rs:1106-1111）。
--   ② 两个角色的职责不含「流程定义」：生产实测 FLOW_CONFIG_APPROVER 只持 4 个 flow 码
--      （approve / instance:create / instance:read / instance:withdraw），零个定义侧码
--      （read/create/update/publish/state-edge）；它的职责是「流程配置复核员」
--      （V0206 种角色、V0247 记数据范围、doc/manual/02-审批流.md：状态机边配置变更的复核岗），
--      复核单子走审批中心。补授 flow:read = 给它们看一个从不需要的定义页 = 生产**新增可见面**，
--      而这次改动本该零授权变化。
--   ③ 根因是时序，不是授权缺失：V0124（实例侧权限点 …0800–…0804）执行时
--      「发起审批 / 审批中心」两个页面还不存在（V0128 才种 …0861/…0862），
--      所以 V0124 只能把这 5 个按钮挂在当时唯一的流程页面 …0780（流程定义）下；
--      V0128 后来建了页面却没有回头改父级。V0239 文件头第 ② 条的本意也是
--      「instance:create/read/withdraw 与 发起审批/审批中心 配套授予」—— 父级关系写错了。
--   ⇒ 归位后：**生产上 sys_role_menu 一行不动**（零新增授权、零撤销），只改 5 行
--      sys_menu.parent_id；8 对孤儿全部消失。这是最小且语义正确的那一刀。
--
-- # 三、修法
--   ① 兜底补授（只在「非标准库」命中；生产与装配库命中 0 行）：
--      若某角色持有这 5 个按钮之一、却没有目标父页的码，先给它补上目标父页的
--      全部节点 + 无码祖先目录。理由：改父级是全局动作（sys_menu.parent_id 不按角色存），
--      不补授就会把孤儿从旧父页搬到新父页 —— 那等于把缺陷换个地方藏起来。
--      客户库上这一档真会发生（管理员只勾按钮没勾页面），所以必须有。
--   ② 归位 5 个按钮（幂等：`AND parent_id IS DISTINCT FROM <新父>`）：
--        …0800 business:flow:instance:create   → …0861 发起审批（/flow/start，码 instance:create）
--        …0801 business:flow:instance:read     → …0862 审批中心（/flow/task，码 instance:read）
--        …0802 business:flow:instance:withdraw → …0862
--        …0803 business:flow:approve           → …0862
--        …0804 business:flow:admin             → …0862
--   ③ 施工后断言（见文件末 DO 块）：
--      硬断言 A：5 个节点的 parent_id 必须是目标父页（结构契约，任何库都必须成立）；
--      硬断言 B：本次归位的 5 个按钮上，孤儿 (角色,码) 对数 = 0（修完必须成立）；
--      软断言 C：全库孤儿 (角色,码) 对数 —— >0 只 RAISE WARNING 点名，**不拦升级**。
--        口径与常驻闸 `scripts/verify/orphan_button_gate.py` 逐条一致（(角色,码) 粒度：
--        该码的每个按钮节点向上走祖先链都有带码祖先未授 = 孤儿；超管按 role_code 排除）。
--        为什么不 EXCEPTION：客户自己勾出来的孤儿不该把升级挡死在启动阶段（V0239 §4 同口径），
--        拦截交给检查链里的常驻闸。
--
-- # 四、幂等 / 回滚 / 维护
--   幂等：UPDATE 带 `IS DISTINCT FROM`，授权插入带 `NOT EXISTS` + 确定性 id；
--        在已归位的库上重放 = 0 更新 / 0 插入（生产同理）。
--   回滚：`V0259__flow_instance_button_parentage.down.sql` —— 把 5 个节点挂回 …0780，
--        并按**同一条确定性 id 公式**删除本迁移插入的授权行（只删本迁移造的行：
--        人工勾选的行 id 是 `gen_random_uuid()`，跟公式 id 不可能碰撞）。
--        代价写在 down 文件头：若某角色的按钮后来被撤销，down 重算不出它的补授行，会留残行。
--   维护：以后新增实例侧按钮，父页必须写对（发起审批 → …0861，审批中心 → …0862），
--        常驻闸会拦。
-- ============================================================================

DO $$
DECLARE
    v_btn_page   uuid := '20000000-0000-0000-0000-000000000862'::uuid;  -- 审批中心（页面节点）
    v_btn_start  uuid := '20000000-0000-0000-0000-000000000861'::uuid;  -- 发起审批（页面节点）
    n_found      int;
    n_bad        int;
    n_moved      int := 0;
    n_moved2     int := 0;
    n_fix        int := 0;
    n_ver        int := 0;
    v_txt        text;
BEGIN
    -- ---------- 0) 前置：5 个按钮节点与 2 个目标父页必须就位（码写错就炸，别静默 0 行） ----------
    SELECT count(*) INTO n_found
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000000800'::uuid,
                  '20000000-0000-0000-0000-000000000801'::uuid,
                  '20000000-0000-0000-0000-000000000802'::uuid,
                  '20000000-0000-0000-0000-000000000803'::uuid,
                  '20000000-0000-0000-0000-000000000804'::uuid)
       AND menu_type = 3 AND deleted_at IS NULL;
    IF n_found <> 5 THEN
        RAISE EXCEPTION 'V0259 前置失败：实例侧按钮节点只找到 % 个（应为 5，V0124 的 …0800–…0804）', n_found;
    END IF;

    SELECT count(*) INTO n_found
      FROM sys_menu
     WHERE (id = v_btn_start AND auth_code = 'business:flow:instance:create')
        OR (id = v_btn_page  AND auth_code = 'business:flow:instance:read');
    IF n_found <> 2 THEN
        RAISE EXCEPTION 'V0259 前置失败：目标父页（…0861 发起审批 / …0862 审批中心）缺失或 auth_code 不符';
    END IF;

    -- ---------- 1) 兜底补授：持有按钮但缺目标父页码的角色（标准库命中 0 行） ----------
    DROP TABLE IF EXISTS _v0259_fix;
    CREATE TEMP TABLE _v0259_fix AS
    SELECT DISTINCT rm.role_id, t.new_parent
      FROM (VALUES
              ('20000000-0000-0000-0000-000000000800'::uuid, v_btn_start),
              ('20000000-0000-0000-0000-000000000801'::uuid, v_btn_page),
              ('20000000-0000-0000-0000-000000000802'::uuid, v_btn_page),
              ('20000000-0000-0000-0000-000000000803'::uuid, v_btn_page),
              ('20000000-0000-0000-0000-000000000804'::uuid, v_btn_page)
           ) AS t(btn_id, new_parent)
      JOIN sys_role_menu rm ON rm.menu_id = t.btn_id
      JOIN sys_role r       ON r.id = rm.role_id AND r.deleted_at IS NULL
     WHERE NOT EXISTS (
             SELECT 1 FROM sys_role_menu rm2
               JOIN sys_menu m2 ON m2.id = rm2.menu_id AND m2.deleted_at IS NULL
              WHERE rm2.role_id = rm.role_id
                AND m2.auth_code = (SELECT auth_code FROM sys_menu WHERE id = t.new_parent));

    IF EXISTS (SELECT 1 FROM _v0259_fix) THEN
        SELECT string_agg(DISTINCT r.role_code, ', ') INTO v_txt
          FROM _v0259_fix f JOIN sys_role r ON r.id = f.role_id;
        RAISE NOTICE 'V0259：这些角色持有实例侧按钮却缺目标父页码，先补授父页 —— %', v_txt;
    END IF;

    -- 展开：目标码的全部活跃节点 + 无码祖先目录（与 V0258 第 4.1/4.2 步同口径）
    DROP TABLE IF EXISTS _v0259_sel;
    CREATE TEMP TABLE _v0259_sel AS
    WITH RECURSIVE
    hit AS (
        SELECT f.role_id, m.id AS menu_id
          FROM _v0259_fix f
          JOIN sys_menu p ON p.id = f.new_parent AND p.deleted_at IS NULL
          JOIN sys_menu m ON m.deleted_at IS NULL AND m.auth_code = p.auth_code
    ),
    anc AS (
        SELECT h.role_id, p.id AS menu_id, coalesce(p.auth_code,'') AS code
          FROM hit h
          JOIN sys_menu c ON c.id = h.menu_id
          JOIN sys_menu p ON p.id = c.parent_id AND p.deleted_at IS NULL
        UNION
        SELECT a.role_id, p.id, coalesce(p.auth_code,'')
          FROM anc a
          JOIN sys_menu c ON c.id = a.menu_id
          JOIN sys_menu p ON p.id = c.parent_id AND p.deleted_at IS NULL
    )
    SELECT role_id, menu_id FROM hit
    UNION
    SELECT role_id, menu_id FROM anc WHERE code = '';

    INSERT INTO sys_role_menu (id, role_id, menu_id, created_at)
    SELECT (
               SELECT (substr(h,1,8) || '-' || substr(h,9,4) || '-4' || substr(h,14,3)
                       || '-8' || substr(h,18,3) || '-' || substr(h,21,12))::uuid
                 FROM (SELECT md5(s.role_id::text || '|' || s.menu_id::text || '|v0259-orphan-parent') AS h) _h
           ), s.role_id, s.menu_id, now()
      FROM _v0259_sel s
     WHERE NOT EXISTS (SELECT 1 FROM sys_role_menu rm
                        WHERE rm.role_id = s.role_id AND rm.menu_id = s.menu_id)
    ON CONFLICT (id) DO NOTHING;
    GET DIAGNOSTICS n_fix = ROW_COUNT;

    -- ---------- 2) 归位：5 个按钮挂到语义正确的父页（幂等） ----------
    UPDATE sys_menu SET parent_id = v_btn_start, updated_at = now()
     WHERE id = '20000000-0000-0000-0000-000000000800'::uuid
       AND parent_id IS DISTINCT FROM v_btn_start;
    GET DIAGNOSTICS n_moved = ROW_COUNT;

    UPDATE sys_menu SET parent_id = v_btn_page, updated_at = now()
     WHERE id IN ('20000000-0000-0000-0000-000000000801'::uuid,
                  '20000000-0000-0000-0000-000000000802'::uuid,
                  '20000000-0000-0000-0000-000000000803'::uuid,
                  '20000000-0000-0000-0000-000000000804'::uuid)
       AND parent_id IS DISTINCT FROM v_btn_page;
    GET DIAGNOSTICS n_moved2 = ROW_COUNT;
    n_moved := n_moved + n_moved2;

    -- ---------- 3) 有补授才推进 token_version（纯结构变更不踢在线会话） ----------
    IF n_fix > 0 THEN
        UPDATE sys_user u
           SET token_version = u.token_version + 1, updated_at = now()
         WHERE u.deleted_at IS NULL
           AND EXISTS (SELECT 1 FROM sys_user_role ur
                        WHERE ur.user_id = u.id
                          AND ur.role_id IN (SELECT role_id FROM _v0259_sel));
        GET DIAGNOSTICS n_ver = ROW_COUNT;
    END IF;

    -- ---------- 4) 施工后断言 ----------
    -- 4.1 结构契约：5 个节点的父页必须归位
    SELECT count(*) INTO n_bad
      FROM sys_menu
     WHERE deleted_at IS NULL
       AND (
            (id = '20000000-0000-0000-0000-000000000800'::uuid AND parent_id IS DISTINCT FROM v_btn_start)
         OR (id IN ('20000000-0000-0000-0000-000000000801'::uuid,
                    '20000000-0000-0000-0000-000000000802'::uuid,
                    '20000000-0000-0000-0000-000000000803'::uuid,
                    '20000000-0000-0000-0000-000000000804'::uuid) AND parent_id IS DISTINCT FROM v_btn_page)
           );
    IF n_bad <> 0 THEN
        RAISE EXCEPTION 'V0259 断言 A 失败：归位后仍有 % 个节点父页不对', n_bad;
    END IF;

    -- 4.2 本迁移 5 个按钮：孤儿 (角色,码) 对数必须为 0（修完必须成立）
    SELECT count(*) INTO n_bad
      FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
      JOIN sys_menu m ON m.id = rm.menu_id AND m.deleted_at IS NULL AND m.menu_type = 3
      JOIN sys_menu p ON p.id = m.parent_id AND p.deleted_at IS NULL
     WHERE m.id IN ('20000000-0000-0000-0000-000000000800'::uuid,
                    '20000000-0000-0000-0000-000000000801'::uuid,
                    '20000000-0000-0000-0000-000000000802'::uuid,
                    '20000000-0000-0000-0000-000000000803'::uuid,
                    '20000000-0000-0000-0000-000000000804'::uuid)
       AND coalesce(p.auth_code,'') <> ''
       AND NOT EXISTS (SELECT 1 FROM sys_role_menu rm2
                         JOIN sys_menu m2 ON m2.id = rm2.menu_id AND m2.deleted_at IS NULL
                        WHERE rm2.role_id = r.id AND m2.auth_code = p.auth_code);
    IF n_bad <> 0 THEN
        RAISE EXCEPTION 'V0259 断言 B 失败：本次归位的 5 个按钮上仍有 % 对孤儿 (角色,码)', n_bad;
    END IF;

    -- 4.3 全库孤儿（口径与常驻闸 scripts/verify/orphan_button_gate.py 逐条一致）：
    --     (角色,码) 粒度 —— 该码的每个按钮节点向上走祖先链，都有带码祖先未授 = 孤儿；
    --     超管按 role_code 排除（*:*:* 通配，sys_role_menu 行对功能零影响，V0258 同口径）。
    --     只告警，不拦升级；拦截由检查链里的常驻闸负责。
    DROP TABLE IF EXISTS _v0259_orphan;
    CREATE TEMP TABLE _v0259_orphan AS
    WITH RECURSIVE btn(role_id, btn_id, code, anc_id) AS (
        SELECT rm.role_id, m.id, m.auth_code, m.parent_id
          FROM sys_role_menu rm
          JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
                         AND r.role_code <> 'SUPER_ADMIN'
          JOIN sys_menu m ON m.id = rm.menu_id AND m.menu_type = 3 AND m.deleted_at IS NULL
        UNION
        SELECT b.role_id, b.btn_id, b.code, p.parent_id
          FROM btn b JOIN sys_menu p ON p.id = b.anc_id AND p.deleted_at IS NULL
    ),
    mark AS (
        SELECT b.role_id, b.btn_id, b.code,
               bool_or(coalesce(a.auth_code, '') <> '' AND NOT EXISTS (
                   SELECT 1 FROM sys_role_menu rm2
                     JOIN sys_menu m2 ON m2.id = rm2.menu_id AND m2.deleted_at IS NULL
                    WHERE rm2.role_id = b.role_id AND m2.auth_code = a.auth_code)) AS blocked
          FROM btn b JOIN sys_menu a ON a.id = b.anc_id
         GROUP BY b.role_id, b.btn_id, b.code
    )
    SELECT m.role_id, m.code
      FROM mark m
     GROUP BY m.role_id, m.code
    HAVING bool_and(m.blocked);

    SELECT count(*) INTO n_bad FROM _v0259_orphan;
    IF n_bad > 0 THEN
        SELECT string_agg(r.role_code || '×' || o.code, ', ' ORDER BY r.role_code || '×' || o.code)
          INTO v_txt
          FROM _v0259_orphan o JOIN sys_role r ON r.id = o.role_id;
        RAISE WARNING 'V0259：本库还有 % 对孤儿按钮（与本迁移无关，需人工或数据修复）—— %', n_bad, v_txt;
    END IF;

    RAISE NOTICE 'V0259 完成：5 个实例侧按钮已归位（本次改父页 % 行）/ 兜底补授 % 行 / 推进 token_version % 个账号',
        n_moved, n_fix, n_ver;
END $$;
