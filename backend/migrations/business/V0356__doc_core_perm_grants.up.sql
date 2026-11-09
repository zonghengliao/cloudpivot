-- ============================================================================
-- V0356：业务单据核心权限码「零授予」补齐（销售退货单 / 收款退款单 / 计量单位新增）
--
-- 卡：t_c7038fa7（E2E-FIX-3，P2，内控红线）
--
-- # 一、现场（E2E-02 演练发现 + 全新库 356 条迁移实测）
--
--   缺陷一（卡面）：`business:erp:sales-return:{create,list,status}` 在 `sys_role_menu`
--   **一行授权都没有**。V0304 只种了菜单承载（1 页面 + 2 按钮），并明确「本迁移不写
--   sys_role_menu：授给哪些角色是管理决策」⇒ 除超管（`*:*:*` 短路）外没人能调。
--   实测症状：`e2e02_appr` 挂了 销售主管 / 财务经理 / 财务总监 / 总经理 / 仓管 五个角色，
--   PATCH 退货单状态仍 **403**；建单 / 列表同样 403。
--   后果：退货单的「审批」只能超管自己批（自建自批），**职责分离形同虚设**。
--
--   缺陷二（② 同类扫描顺带抓到，同一类死锁）：
--     · `business:erp:receipt-refund:{list,detail,create,allocate,status}` —— V0295 同口径
--       零授予（收款退款单 = 销售链「冲收款」的唯一入口，财务一条都用不了）；
--     · `business:erp:unit:create` —— V0141/V0258 只把 `unit:list` 授给了采购/仓储/总经理，
--       建单码零授予 ⇒ 计量单位只能超管建（主数据维护不了新单位就上不了新物料）。
--   扫描口径与完整清单见 `docs/evidence/20261109-e2efix3-perm-grants/`，常驻闸见
--   `scripts/verify/core_perm_grants.py`（本卡新增，run_all.sh ⑤ 挂载）。
--
-- # 二、矩阵（10 对，且**只**授这些）
--
--   | 角色               | 权限码                                                              | 依据 |
--   |--------------------|---------------------------------------------------------------------|------|
--   | SALES_CLERK        | sales-return:list, sales-return:create                              | 建单 + 看列表（销售员执行） |
--   | SALES_SUPERVISOR   | sales-return:list, sales-return:status                              | 提交 / 批准 / 待过账 / 作废（审批人） |
--   | FINANCE_MANAGER    | receipt-refund:list, :detail, :create, :allocate, :status           | 退款登记 → 核销 → 过账（财务） |
--   | PURCHASE_MANAGER   | unit:create                                                          | 计量单位维护（已有 unit:list） |
--
--   ## 为什么 SALES_CLERK 不给 `sales-return:status`（这是本卡的**核心口径**）
--   `status` 是**一个粗码**，一把盖住三条边：
--     DRAFT → SUBMITTED（提交） / SUBMITTED → APPROVED（批准） / PENDING_POST → POSTED（过账）。
--   `sales_return_handler::change_return_status` 里**没有按目标态做角色鉴权**（只有权限码
--   一道闸），所以把 `status` 授给建单人 = 「自己建、自己批」—— 正是本卡要修的那条红线。
--   SoD 只能在**授权层**切开：建单（CLERK）与审批（SUPERVISOR）落到不同角色。
--   代价要说清楚：销售员建完草稿**不能自己提交**，由主管提交/批准/过账。要拆细必须新增
--   `sales-return:submit` / `sales-return:approve` 两个码 + handler 按目标态分组校验
--   （含 `sys_role_menu` 与前端按钮一起改），属后续卡；本迁移不擅自造码（AGENTS.md 约定 7）。
--   本迁移末尾用断言把这条口径钉死：**SALES_CLERK 不得持有 sales-return:status**。
--
--   ## 为什么 SALES_SUPERVISOR 必须同时拿 `:list`
--   `sales-return:status` 按钮（menu_type=3，…3043）的父节点是页面节点…3041，其
--   `auth_code` 就是 `…:list`。只授按钮码不授父页码 ⇒ ① 权限树上按钮不可达、
--   `orphan_button_gate`（check.sh 闸⑦）判孤儿；② 侧边栏整棵子树对非超管不可见
--   （V0258 第五节口径），主管点不进退货单列表，等于没授。
--   `receipt-refund` 的 4 个按钮同理，`detail/create/allocate/status` 都挂在 `:list` 页面下。
--
--   ## 为什么不授 WAREHOUSE_MANAGER / GENERAL_MANAGER 的退货运维
--   「谁能看退货单」是管理决策，本卡只补**卡面点名 + 零授予**的那些码；仓储要不要看退货
--   回库、总经理要不要只读，留给下一次角色矩阵评审（不静默扩权，与 V0250/V0261/V0304 同口径）。
--
-- # 三、幂等 / 回滚 / 影响面
--   幂等：矩阵 10 对写进临时表并断言条数；授权按 (role_id, menu_id) NOT EXISTS + 确定性 id
--        （`md5(role_id|menu_id|'v0356-doc-perm-grants')`，与 V0258/V0267 同一套折叠公式）。
--        重放 = 0 行插入、0 行更新、不推进 token_version（不踢在线会话）。
--   回滚：down 按同一公式精确删本迁移插入的授权行 —— 历史上由别的迁移/数据修复授过的同码行
--        id 不同，**一行不碰**；不删菜单、不删角色（授权面的事只回滚授权面）。
--   影响面：只动 `sys_role_menu` 与受影响用户的 `sys_user.token_version`；
--        不改菜单 path/component/is_visible，不改任何权限码字符串。
--
-- # 四、自检（任一不符 → 迁移失败，宁可启动失败也不要「半套授权」上线）
--   4.1 矩阵 10 对（改了矩阵请同步改本断言）；
--   4.2 矩阵里的角色码都真实存在；
--   4.3 矩阵里的权限码在 `sys_menu` 都有活着的承载行；
--   4.4 越界红线：矩阵只能出现 business:erp:{sales-return|receipt-refund|unit}:* 且动作 ∈ create/list/status
--       （收据退款另有 detail/allocate —— 显式放行这两个动作）；
--   4.5 施工后每个 (角色, 码) 必须真持有；
--   4.6 无孤儿：被授的按钮码，其父页的码必须也授给了同一角色；
--   4.7 SoD：SALES_CLERK **不得**持有 sales-return:status。
-- ============================================================================

-- ---------- ① 矩阵（10 对，唯一真相） ----------
DROP TABLE IF EXISTS _dpg_matrix;
CREATE TEMP TABLE _dpg_matrix (
    role_code text NOT NULL,
    auth_code text NOT NULL,
    PRIMARY KEY (role_code, auth_code)
);

INSERT INTO _dpg_matrix (role_code, auth_code) VALUES
-- SALES_CLERK（销售员）：建单 + 看列表（**不给 status**，见头注释第二节）
    ('SALES_CLERK',      'business:erp:sales-return:list'),
    ('SALES_CLERK',      'business:erp:sales-return:create'),
-- SALES_SUPERVISOR（销售主管）：审批侧（status 的父页 list 必须一起授）
    ('SALES_SUPERVISOR', 'business:erp:sales-return:list'),
    ('SALES_SUPERVISOR', 'business:erp:sales-return:status'),
-- FINANCE_MANAGER（财务经理）：收款退款单全流程（登记 / 核销 / 状态）
    ('FINANCE_MANAGER',  'business:erp:receipt-refund:list'),
    ('FINANCE_MANAGER',  'business:erp:receipt-refund:detail'),
    ('FINANCE_MANAGER',  'business:erp:receipt-refund:create'),
    ('FINANCE_MANAGER',  'business:erp:receipt-refund:allocate'),
    ('FINANCE_MANAGER',  'business:erp:receipt-refund:status'),
-- PURCHASE_MANAGER（采购经理）：计量单位维护（已有 unit:list）
    ('PURCHASE_MANAGER', 'business:erp:unit:create');

DO $$
DECLARE
    v_n       integer;
    v_bad     integer;
    v_str     text;
    v_n_ins   integer := 0;
    v_n_ver   integer := 0;
BEGIN
    -- ---------- 4.1 矩阵条数 ----------
    SELECT count(*) INTO v_n FROM _dpg_matrix;
    IF v_n <> 10 THEN
        RAISE EXCEPTION 'V0356 自检 4.1 失败：矩阵 % 对（期望 10 —— 改了矩阵请同步改本断言）', v_n;
    END IF;

    -- ---------- 4.2 角色码存在（不存在 = 授权静默 0 行） ----------
    SELECT string_agg(DISTINCT mx.role_code, ', ') INTO v_str
      FROM _dpg_matrix mx
     WHERE NOT EXISTS (SELECT 1 FROM sys_role r
                        WHERE r.role_code = mx.role_code AND r.deleted_at IS NULL);
    IF v_str IS NOT NULL THEN
        RAISE EXCEPTION 'V0356 自检 4.2 失败：矩阵里的角色码不存在 → %', v_str;
    END IF;

    -- ---------- 4.3 权限码有菜单承载 ----------
    SELECT string_agg(DISTINCT mx.auth_code, ', ') INTO v_str
      FROM _dpg_matrix mx
     WHERE NOT EXISTS (SELECT 1 FROM sys_menu m
                        WHERE m.auth_code = mx.auth_code AND m.deleted_at IS NULL);
    IF v_str IS NOT NULL THEN
        RAISE EXCEPTION 'V0356 自检 4.3 失败：矩阵里的权限码在 sys_menu 无承载 → %', v_str;
    END IF;

    -- ---------- 4.4 越界红线（防复制粘贴串行 / 把平台治理码授给业务角色） ----------
    SELECT string_agg(DISTINCT mx.auth_code, ', ') INTO v_str
      FROM _dpg_matrix mx
     WHERE mx.auth_code NOT SIMILAR TO
           'business:erp:(sales-return|receipt-refund|unit):(create|list|status|detail|allocate)';
    IF v_str IS NOT NULL THEN
        RAISE EXCEPTION 'V0356 自检 4.4 失败：矩阵越界（命中本卡三组之外的码）→ %', v_str;
    END IF;

    -- ---------- 4.5 展开 + 插入（码的全部节点 + 无码祖先目录闭包，确定性 id） ----------
    DROP TABLE IF EXISTS _dpg_sel;
    CREATE TEMP TABLE _dpg_sel AS
    WITH RECURSIVE hit AS (
        -- 码对应的**全部**节点：页面行与其按钮行共码时整组给，权限树勾选才一致
        SELECT mx.role_code, r.id AS role_id, m.id AS menu_id
          FROM _dpg_matrix mx
          JOIN sys_role r ON r.role_code = mx.role_code AND r.deleted_at IS NULL
          JOIN sys_menu m ON m.auth_code = mx.auth_code AND m.deleted_at IS NULL
    ),
    anc AS (
        -- 祖先闭包：**只补无码目录**（有码的祖先必须出现在矩阵里才会被授，见 V0258 第五节）
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
                 FROM (SELECT md5(s.role_id::text || '|' || s.menu_id::text
                                  || '|v0356-doc-perm-grants') AS h) _h
           ), s.role_id, s.menu_id, now()
      FROM _dpg_sel s
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
                          AND ur.role_id IN (SELECT role_id FROM _dpg_sel));
        GET DIAGNOSTICS v_n_ver = ROW_COUNT;
    END IF;

    -- ---------- 4.5 施工后：每个 (角色,码) 必须真持有 ----------
    SELECT count(*) INTO v_bad
      FROM _dpg_matrix mx
     WHERE NOT EXISTS (
            SELECT 1 FROM sys_role_menu rm
              JOIN sys_menu m ON m.id = rm.menu_id AND m.deleted_at IS NULL
              JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
             WHERE r.role_code = mx.role_code AND m.auth_code = mx.auth_code);
    IF v_bad <> 0 THEN
        RAISE EXCEPTION 'V0356 自检 4.5 失败：施工后仍有 % 个 (角色,码) 未授权', v_bad;
    END IF;

    -- ---------- 4.6 无孤儿按钮：被授按钮码的**父页码**必须同角色真持有 ----------
    -- 口径落在库里（而不是只看矩阵）：父页码可能由更早的迁移授过（例如 unit:list 是 V0258
    -- 给 PURCHASE_MANAGER 的），只要「施工后该角色真持有父页码」即可，不要求它出现在本矩阵。
    SELECT count(*) INTO v_bad
      FROM _dpg_matrix mx
      JOIN sys_role r ON r.role_code = mx.role_code AND r.deleted_at IS NULL
      JOIN sys_menu b ON b.auth_code = mx.auth_code AND b.deleted_at IS NULL AND b.menu_type = 3
      JOIN sys_menu p ON p.id = b.parent_id AND p.deleted_at IS NULL
     WHERE coalesce(p.auth_code, '') <> ''
       AND NOT EXISTS (
            SELECT 1 FROM sys_role_menu rm
              JOIN sys_menu pm ON pm.id = rm.menu_id AND pm.deleted_at IS NULL
             WHERE rm.role_id = r.id AND pm.auth_code = p.auth_code);
    IF v_bad <> 0 THEN
        RAISE EXCEPTION 'V0356 自检 4.6 失败：矩阵里有 % 个按钮码的父页码该角色未持有（权限树上不可达）', v_bad;
    END IF;

    -- ---------- 4.7 SoD：建单角色不得持审批码 ----------
    SELECT count(*) INTO v_bad
      FROM sys_role_menu rm
      JOIN sys_role r ON r.id = rm.role_id AND r.deleted_at IS NULL
      JOIN sys_menu m ON m.id = rm.menu_id AND m.deleted_at IS NULL
     WHERE r.role_code = 'SALES_CLERK'
       AND m.auth_code = 'business:erp:sales-return:status';
    IF v_bad <> 0 THEN
        RAISE EXCEPTION 'V0356 自检 4.7 失败：SALES_CLERK 持有 sales-return:status（自建自批，职责分离失效）';
    END IF;

    RAISE NOTICE 'V0356 完成：矩阵 10 对（销售退货 建单/审批分离 + 收款退款 财务 + 计量单位新增），本次新增授权 % 行，推进 token_version % 人',
        v_n_ins, v_n_ver;
END $$;

DROP TABLE IF EXISTS _dpg_sel;
DROP TABLE IF EXISTS _dpg_matrix;
