-- ============================================================
-- V0212：业务单据附件（`biz_doc_attachment`）—— ARCH-3 §6 **R-P1-3**（断链 DC-17，P1）
--
-- # 这条迁移补的是什么
--
-- 改造前：系统有 storage（对象存储 + `sys_file`）与 `biz_task_attachment`（任务附件），
-- 但**业务单据一张附件都存不了**（ARCH-3 扫描：ERP 30 张表对 `sys_file` 的引用数 = 0）。
-- 现实里合同要扫描件、收货要签收单、付款要发票影像，用户只能存自己电脑 —— 出事时找不到。
--
-- 改造后：一张**桥接表**把「单据」和「文件」连起来（范本就是 `biz_task_attachment`），
-- 文件本体、上传策略、秒传、下载出流、物理回收**全部复用 storage 模块**，本迁移
-- 不新建任何存储设施（卡面红线：不重写文件存储 / 不对接对象存储）。
--
-- # 表结构
--
--   biz_doc_attachment
--     id          主键（UUID v7，由应用侧生成）
--     doc_type    单据类型（5 类，见下）
--     doc_id      单据主键（**不建物理外键**：五类单据分散在五张表，见注释）
--     file_id     → `sys_file.id`（ON DELETE RESTRICT：文件记录还在，附件绑定才有意义）
--     file_role   文件角色（CONTRACT_SCAN / SIGN_RECEIPT / INVOICE / OTHER）
--     remark      备注（可选，如「第二页缺章」）
--     uploaded_by 上传人（→ `sys_user.id`，ON DELETE SET NULL：删账号不该连带删附件）
--     created_at / deleted_at  创建 / 软删时间
--
-- # doc_type 取值（与 `attachment::model::DocKind` 逐字对应）
--
--   CONTRACT        合同      → biz_contract
--   PURCHASE_ORDER  采购单    → biz_purchase_order
--   STOCK_IN        收货单    → biz_stock_in
--   PAYABLE         应付单    → biz_payable     （应付款项的「单据」）
--   PAYMENT         付款单    → biz_payment     （实际付款登记）
--
--   为什么 `doc_id` 不建外键：五类单据在五张不同的表里，单一列无法同时指向它们；
--   若为每类建一列，加一类单据就要改表结构。**存在性由 handler 在写库前用
--   `model::doc_exists()` 逐类校验**（找不到 → 404 + `RESOURCE_NOT_FOUND`，
--   不会写出悬空附件）。这与 `biz_task_attachment` 只有 `task_id` 一列的取舍
--   不同，原因就是「一对多类型」这件事本身。
--
-- # 索引
--
--   idx_biz_doc_attachment_doc   (doc_type, doc_id) WHERE deleted_at IS NULL
--       —— 详情页按单据查附件（唯一的高频查询）
--   idx_biz_doc_attachment_file  (file_id)
--       —— 软删文件前判「还有没有别的单据在用这份文件」（秒传会让多单据共用一个 file_id）
--   uk_biz_doc_attachment        (doc_type, doc_id, file_id) WHERE deleted_at IS NULL
--       —— 同一份文件不能在同一张单据上挂两次；**软删后可重挂**（故为部分唯一索引）
--
-- # 权限码（3 个，卡面 §四）
--
--   business:attachment:upload   上传 / 绑定
--   business:attachment:list     列表 + 下载（下载是「读」的一部分，不另设码）
--   business:attachment:delete   删除他人附件（**行级升权码**：上传人永远能删自己的，
--                                与 `business:task:attachment:delete` 同一口径）
--
--   三个节点挂在「通用底座」目录下、`is_visible = FALSE`：附件区嵌在四个单据详情页里，
--   不产生独立导航项，但权限码必须能在角色授权界面里勾选。
--
-- # 幂等 / 回滚
--
--   CREATE TABLE / INDEX IF NOT EXISTS；约束与权限走 DO 块 + `WHERE NOT EXISTS`。
--   文件内**不写** BEGIN/COMMIT（迁移器自己在事务里跑）。
--   回滚：`V0212__erp_doc_attachment.down.sql`（有**未软删**附件时主动拒绝 → 删权限 → 删表）。
-- ============================================================

-- ============================================================
-- §1 表
-- ============================================================
CREATE TABLE IF NOT EXISTS biz_doc_attachment (
    id          uuid         PRIMARY KEY DEFAULT gen_random_uuid(),
    doc_type    varchar(32)  NOT NULL,
    doc_id      uuid         NOT NULL,
    file_id     uuid         NOT NULL REFERENCES sys_file (id) ON DELETE RESTRICT,
    file_role   varchar(32)  NOT NULL DEFAULT 'OTHER',
    remark      varchar(255),
    uploaded_by uuid         REFERENCES sys_user (id) ON DELETE SET NULL,
    created_at  timestamptz  NOT NULL DEFAULT now(),
    deleted_at  timestamptz
);

COMMENT ON TABLE biz_doc_attachment IS
    '业务单据附件：只存 (doc_type, doc_id) ↔ sys_file 的绑定关系，文件本体与上传策略在 storage 模块（ARCH-3 DC-17）';
COMMENT ON COLUMN biz_doc_attachment.doc_type IS
    '单据类型：CONTRACT / PURCHASE_ORDER / STOCK_IN / PAYABLE / PAYMENT（与 attachment::model::DocKind 一一对应）';
COMMENT ON COLUMN biz_doc_attachment.doc_id IS
    '单据主键（**应用层校验存在性**：五类单据分散在五张表，单列无法建物理外键）';
COMMENT ON COLUMN biz_doc_attachment.file_role IS
    '文件角色：CONTRACT_SCAN 合同扫描件 / SIGN_RECEIPT 签收单 / INVOICE 发票影像 / OTHER 其他';
COMMENT ON COLUMN biz_doc_attachment.uploaded_by IS
    '上传人（行级判据：上传人可删自己的附件；删他人附件需 business:attachment:delete 码）';

-- 约束（幂等：已存在则跳过）
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_doc_attachment_type'
    ) THEN
        ALTER TABLE biz_doc_attachment
            ADD CONSTRAINT ck_biz_doc_attachment_type
            CHECK (doc_type IN ('CONTRACT', 'PURCHASE_ORDER', 'STOCK_IN', 'PAYABLE', 'PAYMENT'));
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_doc_attachment_role'
    ) THEN
        ALTER TABLE biz_doc_attachment
            ADD CONSTRAINT ck_biz_doc_attachment_role
            CHECK (file_role IN ('CONTRACT_SCAN', 'SIGN_RECEIPT', 'INVOICE', 'OTHER'));
    END IF;
END $$;

-- ============================================================
-- §2 索引
-- ============================================================
CREATE INDEX IF NOT EXISTS idx_biz_doc_attachment_doc
    ON biz_doc_attachment (doc_type, doc_id)
    WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_biz_doc_attachment_file
    ON biz_doc_attachment (file_id);

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_doc_attachment
    ON biz_doc_attachment (doc_type, doc_id, file_id)
    WHERE deleted_at IS NULL;

-- ============================================================
-- §3 权限码（3 个）
--
--   `business:attachment:list` 同时是「列表」与「下载」的入口码：下载是读操作的一种，
--   单设 `download` 码会让「能看列表但点下载被拒」变成一种没有业务含义的组合。
--   `business:attachment:delete` 是**行级升权码**（上传人自身永远能删），
--   与任务附件的 `business:task:attachment:delete` 完全同一口径。
-- ============================================================
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001221'::uuid, 'business:attachment:upload', 3,
     'business:attachment:upload', NULL,
     '21000000-0000-0000-0000-000000000001'::uuid, NULL,
     61, 1, NULL, '上传单据附件（合同扫描件 / 签收单 / 发票影像）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001222'::uuid, 'business:attachment:list', 3,
     'business:attachment:list', NULL,
     '21000000-0000-0000-0000-000000000001'::uuid, NULL,
     62, 1, NULL, '查看 / 下载单据附件', FALSE, 'business'),
    ('20000000-0000-0000-0000-000000001223'::uuid, 'business:attachment:delete', 3,
     'business:attachment:delete', NULL,
     '21000000-0000-0000-0000-000000000001'::uuid, NULL,
     63, 1, NULL, '删除他人上传的单据附件（上传人可删自己的）', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- 授权给已存在的角色（空库首启时 sys_role 为空 → 只打 NOTICE，bootstrap 后重放即补齐）
DO $$
DECLARE
    n_roles  integer;
    n_menus  integer;
    n_grants integer;
BEGIN
    SELECT count(*) INTO n_menus
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000001221'::uuid,
                  '20000000-0000-0000-0000-000000001222'::uuid,
                  '20000000-0000-0000-0000-000000001223'::uuid);
    IF n_menus <> 3 THEN
        RAISE EXCEPTION 'V0212 自检失败：预期 3 个单据附件权限节点，实际 %', n_menus;
    END IF;

    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0212：sys_role 为空（空库首启，迁移先于 bootstrap），跳过授权；bootstrap 后重放本迁移即可补齐';
        RETURN;
    END IF;

    INSERT INTO sys_role_menu (id, role_id, menu_id)
    SELECT gen_random_uuid(), r.id, m.id
      FROM sys_role r
      CROSS JOIN (VALUES
            ('20000000-0000-0000-0000-000000001221'::uuid),
            ('20000000-0000-0000-0000-000000001222'::uuid),
            ('20000000-0000-0000-0000-000000001223'::uuid)) AS m(id)
     WHERE r.deleted_at IS NULL
       AND r.role_code IN ('SUPER_ADMIN', 'PURCHASE_MANAGER', 'WAREHOUSE_MANAGER',
                           'FINANCE_MANAGER', 'FINANCE_DIRECTOR', 'GENERAL_MANAGER')
       AND NOT EXISTS (
            SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
       );
    GET DIAGNOSTICS n_grants = ROW_COUNT;
    RAISE NOTICE 'V0212：单据附件 3 个权限码已种，授权新增 % 行（角色数 %）', n_grants, n_roles;
END $$;

-- ============================================================
-- §4 自检（up 后必须全为真）
-- ============================================================
DO $$
DECLARE
    n_tbl   integer;
    n_idx   integer;
    n_ck    integer;
    n_menu  integer;
    n_perm  integer;
BEGIN
    SELECT count(*) INTO n_tbl FROM information_schema.tables
     WHERE table_schema = 'public' AND table_name = 'biz_doc_attachment';
    SELECT count(*) INTO n_idx FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_doc_attachment'
       AND indexname IN ('idx_biz_doc_attachment_doc', 'idx_biz_doc_attachment_file',
                         'uk_biz_doc_attachment');
    SELECT count(*) INTO n_ck FROM pg_constraint
     WHERE conrelid = 'biz_doc_attachment'::regclass
       AND conname IN ('ck_biz_doc_attachment_type', 'ck_biz_doc_attachment_role');
    SELECT count(*) INTO n_menu FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000001221'::uuid,
                  '20000000-0000-0000-0000-000000001222'::uuid,
                  '20000000-0000-0000-0000-000000001223'::uuid);
    SELECT count(*) INTO n_perm FROM sys_menu
     WHERE auth_code IN ('business:attachment:upload', 'business:attachment:list',
                         'business:attachment:delete');

    IF n_tbl <> 1 OR n_idx <> 3 OR n_ck <> 2 OR n_menu <> 3 OR n_perm <> 3 THEN
        RAISE EXCEPTION 'V0212 自检失败：表 % / 索引 % / 约束 % / 权限节点 % / 权限码 %（应 1/3/2/3/3）',
            n_tbl, n_idx, n_ck, n_menu, n_perm;
    END IF;
    RAISE NOTICE 'V0212 完成：biz_doc_attachment 1 表 / 3 索引 / 2 约束 / 3 权限节点就绪';
END $$;
