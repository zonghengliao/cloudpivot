-- ============================================================
-- V0360：联动规则的**触发点覆盖率**补齐（E2E-07 · LNK-07 零规则问题）
--
-- 依据：`backend/src/business/flow/linkage/spec.rs::TRIGGER_SPECS`（16 个 wired 触发点）
--       + 各触发点的 hook（`erp/*_linkage.rs` / `purchase_return_handler` / `instance_linkage`）
--       + `migrations/business/V0254__flow_linkage_presets.up.sql`（同型先例）
--       + `migrations/business/V0323__asset_linkage_rules.up.sql` / `V0351__purchase_return_linkage_rule.up.sql`
--       + `scripts/verify/linkage_rule_invariants.sh`（本迁移落地的口径由这个不变量脚本持续看守）
--
-- # 为什么需要这张迁移
--
--   E2E-07 实测：`LNK-07` 把合同状态变更接进了联动引擎（`contract_linkage.rs` 三个状态推进点
--   真的在调 `engine::run_excluding` / `run_deferred`），`spec::TRIGGER_SPECS` 也标了
--   `wired = true`，**但迁移全集跑完后 `biz_flow_linkage_rule` 里 CONTRACT 规则 0 条** ⇒
--   合同签定 / 终止**一条通知都不发**，而且没有任何报错。卡面那句「两道审批路径只触发一次联动」
--   在默认配置下无从体现（只配了 0 条规则 = 联动这条能力不存在）。
--
--   全系统扫描（145，迁移到 V0359 后）确认这不是 CONTRACT 一个点的问题 —— **16 个 wired
--   触发点里只有 4 个有存活规则**（STOCK_IN×2 / FIXED_ASSET×3 / PURCHASE_RETURN×1 / TASK×1），
--   其余 12 个（含 TASK 在演练清场时被删掉的那条）都是「接线了但零规则」：
--
--     CONTRACT · STOCK_OUT · STOCK_TRANSFER · PURCHASE_ORDER · PAYABLE · VOUCHER ·
--     ACCOUNTING_PERIOD · ACCOUNTING_YEAR · STOCK_TAKE · DEPRECIATION · EXPENSE_REPORT ·
--     FLOW_INSTANCE（其 `APPROVED` 行被 LNK-12 取证收口时软删）
--
--   与 V0323（FIXED_ASSET）/ V0351（PURCHASE_RETURN）同源：空表是 V0205 刻意的初始态，
--   但**只有空表**就等于「这 12 个触发点的通知能力不存在」——没有人会想到
--   「合同签了要先去配一条联动规则才会有人收到通知」。
--
-- # 15 条规则（每个触发点一条，落在它**真的会触发**的那个业务结果态上）
--
--   | # | 规则名 | 触发点 | 事务归属 |
--   |---|--------|--------|----------|
--   | 1 | 合同签订 → 通知制单人             | CONTRACT/SIGNED            | AFTER_COMMIT |
--   | 2 | 合同终止 → 通知制单人             | CONTRACT/TERMINATED        | AFTER_COMMIT |
--   | 3 | 出库过账 → 通知制单人             | STOCK_OUT/POSTED           | AFTER_COMMIT |
--   | 4 | 调拨完成 → 通知制单人             | STOCK_TRANSFER/DONE        | AFTER_COMMIT |
--   | 5 | 采购订单全部收货 → 通知制单人     | PURCHASE_ORDER/RECEIVED    | AFTER_COMMIT |
--   | 6 | 应付结清 → 通知制单人             | PAYABLE/SETTLED            | AFTER_COMMIT |
--   | 7 | 凭证记账 → 通知制单人             | VOUCHER/POSTED             | AFTER_COMMIT |
--   | 8 | 会计期间结账 → 通知操作人         | ACCOUNTING_PERIOD/CLOSED   | AFTER_COMMIT |
--   | 9 | 会计年度结转 → 通知操作人         | ACCOUNTING_YEAR/CLOSED     | AFTER_COMMIT |
--   |10 | 盘点过账 → 通知制单人             | STOCK_TAKE/POSTED          | AFTER_COMMIT |
--   |11 | 折旧过账 → 通知制单人             | DEPRECIATION/POSTED        | AFTER_COMMIT |
--   |12 | 报销付清 → 通知制单人             | EXPENSE_REPORT/PAID        | AFTER_COMMIT |
--   |13 | 任务开工 → 通知负责人与创建人     | TASK/DOING                 | AFTER_COMMIT |
--   |14 | 审批通过 → 通知发起人             | FLOW_INSTANCE/APPROVED     | AFTER_COMMIT |
--   |15 | 审批驳回 → 通知发起人             | FLOW_INSTANCE/REJECTED     | AFTER_COMMIT |
--
--   ## 为什么状态选「这一个」而不是全状态铺满
--
--   每条规则必须落在**真的有调用点**的目标态上（否则就是「配得上、永远不响」）：
--
--   * `STOCK_OUT` 只在 `post_out_tx` 里过账时接引擎 ⇒ 只有 `POSTED`；
--   * `STOCK_TRANSFER` 六态都接（`apply_transition` 唯一状态写入点），选终态 `DONE`
--     （「调拨完成了」才是收件人关心的事；中间态各发一条就是刷屏）；
--   * `PURCHASE_ORDER` 两处 hook（`apply_status` + `sync_purchase_status`），选业务终态
--     `RECEIVED`（全部收货）；`APPROVED` 由审批流程自己的通知节点负责，不重复发；
--   * `PAYABLE` 只有付款事务接引擎 ⇒ 选 `SETTLED`（结清）；部分付款不发通知（`PARTIAL_PAID`）；
--   * `VOUCHER` 只接「凭证自身的人工 / 红冲流转」⇒ 选 `POSTED`（记账完成）；
--   * `ACCOUNTING_PERIOD` 只有结账 `CLOSED` / 反结账 `OPEN` 接引擎，选 `CLOSED`
--     （开账是批量补行，挂上就是启动扇出 N 条通知）；
--   * `ACCOUNTING_YEAR` 只有年结 `CLOSED` / 反年结 `NOT_CLOSED`，选 `CLOSED`；
--   * `STOCK_TAKE` 只有过账 `POSTED` / 作废 `CANCELLED`，选 `POSTED`；
--   * `DEPRECIATION` 只有过账 `POSTED` / 红冲 `REVERSED`（`CALCULATED` 是同事务中间态、
--     `SKIPPED` 是空跑，两者都没触发源 —— 见 spec.rs 该触发点的 wiring 说明），选 `POSTED`；
--   * `EXPENSE_REPORT` 五个推进点都接，选 `PAID`（钱付出去了）。`SUBMITTED / APPROVED`
--     由审批流程的通知节点负责，不重复发；
--   * `TASK/DOING` 是 V0254 的键（基线口径，`status_notice` 专用路由），本迁移把它重新种回来
--     —— 145 演练清场时这行被硬删了，导致「任务开工 → 通知」这条基线能力也消失了；
--   * `FLOW_INSTANCE` 三个终态都接（`instance_linkage::fire_after_commit`），种 `APPROVED`
--     与 `REJECTED` 两条。**注意**：145 上 `FLOW_INSTANCE/APPROVED` 有一行软删的 LNK-12
--     取证残留（`name = 'LNK12 审批通过取证'`），本迁移按 `DO NOTHING` 保留运维/取证痕迹，
--     不复活它；`REJECTED` 是新键，两条合起来保证「审批结论至少有一种结论会通知」。
--
--   ## 动作为什么是 SEND_NOTICE + TAKEOVER
--
--   * 这些触发点上**能用的已接管动作只有 SEND_NOTICE**：动账 / 凭证类动作由各自单据的专用
--     路径落账（见 spec.rs 里每个 `*_EXCLUDED_ACTIONS`），其余动作（`CREATE_TASK` 之外）仍是占位、
--     写配置阶段就被拒。
--   * `SHADOW` 只做影子对比、不投递 —— 这些触发点**没有旧代码在发通知**（各 `*_linkage.rs`
--     只把 `NoticeDraft` 放进 `LinkageContext.notice`，投递完全靠引擎）⇒ 写成 SHADOW 等于
--     规则配了也一条不发。所以必须 `{"mode": "TAKEOVER"}`（与 V0254 / V0323 / V0351 同口径）。
--   * 不写 `recipient` = `LEGACY` = 沿用触发点算好的收件人（合同 / 出库 / 调拨 / 采购 / 应付 /
--     凭证 / 盘点 / 折旧 / 报销取制单人（缺失退操作人）；期间 / 年结取操作人；审批实例取发起人）。
--     标题 / 正文同理不覆盖：各 `*_linkage.rs` 已经拼好了人读文案并带详情页链接
--     （`零重复真相`）。
--
-- # 幂等（生产上可能已经手工配过规则）
--
--   `ON CONFLICT (trigger_entity, trigger_status, action_code) DO NOTHING`：同一键上
--   **已经存在的行**（运维手工配的、被停用的、被软删的）一律**原样保留** —— 那是运维的决定，
--   迁移不该覆盖。自检因此只断言「15 个键各有一行」，不强推 `enabled` / `TAKEOVER`；
--   在全新库上这 15 行由本迁移插入，必然 `enabled + TAKEOVER`（验收见 NOTICE）。
--   不写 BEGIN/COMMIT（迁移器自己在事务里执行）。
--
-- # 落地后的持续看守
--
--   本迁移一次性补齐存量缺口；「以后新接一个触发点却忘了种规则」由两道闸兜住：
--
--   1. 代码侧单测 `linkage::tests_presets_coverage::every_wired_trigger_has_a_preset`
--      （把 `spec::wired_triggers()` 与全部预置迁移原文对拍：新 wired 触发点没有预置 → 红）；
--   2. 库侧不变量 `scripts/verify/linkage_rule_invariants.sh`
--      （wired 但零 enabled 规则 / 规则挂在未接线触发点 / 状态写错 / 动作被结构性禁止 → 红）。
-- ============================================================

INSERT INTO biz_flow_linkage_rule
    (id, name, trigger_entity, trigger_status, action_code, action_params,
     enabled, seq, created_at, updated_at, create_by, update_by, deleted_at)
VALUES
    -- ① 合同签订（PENDING_APPROVAL → SIGNED）：E2E-07 发现的零规则触发点（本卡主角）
    (gen_random_uuid(),
     '合同签订 → 通知制单人',
     'CONTRACT', 'SIGNED', 'SEND_NOTICE',
     '{"mode": "TAKEOVER"}'::jsonb,
     true, 10, now(), now(), NULL, NULL, NULL),
    -- ② 合同终止（SIGNED|ACTIVE → TERMINATED）
    (gen_random_uuid(),
     '合同终止 → 通知制单人',
     'CONTRACT', 'TERMINATED', 'SEND_NOTICE',
     '{"mode": "TAKEOVER"}'::jsonb,
     true, 20, now(), now(), NULL, NULL, NULL),
    -- ③ 出库过账（post_out_tx ⑥ 通用引擎）
    (gen_random_uuid(),
     '出库过账 → 通知制单人',
     'STOCK_OUT', 'POSTED', 'SEND_NOTICE',
     '{"mode": "TAKEOVER"}'::jsonb,
     true, 10, now(), now(), NULL, NULL, NULL),
    -- ④ 调拨完成（IN_TRANSIT → DONE）
    (gen_random_uuid(),
     '调拨完成 → 通知制单人',
     'STOCK_TRANSFER', 'DONE', 'SEND_NOTICE',
     '{"mode": "TAKEOVER"}'::jsonb,
     true, 10, now(), now(), NULL, NULL, NULL),
    -- ⑤ 采购订单全部收货（sync_purchase_status → RECEIVED）
    (gen_random_uuid(),
     '采购订单全部收货 → 通知制单人',
     'PURCHASE_ORDER', 'RECEIVED', 'SEND_NOTICE',
     '{"mode": "TAKEOVER"}'::jsonb,
     true, 10, now(), now(), NULL, NULL, NULL),
    -- ⑥ 应付结清（付款事务 ⑤ → SETTLED）
    (gen_random_uuid(),
     '应付结清 → 通知制单人',
     'PAYABLE', 'SETTLED', 'SEND_NOTICE',
     '{"mode": "TAKEOVER"}'::jsonb,
     true, 10, now(), now(), NULL, NULL, NULL),
    -- ⑦ 凭证记账（post_audited_voucher → POSTED）
    (gen_random_uuid(),
     '凭证记账 → 通知制单人',
     'VOUCHER', 'POSTED', 'SEND_NOTICE',
     '{"mode": "TAKEOVER"}'::jsonb,
     true, 10, now(), now(), NULL, NULL, NULL),
    -- ⑧ 会计期间结账（close_period → CLOSED）
    (gen_random_uuid(),
     '会计期间结账 → 通知操作人',
     'ACCOUNTING_PERIOD', 'CLOSED', 'SEND_NOTICE',
     '{"mode": "TAKEOVER"}'::jsonb,
     true, 10, now(), now(), NULL, NULL, NULL),
    -- ⑨ 会计年度结转（close_year → CLOSED）
    (gen_random_uuid(),
     '会计年度结转 → 通知操作人',
     'ACCOUNTING_YEAR', 'CLOSED', 'SEND_NOTICE',
     '{"mode": "TAKEOVER"}'::jsonb,
     true, 10, now(), now(), NULL, NULL, NULL),
    -- ⑩ 盘点过账（post_tx ⑥ → POSTED）
    (gen_random_uuid(),
     '盘点过账 → 通知制单人',
     'STOCK_TAKE', 'POSTED', 'SEND_NOTICE',
     '{"mode": "TAKEOVER"}'::jsonb,
     true, 10, now(), now(), NULL, NULL, NULL),
    -- ⑪ 折旧过账（run_batch ⑤ → POSTED）
    (gen_random_uuid(),
     '折旧过账 → 通知制单人',
     'DEPRECIATION', 'POSTED', 'SEND_NOTICE',
     '{"mode": "TAKEOVER"}'::jsonb,
     true, 10, now(), now(), NULL, NULL, NULL),
    -- ⑫ 报销付清（report_pay → PAID）
    (gen_random_uuid(),
     '报销付清 → 通知制单人',
     'EXPENSE_REPORT', 'PAID', 'SEND_NOTICE',
     '{"mode": "TAKEOVER"}'::jsonb,
     true, 10, now(), now(), NULL, NULL, NULL),
    -- ⑬ 任务开工（TODO → DOING）：V0254 的基线键，145 演练清场时被硬删，这里种回来
    (gen_random_uuid(),
     '任务开工 → 通知负责人与创建人',
     'TASK', 'DOING', 'SEND_NOTICE',
     '{"mode": "TAKEOVER"}'::jsonb,
     true, 10, now(), now(), NULL, NULL, NULL),
    -- ⑭ 审批通过（instance_linkage::fire_after_commit → APPROVED）
    (gen_random_uuid(),
     '审批通过 → 通知发起人',
     'FLOW_INSTANCE', 'APPROVED', 'SEND_NOTICE',
     '{"mode": "TAKEOVER"}'::jsonb,
     true, 10, now(), now(), NULL, NULL, NULL),
    -- ⑮ 审批驳回（instance_linkage::fire_after_commit → REJECTED）
    (gen_random_uuid(),
     '审批驳回 → 通知发起人',
     'FLOW_INSTANCE', 'REJECTED', 'SEND_NOTICE',
     '{"mode": "TAKEOVER"}'::jsonb,
     true, 10, now(), now(), NULL, NULL, NULL)
ON CONFLICT (trigger_entity, trigger_status, action_code) DO NOTHING;

-- ------------------------------------------------------------
-- 自检（迁移期断言）
--
-- 分两类：
--   * **本迁移自己插入的行**缺失 → EXCEPTION（说明 INSERT 写错了，不能让升级静默少一条）；
--   * **该触发点上出现结构性被禁的动账 / 凭证动作** → EXCEPTION（绕过 API 写进来的脏行，
--     通用引擎再跑一次就是重复落账，必须当场红 —— 与 V0323 / V0351 同口径）；
--   * **wired 触发点仍然零 enabled 规则** → WARNING 点名（运营可能刻意停用 / 删了某条规则，
--     迁移不该把升级挡下来；缺口交给 scripts/verify/linkage_rule_invariants.sh 持续报警）。
-- ------------------------------------------------------------
DO $$
DECLARE
    n_keys    integer;
    n_alive   integer;
    n_take    integer;
    n_bad_act integer;
    n_gap     integer;
    names     text;
BEGIN
    -- ① 15 个键必须各有一行（插入成功 / 早已存在都满足）
    SELECT count(*) INTO n_keys FROM biz_flow_linkage_rule
     WHERE (trigger_entity, trigger_status, action_code) IN (
           ('CONTRACT',          'SIGNED',    'SEND_NOTICE'),
           ('CONTRACT',          'TERMINATED','SEND_NOTICE'),
           ('STOCK_OUT',         'POSTED',    'SEND_NOTICE'),
           ('STOCK_TRANSFER',    'DONE',      'SEND_NOTICE'),
           ('PURCHASE_ORDER',    'RECEIVED',  'SEND_NOTICE'),
           ('PAYABLE',           'SETTLED',   'SEND_NOTICE'),
           ('VOUCHER',           'POSTED',    'SEND_NOTICE'),
           ('ACCOUNTING_PERIOD', 'CLOSED',    'SEND_NOTICE'),
           ('ACCOUNTING_YEAR',   'CLOSED',    'SEND_NOTICE'),
           ('STOCK_TAKE',        'POSTED',    'SEND_NOTICE'),
           ('DEPRECIATION',      'POSTED',    'SEND_NOTICE'),
           ('EXPENSE_REPORT',    'PAID',      'SEND_NOTICE'),
           ('TASK',              'DOING',     'SEND_NOTICE'),
           ('FLOW_INSTANCE',     'APPROVED',  'SEND_NOTICE'),
           ('FLOW_INSTANCE',     'REJECTED',  'SEND_NOTICE')
       );
    IF n_keys <> 15 THEN
        RAISE EXCEPTION 'V0360 自检失败：15 个预置键应各有一行，实际 % 行', n_keys;
    END IF;

    -- ② 结构性被禁的动作不许出现在本卡补的三个触发点上（合同 / 凭证 / 审批实例）：
    --    合同不产生凭证（合同号没有借贷科目口径）、凭证上生成凭证是死循环、
    --    审批实例不落账（改单据状态 / 写库存 / 出凭证归业务桥的专用实现）。
    SELECT count(*) INTO n_bad_act FROM biz_flow_linkage_rule
     WHERE deleted_at IS NULL
       AND ((trigger_entity = 'CONTRACT'      AND action_code = 'GENERATE_VOUCHER')
         OR (trigger_entity = 'VOUCHER'       AND action_code = 'GENERATE_VOUCHER')
         OR (trigger_entity = 'FLOW_INSTANCE' AND action_code IN (
                'WRITE_INVENTORY', 'WRITE_INVENTORY_TXN', 'UPDATE_COST', 'GENERATE_VOUCHER',
                'REVERSE_VOUCHER', 'UPDATE_RECEIVED_QTY', 'UPDATE_PAYABLE_BALANCE',
                'UPDATE_DOC_STATUS', 'LOCK_DOCUMENT')));
    IF n_bad_act <> 0 THEN
        RAISE EXCEPTION 'V0360 自检失败：CONTRACT / VOUCHER / FLOW_INSTANCE 上有 % 条结构性被禁的规则（绕过 API 写进来的脏行，运行时只会被排除，等于配了不跑）', n_bad_act;
    END IF;

    -- ③ 统计口径（NOTICE 用）
    SELECT count(*) INTO n_alive FROM biz_flow_linkage_rule
     WHERE (trigger_entity, trigger_status, action_code) IN (
           ('CONTRACT',          'SIGNED',    'SEND_NOTICE'),
           ('CONTRACT',          'TERMINATED','SEND_NOTICE'),
           ('STOCK_OUT',         'POSTED',    'SEND_NOTICE'),
           ('STOCK_TRANSFER',    'DONE',      'SEND_NOTICE'),
           ('PURCHASE_ORDER',    'RECEIVED',  'SEND_NOTICE'),
           ('PAYABLE',           'SETTLED',   'SEND_NOTICE'),
           ('VOUCHER',           'POSTED',    'SEND_NOTICE'),
           ('ACCOUNTING_PERIOD', 'CLOSED',    'SEND_NOTICE'),
           ('ACCOUNTING_YEAR',   'CLOSED',    'SEND_NOTICE'),
           ('STOCK_TAKE',        'POSTED',    'SEND_NOTICE'),
           ('DEPRECIATION',      'POSTED',    'SEND_NOTICE'),
           ('EXPENSE_REPORT',    'PAID',      'SEND_NOTICE'),
           ('TASK',              'DOING',     'SEND_NOTICE'),
           ('FLOW_INSTANCE',     'APPROVED',  'SEND_NOTICE'),
           ('FLOW_INSTANCE',     'REJECTED',  'SEND_NOTICE')
       )
       AND deleted_at IS NULL AND enabled = true;
    SELECT count(*) INTO n_take FROM biz_flow_linkage_rule
     WHERE (trigger_entity, trigger_status, action_code) IN (
           ('CONTRACT',          'SIGNED',    'SEND_NOTICE'),
           ('CONTRACT',          'TERMINATED','SEND_NOTICE'),
           ('STOCK_OUT',         'POSTED',    'SEND_NOTICE'),
           ('STOCK_TRANSFER',    'DONE',      'SEND_NOTICE'),
           ('PURCHASE_ORDER',    'RECEIVED',  'SEND_NOTICE'),
           ('PAYABLE',           'SETTLED',   'SEND_NOTICE'),
           ('VOUCHER',           'POSTED',    'SEND_NOTICE'),
           ('ACCOUNTING_PERIOD', 'CLOSED',    'SEND_NOTICE'),
           ('ACCOUNTING_YEAR',   'CLOSED',    'SEND_NOTICE'),
           ('STOCK_TAKE',        'POSTED',    'SEND_NOTICE'),
           ('DEPRECIATION',      'POSTED',    'SEND_NOTICE'),
           ('EXPENSE_REPORT',    'PAID',      'SEND_NOTICE'),
           ('TASK',              'DOING',     'SEND_NOTICE'),
           ('FLOW_INSTANCE',     'APPROVED',  'SEND_NOTICE'),
           ('FLOW_INSTANCE',     'REJECTED',  'SEND_NOTICE')
       )
       AND deleted_at IS NULL AND enabled = true
       AND action_params ->> 'mode' = 'TAKEOVER';

    -- ④ 不变量：16 个 wired 触发点每个都要有 ≥1 条 enabled 规则。
    --    这里只 WARNING 点名，不挡升级（运营可能刻意停用；缺口由不变量脚本持续报警）。
    SELECT count(*), string_agg(t.entity, ' / ' ORDER BY t.entity)
      INTO n_gap, names
      FROM (VALUES
        ('STOCK_IN'), ('STOCK_OUT'), ('STOCK_TRANSFER'), ('PURCHASE_ORDER'),
        ('PAYABLE'), ('VOUCHER'), ('ACCOUNTING_PERIOD'), ('ACCOUNTING_YEAR'),
        ('CONTRACT'), ('STOCK_TAKE'), ('FIXED_ASSET'), ('DEPRECIATION'),
        ('EXPENSE_REPORT'), ('PURCHASE_RETURN'), ('TASK'), ('FLOW_INSTANCE')
      ) t(entity)
     WHERE NOT EXISTS (
        SELECT 1 FROM biz_flow_linkage_rule r
         WHERE r.trigger_entity = t.entity AND r.enabled AND r.deleted_at IS NULL);
    IF n_gap > 0 THEN
        RAISE WARNING 'V0360：以下 wired 触发点仍然零条 enabled 规则（接线了但能力不存在）：%', names;
    END IF;

    RAISE NOTICE 'V0360 完成：补齐 15 条 SEND_NOTICE/TAKEOVER 预置规则（CONTRACT×2 / STOCK_OUT / STOCK_TRANSFER / PURCHASE_ORDER / PAYABLE / VOUCHER / ACCOUNTING_PERIOD / ACCOUNTING_YEAR / STOCK_TAKE / DEPRECIATION / EXPENSE_REPORT / TASK / FLOW_INSTANCE×2）；当前存活 % 条、其中 enabled+TAKEOVER % 条（已有手工配置时按运维的来，不覆盖）', n_alive, n_take;
END $$;
