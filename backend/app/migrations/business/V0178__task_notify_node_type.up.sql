-- ======================================================
-- V0178：节点类型扩两种（TASK_NODE / NOTIFY_NODE）+ config 分区 CHECK
--
-- 依据：`docs/architecture/流程全配置化-设计.md` §2.7（commit 09b87fe）
-- 回滚：`V0178__task_notify_node_type.down.sql`
--
-- ── 为什么 7 → 9 而不是「把 APPROVAL 改成能生成任务」────────
-- `APPROVAL` / `COUNTERSIGN` 生成的是 `biz_flow_task`，语义是「我要你**签字**」，
-- 收件人只有同意 / 驳回两个动作，生命周期绑死在实例节点上。
-- `TASK_NODE` 生成的是 `biz_task`，语义是「我要你**干活**」：有进度、工时、子任务、
-- 附件、评论、依赖、可验收、可被打回重做、能被统计和排期。
-- 混成一体会让「审批中心」与「工作任务」两个入口都变脏。
--
-- ── JSONB CHECK 只兜「关键形状 + 关键枚举」─────────────
-- 跨字段交叉约束（`due.mode` 与必备键、`need_review × on_complete` 矩阵、模板变量白名单）
-- 交给 Rust 解析器（`flow::node_config` + `flow::graph` 的 R18 / R20 / R21）：
-- jsonb CHECK 写 allOf / if-then 会变成不可读的一坨，而且**错误文案无法解释**。
-- 这与既有 `ck_biz_flow_node_expression` 的分工一致（它也只看 rules 数组长度）。
--
-- ── 1A 阶段说明（重要）──────────────
-- 本迁移把 NOTIFY_NODE 的取值域与形状都放开了，但**引擎分支在 P-FLOW-2**。
-- 为避免「发布得出去、跑起来静默不生效」，P-FLOW-1A 的发布校验 R19 **直接拒绝** NOTIFY_NODE
-- （见 `flow/graph.rs` 的 `validate_with`）。P-FLOW-2 落地引擎后删除那条阶段闸。
-- ======================================================

-- ---------- ① node_type 扩两个值（7 → 9）----------
-- PostgreSQL 无 ALTER CHECK，必须 DROP + ADD。`biz_flow_node` 当前 16 行，扫描成本可忽略。
ALTER TABLE biz_flow_node DROP CONSTRAINT biz_flow_node_node_type_check;
ALTER TABLE biz_flow_node ADD CONSTRAINT biz_flow_node_node_type_check CHECK (
    node_type::text = ANY (ARRAY[
      'START','APPROVAL','COUNTERSIGN','CONDITION','PARALLEL','CC','END',
      'TASK_NODE',      -- ★ 新增：流程节点生成工作任务
      'NOTIFY_NODE'     -- ★ 新增：通知节点（引擎在 P-FLOW-2）
    ]::text[])
);

-- ---------- ② TASK_NODE 的 config.task 形状 ----------
ALTER TABLE biz_flow_node ADD CONSTRAINT ck_biz_flow_node_task_config CHECK (
    node_type::text <> 'TASK_NODE' OR (
        jsonb_typeof(config -> 'task') = 'object'
        AND jsonb_typeof((config -> 'task') -> 'title_template') = 'string'
        AND length((config -> 'task') ->> 'title_template') BETWEEN 1 AND 200
        AND jsonb_typeof((config -> 'task') -> 'on_complete') = 'string'
        AND ((config -> 'task') ->> 'on_complete') IN ('DIRECT','VERIFY','APPROVE')
        AND (NOT ((config -> 'task') ? 'priority')
             OR ((config -> 'task') ->> 'priority') IN ('P0','P1','P2','P3'))
        AND (NOT ((config -> 'task') ? 'on_reject')
             OR ((config -> 'task') ->> 'on_reject') IN ('BACK_TO_PREV','TERMINATE','STAY'))
        AND (NOT ((config -> 'task') ? 'on_cancel')
             OR ((config -> 'task') ->> 'on_cancel') IN ('BACK_TO_PREV','TERMINATE','STAY'))
        -- 互斥：一个节点不能又是任务又是通知
        AND NOT ((config -> 'task') ? 'notify')
    )
);

-- ---------- ③ NOTIFY_NODE 的 config.notify 形状 ----------
ALTER TABLE biz_flow_node ADD CONSTRAINT ck_biz_flow_node_notify_config CHECK (
    node_type::text <> 'NOTIFY_NODE' OR (
        jsonb_typeof(config -> 'notify') = 'object'
        AND jsonb_typeof((config -> 'notify') -> 'targets') = 'array'
        AND jsonb_array_length((config -> 'notify') -> 'targets') BETWEEN 1 AND 5
        AND jsonb_typeof((config -> 'notify') -> 'when') = 'string'
        AND ((config -> 'notify') ->> 'when') IN ('ON_ENTER','ON_LEAVE','ON_TIMEOUT')
        -- 模板二选一：恰好一个
        AND ( ((config -> 'notify') ? 'template_code')
              <> ((config -> 'notify') ? 'inline') )
        AND NOT ((config -> 'notify') ? 'task')
    )
);

-- ---------- ④ 反向：task / notify 只许出现在对应类型上（防「改类型不改配置」）----------
ALTER TABLE biz_flow_node ADD CONSTRAINT ck_biz_flow_node_config_partition CHECK (
    (node_type::text = 'TASK_NODE'   OR NOT (config ? 'task'))
AND (node_type::text = 'NOTIFY_NODE' OR NOT (config ? 'notify'))
);

COMMENT ON COLUMN biz_flow_node.config IS
'节点扩展配置（JSONB）。按节点类型分区，互斥：
 · assignee_type=EXPRESSION → {"expression":{version,rules[],otherwise}}（现有）
 · node_type=TASK_NODE       → {"task":{title_template,on_complete,due,…}}
 · node_type=NOTIFY_NODE     → {"notify":{when,targets[],template_code|inline,…}}
形状由 ck_biz_flow_node_* CHECK 兜关键字段，跨字段约束由 flow::graph::validate (R17–R21) 兜。
发布后随 graph_snapshot 冻结，改配置 = 发新版本。';

-- ---------- ⑤ 自检 ----------
DO $$
DECLARE
    wrong     int;
    node_type_def text;
BEGIN
    SELECT count(*) INTO wrong
      FROM pg_constraint
     WHERE conrelid = 'biz_flow_node'::regclass
       AND conname IN ('ck_biz_flow_node_task_config',
                       'ck_biz_flow_node_notify_config',
                       'ck_biz_flow_node_config_partition');
    IF wrong <> 3 THEN
        RAISE EXCEPTION 'V0178 自检失败：三条 config 分区 CHECK 只存在 % 条（应为 3）', wrong;
    END IF;

    -- 取值域：约束定义里必须同时出现这两个新值（不插数据探测，避免污染 16 行存量）
    SELECT pg_get_constraintdef(oid) INTO node_type_def
      FROM pg_constraint
     WHERE conrelid = 'biz_flow_node'::regclass
       AND conname = 'biz_flow_node_node_type_check';
    IF node_type_def IS NULL
       OR node_type_def NOT LIKE '%TASK_NODE%'
       OR node_type_def NOT LIKE '%NOTIFY_NODE%' THEN
        RAISE EXCEPTION 'V0178 自检失败：node_type CHECK 没有同时包含 TASK_NODE 与 NOTIFY_NODE（实际：%）', node_type_def;
    END IF;

    RAISE NOTICE 'V0178 自检通过：node_type 扩到 9 值 + 3 条 config 分区 CHECK';
END
$$;
