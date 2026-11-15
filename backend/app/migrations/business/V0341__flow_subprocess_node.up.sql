-- ======================================================
-- V0341：SUBPROCESS 子流程节点 + 父子实例关联（ARCH-5 §4.14 · P3-08）
--
-- 依据：`docs/architecture/ARCH-5-工作流引擎设计.md` §4.14（1822–1846 行）
--       + §1.1「层级子流程」行 + §3.9 / §18.3 迁移清单
-- 回滚：`V0341__flow_subprocess_node.down.sql`
-- 迁移号：动手前核对 `ls backend/migrations/business | grep -o '^V[0-9]*' | sort -u | tail`
--         → 最大号是 V0340（P3-07 的 SoD 开关）⇒ 本卡取 V0341（V0337=P3-06 / V0338=P3-05 /
--         V0339=P3-06 冻结 / V0340=P3-07 已占）。
--
-- ── 为什么是「一个节点跑另一张完整的图」而不是「把子流程展开进来」──────
-- 展开（inline expand）会让子流程的节点 key 与父流程撞名，只能加前缀；
-- 前缀一加，子流程改版就得重新展开所有父流程，且子流程自己的审批记录、超时、
-- 统计全部混在父实例里，谁也说不清「这张单到底卡在哪」。所以落地方式是**真子实例**：
-- 父实例的 SUBPROCESS 节点保持 ACTIVE，子实例自己跑完自己的图，结算时回调父节点。
--
-- ── 三处结构变更（缺一处就是「代码加了、迁移没跟上 → 生产 500」）────
-- ① `biz_flow_node.node_type` 扩到 10 值（9 → 10）；
-- ② `biz_flow_node.subprocess_def_id` + `config.sub` 形状 CHECK
--    + `ck_biz_flow_node_config_partition` **必须同步加 `sub` 那一条**
--    （分区 CHECK 的作用就是「不许把 sub 配到别的类型上」，漏改它 = 插 config.sub 直接失败）；
-- ③ `biz_flow_instance.parent_instance_id` + `parent_node_key` + 部分索引。
--
-- ── 子实例的 `biz_ref` 口径（卡面「决策」）──────────────────────
-- 子实例一律 `biz_id = NULL` / `biz_no = NULL`：`uk_biz_flow_instance_biz_active`
-- 是 `(biz_type, biz_id) WHERE status='PENDING' AND biz_id IS NOT NULL` 的**部分**唯一索引，
-- 子实例留空 ⇒ 既不撞父流程的唯一键（同单据同时只能一个在跑），也不与兄弟子实例相撞。
-- 子实例靠 `parent_instance_id` + `parent_node_key` 就能被精确定位（见部分索引）。
-- ======================================================

-- ---------- ① node_type 扩一个值（9 → 10）----------
-- PostgreSQL 无 ALTER CHECK，必须 DROP + ADD。`biz_flow_node` 当前行数很少，扫描成本可忽略。
ALTER TABLE biz_flow_node DROP CONSTRAINT IF EXISTS biz_flow_node_node_type_check;
ALTER TABLE biz_flow_node ADD CONSTRAINT biz_flow_node_node_type_check CHECK (
    node_type::text = ANY (ARRAY[
      'START','APPROVAL','COUNTERSIGN','CONDITION','PARALLEL','CC','END',
      'TASK_NODE',      -- V0178：流程节点生成工作任务
      'NOTIFY_NODE',    -- V0178：通知节点
      'SUBPROCESS'      -- ★ V0341：子流程节点（跑另一张完整的图）
    ]::text[])
);

-- ---------- ② 子流程定义指针（FK RESTRICT）----------
-- RESTRICT 而不是 CASCADE：被流程引用的子流程定义**不许被物理删**。
-- （`biz_flow_def` 的删除是软删 `deleted_at`，物理删只发生在测试清理；RESTRICT 兜住
--  「删了定义，父流程发布校验时查不到」这类脏数据。）
ALTER TABLE biz_flow_node ADD COLUMN IF NOT EXISTS subprocess_def_id UUID;

ALTER TABLE biz_flow_node DROP CONSTRAINT IF EXISTS fk_biz_flow_node_subprocess_def;
ALTER TABLE biz_flow_node ADD CONSTRAINT fk_biz_flow_node_subprocess_def
    FOREIGN KEY (subprocess_def_id) REFERENCES biz_flow_def(id) ON DELETE RESTRICT;

COMMENT ON COLUMN biz_flow_node.subprocess_def_id IS
    '子流程定义（仅 node_type=SUBPROCESS）：指向要跑的那张图。NULL = 未配置（发布期 R27 拦）';

-- ---------- ③ config.sub 的形状 ----------
-- 分工与 V0178 一致：JSONB CHECK 只兜「关键形状 + 关键枚举」，跨字段语义
-- （版本解析、自递归、嵌套深度、param_mapping 键是否存在于子流程表单）
-- 交给 Rust 发布校验 R27 —— 那些都要查库，写进 CHECK 既读不懂也报不清。
--
-- 默认值口径（与 `flow::subprocess::parse_config` 逐字一致）：
--   · `version_policy` 缺省 = `PIN_CURRENT`（发起时钉当前版，子流程改版不影响在跑的父流程）
--   · `fail_action`    缺省 = `ABORT`（子流程没成，父流程不能当没事发生）
ALTER TABLE biz_flow_node DROP CONSTRAINT IF EXISTS ck_biz_flow_node_subprocess_config;
ALTER TABLE biz_flow_node ADD CONSTRAINT ck_biz_flow_node_subprocess_config CHECK (
    node_type::text <> 'SUBPROCESS' OR (
        subprocess_def_id IS NOT NULL
        AND jsonb_typeof(config -> 'sub') = 'object'
        AND (NOT ((config -> 'sub') ? 'version_policy')
             OR ((config -> 'sub') ->> 'version_policy') IN ('PIN_CURRENT','FOLLOW_LATEST'))
        AND (NOT ((config -> 'sub') ? 'fail_action')
             OR ((config -> 'sub') ->> 'fail_action') IN ('ABORT','CONTINUE'))
        AND (NOT ((config -> 'sub') ? 'spawn_task')
             OR jsonb_typeof((config -> 'sub') -> 'spawn_task') = 'boolean')
        AND (NOT ((config -> 'sub') ? 'param_mapping')
             OR jsonb_typeof((config -> 'sub') -> 'param_mapping') = 'object')
        AND (NOT ((config -> 'sub') ? 'result_mapping')
             OR jsonb_typeof((config -> 'sub') -> 'result_mapping') = 'object')
        -- 互斥：一个节点不能既是子流程又是任务 / 通知（分区 CHECK 的反向已兜，这里提前给出人读原因）
        AND NOT ((config -> 'sub') ? 'task')
        AND NOT ((config -> 'sub') ? 'notify')
    )
);

-- ---------- ④ 分区 CHECK 同步扩（★ 卡面硬约束）----------
-- V0178 那条只说了 task / notify 两个键；不加 `sub` 这一条，插 `config.sub` 会直接
-- 撞 `ck_biz_flow_node_config_partition`（报的是库错误而不是「配置放错节点类型」）。
ALTER TABLE biz_flow_node DROP CONSTRAINT IF EXISTS ck_biz_flow_node_config_partition;
ALTER TABLE biz_flow_node ADD CONSTRAINT ck_biz_flow_node_config_partition CHECK (
    (node_type::text = 'TASK_NODE'   OR NOT (config ? 'task'))
AND (node_type::text = 'NOTIFY_NODE' OR NOT (config ? 'notify'))
AND (node_type::text = 'SUBPROCESS'  OR NOT (config ? 'sub'))
);

COMMENT ON COLUMN biz_flow_node.config IS
'节点扩展配置（JSONB）。按节点类型分区，互斥：
 · assignee_type=EXPRESSION → {"expression":{version,rules[],otherwise}}（现有）
 · node_type=TASK_NODE       → {"task":{title_template,on_complete,due,…}}
 · node_type=NOTIFY_NODE     → {"notify":{when,targets[],template_code|inline,…}}
 · node_type=SUBPROCESS      → {"sub":{version_policy,fail_action,param_mapping,result_mapping,spawn_task}}
形状由 ck_biz_flow_node_* CHECK 兜关键字段，跨字段约束由 flow::graph::validate (R17–R27) 兜。
发布后随 graph_snapshot 冻结，改配置 = 发新版本。';

-- ---------- ⑤ 实例侧：父子关联 ----------
-- 两列成对出现（`ck_biz_flow_instance_parent` 兜住）：只有 parent_instance_id 没有
-- parent_node_key 就没法把子实例的结论「送回哪个节点」，反之亦然。
ALTER TABLE biz_flow_instance ADD COLUMN IF NOT EXISTS parent_instance_id UUID;
ALTER TABLE biz_flow_instance ADD COLUMN IF NOT EXISTS parent_node_key VARCHAR(64);

ALTER TABLE biz_flow_instance DROP CONSTRAINT IF EXISTS fk_biz_flow_instance_parent;
ALTER TABLE biz_flow_instance ADD CONSTRAINT fk_biz_flow_instance_parent
    FOREIGN KEY (parent_instance_id) REFERENCES biz_flow_instance(id) ON DELETE RESTRICT;

ALTER TABLE biz_flow_instance DROP CONSTRAINT IF EXISTS ck_biz_flow_instance_parent;
ALTER TABLE biz_flow_instance ADD CONSTRAINT ck_biz_flow_instance_parent CHECK (
    (parent_instance_id IS NULL) = (parent_node_key IS NULL)
);

CREATE INDEX IF NOT EXISTS idx_biz_flow_instance_parent
    ON biz_flow_instance (parent_instance_id)
    WHERE parent_instance_id IS NOT NULL;

COMMENT ON COLUMN biz_flow_instance.parent_instance_id IS
    '父实例（仅子流程实例）：由 SUBPROCESS 节点在事务内建出来的实例。NULL = 顶层实例';
COMMENT ON COLUMN biz_flow_instance.parent_node_key IS
    '父实例里触发本子实例的节点 key：子实例结算时按 (parent_instance_id, parent_node_key) 回调父节点';

-- ---------- ⑥ 自检（缺一处就迁移失败 = 启动失败）----------
DO $$
DECLARE
    node_type_def text;
    partition_def text;
    cols int;
    idx int;
BEGIN
    -- node_type 取值域必须含 10 个值
    SELECT pg_get_constraintdef(oid) INTO node_type_def
      FROM pg_constraint
     WHERE conrelid = 'biz_flow_node'::regclass
       AND conname = 'biz_flow_node_node_type_check';
    IF node_type_def IS NULL OR node_type_def NOT LIKE '%SUBPROCESS%' THEN
        RAISE EXCEPTION 'V0341 自检失败：node_type CHECK 没有包含 SUBPROCESS（实际：%）', node_type_def;
    END IF;

    -- 分区 CHECK 必须同时出现 task / notify / sub 三个键
    SELECT pg_get_constraintdef(oid) INTO partition_def
      FROM pg_constraint
     WHERE conrelid = 'biz_flow_node'::regclass
       AND conname = 'ck_biz_flow_node_config_partition';
    IF partition_def IS NULL
       OR partition_def NOT LIKE '%sub%'
       OR partition_def NOT LIKE '%task%'
       OR partition_def NOT LIKE '%notify%' THEN
        RAISE EXCEPTION 'V0341 自检失败：config 分区 CHECK 没有同时包含 task/notify/sub（实际：%）', partition_def;
    END IF;

    -- 两条 subprocess 约束存在
    SELECT count(*) INTO cols
      FROM pg_constraint
     WHERE conrelid = 'biz_flow_node'::regclass
       AND conname IN ('ck_biz_flow_node_subprocess_config', 'fk_biz_flow_node_subprocess_def');
    IF cols <> 2 THEN
        RAISE EXCEPTION 'V0341 自检失败：子流程相关约束只存在 % 条（应为 2）', cols;
    END IF;

    -- 实例侧两列 + 部分索引
    SELECT count(*) INTO cols
      FROM information_schema.columns
     WHERE table_name = 'biz_flow_instance'
       AND column_name IN ('parent_instance_id', 'parent_node_key');
    IF cols <> 2 THEN
        RAISE EXCEPTION 'V0341 自检失败：biz_flow_instance 父子关联列只存在 % 列（应为 2）', cols;
    END IF;

    SELECT count(*) INTO idx
      FROM pg_indexes
     WHERE tablename = 'biz_flow_instance'
       AND indexname = 'idx_biz_flow_instance_parent';
    IF idx <> 1 THEN
        RAISE EXCEPTION 'V0341 自检失败：部分索引 idx_biz_flow_instance_parent 不存在';
    END IF;

    RAISE NOTICE 'V0341 自检通过：node_type 10 值 + config.sub 形状 + 分区 CHECK + 父子实例两列 + 部分索引';
END
$$;
