-- ======================================================
-- V0131 回滚：删除示范帮助数据（零残留）
--
-- 顺序必须是「版本快照 → 文档 → 分类」：
--   sys_help_doc_version.doc_id  → ON DELETE CASCADE 到 sys_help_doc，
--   sys_help_doc.category_id     → ON DELETE RESTRICT 到 sys_help_category，
-- 所以先删文档再删分类，否则分类上还挂着文档会被 RESTRICT 拦住。
--
-- **只删本迁移自己造的行**（固定 UUID）；V0086 种的 3 个默认分类
-- （getting-started / faq / features）与其它文档一律不动。
-- ======================================================

-- ---------- 1) 版本快照 ----------
DELETE FROM sys_help_doc_version
 WHERE doc_id IN (
        '26500000-0000-4000-8000-000000000001'::uuid,
        '26500000-0000-4000-8000-000000000002'::uuid,
        '26500000-0000-4000-8000-000000000003'::uuid,
        '26500000-0000-4000-8000-000000000004'::uuid,
        '26500000-0000-4000-8000-000000000005'::uuid
   );

-- ---------- 2) 文档（按固定 id，外加同 slug 兜底：防止有人在管理后台
--              把示范文档删了又用同 slug 重建，留下同名残留） ----------
DELETE FROM sys_help_doc
 WHERE id IN (
        '26500000-0000-4000-8000-000000000001'::uuid,
        '26500000-0000-4000-8000-000000000002'::uuid,
        '26500000-0000-4000-8000-000000000003'::uuid,
        '26500000-0000-4000-8000-000000000004'::uuid,
        '26500000-0000-4000-8000-000000000005'::uuid
   )
    OR slug IN (
        'what-is-cloudpivot',
        'create-first-task',
        'task-status-and-progress',
        'subtask-and-dependency',
        'draw-first-approval-flow'
   );

-- ---------- 3) 两个新分类（先清空 parent_id 引用：本迁移建的都是根节点，
--              这行是给「有人把子分类挂到示范分类下」留的安全垫） ----------
UPDATE sys_help_category
   SET parent_id = NULL
 WHERE parent_id IN ('26000000-0000-4000-8000-000000000004'::uuid,
                     '26000000-0000-4000-8000-000000000005'::uuid);

DELETE FROM sys_help_category
 WHERE id IN ('26000000-0000-4000-8000-000000000004'::uuid,
              '26000000-0000-4000-8000-000000000005'::uuid)
    OR code IN ('task-management', 'approval-flow');
