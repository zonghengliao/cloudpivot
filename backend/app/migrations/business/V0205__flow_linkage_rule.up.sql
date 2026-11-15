-- ============================================================
-- V0205：跨单据联动动作白名单 `biz_flow_linkage_rule`（【D2】流程配置化 · 阶段 4）
--
-- 依据：backend/src/business/flow/linkage/（spec.rs 白名单 + model.rs 存取 +
--       engine.rs 执行引擎 + actions.rs 动作实现）
--
-- # 迁移号
--
--   卡面写的是「迁移 V0202（先复核号段空着）」。**核对后 V0202 已被占用**
--   （`V0202__erp_approval_flow_health`），当前最大号是 V0204
--   （`V0204__flow_state_edge`，D1 状态机边配置），故本卡取下一个空号 **V0205**。
--   号段复核命令：`ls backend/migrations/business | grep -oP 'V\d+' | sort -u | tail -3`
--
-- # 这张表是什么
--
--   「单据状态变更之后要做什么」的配置载体：一行 = 一个触发点
--   （`trigger_entity` + `trigger_status`）上的一个**白名单动作**。
--   `action_code` 只能是代码里 `linkage::spec::ACTIONS` 声明过的原子动作，
--   每个动作有固定的入参（`action_params` 的键集合）与事务语义，
--   **不暴露任何 SQL**——这是本卡的核心禁令。
--
-- # 三层校验（本表是第 1 层的落点之一）
--
--   第 1 层：`ck_flow_linkage_action_code` —— action_code 必须是白名单里的；
--            `ck_flow_linkage_no_sql_key` —— 参数里出现 sql/statement/query… 这类键直接拒绝
--            （应用层 `spec::validate_params` 有同口径的判据，两层一致）。
--   第 2 层：后端写入时校验 action_params 的键集合 = 该动作声明的参数（写入口 400）。
--   第 3 层：运行时执行前再校验一次 target 存在、参数合法（失败 → ERROR 日志 + 跳过）。
--
-- # 为什么唯一约束是 `(trigger_entity, trigger_status, action_code)`
--
--   同一个触发点上同一个动作配两次是纯粹的「双真相」（谁先谁后、参数取哪份都不可判）。
--   要改参数就改那一行；要同一动作跑两次请拆成两个不同动作或用 seq 排序的其它动作。
--   软删（`deleted_at`）+ 唯一约束的组合：`upsert` 时 `ON CONFLICT … DO UPDATE
--   SET deleted_at = NULL` 即可「复活」旧行，不需要为「删了再配」另开路径。
--
-- # 留痕
--
--   本表**不是**审计记录：谁在什么时候把哪条规则改成什么，写在 `biz_status_log`
--   （`entity_type = 'FLOW_LINKAGE_CFG'`，只增不改）。改配置的入口在代码里：
--   `linkage::model::upsert_rule` / `delete_rule`，都要 SUPER_ADMIN + 二次确认，
--   且写前 / 写后各留一行快照。
--
-- # 幂等
--
--   CREATE TABLE / INDEX / CONSTRAINT IF NOT EXISTS；不写 BEGIN/COMMIT
--   （迁移器自己在事务里执行）。**不种任何数据**：空表 = 引擎读到 0 条规则
--   = 现有业务行为逐字不变（第 1 步「只读不接管」的证明）。
-- ============================================================

CREATE TABLE IF NOT EXISTS biz_flow_linkage_rule (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    -- 人读的规则名（「收货过账 → 通知仓管」），必填
    name            varchar(128) NOT NULL,
    -- 触发单据类型：与 shared/state_machine.rs 的 Entity::as_str 逐字一致，
    -- 外加 TASK（任务不在状态机注册表里，但有状态流转）
    trigger_entity  varchar(32) NOT NULL,
    -- 触发状态（如 STOCK_IN 的 POSTED、TASK 的 DOING）
    trigger_status  varchar(32) NOT NULL,
    -- 白名单动作码（只能是 linkage::spec::ACTIONS 里的 code）
    action_code     varchar(64) NOT NULL,
    -- 动作参数：**只允许该动作声明的键**，不允许 SQL
    action_params   jsonb       NOT NULL DEFAULT '{}'::jsonb,
    -- 是否启用（false = 不执行；配置保留但休眠）
    enabled         boolean     NOT NULL DEFAULT true,
    -- 执行顺序（同一触发点内升序；相同 seq 按 id 稳定排序）
    seq             integer     NOT NULL DEFAULT 0,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    create_by       uuid,
    update_by       uuid,
    -- 软删：配置「删掉」= 不再执行这条联动
    deleted_at      timestamptz,

    -- 第 1 层（数据库侧）：动作码白名单。
    -- 与 linkage::spec::ACTIONS 的 code 集合逐字一致，由单测
    -- `migration_check_covers_every_action_code` 钉死（改一处漏一处立刻红）。
    CONSTRAINT ck_flow_linkage_action_code CHECK (action_code IN (
        'WRITE_INVENTORY',
        'WRITE_INVENTORY_TXN',
        'UPDATE_COST',
        'GENERATE_VOUCHER',
        'REVERSE_VOUCHER',
        'UPDATE_DOC_STATUS',
        'UPDATE_RECEIVED_QTY',
        'UPDATE_PAYABLE_BALANCE',
        'CREATE_TASK',
        'SEND_NOTICE',
        'LOCK_DOCUMENT'
    )),
    -- 第 1 层（数据库侧）：参数里**不许出现**任何「能被当成语句执行」的键。
    -- 应用层 spec::FORBIDDEN_KEYS 是同口径判据；这一层拦住绕过 API 直接写库的人。
    CONSTRAINT ck_flow_linkage_no_sql_key CHECK (NOT (action_params ?| ARRAY[
        'sql', 'sql_text', 'statement', 'stmt', 'query', 'queries',
        'script', 'expression', 'exec', 'execute', 'shell', 'cmd'
    ])),
    -- 参数必须是 JSON 对象（数组 / 标量都无法表达「键 = 参数」）
    CONSTRAINT ck_flow_linkage_params_object CHECK (jsonb_typeof(action_params) = 'object'),
    CONSTRAINT ck_flow_linkage_seq CHECK (seq >= 0),
    CONSTRAINT ck_flow_linkage_name CHECK (length(btrim(name)) > 0),
    CONSTRAINT uk_flow_linkage_rule UNIQUE (trigger_entity, trigger_status, action_code)
);

COMMENT ON TABLE biz_flow_linkage_rule IS
    '跨单据联动规则（动作白名单，禁配 SQL）：一行 = 触发点(trigger_entity+trigger_status) 上的一个白名单动作(action_code)，按 seq 顺序执行；改配置需 SUPER_ADMIN + 二次确认，留痕写 biz_status_log(entity_type=FLOW_LINKAGE_CFG)。';
COMMENT ON COLUMN biz_flow_linkage_rule.action_code IS
    '白名单动作码（ck_flow_linkage_action_code 与 linkage::spec::ACTIONS 逐字一致）；动作内部实现复用既有 handler/model，不重写业务算法。';
COMMENT ON COLUMN biz_flow_linkage_rule.action_params IS
    '动作参数 JSONB：键集合必须等于该动作声明的参数（第 2 层校验）；出现 sql/statement/query 等键会被 ck_flow_linkage_no_sql_key 拒绝。';
COMMENT ON COLUMN biz_flow_linkage_rule.seq IS
    '同一触发点内的执行顺序（升序）；相同 seq 按 id 稳定排序。事务内动作与提交后动作各自按 seq 排（见 linkage::engine 的事务边界说明）。';

-- 读路径：按触发点取存活规则（引擎每次状态变更都要查）
CREATE INDEX IF NOT EXISTS idx_flow_linkage_rule_trigger
    ON biz_flow_linkage_rule (trigger_entity, trigger_status, seq)
    WHERE deleted_at IS NULL;

-- 配置页/巡检：按动作码反查「这个动作被哪些规则用了」
CREATE INDEX IF NOT EXISTS idx_flow_linkage_rule_action
    ON biz_flow_linkage_rule (action_code)
    WHERE deleted_at IS NULL;

-- ------------------------------------------------------------
-- 结构自检（迁移期断言，失败即整笔回滚）
-- ------------------------------------------------------------
DO $$
DECLARE
    n_col   integer;
    n_ck    integer;
    n_idx   integer;
    n_rows  integer;
BEGIN
    SELECT count(*) INTO n_col FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_flow_linkage_rule'
       AND column_name IN ('id', 'name', 'trigger_entity', 'trigger_status', 'action_code',
                           'action_params', 'enabled', 'seq', 'created_at', 'updated_at',
                           'create_by', 'update_by', 'deleted_at');
    IF n_col <> 13 THEN
        RAISE EXCEPTION 'V0205 自检失败：biz_flow_linkage_rule 列数应为 13，实际 %', n_col;
    END IF;

    -- 三条 CHECK + 一条 UNIQUE 必须都在（第 1 层防护的落点）
    SELECT count(*) INTO n_ck FROM pg_constraint
     WHERE conrelid = 'biz_flow_linkage_rule'::regclass
       AND conname IN ('ck_flow_linkage_action_code', 'ck_flow_linkage_no_sql_key',
                       'ck_flow_linkage_params_object', 'ck_flow_linkage_seq',
                       'ck_flow_linkage_name', 'uk_flow_linkage_rule');
    IF n_ck <> 6 THEN
        RAISE EXCEPTION 'V0205 自检失败：约束数应为 6（5 CHECK + 1 UNIQUE），实际 %', n_ck;
    END IF;

    SELECT count(*) INTO n_idx FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_flow_linkage_rule'
       AND indexname IN ('idx_flow_linkage_rule_trigger', 'idx_flow_linkage_rule_action');
    IF n_idx <> 2 THEN
        RAISE EXCEPTION 'V0205 自检失败：索引数应为 2，实际 %', n_idx;
    END IF;

    -- 空表是刻意的：0 条规则 ⇒ 引擎一条动作都不执行 ⇒ 现有业务行为逐字不变
    SELECT count(*) INTO n_rows FROM biz_flow_linkage_rule;
    IF n_rows <> 0 THEN
        RAISE NOTICE 'V0205：biz_flow_linkage_rule 已有 % 行配置（沿用，未改动）', n_rows;
    END IF;

    RAISE NOTICE 'V0205 完成：biz_flow_linkage_rule 建表 + 白名单 CHECK + 禁 SQL 键 CHECK + 触发点索引，初始 0 行（引擎不接管任何联动）';
END $$;
