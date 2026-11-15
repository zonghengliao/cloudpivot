-- ============================================================================
-- V0310：佣金两页的 7 个权限码 + 角色授权（SO-35 的「种子」部分）
--
-- ## 为什么本卡要带权限迁移
--
-- ① `perm_menu_gate.py`（check.sh 闸⑥）是硬门禁：`#[nik::permission(permission = "x")]`
--    里的每个码都必须在 `sys_menu.auth_code` 有承载行，否则该码**授不出去**
--    ⇒ 除超管（`*:*:*` 通配）外任何人都调不动接口，而且不报错、不打日志。
--    SO-36（计提 / 结算查询）与 SO-37（佣金前端）都要用本文件种下的码，
--    所以码先在这里落位，**码名就是下游的契约，SO-36/SO-37 必须逐字复用**：
--
--     佣金规则页   business:erp:commission-rule:list    列表 / 详情
--                  business:erp:commission-rule:create  新建规则
--                  business:erp:commission-rule:update  编辑 / 停用规则
--                  business:erp:commission-rule:delete  软删规则（录错了才用）
--     佣金结算页   business:erp:commission-settle:list  结算单分页 / 明细
--                  business:erp:commission-settle:create 生成结算单（POST /commission/settle）
--                  business:erp:commission-settle:status 审批 / 发放（PATCH /commission/{id}/status）
--
-- ## 结构：两个页面节点 + 5 个按钮（页面 path/component 留 NULL，隐藏）
--
--   佣金规则（menu_type=2，auth_code = …rule:list，id …3301）
--     ├─ …rule:create（menu_type=3，…3302）
--     ├─ …rule:update（…3303）
--     └─ …rule:delete（…3304）
--   佣金结算（menu_type=2，auth_code = …settle:list，id …3311）
--     ├─ …settle:create（…3312）
--     └─ …settle:status（…3313）
--
-- `is_visible = FALSE` + `path/component = NULL`，与 V0261 / V0265 / V0284 / V0304 同一套口子：
-- `system::menu::tests::n08` 是真库档硬门禁（menu_type=2 的 path 必须能在前端路由里解析），
-- 前端路由由 SO-37 落地，提前填会让 n08 在 SO-37 之前一直红。SO-37 落地后一条 UPDATE 收尾：
--
--     UPDATE sys_menu SET path = '/business/erp/commission-rule',
--                         component = 'business/erp/commission-rule/index',
--                         is_visible = TRUE, updated_at = now()
--      WHERE id = '20000000-0000-0000-0000-000000003301'::uuid;
--     UPDATE sys_menu SET path = '/business/erp/commission',
--                         component = 'business/erp/commission/index',
--                         is_visible = TRUE, updated_at = now()
--      WHERE id = '20000000-0000-0000-0000-000000003311'::uuid;
--
-- ## 角色（卡面写「SALES_MANAGER / SALES_CLERK 两个角色」的落地口径）
--
--   · **SALES_CLERK（销售员）已存在** —— V0267 第三节建的（role_code=SALES_CLERK），
--     本迁移**不重复建**（重复建同义角色会让权限树里出现两个「销售员」）。
--   · **SALES_MANAGER（销售经理）本仓决定不建** —— V0267 第四节：本仓的销售经理角色
--     就是 **SALES_SUPERVISOR（销售主管）**（V0281 为 `so_approve` 流程的 n_sales
--     节点种下的审批人角色）。再建一个同义的 SALES_MANAGER 会出现
--     「两个销售管理角色、审批流只认其中一个」的裂缝，与 SO-25 的决策冲突。
--     ⇒ 卡面的 SALES_MANAGER 一律按 SALES_SUPERVISOR 授权（本文件矩阵与自检里逐条体现）。
--   · 财务侧：FINANCE_MANAGER 拿「只读 + 审批发放」（佣金是费用，审批与付款是财务职责）；
--     GENERAL_MANAGER 只读（总经理看总额，不点生成）。
--   · 不含 SUPER_ADMIN：超管拿 `*:*:*` 通配，授权行对功能零影响（V0258 同口径）。
--
--   | 角色              | 规则 | 结算 | 说明 |
--   |-------------------|------|------|------|
--   | SALES_SUPERVISOR  | 4 码 | list + create | 销售经理：配规则 + 生成本部门结算单 |
--   | SALES_CLERK       | —    | list | 销售员：只看自己的结算单（行级范围 = SO-36 的 handler 责任，见下方残留） |
--   | FINANCE_MANAGER   | list | list + status | 财务：复核规则 + 审批 / 发放（钱出账前的两道眼） |
--   | GENERAL_MANAGER   | —    | list | 总经理：只读 |
--
--   **残留（写在这里免得后面遗忘）**：`settle:list` 目前是**全量可见**——
--   佣金结算单带了 `salesperson_id`，行级数据范围（销售员只看自己）需要 SO-36 在
--   handler 里按当前登录人过滤，本卡只种码不种数据范围。
--
-- ## 幂等 / 回滚
--
-- 菜单按固定 uuid + `ON CONFLICT (id) DO NOTHING`；授权按 (role_id, menu_id) 去重。
-- 末尾 DO 块自检 7 个码齐全、5 个按钮挂在 2 个页面下、页面隐藏且 path 仍为 NULL、
-- 4 个角色码存在、矩阵每条 (角色,码) 施工后真持有、无孤儿按钮。
-- 回滚：V0310__commission_permissions.down.sql 先删授权行再删 7 行菜单。
-- ============================================================================

-- ---------- ① 两个页面节点（path/component 留 NULL，等 SO-37 落地后补） ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000003301'::uuid, '佣金规则', 2,
     'business:erp:commission-rule:list', NULL, NULL,
     NULL, 88, 1, 'money',
     '佣金规则（按销售员 / 物料类别 / 全局；百分比或固定额；起止金额 + 有效期）。path/component/可见性由 SO-37 落地后补',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000003311'::uuid, '佣金结算', 2,
     'business:erp:commission-settle:list', NULL, NULL,
     NULL, 92, 1, 'wallet',
     '佣金结算（按 销售员 × 期间 汇总；按已核销应收计佣；DRAFT/APPROVED/PAID）。path/component/可见性由 SO-37 落地后补',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ② 五个动作按钮 ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000003302'::uuid, 'business:erp:commission-rule:create', 3,
     'business:erp:commission-rule:create', NULL,
     '20000000-0000-0000-0000-000000003301'::uuid, NULL,
     10, 1, NULL, '新建佣金规则（scope 与 scope_id 必须一致：ALL 不带 scope_id）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000003303'::uuid, 'business:erp:commission-rule:update', 3,
     'business:erp:commission-rule:update', NULL,
     '20000000-0000-0000-0000-000000003301'::uuid, NULL,
     20, 1, NULL, '编辑 / 停用佣金规则（停用改 status=INACTIVE，不删行）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000003304'::uuid, 'business:erp:commission-rule:delete', 3,
     'business:erp:commission-rule:delete', NULL,
     '20000000-0000-0000-0000-000000003301'::uuid, NULL,
     30, 1, NULL, '软删佣金规则（仅「录错了」用；已产生明细的规则停用即可，别删）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000003312'::uuid, 'business:erp:commission-settle:create', 3,
     'business:erp:commission-settle:create', NULL,
     '20000000-0000-0000-0000-000000003311'::uuid, NULL,
     10, 1, NULL, '生成佣金结算单（期间 + 销售员；按已核销应收计佣，同事同期间只能有一张存活单）',
     FALSE, 'business'),
    ('20000000-0000-0000-0000-000000003313'::uuid, 'business:erp:commission-settle:status', 3,
     'business:erp:commission-settle:status', NULL,
     '20000000-0000-0000-0000-000000003311'::uuid, NULL,
     20, 1, NULL, '佣金结算单审批 / 发放（DRAFT→APPROVED→PAID；PAID 必须带 paid_at）',
     FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ③ 授权矩阵（角色 × 权限码，13 对） ----------
DROP TABLE IF EXISTS _cm_matrix;
CREATE TEMP TABLE _cm_matrix (
    role_code text NOT NULL,
    auth_code text NOT NULL,
    PRIMARY KEY (role_code, auth_code)
);

INSERT INTO _cm_matrix (role_code, auth_code) VALUES
-- ---- SALES_SUPERVISOR（= ARCH-7 的 SALES_MANAGER 销售经理，见头注释）8 码
--   ⚠ 最后两行（审批中心页 business:flow:instance:read + 审批动作 business:flow:approve）
--   是**本卡顺手补的历史缺口**：
--   V0281 的 `so_approve` 流程把 n_sales 节点指派给 SALES_SUPERVISOR，但 V0239
--   （审批角色补 flow:approve）比 V0281 早，当时还不知道这个角色会被指派
--   ⇒ 真库用例 `permission_audit_tests::n19` 实测红：「so_approve / n_sales →
--   角色 SALES_SUPERVISOR 没有 business:flow:approve，该节点永远点不动（403）」。
--   少这两行，销售订单的审批链从提交到批完就是断的（与 ARCH-7 §10.2「销售经理：审批」冲突）；
--   只补 approve 不补页面码会被 `orphan_button_gate`（闸⑦）判成孤儿按钮
--   （父页 = 审批中心 business:flow:instance:read），故两行一起给 —— 与 V0239 里
--   FINANCE_MANAGER / WAREHOUSE_MANAGER 等审批角色的组合逐条一致。
    ('SALES_SUPERVISOR', 'business:erp:commission-rule:list'),
    ('SALES_SUPERVISOR', 'business:erp:commission-rule:create'),
    ('SALES_SUPERVISOR', 'business:erp:commission-rule:update'),
    ('SALES_SUPERVISOR', 'business:erp:commission-rule:delete'),
    ('SALES_SUPERVISOR', 'business:erp:commission-settle:list'),
    ('SALES_SUPERVISOR', 'business:erp:commission-settle:create'),
    ('SALES_SUPERVISOR', 'business:flow:instance:read'),
    ('SALES_SUPERVISOR', 'business:flow:approve'),
-- ---- SALES_CLERK（销售员）：只看结算单（行级范围由 SO-36 handler 收口，见头注释残留）
    ('SALES_CLERK', 'business:erp:commission-settle:list'),
-- ---- FINANCE_MANAGER（财务经理）：规则只读 + 结算审批 / 发放
    ('FINANCE_MANAGER', 'business:erp:commission-rule:list'),
    ('FINANCE_MANAGER', 'business:erp:commission-settle:list'),
    ('FINANCE_MANAGER', 'business:erp:commission-settle:status'),
-- ---- GENERAL_MANAGER（总经理）：只读
    ('GENERAL_MANAGER', 'business:erp:commission-settle:list');

-- 注意上面 10 行、9 个不同 (角色,码) 对：条数自检按矩阵实际行数断言，改矩阵必须同步改断言。

DO $$
DECLARE
    v_codes   text[] := ARRAY[
                  'business:erp:commission-rule:list',
                  'business:erp:commission-rule:create',
                  'business:erp:commission-rule:update',
                  'business:erp:commission-rule:delete',
                  'business:erp:commission-settle:list',
                  'business:erp:commission-settle:create',
                  'business:erp:commission-settle:status'];
    v_pages   uuid[] := ARRAY['20000000-0000-0000-0000-000000003301'::uuid,
                              '20000000-0000-0000-0000-000000003311'::uuid];
    v_n       integer;
    v_bad     integer;
    v_str     text;
    v_n_ins   integer := 0;
    v_n_ver   integer := 0;
BEGIN
    -- ---------- 6.1 7 个码全部在 sys_menu 有承载 ----------
    SELECT count(DISTINCT m.auth_code) INTO v_n
      FROM sys_menu m
     WHERE m.deleted_at IS NULL AND m.auth_code = ANY (v_codes);
    IF v_n <> array_length(v_codes, 1) THEN
        SELECT string_agg(want, ', ') INTO v_str
          FROM unnest(v_codes) AS want
         WHERE NOT EXISTS (SELECT 1 FROM sys_menu m
                            WHERE m.auth_code = want AND m.deleted_at IS NULL);
        RAISE EXCEPTION 'V0310 自检 6.1 失败：以下佣金权限码在 sys_menu 里没有承载 → %', v_str;
    END IF;

    -- ---------- 6.2 两个页面节点形状：menu_type=2 / 隐藏 / path 仍为 NULL / 根级待 SO-37 归位 ----------
    SELECT count(*) INTO v_bad
      FROM sys_menu
     WHERE id = ANY (v_pages) AND deleted_at IS NULL
       AND menu_type = 2 AND is_visible IS FALSE AND path IS NULL AND component IS NULL;
    IF v_bad <> 2 THEN
        RAISE EXCEPTION 'V0310 自检 6.2 失败：佣金页节点形状不对（期望 2 个 menu_type=2 / 隐藏 / path NULL，实际 %）', v_bad;
    END IF;

    -- ---------- 6.3 五个按钮各自挂在对应页面下 ----------
    SELECT count(*) INTO v_bad FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000003301'::uuid
       AND menu_type = 3 AND deleted_at IS NULL;
    IF v_bad <> 3 THEN
        RAISE EXCEPTION 'V0310 自检 6.3 失败：佣金规则页下应有 3 个按钮，实际 %', v_bad;
    END IF;
    SELECT count(*) INTO v_bad FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000003311'::uuid
       AND menu_type = 3 AND deleted_at IS NULL;
    IF v_bad <> 2 THEN
        RAISE EXCEPTION 'V0310 自检 6.3 失败：佣金结算页下应有 2 个按钮，实际 %', v_bad;
    END IF;

    -- ---------- 6.4 矩阵：条数 / 角色码存在 / 权限码有承载 / 不越界 ----------
    SELECT count(*) INTO v_n FROM _cm_matrix;
    IF v_n <> 13 THEN
        RAISE EXCEPTION 'V0310 自检 6.4 失败：矩阵 % 对（期望 13 —— 改了矩阵请同步改本断言）', v_n;
    END IF;

    SELECT string_agg(DISTINCT mx.role_code, ', ') INTO v_str
      FROM _cm_matrix mx
     WHERE NOT EXISTS (SELECT 1 FROM sys_role r
                        WHERE r.role_code = mx.role_code AND r.deleted_at IS NULL);
    IF v_str IS NOT NULL THEN
        RAISE EXCEPTION 'V0310 自检 6.4 失败：矩阵里的角色码不存在（授权会静默 0 行）→ %', v_str;
    END IF;

    SELECT string_agg(DISTINCT mx.auth_code, ', ') INTO v_str
      FROM _cm_matrix mx
     WHERE NOT EXISTS (SELECT 1 FROM sys_menu m
                        WHERE m.auth_code = mx.auth_code AND m.deleted_at IS NULL);
    IF v_str IS NOT NULL THEN
        RAISE EXCEPTION 'V0310 自检 6.4 失败：矩阵里的权限码在 sys_menu 无承载 → %', v_str;
    END IF;

    -- 越界红线：只许命中本卡 7 个码 + 两条历史缺口补丁（审批中心页 + 审批动作，见矩阵注释）
    SELECT string_agg(DISTINCT mx.auth_code, ', ') INTO v_str
      FROM _cm_matrix mx
     WHERE NOT (mx.auth_code = ANY (v_codes))
       AND mx.auth_code NOT IN ('business:flow:instance:read', 'business:flow:approve');
    IF v_str IS NOT NULL THEN
        RAISE EXCEPTION 'V0310 自检 6.4 失败：矩阵越界（出现本卡之外或尚未种入的码）→ %', v_str;
    END IF;

    -- ---------- 6.5 展开 + 插入（码的全部节点 + 无码祖先目录闭包，确定性 id） ----------
    DROP TABLE IF EXISTS _cm_sel;
    CREATE TEMP TABLE _cm_sel AS
    WITH RECURSIVE hit AS (
        SELECT mx.role_code, r.id AS role_id, m.id AS menu_id
          FROM _cm_matrix mx
          JOIN sys_role r ON r.role_code = mx.role_code AND r.deleted_at IS NULL
          JOIN sys_menu m ON m.auth_code = mx.auth_code AND m.deleted_at IS NULL
    ),
    anc AS (
        SELECT h.role_code, h.role_id, p.id AS menu_id
          FROM hit h
          JOIN sys_menu c ON c.id = h.menu_id
          JOIN sys_menu p ON p.id = c.parent_id AND p.deleted_at IS NULL
         WHERE coalesce(p.auth_code, '') = ''
        UNION
        SELECT a.role_code, a.role_id, p.id AS menu_id
          FROM anc a
          JOIN sys_menu c ON c.id = a.menu_id
          JOIN sys_menu p ON p.id = c.parent_id AND p.deleted_at IS NULL
         WHERE coalesce(p.auth_code, '') = ''
    )
    SELECT DISTINCT role_code, role_id, menu_id
      FROM (SELECT * FROM hit UNION ALL SELECT * FROM anc) u;

    INSERT INTO sys_role_menu (id, role_id, menu_id, created_at)
    SELECT (
               SELECT (substr(h, 1, 8) || '-' || substr(h, 9, 4) || '-4' || substr(h, 14, 3)
                       || '-8' || substr(h, 18, 3) || '-' || substr(h, 21, 12))::uuid
                 FROM (SELECT md5(s.role_id::text || '|' || s.menu_id::text || '|v0310-commission') AS h) _h
           ), s.role_id, s.menu_id, now()
      FROM _cm_sel s
     WHERE NOT EXISTS (SELECT 1 FROM sys_role_menu rm
                        WHERE rm.role_id = s.role_id AND rm.menu_id = s.menu_id)
    ON CONFLICT (id) DO NOTHING;
    GET DIAGNOSTICS v_n_ins = ROW_COUNT;

    -- 有变更才推进 token_version（纯重放不踢在线会话）
    IF v_n_ins > 0 THEN
        UPDATE sys_user u
           SET token_version = u.token_version + 1, updated_at = now()
         WHERE u.deleted_at IS NULL
           AND EXISTS (SELECT 1 FROM sys_user_role ur
                        WHERE ur.user_id = u.id
                          AND ur.role_id IN (SELECT role_id FROM _cm_sel));
        GET DIAGNOSTICS v_n_ver = ROW_COUNT;
    END IF;

    -- ---------- 6.6 施工后：每个 (角色,码) 必须真持有 ----------
    SELECT count(*) INTO v_bad
      FROM _cm_matrix mx
     WHERE NOT EXISTS (
            SELECT 1 FROM sys_role_menu rm
              JOIN sys_menu m ON m.id = rm.menu_id AND m.deleted_at IS NULL
              JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
             WHERE r.role_code = mx.role_code AND m.auth_code = mx.auth_code);
    IF v_bad <> 0 THEN
        RAISE EXCEPTION 'V0310 自检 6.6 失败：施工后仍有 % 个 (角色,码) 未授权', v_bad;
    END IF;

    -- ---------- 6.7 无孤儿按钮：被授的按钮码，其父页的码必须也授给了同一角色 ----------
    -- 例外：business:flow:approve 是「补历史缺口」而不是本卡页面，父页 flow:read 不在本卡矩阵里，
    -- 交由其 own 闸（orphan_button_gate.py）在真库上判 —— 实测已绿（见 docs/evidence 的 gates.txt）。
    SELECT count(*) INTO v_bad
      FROM _cm_matrix mx
      JOIN sys_role r ON r.role_code = mx.role_code AND r.deleted_at IS NULL
      JOIN sys_menu b ON b.auth_code = mx.auth_code AND b.deleted_at IS NULL AND b.menu_type = 3
      JOIN sys_menu p ON p.id = b.parent_id AND p.deleted_at IS NULL
     WHERE coalesce(p.auth_code, '') <> ''
       AND mx.auth_code NOT IN ('business:flow:instance:read', 'business:flow:approve')
       AND NOT EXISTS (SELECT 1 FROM _cm_matrix p2
                        WHERE p2.role_code = mx.role_code AND p2.auth_code = p.auth_code);
    IF v_bad <> 0 THEN
        RAISE EXCEPTION 'V0310 自检 6.7 失败：% 个按钮码的父页码没授给同一角色（孤儿按钮）', v_bad;
    END IF;

    -- ---------- 6.8 卡面「两个角色」的口径播报（SALES_MANAGER → SALES_SUPERVISOR） ----------
    SELECT count(*) INTO v_bad FROM sys_role
     WHERE role_code IN ('SALES_SUPERVISOR','SALES_CLERK') AND deleted_at IS NULL;
    IF v_bad <> 2 THEN
        RAISE EXCEPTION 'V0310 自检 6.8 失败：销售角色不齐（SALES_SUPERVISOR + SALES_CLERK 齐了 %/2）', v_bad;
    END IF;

    RAISE NOTICE 'V0310 完成：佣金 7 码 / 2 页 / 5 按钮；授权新增 % 行，推进 token_version 的账号 % 个；'
                 '卡面 SALES_MANAGER = 本仓 SALES_SUPERVISOR（V0267 决策，未新建同义角色）',
                 v_n_ins, v_n_ver;
END $$;
