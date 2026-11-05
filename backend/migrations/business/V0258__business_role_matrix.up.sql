-- ============================================================================
-- V0258：业务角色权限矩阵 —— 把「生产已验证的授予」固化成装配资产
--
-- 依据：kanban 卡 t_227464dd（P2，新装配库权限矩阵缺卡）
--
-- # 一、问题（真库实测，非推测）
--   空库首启时 `migrations/seed/S__role_data.sql` 只种 SUPER_ADMIN，且**种子在所有版本化
--   迁移之后**执行（nik-db `MigrationKind::execution_rank`：versioned=1 < seed=3）。
--   而 V0141…V0213 共 27 条迁移把授权写在版本化迁移里，并带空跑保护
--   `IF n_roles = 0 THEN RAISE NOTICE '…跳过授权…'` —— 空库上必然命中。
--   业务角色又由 V0182/V0191/V0206 **更晚**的迁移才建出来，没有任何机制回头补授权。
--
--   复现（cloudpivot_t2274 = 全量 189 条版本化迁移 + 10 个种子 + bootstrap，无人点过权限页）：
--     FINANCE_MANAGER 持有 74 个码（生产 142）；5 个验收码里 4 个「零角色持有」：
--       business:erp:period:remind    → <无任何角色>
--       business:erp:voucher:create   → <无任何角色>
--       business:erp:stock-in:status  → <无任何角色>
--       business:erp:transfer:status  → <无任何角色>
--       business:erp:voucher:post     → 仅 SUPER_ADMIN（来自 S__voucher_three_state_permissions）
--   后果：非超管看不到「催办」按钮（前端 canRemind=false）；超管点催办也发不出去 ——
--   `period_owner::load_owner` 按 `sys_menu.auth_code='business:erp:voucher:create'` 反查角色，
--   查不到 ⇒ 每条明细都算「未指定处理人」→ sent=0 / skipped_no_owner=N。
--
-- # 二、为什么不是「把 V0207 改对」，也不是方案 A/B/C
--   · 已应用迁移有内容指纹校验、**不可改写**（AGENTS.md 约定 6），只能新增。
--   · 方案 A「新增一条 bootstrap 之后的版本化迁移」在架构上不存在：bootstrap 由
--     `App::on_startup()` 调用，**在所有迁移之后**；版本化迁移永远跑在种子与 bootstrap 之前。
--     但**新版本化迁移对已存在的业务角色有效** —— 角色由 V0182/V0191/V0206 更早建出，
--     所以 V0258 在空库上执行时 sys_role 已有这 7 个角色。这正是本文件能一次补齐的原因。
--   · 方案 B（框架层自动重放空跑迁移）要改 nik-db 执行器，且「哪些迁移空跑过」无可靠判据
--     （迁移本身不记录是否命中 0 行）；风险远大于收益。
--   · 方案 C（授权改数据种子）与 B 同病且更糟：种子是 repeatable，**指纹一改就重灌**，
--     会把「管理员在权限页撤销的授予」悄悄加回来 —— 权限表最不能有这种副作用。
--   ⇒ 选「版本化迁移 + 按码判重 + 确定性 id 回退」：一次性、指纹锁定、可 down、不复活撤销。
--
-- # 三、矩阵从哪来（唯一真相源）
--   生产 172.18.10.144 / cloudpivot 当前生效的 `sys_role_menu`（business:* 码，7 个真实业务
--   角色，共 450 对）。这套矩阵是 t_34cee9ea「业务角色按职能归位补码」
--   （scripts/datafix/bizperm/，plan.sql 为声明矩阵）施工后的**运行结果**，已是治理过的基线；
--   本文件把它冻结进装配路径，新库自动收敛到生产。
--
--   不含 SUPER_ADMIN：超管在 `system/auth/model.rs::load_principal_snapshot` 拿 `*:*:*` 通配，
--   前端 `hasPermission` 也认通配 ⇒ 它的 sys_role_menu 行对功能零影响，只影响权限树展示；
--   `scripts/datafix/bizperm/expand.sql` 第 4.1 条亦明令禁止矩阵里出现超管。
--
-- # 四、为什么「按码判重」而不是「按行判重」（生产零变更的关键）
--   同一 auth_code 可能挂在多个节点上（页面 menu_type=2 与其按钮 3 共用一码，本仓有 26 例）。
--   若按 (role, menu_id) 判重，生产会因「码已有、节点行不同」被插入若干同码行。
--   本文件统一按 **码** 判重：角色已持有该码 ⇒ 整组跳过。
--   ⇒ 生产上（450 对全部已存在）执行本迁移选中 **0 行**，授权行逐行不变、权限码集合不变。
--     验证：`python3 scripts/verify/role_matrix_gate.py --dry-run`（生产只读预演，期望 0 行）。
--
-- # 五、祖先闭包（侧边栏渲染必需）
--   非超管侧边栏靠 sys_menu 树渲染，父目录没授权整棵子树会被静默丢掉
--   （scripts/datafix/bizperm/expand.sql 第 3 步踩过）。本文件为每个被授节点补祖先目录，
--   **但只补「无码目录」或「本次要一并授予该码的目录」** —— 否则会顺手加上矩阵里没有的码。
--   实测：FLOW_CONFIG_APPROVER 持有 business:flow:approve，其父页 /flow/def 的码是
--   business:flow:read，无条件补祖先会把 flow:read 授予该角色 = 越出矩阵、且改生产。
--   这类「孤儿按钮」（有按钮码、缺父页码）是**既有**缺陷，保持与生产同形，另开卡处理。
--
-- # 六、幂等 / 回滚 / 维护
--   幂等：按码 NOT EXISTS + `ON CONFLICT (id) DO NOTHING`；重放选中 0 行、不踢在线会话。
--   回滚：`V0258__business_role_matrix.down.sql` 用**同一条确定性 id 公式**精确删除，
--        只删本迁移插入的行，既有授权一行不碰。
--   维护：新增权限码后矩阵不会自动跟上 —— 由 `scripts/verify/role_matrix_gate.py` 报警，
--        按提示**追加新迁移**，不改本文件（已应用迁移不可改写）。
-- ============================================================================

-- ---------- 1) 矩阵（角色 × 权限码，450 对）----------
DROP TABLE IF EXISTS _brm_matrix;
CREATE TEMP TABLE _brm_matrix (
    role_code text NOT NULL,
    auth_code text NOT NULL,
    PRIMARY KEY (role_code, auth_code)
);

INSERT INTO _brm_matrix (role_code, auth_code) VALUES
-- ---- DEPT_LEADER（67 个码）
    ('DEPT_LEADER', 'business:announcement:list'), ('DEPT_LEADER', 'business:announcement:read'), ('DEPT_LEADER', 'business:erp:approval-flow:health'),
    ('DEPT_LEADER', 'business:erp:asset-category:create'), ('DEPT_LEADER', 'business:erp:asset-category:delete'), ('DEPT_LEADER', 'business:erp:asset-category:list'),
    ('DEPT_LEADER', 'business:erp:asset-category:update'), ('DEPT_LEADER', 'business:erp:asset:create'), ('DEPT_LEADER', 'business:erp:asset:delete'),
    ('DEPT_LEADER', 'business:erp:asset:detail'), ('DEPT_LEADER', 'business:erp:asset:dispose'), ('DEPT_LEADER', 'business:erp:asset:list'),
    ('DEPT_LEADER', 'business:erp:asset:transfer'), ('DEPT_LEADER', 'business:erp:asset:update'), ('DEPT_LEADER', 'business:erp:budget:adjust'),
    ('DEPT_LEADER', 'business:erp:budget:create'), ('DEPT_LEADER', 'business:erp:budget:delete'), ('DEPT_LEADER', 'business:erp:budget:list'),
    ('DEPT_LEADER', 'business:erp:budget:mapping'), ('DEPT_LEADER', 'business:erp:budget:report'), ('DEPT_LEADER', 'business:erp:budget:submit'),
    ('DEPT_LEADER', 'business:erp:budget:update'), ('DEPT_LEADER', 'business:erp:depreciation:list'), ('DEPT_LEADER', 'business:erp:depreciation:reverse'),
    ('DEPT_LEADER', 'business:erp:depreciation:run'), ('DEPT_LEADER', 'business:erp:expense-type:create'), ('DEPT_LEADER', 'business:erp:expense-type:delete'),
    ('DEPT_LEADER', 'business:erp:expense-type:list'), ('DEPT_LEADER', 'business:erp:expense-type:update'), ('DEPT_LEADER', 'business:erp:expense:cancel'),
    ('DEPT_LEADER', 'business:erp:expense:create'), ('DEPT_LEADER', 'business:erp:expense:delete'), ('DEPT_LEADER', 'business:erp:expense:detail'),
    ('DEPT_LEADER', 'business:erp:expense:list'), ('DEPT_LEADER', 'business:erp:expense:pay'), ('DEPT_LEADER', 'business:erp:expense:submit'),
    ('DEPT_LEADER', 'business:erp:expense:update'), ('DEPT_LEADER', 'business:flow:approve'), ('DEPT_LEADER', 'business:flow:instance:create'),
    ('DEPT_LEADER', 'business:flow:instance:read'), ('DEPT_LEADER', 'business:flow:instance:withdraw'), ('DEPT_LEADER', 'business:flow:read'),
    ('DEPT_LEADER', 'business:flow:state-edge:list'), ('DEPT_LEADER', 'business:flow:tasknode:read'), ('DEPT_LEADER', 'business:notice:list'),
    ('DEPT_LEADER', 'business:notice:mine'), ('DEPT_LEADER', 'business:notice:send'), ('DEPT_LEADER', 'business:notice:template:list'),
    ('DEPT_LEADER', 'business:project:create'), ('DEPT_LEADER', 'business:project:delete'), ('DEPT_LEADER', 'business:project:read'),
    ('DEPT_LEADER', 'business:project:update'), ('DEPT_LEADER', 'business:storage:download'), ('DEPT_LEADER', 'business:storage:list'),
    ('DEPT_LEADER', 'business:task:assign'), ('DEPT_LEADER', 'business:task:attachment:delete'), ('DEPT_LEADER', 'business:task:comment:delete'),
    ('DEPT_LEADER', 'business:task:create'), ('DEPT_LEADER', 'business:task:delete'), ('DEPT_LEADER', 'business:task:dependency'),
    ('DEPT_LEADER', 'business:task:member'), ('DEPT_LEADER', 'business:task:progress'), ('DEPT_LEADER', 'business:task:read'),
    ('DEPT_LEADER', 'business:task:status'), ('DEPT_LEADER', 'business:task:subtask'), ('DEPT_LEADER', 'business:task:update'),
    ('DEPT_LEADER', 'business:task:worklog'),
-- ---- PURCHASE_MANAGER（66 个码）
    ('PURCHASE_MANAGER', 'business:announcement:read'), ('PURCHASE_MANAGER', 'business:attachment:delete'), ('PURCHASE_MANAGER', 'business:attachment:list'),
    ('PURCHASE_MANAGER', 'business:attachment:upload'), ('PURCHASE_MANAGER', 'business:erp:approval-flow:health'), ('PURCHASE_MANAGER', 'business:erp:contract:create'),
    ('PURCHASE_MANAGER', 'business:erp:contract:delete'), ('PURCHASE_MANAGER', 'business:erp:contract:list'), ('PURCHASE_MANAGER', 'business:erp:contract:status'),
    ('PURCHASE_MANAGER', 'business:erp:contract:update'), ('PURCHASE_MANAGER', 'business:erp:doc:cancel-cascade'), ('PURCHASE_MANAGER', 'business:erp:doc:cancel-impact'),
    ('PURCHASE_MANAGER', 'business:erp:inventory-location:list'), ('PURCHASE_MANAGER', 'business:erp:inventory-txn:list'), ('PURCHASE_MANAGER', 'business:erp:inventory:list'),
    ('PURCHASE_MANAGER', 'business:erp:location:detail'), ('PURCHASE_MANAGER', 'business:erp:location:list'), ('PURCHASE_MANAGER', 'business:erp:material:create'),
    ('PURCHASE_MANAGER', 'business:erp:material:delete'), ('PURCHASE_MANAGER', 'business:erp:material:detail'), ('PURCHASE_MANAGER', 'business:erp:material:list'),
    ('PURCHASE_MANAGER', 'business:erp:material:update'), ('PURCHASE_MANAGER', 'business:erp:payable:detail'), ('PURCHASE_MANAGER', 'business:erp:payable:list'),
    ('PURCHASE_MANAGER', 'business:erp:purchase-order-item:list'), ('PURCHASE_MANAGER', 'business:erp:purchase-order:create'), ('PURCHASE_MANAGER', 'business:erp:purchase-order:delete'),
    ('PURCHASE_MANAGER', 'business:erp:purchase-order:list'), ('PURCHASE_MANAGER', 'business:erp:purchase-order:status'), ('PURCHASE_MANAGER', 'business:erp:purchase-order:submit-approval'),
    ('PURCHASE_MANAGER', 'business:erp:purchase-order:update'), ('PURCHASE_MANAGER', 'business:erp:reminder:run'), ('PURCHASE_MANAGER', 'business:erp:serial:detail'),
    ('PURCHASE_MANAGER', 'business:erp:serial:list'), ('PURCHASE_MANAGER', 'business:erp:serial:view'), ('PURCHASE_MANAGER', 'business:erp:stock-in:detail'),
    ('PURCHASE_MANAGER', 'business:erp:stock-in:list'), ('PURCHASE_MANAGER', 'business:erp:stock-out:create'), ('PURCHASE_MANAGER', 'business:erp:stock-out:delete'),
    ('PURCHASE_MANAGER', 'business:erp:stock-out:detail'), ('PURCHASE_MANAGER', 'business:erp:stock-out:list'), ('PURCHASE_MANAGER', 'business:erp:stock-out:status'),
    ('PURCHASE_MANAGER', 'business:erp:stock-out:update'), ('PURCHASE_MANAGER', 'business:erp:stock-take:detail'), ('PURCHASE_MANAGER', 'business:erp:stock-take:list'),
    ('PURCHASE_MANAGER', 'business:erp:supplier:create'), ('PURCHASE_MANAGER', 'business:erp:supplier:list'), ('PURCHASE_MANAGER', 'business:erp:supplier:update'),
    ('PURCHASE_MANAGER', 'business:erp:transfer:detail'), ('PURCHASE_MANAGER', 'business:erp:transfer:list'), ('PURCHASE_MANAGER', 'business:erp:unit:list'),
    ('PURCHASE_MANAGER', 'business:erp:warehouse:detail'), ('PURCHASE_MANAGER', 'business:erp:warehouse:list'), ('PURCHASE_MANAGER', 'business:flow:approve'),
    ('PURCHASE_MANAGER', 'business:flow:instance:create'), ('PURCHASE_MANAGER', 'business:flow:instance:read'), ('PURCHASE_MANAGER', 'business:flow:instance:withdraw'),
    ('PURCHASE_MANAGER', 'business:flow:read'), ('PURCHASE_MANAGER', 'business:flow:state-edge:list'), ('PURCHASE_MANAGER', 'business:flow:tasknode:read'),
    ('PURCHASE_MANAGER', 'business:notice:list'), ('PURCHASE_MANAGER', 'business:notice:mine'), ('PURCHASE_MANAGER', 'business:notice:send'),
    ('PURCHASE_MANAGER', 'business:notice:template:list'), ('PURCHASE_MANAGER', 'business:storage:download'), ('PURCHASE_MANAGER', 'business:storage:list'),
-- ---- WAREHOUSE_MANAGER（71 个码）
    ('WAREHOUSE_MANAGER', 'business:announcement:read'), ('WAREHOUSE_MANAGER', 'business:attachment:delete'), ('WAREHOUSE_MANAGER', 'business:attachment:list'),
    ('WAREHOUSE_MANAGER', 'business:attachment:upload'), ('WAREHOUSE_MANAGER', 'business:erp:approval-flow:health'), ('WAREHOUSE_MANAGER', 'business:erp:doc:cancel-cascade'),
    ('WAREHOUSE_MANAGER', 'business:erp:doc:cancel-impact'), ('WAREHOUSE_MANAGER', 'business:erp:inventory-location:list'), ('WAREHOUSE_MANAGER', 'business:erp:inventory-txn:list'),
    ('WAREHOUSE_MANAGER', 'business:erp:inventory:audit'), ('WAREHOUSE_MANAGER', 'business:erp:inventory:list'), ('WAREHOUSE_MANAGER', 'business:erp:location:create'),
    ('WAREHOUSE_MANAGER', 'business:erp:location:delete'), ('WAREHOUSE_MANAGER', 'business:erp:location:detail'), ('WAREHOUSE_MANAGER', 'business:erp:location:list'),
    ('WAREHOUSE_MANAGER', 'business:erp:location:update'), ('WAREHOUSE_MANAGER', 'business:erp:material:detail'), ('WAREHOUSE_MANAGER', 'business:erp:material:list'),
    ('WAREHOUSE_MANAGER', 'business:erp:purchase-order-item:list'), ('WAREHOUSE_MANAGER', 'business:erp:purchase-order:list'), ('WAREHOUSE_MANAGER', 'business:erp:reminder:run'),
    ('WAREHOUSE_MANAGER', 'business:erp:serial:detail'), ('WAREHOUSE_MANAGER', 'business:erp:serial:generate'), ('WAREHOUSE_MANAGER', 'business:erp:serial:list'),
    ('WAREHOUSE_MANAGER', 'business:erp:serial:status'), ('WAREHOUSE_MANAGER', 'business:erp:serial:view'), ('WAREHOUSE_MANAGER', 'business:erp:stock-in:create'),
    ('WAREHOUSE_MANAGER', 'business:erp:stock-in:delete'), ('WAREHOUSE_MANAGER', 'business:erp:stock-in:detail'), ('WAREHOUSE_MANAGER', 'business:erp:stock-in:list'),
    ('WAREHOUSE_MANAGER', 'business:erp:stock-in:status'), ('WAREHOUSE_MANAGER', 'business:erp:stock-in:submit-post-approval'), ('WAREHOUSE_MANAGER', 'business:erp:stock-in:update'),
    ('WAREHOUSE_MANAGER', 'business:erp:stock-out:create'), ('WAREHOUSE_MANAGER', 'business:erp:stock-out:delete'), ('WAREHOUSE_MANAGER', 'business:erp:stock-out:detail'),
    ('WAREHOUSE_MANAGER', 'business:erp:stock-out:list'), ('WAREHOUSE_MANAGER', 'business:erp:stock-out:status'), ('WAREHOUSE_MANAGER', 'business:erp:stock-out:update'),
    ('WAREHOUSE_MANAGER', 'business:erp:stock-take:create'), ('WAREHOUSE_MANAGER', 'business:erp:stock-take:delete'), ('WAREHOUSE_MANAGER', 'business:erp:stock-take:detail'),
    ('WAREHOUSE_MANAGER', 'business:erp:stock-take:list'), ('WAREHOUSE_MANAGER', 'business:erp:stock-take:status'), ('WAREHOUSE_MANAGER', 'business:erp:stock-take:update'),
    ('WAREHOUSE_MANAGER', 'business:erp:supplier:list'), ('WAREHOUSE_MANAGER', 'business:erp:transfer:create'), ('WAREHOUSE_MANAGER', 'business:erp:transfer:delete'),
    ('WAREHOUSE_MANAGER', 'business:erp:transfer:detail'), ('WAREHOUSE_MANAGER', 'business:erp:transfer:list'), ('WAREHOUSE_MANAGER', 'business:erp:transfer:status'),
    ('WAREHOUSE_MANAGER', 'business:erp:transfer:update'), ('WAREHOUSE_MANAGER', 'business:erp:unit:list'), ('WAREHOUSE_MANAGER', 'business:erp:warehouse:create'),
    ('WAREHOUSE_MANAGER', 'business:erp:warehouse:delete'), ('WAREHOUSE_MANAGER', 'business:erp:warehouse:detail'), ('WAREHOUSE_MANAGER', 'business:erp:warehouse:list'),
    ('WAREHOUSE_MANAGER', 'business:erp:warehouse:update'), ('WAREHOUSE_MANAGER', 'business:flow:approve'), ('WAREHOUSE_MANAGER', 'business:flow:instance:create'),
    ('WAREHOUSE_MANAGER', 'business:flow:instance:read'), ('WAREHOUSE_MANAGER', 'business:flow:instance:withdraw'), ('WAREHOUSE_MANAGER', 'business:flow:read'),
    ('WAREHOUSE_MANAGER', 'business:flow:state-edge:list'), ('WAREHOUSE_MANAGER', 'business:flow:tasknode:read'), ('WAREHOUSE_MANAGER', 'business:notice:list'),
    ('WAREHOUSE_MANAGER', 'business:notice:mine'), ('WAREHOUSE_MANAGER', 'business:notice:send'), ('WAREHOUSE_MANAGER', 'business:notice:template:list'),
    ('WAREHOUSE_MANAGER', 'business:storage:download'), ('WAREHOUSE_MANAGER', 'business:storage:list'),
-- ---- FINANCE_MANAGER（111 个码）
    ('FINANCE_MANAGER', 'business:announcement:read'), ('FINANCE_MANAGER', 'business:attachment:delete'), ('FINANCE_MANAGER', 'business:attachment:list'),
    ('FINANCE_MANAGER', 'business:attachment:upload'), ('FINANCE_MANAGER', 'business:erp:account:list'), ('FINANCE_MANAGER', 'business:erp:approval-flow:health'),
    ('FINANCE_MANAGER', 'business:erp:asset-category:create'), ('FINANCE_MANAGER', 'business:erp:asset-category:delete'), ('FINANCE_MANAGER', 'business:erp:asset-category:list'),
    ('FINANCE_MANAGER', 'business:erp:asset-category:update'), ('FINANCE_MANAGER', 'business:erp:asset:create'), ('FINANCE_MANAGER', 'business:erp:asset:delete'),
    ('FINANCE_MANAGER', 'business:erp:asset:detail'), ('FINANCE_MANAGER', 'business:erp:asset:dispose'), ('FINANCE_MANAGER', 'business:erp:asset:list'),
    ('FINANCE_MANAGER', 'business:erp:asset:transfer'), ('FINANCE_MANAGER', 'business:erp:asset:update'), ('FINANCE_MANAGER', 'business:erp:budget:adjust'),
    ('FINANCE_MANAGER', 'business:erp:budget:create'), ('FINANCE_MANAGER', 'business:erp:budget:delete'), ('FINANCE_MANAGER', 'business:erp:budget:list'),
    ('FINANCE_MANAGER', 'business:erp:budget:mapping'), ('FINANCE_MANAGER', 'business:erp:budget:report'), ('FINANCE_MANAGER', 'business:erp:budget:submit'),
    ('FINANCE_MANAGER', 'business:erp:budget:update'), ('FINANCE_MANAGER', 'business:erp:contract:list'), ('FINANCE_MANAGER', 'business:erp:depreciation:list'),
    ('FINANCE_MANAGER', 'business:erp:depreciation:reverse'), ('FINANCE_MANAGER', 'business:erp:depreciation:run'), ('FINANCE_MANAGER', 'business:erp:doc:cancel-cascade'),
    ('FINANCE_MANAGER', 'business:erp:doc:cancel-impact'), ('FINANCE_MANAGER', 'business:erp:expense-type:create'), ('FINANCE_MANAGER', 'business:erp:expense-type:delete'),
    ('FINANCE_MANAGER', 'business:erp:expense-type:list'), ('FINANCE_MANAGER', 'business:erp:expense-type:update'), ('FINANCE_MANAGER', 'business:erp:expense:cancel'),
    ('FINANCE_MANAGER', 'business:erp:expense:create'), ('FINANCE_MANAGER', 'business:erp:expense:delete'), ('FINANCE_MANAGER', 'business:erp:expense:detail'),
    ('FINANCE_MANAGER', 'business:erp:expense:list'), ('FINANCE_MANAGER', 'business:erp:expense:pay'), ('FINANCE_MANAGER', 'business:erp:expense:submit'),
    ('FINANCE_MANAGER', 'business:erp:expense:update'), ('FINANCE_MANAGER', 'business:erp:inventory:list'), ('FINANCE_MANAGER', 'business:erp:ledger:aux-balance'),
    ('FINANCE_MANAGER', 'business:erp:ledger:list'), ('FINANCE_MANAGER', 'business:erp:ledger:trial-balance'), ('FINANCE_MANAGER', 'business:erp:material:detail'),
    ('FINANCE_MANAGER', 'business:erp:material:list'), ('FINANCE_MANAGER', 'business:erp:opening:inventory'), ('FINANCE_MANAGER', 'business:erp:opening:read'),
    ('FINANCE_MANAGER', 'business:erp:opening:write'), ('FINANCE_MANAGER', 'business:erp:payable:cancel'), ('FINANCE_MANAGER', 'business:erp:payable:detail'),
    ('FINANCE_MANAGER', 'business:erp:payable:list'), ('FINANCE_MANAGER', 'business:erp:payable:pay'), ('FINANCE_MANAGER', 'business:erp:payable:submit-cancel'),
    ('FINANCE_MANAGER', 'business:erp:payable:withdraw-cancel'), ('FINANCE_MANAGER', 'business:erp:period:close'), ('FINANCE_MANAGER', 'business:erp:period:list'),
    ('FINANCE_MANAGER', 'business:erp:period:open'), ('FINANCE_MANAGER', 'business:erp:period:remind'), ('FINANCE_MANAGER', 'business:erp:period:reopen'),
    ('FINANCE_MANAGER', 'business:erp:purchase-order-item:list'), ('FINANCE_MANAGER', 'business:erp:purchase-order:list'), ('FINANCE_MANAGER', 'business:erp:reminder:run'),
    ('FINANCE_MANAGER', 'business:erp:report-item:list'), ('FINANCE_MANAGER', 'business:erp:report-item:manage'), ('FINANCE_MANAGER', 'business:erp:report:balance-sheet'),
    ('FINANCE_MANAGER', 'business:erp:report:cash-flow'), ('FINANCE_MANAGER', 'business:erp:report:export'), ('FINANCE_MANAGER', 'business:erp:report:income-statement'),
    ('FINANCE_MANAGER', 'business:erp:report:payable-aging'), ('FINANCE_MANAGER', 'business:erp:seq:list'), ('FINANCE_MANAGER', 'business:erp:stock-in:detail'),
    ('FINANCE_MANAGER', 'business:erp:stock-in:list'), ('FINANCE_MANAGER', 'business:erp:stock-out:detail'), ('FINANCE_MANAGER', 'business:erp:stock-out:list'),
    ('FINANCE_MANAGER', 'business:erp:stock-take:create'), ('FINANCE_MANAGER', 'business:erp:stock-take:delete'), ('FINANCE_MANAGER', 'business:erp:stock-take:detail'),
    ('FINANCE_MANAGER', 'business:erp:stock-take:list'), ('FINANCE_MANAGER', 'business:erp:stock-take:status'), ('FINANCE_MANAGER', 'business:erp:stock-take:update'),
    ('FINANCE_MANAGER', 'business:erp:supplier:list'), ('FINANCE_MANAGER', 'business:erp:voucher:audit'), ('FINANCE_MANAGER', 'business:erp:voucher:create'),
    ('FINANCE_MANAGER', 'business:erp:voucher:detail'), ('FINANCE_MANAGER', 'business:erp:voucher:list'), ('FINANCE_MANAGER', 'business:erp:voucher:post'),
    ('FINANCE_MANAGER', 'business:erp:voucher:reverse'), ('FINANCE_MANAGER', 'business:erp:voucher:submit-reverse'), ('FINANCE_MANAGER', 'business:erp:voucher:un-audit'),
    ('FINANCE_MANAGER', 'business:erp:voucher:withdraw-reverse'), ('FINANCE_MANAGER', 'business:erp:year-end:remind'), ('FINANCE_MANAGER', 'business:erp:yearend:close'),
    ('FINANCE_MANAGER', 'business:erp:yearend:read'), ('FINANCE_MANAGER', 'business:erp:yearend:reopen'), ('FINANCE_MANAGER', 'business:flow:approve'),
    ('FINANCE_MANAGER', 'business:flow:instance:create'), ('FINANCE_MANAGER', 'business:flow:instance:read'), ('FINANCE_MANAGER', 'business:flow:instance:withdraw'),
    ('FINANCE_MANAGER', 'business:flow:read'), ('FINANCE_MANAGER', 'business:flow:state-edge:list'), ('FINANCE_MANAGER', 'business:flow:tasknode:read'),
    ('FINANCE_MANAGER', 'business:notice:list'), ('FINANCE_MANAGER', 'business:notice:mine'), ('FINANCE_MANAGER', 'business:notice:send'),
    ('FINANCE_MANAGER', 'business:notice:template:list'), ('FINANCE_MANAGER', 'business:storage:download'), ('FINANCE_MANAGER', 'business:storage:list'),
-- ---- FINANCE_DIRECTOR（44 个码）
    ('FINANCE_DIRECTOR', 'business:attachment:delete'), ('FINANCE_DIRECTOR', 'business:attachment:list'), ('FINANCE_DIRECTOR', 'business:attachment:upload'),
    ('FINANCE_DIRECTOR', 'business:erp:asset-category:create'), ('FINANCE_DIRECTOR', 'business:erp:asset-category:delete'), ('FINANCE_DIRECTOR', 'business:erp:asset-category:list'),
    ('FINANCE_DIRECTOR', 'business:erp:asset-category:update'), ('FINANCE_DIRECTOR', 'business:erp:asset:create'), ('FINANCE_DIRECTOR', 'business:erp:asset:delete'),
    ('FINANCE_DIRECTOR', 'business:erp:asset:detail'), ('FINANCE_DIRECTOR', 'business:erp:asset:dispose'), ('FINANCE_DIRECTOR', 'business:erp:asset:list'),
    ('FINANCE_DIRECTOR', 'business:erp:asset:transfer'), ('FINANCE_DIRECTOR', 'business:erp:asset:update'), ('FINANCE_DIRECTOR', 'business:erp:budget:adjust'),
    ('FINANCE_DIRECTOR', 'business:erp:budget:create'), ('FINANCE_DIRECTOR', 'business:erp:budget:delete'), ('FINANCE_DIRECTOR', 'business:erp:budget:list'),
    ('FINANCE_DIRECTOR', 'business:erp:budget:mapping'), ('FINANCE_DIRECTOR', 'business:erp:budget:report'), ('FINANCE_DIRECTOR', 'business:erp:budget:submit'),
    ('FINANCE_DIRECTOR', 'business:erp:budget:update'), ('FINANCE_DIRECTOR', 'business:erp:depreciation:list'), ('FINANCE_DIRECTOR', 'business:erp:depreciation:reverse'),
    ('FINANCE_DIRECTOR', 'business:erp:depreciation:run'), ('FINANCE_DIRECTOR', 'business:erp:expense-type:create'), ('FINANCE_DIRECTOR', 'business:erp:expense-type:delete'),
    ('FINANCE_DIRECTOR', 'business:erp:expense-type:list'), ('FINANCE_DIRECTOR', 'business:erp:expense-type:update'), ('FINANCE_DIRECTOR', 'business:erp:expense:cancel'),
    ('FINANCE_DIRECTOR', 'business:erp:expense:create'), ('FINANCE_DIRECTOR', 'business:erp:expense:delete'), ('FINANCE_DIRECTOR', 'business:erp:expense:detail'),
    ('FINANCE_DIRECTOR', 'business:erp:expense:list'), ('FINANCE_DIRECTOR', 'business:erp:expense:pay'), ('FINANCE_DIRECTOR', 'business:erp:expense:submit'),
    ('FINANCE_DIRECTOR', 'business:erp:expense:update'), ('FINANCE_DIRECTOR', 'business:erp:reminder:run'), ('FINANCE_DIRECTOR', 'business:erp:report-item:list'),
    ('FINANCE_DIRECTOR', 'business:erp:report:payable-aging'), ('FINANCE_DIRECTOR', 'business:flow:approve'), ('FINANCE_DIRECTOR', 'business:flow:instance:create'),
    ('FINANCE_DIRECTOR', 'business:flow:instance:read'), ('FINANCE_DIRECTOR', 'business:flow:instance:withdraw'),
-- ---- GENERAL_MANAGER（87 个码）
    ('GENERAL_MANAGER', 'business:announcement:create'), ('GENERAL_MANAGER', 'business:announcement:delete'), ('GENERAL_MANAGER', 'business:announcement:list'),
    ('GENERAL_MANAGER', 'business:announcement:offline'), ('GENERAL_MANAGER', 'business:announcement:publish'), ('GENERAL_MANAGER', 'business:announcement:read'),
    ('GENERAL_MANAGER', 'business:announcement:update'), ('GENERAL_MANAGER', 'business:attachment:delete'), ('GENERAL_MANAGER', 'business:attachment:list'),
    ('GENERAL_MANAGER', 'business:attachment:upload'), ('GENERAL_MANAGER', 'business:erp:account:list'), ('GENERAL_MANAGER', 'business:erp:approval-flow:health'),
    ('GENERAL_MANAGER', 'business:erp:asset-category:list'), ('GENERAL_MANAGER', 'business:erp:asset:detail'), ('GENERAL_MANAGER', 'business:erp:asset:list'),
    ('GENERAL_MANAGER', 'business:erp:budget:list'), ('GENERAL_MANAGER', 'business:erp:budget:mapping'), ('GENERAL_MANAGER', 'business:erp:budget:report'),
    ('GENERAL_MANAGER', 'business:erp:contract:list'), ('GENERAL_MANAGER', 'business:erp:depreciation:list'), ('GENERAL_MANAGER', 'business:erp:doc:cancel-impact'),
    ('GENERAL_MANAGER', 'business:erp:expense-type:list'), ('GENERAL_MANAGER', 'business:erp:expense:detail'), ('GENERAL_MANAGER', 'business:erp:expense:list'),
    ('GENERAL_MANAGER', 'business:erp:expense:pay'), ('GENERAL_MANAGER', 'business:erp:inventory-location:list'), ('GENERAL_MANAGER', 'business:erp:inventory-txn:list'),
    ('GENERAL_MANAGER', 'business:erp:inventory:list'), ('GENERAL_MANAGER', 'business:erp:ledger:aux-balance'), ('GENERAL_MANAGER', 'business:erp:ledger:list'),
    ('GENERAL_MANAGER', 'business:erp:ledger:trial-balance'), ('GENERAL_MANAGER', 'business:erp:location:detail'), ('GENERAL_MANAGER', 'business:erp:location:list'),
    ('GENERAL_MANAGER', 'business:erp:material:detail'), ('GENERAL_MANAGER', 'business:erp:material:list'), ('GENERAL_MANAGER', 'business:erp:opening:read'),
    ('GENERAL_MANAGER', 'business:erp:payable:detail'), ('GENERAL_MANAGER', 'business:erp:payable:list'), ('GENERAL_MANAGER', 'business:erp:period:list'),
    ('GENERAL_MANAGER', 'business:erp:purchase-order-item:list'), ('GENERAL_MANAGER', 'business:erp:purchase-order:list'), ('GENERAL_MANAGER', 'business:erp:reminder:run'),
    ('GENERAL_MANAGER', 'business:erp:report-item:list'), ('GENERAL_MANAGER', 'business:erp:report:balance-sheet'), ('GENERAL_MANAGER', 'business:erp:report:cash-flow'),
    ('GENERAL_MANAGER', 'business:erp:report:export'), ('GENERAL_MANAGER', 'business:erp:report:income-statement'), ('GENERAL_MANAGER', 'business:erp:report:payable-aging'),
    ('GENERAL_MANAGER', 'business:erp:seq:list'), ('GENERAL_MANAGER', 'business:erp:serial:detail'), ('GENERAL_MANAGER', 'business:erp:serial:list'),
    ('GENERAL_MANAGER', 'business:erp:serial:view'), ('GENERAL_MANAGER', 'business:erp:stock-in:detail'), ('GENERAL_MANAGER', 'business:erp:stock-in:list'),
    ('GENERAL_MANAGER', 'business:erp:stock-out:detail'), ('GENERAL_MANAGER', 'business:erp:stock-out:list'), ('GENERAL_MANAGER', 'business:erp:stock-take:detail'),
    ('GENERAL_MANAGER', 'business:erp:stock-take:list'), ('GENERAL_MANAGER', 'business:erp:supplier:list'), ('GENERAL_MANAGER', 'business:erp:transfer:detail'),
    ('GENERAL_MANAGER', 'business:erp:transfer:list'), ('GENERAL_MANAGER', 'business:erp:unit:list'), ('GENERAL_MANAGER', 'business:erp:voucher:detail'),
    ('GENERAL_MANAGER', 'business:erp:voucher:list'), ('GENERAL_MANAGER', 'business:erp:warehouse:detail'), ('GENERAL_MANAGER', 'business:erp:warehouse:list'),
    ('GENERAL_MANAGER', 'business:erp:yearend:read'), ('GENERAL_MANAGER', 'business:flow:approve'), ('GENERAL_MANAGER', 'business:flow:instance:create'),
    ('GENERAL_MANAGER', 'business:flow:instance:read'), ('GENERAL_MANAGER', 'business:flow:instance:withdraw'), ('GENERAL_MANAGER', 'business:flow:read'),
    ('GENERAL_MANAGER', 'business:flow:state-edge:list'), ('GENERAL_MANAGER', 'business:flow:tasknode:read'), ('GENERAL_MANAGER', 'business:notice:channel:config'),
    ('GENERAL_MANAGER', 'business:notice:delete'), ('GENERAL_MANAGER', 'business:notice:list'), ('GENERAL_MANAGER', 'business:notice:mine'),
    ('GENERAL_MANAGER', 'business:notice:send'), ('GENERAL_MANAGER', 'business:notice:template:create'), ('GENERAL_MANAGER', 'business:notice:template:delete'),
    ('GENERAL_MANAGER', 'business:notice:template:list'), ('GENERAL_MANAGER', 'business:notice:template:update'), ('GENERAL_MANAGER', 'business:project:read'),
    ('GENERAL_MANAGER', 'business:storage:download'), ('GENERAL_MANAGER', 'business:storage:list'), ('GENERAL_MANAGER', 'business:task:read'),
-- ---- FLOW_CONFIG_APPROVER（4 个码）
    ('FLOW_CONFIG_APPROVER', 'business:flow:approve'), ('FLOW_CONFIG_APPROVER', 'business:flow:instance:create'), ('FLOW_CONFIG_APPROVER', 'business:flow:instance:read'),
    ('FLOW_CONFIG_APPROVER', 'business:flow:instance:withdraw')
;

DO $$
DECLARE
    n_matrix integer;
    n_bad    integer;
    n_ins    integer;
    n_ver    integer;
    v        text;
BEGIN
    -- ---------- 2) 矩阵自检 ----------
    SELECT count(*) INTO n_matrix FROM _brm_matrix;
    IF n_matrix <> 450 THEN
        RAISE EXCEPTION 'V0258 自检失败：矩阵 % 对（期望 450）', n_matrix;
    END IF;

    -- 矩阵引用的角色必须存在（角色码写错会静默 0 行，必须炸）
    SELECT string_agg(DISTINCT mx.role_code, ', ') INTO v
      FROM _brm_matrix mx
     WHERE NOT EXISTS (SELECT 1 FROM sys_role r
                        WHERE r.role_code = mx.role_code AND r.deleted_at IS NULL);
    IF v IS NOT NULL THEN
        RAISE EXCEPTION 'V0258 自检失败：矩阵里的角色码不存在 → %', v;
    END IF;

    -- 矩阵引用的权限码必须存在（码被改名/删除 = 矩阵失效，必须炸而非静默跳过）
    SELECT string_agg(DISTINCT mx.auth_code, ', ') INTO v
      FROM _brm_matrix mx
     WHERE NOT EXISTS (SELECT 1 FROM sys_menu m
                        WHERE m.auth_code = mx.auth_code AND m.deleted_at IS NULL);
    IF v IS NOT NULL THEN
        RAISE EXCEPTION 'V0258 自检失败：矩阵里有 sys_menu 不存在的权限码 → %', v;
    END IF;

    -- 平台治理页红线（与 scripts/datafix/bizperm/expand.sql 4.2 同口径）
    SELECT count(*) INTO n_bad
      FROM _brm_matrix mx
      JOIN sys_menu m ON m.auth_code = mx.auth_code AND m.deleted_at IS NULL
     WHERE m.path IN ('/system/user','/system/role','/system/dept','/system/menu','/system/data-scope',
                      '/system/field-mask','/system/role-mutex','/system/change-log',
                      '/business/storage/backend','/support/config','/support/dict','/support/file',
                      '/support/job','/support/log','/support/online');
    IF n_bad > 0 THEN
        RAISE EXCEPTION 'V0258 自检失败：矩阵越界命中 % 个平台治理节点', n_bad;
    END IF;

    -- ---------- 3) 待补清单：按「码」判重（生产 = 空集）----------
    DROP TABLE IF EXISTS _brm_todo;
    CREATE TEMP TABLE _brm_todo AS
    SELECT mx.role_code, r.id AS role_id, mx.auth_code
      FROM _brm_matrix mx
      JOIN sys_role r ON r.role_code = mx.role_code AND r.deleted_at IS NULL
     WHERE NOT EXISTS (
            SELECT 1 FROM sys_role_menu rm
              JOIN sys_menu m ON m.id = rm.menu_id AND m.deleted_at IS NULL
             WHERE rm.role_id = r.id AND m.auth_code = mx.auth_code);

    -- ---------- 4) 展开：码的全部节点 + 祖先目录 ----------
    DROP TABLE IF EXISTS _brm_sel;
    CREATE TEMP TABLE _brm_sel AS
    WITH RECURSIVE
    -- 4.1 待补码挂载的**全部**节点（页面与其按钮共码时整组给，角色授权页勾选才一致）
    hit AS (
        SELECT t.role_code, t.role_id, m.id AS menu_id, coalesce(m.auth_code,'') AS code
          FROM _brm_todo t
          JOIN sys_menu m ON m.auth_code = t.auth_code AND m.deleted_at IS NULL
    ),
    -- 4.2 祖先目录（递归向上）
    anc AS (
        SELECT h.role_code, h.role_id, p.id AS menu_id, coalesce(p.auth_code,'') AS code
          FROM hit h
          JOIN sys_menu c ON c.id = h.menu_id
          JOIN sys_menu p ON p.id = c.parent_id AND p.deleted_at IS NULL
        UNION
        SELECT a.role_code, a.role_id, p.id, coalesce(p.auth_code,'')
          FROM anc a
          JOIN sys_menu c ON c.id = a.menu_id
          JOIN sys_menu p ON p.id = c.parent_id AND p.deleted_at IS NULL
    )
    SELECT role_code, role_id, menu_id, code FROM hit
    UNION
    SELECT a.role_code, a.role_id, a.menu_id, a.code
      FROM anc a
     WHERE a.code = ''                                    -- 无码目录：纯渲染用，不授予任何码
        OR EXISTS (SELECT 1 FROM _brm_todo t2
                    WHERE t2.role_id = a.role_id AND t2.auth_code = a.code); -- 本次一并要授的码

    -- ---------- 5) 插入（确定性 id，幂等）----------
    INSERT INTO sys_role_menu (id, role_id, menu_id, created_at)
    SELECT (
                SELECT (substr(h,1,8) || '-' || substr(h,9,4) || '-4' || substr(h,14,3)
                        || '-8' || substr(h,18,3) || '-' || substr(h,21,12))::uuid
                  FROM (SELECT md5(s.role_id::text || '|' || s.menu_id::text || '|v0258-role-matrix') AS h) _h
              ), s.role_id, s.menu_id, now()
      FROM _brm_sel s
     WHERE NOT EXISTS (SELECT 1 FROM sys_role_menu rm
                        WHERE rm.role_id = s.role_id AND rm.menu_id = s.menu_id)
    ON CONFLICT (id) DO NOTHING;
    GET DIAGNOSTICS n_ins = ROW_COUNT;

    -- ---------- 6) 有变更才推进 token_version（纯重放不踢在线会话）----------
    n_ver := 0;
    IF n_ins > 0 THEN
        UPDATE sys_user u
           SET token_version = u.token_version + 1, updated_at = now()
         WHERE u.deleted_at IS NULL
           AND EXISTS (SELECT 1 FROM sys_user_role ur
                        WHERE ur.user_id = u.id
                          AND ur.role_id IN (SELECT role_id FROM _brm_sel));
        GET DIAGNOSTICS n_ver = ROW_COUNT;
    END IF;

    -- ---------- 7) 施工后硬断言 ----------
    -- 7.1 待补清单必须清空：每个 (角色, 码) 现在都持有
    SELECT count(*) INTO n_bad
      FROM _brm_todo t
     WHERE NOT EXISTS (SELECT 1 FROM sys_role_menu rm
                         JOIN sys_menu m ON m.id = rm.menu_id AND m.deleted_at IS NULL
                        WHERE rm.role_id = t.role_id AND m.auth_code = t.auth_code);
    IF n_bad <> 0 THEN
        RAISE EXCEPTION 'V0258 施工后断言失败：仍有 % 个 (角色,码) 未授权', n_bad;
    END IF;

    -- 7.2 越界复查：业务角色不得因祖先闭包拿到治理页
    SELECT count(*) INTO n_bad
      FROM sys_role r
      JOIN sys_role_menu rm ON rm.role_id = r.id
      JOIN sys_menu m ON m.id = rm.menu_id AND m.deleted_at IS NULL
     WHERE r.role_code IN (SELECT DISTINCT role_code FROM _brm_matrix)
       AND m.path IN ('/system/user','/system/role','/system/dept','/system/menu','/system/data-scope',
                      '/system/field-mask','/system/role-mutex','/system/change-log',
                      '/business/storage/backend','/support/config','/support/dict','/support/file',
                      '/support/job','/support/log','/support/online')
       AND NOT EXISTS (SELECT 1 FROM _brm_matrix mx
                        WHERE mx.role_code = r.role_code AND mx.auth_code = m.auth_code);
    IF n_bad > 0 THEN
        RAISE EXCEPTION 'V0258 施工后断言失败：业务角色越界持有 % 个治理节点', n_bad;
    END IF;

    -- 7.3 本卡 5 个验收码必须落到预期角色（缺 voucher:create 则催办永远「未指定处理人」）
    SELECT count(*) INTO n_bad
      FROM (VALUES ('FINANCE_MANAGER','business:erp:period:remind'),
                   ('FINANCE_MANAGER','business:erp:voucher:post'),
                   ('FINANCE_MANAGER','business:erp:voucher:create'),
                   ('WAREHOUSE_MANAGER','business:erp:stock-in:status'),
                   ('WAREHOUSE_MANAGER','business:erp:transfer:status')) AS want(role_code, auth_code)
     WHERE NOT EXISTS (SELECT 1 FROM sys_role r
                         JOIN sys_role_menu rm ON rm.role_id = r.id
                         JOIN sys_menu m ON m.id = rm.menu_id AND m.deleted_at IS NULL
                        WHERE r.role_code = want.role_code AND m.auth_code = want.auth_code);
    IF n_bad <> 0 THEN
        RAISE EXCEPTION 'V0258 施工后断言失败：% 个验收码没有落到预期角色', n_bad;
    END IF;

    RAISE NOTICE 'V0258 完成：矩阵 450 对 / 本次新增授权 % 行 / 推进 token_version % 个账号',
        n_ins, n_ver;
END $$;
