-- ============================================================================
-- V0258 down：按与 up 逐字相同的确定性 id 公式精确删除
--
-- 只删本迁移插进去的行（id = md5(role|menu|v0258-role-matrix) 折叠出的 uuid），
-- 既有授权一行不碰；祖先闭包插入的无码目录行同样由该 id 定位，一并删除。
-- 幂等：重放删 0 行。
--
-- ⚠ 回滚语义（实测踩出来的，别忽略）：
--   V0258 是**版本化迁移**，执行时机在种子迁移之前。当它插入的 (role_id, menu_id)
--   与某个种子（S__task_permissions / S__task_project_permissions 等）本该插入的行重合时，
--   种子的 INSERT 会因 `UNIQUE (role_id, menu_id)` 而跳过 —— 于是这些行带着**本迁移的
--   确定性 id** 落库。down 按该 id 精确删除，会把它们一起删掉，而种子指纹没变、不会重跑。
--   实测（cloudpivot_t2274b）：up 插入 367 行 → down 删 367 行 → 321 行；
--   而「不含本迁移的干净装配基线」是 343 行 ⇒ 有 22 行是「本迁移替种子先插的」。
--   ⇒ down 之后若要让库回到装配基线，**再执行一次 up 即可**（幂等，会把这 22 行重新种上）。
--     这也是本迁移保持「只增不减 + 幂等」的原因：回滚是可逆的，且可逆性由 up 保证。
-- ============================================================================

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
    n_del  integer;
    n_left integer;
    n_ver  integer;
BEGIN
    DROP TABLE IF EXISTS _brm_sel;
    CREATE TEMP TABLE _brm_sel AS
    WITH RECURSIVE
    hit AS (
        SELECT mx.role_code, r.id AS role_id, m.id AS menu_id, coalesce(m.auth_code,'') AS code
          FROM _brm_matrix mx
          JOIN sys_role r ON r.role_code = mx.role_code AND r.deleted_at IS NULL
          JOIN sys_menu m ON m.auth_code = mx.auth_code AND m.deleted_at IS NULL
    ),
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
     WHERE a.code = ''
        OR EXISTS (SELECT 1 FROM _brm_matrix mx2
                    WHERE mx2.role_code = a.role_code AND mx2.auth_code = a.code);

    DELETE FROM sys_role_menu rm
     WHERE rm.id = (
                SELECT (substr(h,1,8) || '-' || substr(h,9,4) || '-4' || substr(h,14,3)
                        || '-8' || substr(h,18,3) || '-' || substr(h,21,12))::uuid
                  FROM (SELECT md5(rm.role_id::text || '|' || rm.menu_id::text || '|v0258-role-matrix') AS h) _h
              )
       AND EXISTS (SELECT 1 FROM _brm_sel s
                    WHERE s.role_id = rm.role_id AND s.menu_id = rm.menu_id);
    GET DIAGNOSTICS n_del = ROW_COUNT;

    SELECT count(*) INTO n_left
      FROM sys_role_menu rm
     WHERE rm.id = (
                SELECT (substr(h,1,8) || '-' || substr(h,9,4) || '-4' || substr(h,14,3)
                        || '-8' || substr(h,18,3) || '-' || substr(h,21,12))::uuid
                  FROM (SELECT md5(rm.role_id::text || '|' || rm.menu_id::text || '|v0258-role-matrix') AS h) _h
              )
       AND EXISTS (SELECT 1 FROM _brm_sel s
                    WHERE s.role_id = rm.role_id AND s.menu_id = rm.menu_id);
    IF n_left <> 0 THEN
        RAISE EXCEPTION 'V0258 回退后断言失败：仍有 % 行本迁移授权未删', n_left;
    END IF;

    n_ver := 0;
    IF n_del > 0 THEN
        UPDATE sys_user u
           SET token_version = u.token_version + 1, updated_at = now()
         WHERE u.deleted_at IS NULL
           AND EXISTS (SELECT 1 FROM sys_user_role ur
                        WHERE ur.user_id = u.id
                          AND ur.role_id IN (SELECT role_id FROM _brm_sel));
        GET DIAGNOSTICS n_ver = ROW_COUNT;
    END IF;

    RAISE NOTICE 'V0258 down 完成：删除授权 % 行 / 推进 token_version % 个账号（如需回到装配基线请再执行一次 up：幂等，会补回被本迁移抢先插入的种子行）', n_del, n_ver;
END $$;
