-- ============================================================
-- V0210：业务提醒任务包（应付到期 / 采购超期 / 合同到期 / 低库存）
--
-- 依据：`docs/architecture/ARCH-3-模块勾稽关系审计.md` §6 R-P1-2（断链 DC-06 / DC-12 的
--       `safety_stock` 部分）+ kanban 卡 t_b5bcc0bd
-- 代码：`backend/src/business/erp/reminder.rs`（核心：幂等台账 / 收件人解析 / 注册）
--       `backend/src/business/erp/reminder_{payable,purchase,contract,stock}.rs`（4 个扫描）
-- 范本：`backend/src/business/erp/transfer_overdue.rs`（在途调拨超期告警，LNK-F12）
--
-- # 为什么需要这条迁移
--
--   系统里有 4 个「该提醒」的场景，任何一处都没有提醒入口：
--     ① 应付到期     `biz_payable.due_date` 存在，到期前没人知道（逾期没付也不会有任何信号）
--     ② 采购超期     `biz_purchase_order.expect_date` 存在，超期未收完只能靠人翻列表
--     ③ 合同到期     `biz_contract.end_date` 存在，到期前没人知道（断链 DC-06）
--     ④ 安全库存     `biz_material.safety_stock` 填了，但从不参与任何判定 —— DC-12 摆设字段
--
--   提醒是**周期任务**：必须有地方记「这张单今天提醒过没有」，否则每小时/每天重复轰炸。
--   `sys_notice.idempotency_key` 只能挡住「同一轮重复发送」，挡不住「下一轮换了个键」；
--   而且通知会过期被 `notice_expire_cleanup` 物理删除（`expire_at < now()`），
--   删掉之后幂等键也没了 —— 所以**幂等必须以本地的台账为准**，通知幂等键只是第二层。
--
-- # 台账为什么是一张新表，而不是给 4 张业务表各加一列
--
--   · 4 个场景的单据维度不同：应付/采购/合同是「单据」，低库存是「物料×仓库的库存行」
--     （`biz_inventory` 没有 `deleted_at`、没有单号，加审计列等于污染账务表）；
--   · 应付与合同各有多个提醒档位（D-7/D-3/D-1、D-30/D-15/D-7/D-1），
--     单列 `last_remind_at` 表达不了「这一档提醒过了、那一档还没」；
--   · 台账是纯 append 的事实表（谁在哪天因为什么被提醒过），运维排障与验收都要用；
--   · 不改 4 张业务表 = 零回归风险（不碰任何既有读写路径的列清单）。
--
-- # 幂等口径（代码与索引是同一句话）
--
--   唯一键 `(doc_type, doc_id, remind_kind, remind_date)`：
--     同一份单据 × 同一种提醒 × 同一个业务日（Asia/Shanghai）**只有一行**。
--     任务重跑、并发抢跑、手动触发都落在 `INSERT … ON CONFLICT DO NOTHING` 上，
--     `affected = 0` 即「今天提醒过了」，不再发通知。
--
-- # 幂等
--
--   建表 `IF NOT EXISTS`、模板 `ON CONFLICT (id) DO NOTHING`、
--   任务按 `job_name` 去重、配置 `ON CONFLICT (config_key) WHERE deleted_at IS NULL DO NOTHING`、
--   权限节点 `ON CONFLICT (id) DO NOTHING`。重放不报错、不覆盖运维改过的值。
-- 回滚：`V0210__erp_business_reminder.down.sql`（删台账表 + 模板 + 任务 + 配置 + 权限节点）
-- ============================================================

-- ---------- ① 提醒台账 ----------
CREATE TABLE IF NOT EXISTS biz_reminder_log (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    doc_type       VARCHAR(24) NOT NULL,   -- 单据类型：PAYABLE / PURCHASE_ORDER / CONTRACT / INVENTORY
    doc_id         UUID        NOT NULL,   -- 单据主键（INVENTORY = biz_inventory.id，即「物料×仓库」行）
    doc_no         VARCHAR(64),            -- 单号快照（低库存记物料编码；排查用，不参与判定）
    remind_kind    VARCHAR(32) NOT NULL,   -- 提醒类型：PAYABLE_DUE / PO_OVERDUE / CONTRACT_EXPIRE / LOW_STOCK
    stage          VARCHAR(16) NOT NULL,   -- 档位：D-7 / D-3 / D-1 / D-30 / OVERDUE-12 / LOW
    remind_date    DATE        NOT NULL,   -- 业务日（Asia/Shanghai）—— 幂等的第二维
    notice_id      UUID,                   -- 对应的 sys_notice.id（通知过期清理后这里仍可追溯）
    receiver_count INT         NOT NULL DEFAULT 0, -- 当时扇出的收件人数（0 = 定位不到人，已记 WARN）
    sent_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT ck_biz_reminder_log_doc_type
        CHECK (doc_type IN ('PAYABLE', 'PURCHASE_ORDER', 'CONTRACT', 'INVENTORY')),
    CONSTRAINT ck_biz_reminder_log_kind
        CHECK (remind_kind IN ('PAYABLE_DUE', 'PO_OVERDUE', 'CONTRACT_EXPIRE', 'LOW_STOCK'))
);

COMMENT ON TABLE  biz_reminder_log IS
    '业务提醒台账（append-only）：一行 = 某份单据在某业务日被某种提醒通知过一次；唯一键即为幂等判据';
COMMENT ON COLUMN biz_reminder_log.doc_type IS
    '单据类型：PAYABLE 应付 / PURCHASE_ORDER 采购单 / CONTRACT 合同 / INVENTORY 库存行（物料×仓库）';
COMMENT ON COLUMN biz_reminder_log.doc_id IS
    '单据主键；INVENTORY 时是 biz_inventory.id（该表无 deleted_at，行永不复用）';
COMMENT ON COLUMN biz_reminder_log.doc_no IS '单号快照（低库存存物料编码），只为人工排查，不参与任何判定';
COMMENT ON COLUMN biz_reminder_log.remind_kind IS
    '提醒类型：PAYABLE_DUE 应付到期 / PO_OVERDUE 采购超期 / CONTRACT_EXPIRE 合同到期 / LOW_STOCK 低库存';
COMMENT ON COLUMN biz_reminder_log.stage IS
    '档位：应付 D-7/D-3/D-1、合同 D-30/D-15/D-7/D-1、采购超期 OVERDUE-{天数}、低库存 LOW';
COMMENT ON COLUMN biz_reminder_log.remind_date IS
    '业务日（按 Asia/Shanghai 折算）：同一单据同一天只允许一行 —— 防「每小时/每天狂轰」';
COMMENT ON COLUMN biz_reminder_log.notice_id IS
    '发出的 sys_notice.id（通知按 expire_at 过期清理后，台账仍能追溯「当时发过」）';
COMMENT ON COLUMN biz_reminder_log.receiver_count IS
    '当时实际扇出的收件人数；0 表示「定位不到人」（任务记 WARN，不许静默丢弃）';

-- 幂等判据（唯一真源）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_reminder_log_dedupe
    ON biz_reminder_log (doc_type, doc_id, remind_kind, remind_date);
-- 「这一类提醒最近提醒过谁」：低库存/采购超期的重提醒间隔靠它一次查完
CREATE INDEX IF NOT EXISTS idx_biz_reminder_log_recent
    ON biz_reminder_log (remind_kind, remind_date DESC);
-- 排查某个单据的全部提醒历史
CREATE INDEX IF NOT EXISTS idx_biz_reminder_log_doc
    ON biz_reminder_log (doc_type, doc_id, remind_date DESC);

-- ---------- ② 4 个通知模板 ----------
-- 变量与代码里 `template_vars` 逐字对应（`render` 缺变量会 422，不许偷懒少传）。
INSERT INTO sys_notice_template (id, template_code, template_name, notice_type, default_level,
                                 title_template, content_template, content_format, link_template,
                                 channels, variables, status, remark)
VALUES
    ('40000000-0000-0000-0000-0000000000b1'::uuid, 'ERP_REMINDER_PAYABLE_DUE', 'ERP：应付账款到期提醒',
     'ALERT', 'WARN',
     '应付单 {{daysLeft}} 天后到期：{{payableNo}}',
     '应付单 {{payableNo}}（供应商：{{supplierName}}）未付金额 {{unpaidAmount}}，到期日 {{dueDate}}，还剩 {{daysLeft}} 天。'
     || E'\n请核对本期资金计划并及时安排付款；确需延期的，请提前与供应商沟通并更新账期。',
     'TEXT', '/business/erp/payable',
     'INBOX',
     '[{"name":"payableNo","label":"应付单号","required":true,"sample":"AP-20261103-000001"},
       {"name":"supplierName","label":"供应商","required":true,"sample":"上海示例供应商"},
       {"name":"unpaidAmount","label":"未付金额","required":true,"sample":"12,345.00"},
       {"name":"dueDate","label":"到期日","required":true,"sample":"2026-11-06"},
       {"name":"daysLeft","label":"剩余天数","required":true,"sample":"3"},
       {"name":"action","label":"该做什么","required":true,"sample":"安排付款"}]'::jsonb,
     1, 'R-P1-2 业务提醒：应付到期前 7/3/1 天各提醒一次（erp_payable_due_scan）'),
    ('40000000-0000-0000-0000-0000000000b2'::uuid, 'ERP_REMINDER_PO_OVERDUE', 'ERP：采购订单超期未收货提醒',
     'ALERT', 'WARN',
     '采购单超期 {{overdueDays}} 天未收货：{{poNo}}',
     '采购单 {{poNo}}（供应商：{{supplierName}}）期望到货日 {{expectDate}}，已逾期 {{overdueDays}} 天仍未收完（收货仓库：{{warehouseName}}）。'
     || E'\n请催供应商交货，或在本单上改期 / 关闭，避免「已审批未到货」长期悬空拖累计划与应付。',
     'TEXT', '/business/erp/purchase-order',
     'INBOX',
     '[{"name":"poNo","label":"采购单号","required":true,"sample":"PO-20261001-000007"},
       {"name":"supplierName","label":"供应商","required":true,"sample":"上海示例供应商"},
       {"name":"expectDate","label":"期望到货日","required":true,"sample":"2026-10-20"},
       {"name":"overdueDays","label":"逾期天数","required":true,"sample":"14"},
       {"name":"warehouseName","label":"收货仓库","required":true,"sample":"原料仓"},
       {"name":"action","label":"该做什么","required":true,"sample":"催货或改期"}]'::jsonb,
     1, 'R-P1-2 业务提醒：采购单超过 expect_date 未收完（erp_po_overdue_scan）'),
    ('40000000-0000-0000-0000-0000000000b3'::uuid, 'ERP_REMINDER_CONTRACT_EXPIRE', 'ERP：合同到期提醒',
     'ALERT', 'WARN',
     '合同 {{daysLeft}} 天后到期：{{contractNo}}',
     '合同 {{contractNo}}（{{contractName}}，乙方：{{partyBName}}）将于 {{endDate}} 到期，还剩 {{daysLeft}} 天。'
     || E'\n请确认是否续签：续签请在本合同到期前起草新合同并走审批；不续签请及时终止合同并结清未了事项。',
     'TEXT', '/business/erp/contract',
     'INBOX',
     '[{"name":"contractNo","label":"合同号","required":true,"sample":"HT-20260101-000001"},
       {"name":"contractName","label":"合同名称","required":true,"sample":"2026 年度原料采购框架协议"},
       {"name":"partyBName","label":"乙方","required":true,"sample":"上海示例供应商"},
       {"name":"endDate","label":"到期日","required":true,"sample":"2026-12-31"},
       {"name":"daysLeft","label":"剩余天数","required":true,"sample":"30"},
       {"name":"action","label":"该做什么","required":true,"sample":"确认是否续签"}]'::jsonb,
     1, 'R-P1-2 业务提醒：合同到期前 30/15/7/1 天各提醒一次（erp_contract_expire_scan）'),
    ('40000000-0000-0000-0000-0000000000b4'::uuid, 'ERP_REMINDER_LOW_STOCK', 'ERP：库存低于安全库存提醒',
     'ALERT', 'WARN',
     '库存低于安全库存：{{materialCode}} {{materialName}}',
     '物料 {{materialCode}} {{materialName}} 在仓库「{{warehouseName}}」的当前库存 {{currentQty}}，'
     || '低于安全库存 {{safetyStock}}，缺口 {{gapQty}}。'
     || E'\n请安排补货（下采购单）或复核安全库存值是否该调整，避免断料停线。',
     'TEXT', '/business/erp/inventory',
     'INBOX',
     '[{"name":"materialCode","label":"物料编码","required":true,"sample":"M-0001"},
       {"name":"materialName","label":"物料名称","required":true,"sample":"冷轧钢板"},
       {"name":"warehouseName","label":"仓库","required":true,"sample":"原料仓"},
       {"name":"currentQty","label":"当前库存","required":true,"sample":"12"},
       {"name":"safetyStock","label":"安全库存","required":true,"sample":"50"},
       {"name":"gapQty","label":"缺口","required":true,"sample":"38"},
       {"name":"action","label":"该做什么","required":true,"sample":"安排补货"}]'::jsonb,
     1, 'R-P1-2 业务提醒：库存 < safety_stock（erp_low_stock_scan，DC-12 摆设字段落地）')
ON CONFLICT (id) DO NOTHING;

-- ---------- ③ 4 条 sys_job 种子（定时配置） ----------
-- ⚠️ 这 4 个任务**已由代码内置注册**（`Scheduler::register_builtin` →
--    `erp::reminder::register_builtin`，与 `transfer_overdue.rs` 同一范式）：
--    内置注册保证「迁移没跑 / 配置被停用 / 任务行被删」时提醒照跑。
--    因此这 4 行种子一律 `status = 0`（停用）：只为在「定时任务」页面**可见 / 可查 cron /
--    可手动执行一次**。若把 status 改成 1，同一 cron 会注册两次（内置一次 + 动态一次），
--    通知侧有台账 + 幂等键兜着不会重复推送，但属于白跑，故默认停用。
--    ⚠️ 这也意味着：**别把 `status` 当开关用**。要停提醒请改造
--    `sys_config.erp.reminder.*.enabled`（本迁移未引入，保持「默认全开」，见头注释与卡面）。
INSERT INTO sys_job (job_name, job_group, cron_expr, invoke_target, params, status, remark)
SELECT v.job_name, 'ERP', v.cron_expr, v.invoke_target, NULL, 0, v.remark
  FROM (VALUES
        ('erp_payable_due_scan', '0 10 7 * * *', 'erp_payable_due_scan',
         '应付到期提醒（内置任务已注册，本行仅供查看/手动执行）：每天 07:10 扫描「7/3/1 天后到期」的未付应付单并通知采购员 + 财务'),
        ('erp_po_overdue_scan', '0 20 7 * * *', 'erp_po_overdue_scan',
         '采购超期提醒（内置任务已注册，本行仅供查看/手动执行）：每天 07:20 扫描「超过期望到货日仍未收完」的采购单并通知制单人 + 采购经理'),
        ('erp_contract_expire_scan', '0 30 7 * * *', 'erp_contract_expire_scan',
         '合同到期提醒（内置任务已注册，本行仅供查看/手动执行）：每天 07:30 扫描「30/15/7/1 天后到期」的生效合同并通知制单人 + 采购经理'),
        ('erp_low_stock_scan', '0 40 7 * * *', 'erp_low_stock_scan',
         '低库存提醒（内置任务已注册，本行仅供查看/手动执行）：每天 07:40 扫描「当前库存 < 安全库存」的物料×仓库并通知仓管主管')
       ) AS v(job_name, cron_expr, invoke_target, remark)
 WHERE NOT EXISTS (SELECT 1 FROM sys_job j WHERE j.job_name = v.job_name AND j.deleted_at IS NULL);

-- ---------- ④ 2 个系统参数（重提醒间隔；档位表在代码里，是业务口径不是运维旋钮） ----------
INSERT INTO sys_config (
    id, config_key, config_value, config_type, description,
    category, config_level, is_modifiable, is_encrypted, is_restart, status,
    sort_order, ref_value
) VALUES
    ('2f121000-0000-4000-8000-000000000001', 'erp.reminder.po_overdue.realert_days', '3',
     'NUMBER',
     '采购超期重提醒间隔（天）：同一张采购单在间隔内不重复通知（台账 biz_reminder_log 判定；1 = 每天一次）',
     'erp', 2, TRUE, FALSE, FALSE, 1, 30, '1~90'),
    ('2f121000-0000-4000-8000-000000000002', 'erp.reminder.low_stock.realert_days', '3',
     'NUMBER',
     '低库存重提醒间隔（天）：同一「物料×仓库」在间隔内不重复通知（台账 biz_reminder_log 判定；1 = 每天一次）',
     'erp', 2, TRUE, FALSE, FALSE, 1, 40, '1~90')
ON CONFLICT (config_key) WHERE deleted_at IS NULL DO NOTHING;

-- ---------- ⑤ 权限节点：手动触发提醒（运维验收 / 事后补跑） ----------
-- 一个码：`business:erp:reminder:run`（挂「ERP 台账」目录 …0946 下的按钮节点）。
-- 为什么要有这个码：定时任务自己有调度，但「验收 / 补跑某一天」必须有个**明确授权**的入口，
-- 不能让任何登录用户都能把全公司的到期提醒重放一遍。
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001214'::uuid, 'business:erp:reminder:run', 3,
     'business:erp:reminder:run', NULL,
     '20000000-0000-0000-0000-000000000946'::uuid, NULL,
     90, 1, NULL, '手动触发业务提醒', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

DO $$
DECLARE
    n_tbl      integer;
    n_uniq     integer;
    n_tpl      integer;
    n_job      integer;
    n_cfg      integer;
    n_node     integer;
    n_bad_code integer;
    n_roles    integer;
    n_grants   integer;
BEGIN
    -- ---------- 台账表 ----------
    SELECT count(*) INTO n_tbl
      FROM information_schema.columns
     WHERE table_name = 'biz_reminder_log'
       AND column_name IN ('doc_type', 'doc_id', 'doc_no', 'remind_kind', 'stage',
                           'remind_date', 'notice_id', 'receiver_count', 'sent_at');
    IF n_tbl <> 9 THEN
        RAISE EXCEPTION 'V0210 自检失败：biz_reminder_log 业务列应有 9 个，实际 %', n_tbl;
    END IF;

    SELECT count(*) INTO n_uniq
      FROM pg_indexes
     WHERE tablename = 'biz_reminder_log' AND indexname = 'uk_biz_reminder_log_dedupe';
    IF n_uniq <> 1 THEN
        RAISE EXCEPTION 'V0210 自检失败：幂等唯一索引 uk_biz_reminder_log_dedupe 未建立';
    END IF;

    -- ---------- 4 个模板 ----------
    SELECT count(*) INTO n_tpl
      FROM sys_notice_template
     WHERE deleted_at IS NULL
       AND template_code IN ('ERP_REMINDER_PAYABLE_DUE', 'ERP_REMINDER_PO_OVERDUE',
                             'ERP_REMINDER_CONTRACT_EXPIRE', 'ERP_REMINDER_LOW_STOCK')
       AND status = 1;
    IF n_tpl <> 4 THEN
        RAISE EXCEPTION 'V0210 自检失败：业务提醒模板应有 4 个启用行，实际 %', n_tpl;
    END IF;

    -- ---------- 4 条任务（cron 逐字校验：任务名 ↔ cron ↔ invoke_target 三方一致） ----------
    SELECT count(*) INTO n_job
      FROM sys_job
     WHERE deleted_at IS NULL
       AND (job_name, cron_expr, invoke_target) IN (
             ('erp_payable_due_scan',      '0 10 7 * * *', 'erp_payable_due_scan'),
             ('erp_po_overdue_scan',       '0 20 7 * * *', 'erp_po_overdue_scan'),
             ('erp_contract_expire_scan',  '0 30 7 * * *', 'erp_contract_expire_scan'),
             ('erp_low_stock_scan',        '0 40 7 * * *', 'erp_low_stock_scan')
           );
    IF n_job <> 4 THEN
        RAISE EXCEPTION 'V0210 自检失败：业务提醒任务应有 4 条（名称/cron/调用目标一致），实际 %', n_job;
    END IF;

    -- ---------- 2 个参数 ----------
    SELECT count(*) INTO n_cfg
      FROM sys_config
     WHERE deleted_at IS NULL
       AND config_key IN ('erp.reminder.po_overdue.realert_days',
                          'erp.reminder.low_stock.realert_days')
       AND config_value ~ '^[1-9][0-9]*$';
    IF n_cfg <> 2 THEN
        RAISE EXCEPTION 'V0210 自检失败：重提醒间隔参数应有 2 个正整数行，实际 %', n_cfg;
    END IF;

    -- ---------- 权限节点（一码一节点） ----------
    SELECT count(*) INTO n_node
      FROM sys_menu
     WHERE id = '20000000-0000-0000-0000-000000001214'::uuid
       AND auth_code = 'business:erp:reminder:run' AND menu_type = 3;
    IF n_node <> 1 THEN
        RAISE EXCEPTION 'V0210 自检失败：手动触发提醒的权限节点没种上（% 行）', n_node;
    END IF;

    SELECT count(*) INTO n_bad_code
      FROM sys_menu
     WHERE auth_code = 'business:erp:reminder:run' AND deleted_at IS NULL;
    IF n_bad_code <> 1 THEN
        RAISE EXCEPTION 'V0210 自检失败：business:erp:reminder:run 在 sys_menu 里出现 % 次（应为 1）', n_bad_code;
    END IF;

    -- ---------- 授权（空库首启时 sys_role 可能还没有行，跳过并提示） ----------
    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0210：sys_role 为空（空库首启，迁移先于 bootstrap），跳过授权；bootstrap 后重放本迁移即可补齐';
    ELSE
        INSERT INTO sys_role_menu (id, role_id, menu_id)
        SELECT gen_random_uuid(), r.id, '20000000-0000-0000-0000-000000001214'::uuid
          FROM sys_role r
         WHERE r.deleted_at IS NULL
           AND r.role_code IN ('SUPER_ADMIN', 'FINANCE_MANAGER', 'FINANCE_DIRECTOR',
                               'PURCHASE_MANAGER', 'WAREHOUSE_MANAGER', 'GENERAL_MANAGER')
           AND NOT EXISTS (
                SELECT 1 FROM sys_role_menu rm
                 WHERE rm.role_id = r.id
                   AND rm.menu_id = '20000000-0000-0000-0000-000000001214'::uuid
           );
        GET DIAGNOSTICS n_grants = ROW_COUNT;
        RAISE NOTICE 'V0210：手动触发提醒权限已种，授权新增 % 行', n_grants;
    END IF;

    RAISE NOTICE 'V0210 自检通过：台账 9 列 + 幂等唯一索引 + 4 模板 + 4 任务 + 2 参数 + 1 权限节点';
END $$;
