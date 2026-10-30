-- ============================================================
-- V0136：审批流 · 动态审批人表达式（EXPRESSION）
--
-- 依据：`docs/批次D-EXPRESSION-DSL-规范.md` §7（DDL 提案，PG 18.6 真跑）
-- ① 放开 assignee_type 枚举 ② EXPRESSION ⇒ config.expression 存在性兜底
-- ③ biz_flow_task.assignee_rule_id（命中规则 id，审计「为什么是他审我」） ④ 自检
-- 回滚：V0136__flow_expression_assignee.down.sql
--
-- ⚠ 编号说明：设计文档与卡面都写 V0125，但 V0125 已被
--    `V0125__flow_notice_templates` 占用（迁移号是全仓共享资源），
--    因此本卡取「当前最大号 + 1」= V0136（V0135 是当时最大）。
--
-- ⚠ 与设计文档 §7.3 全文的两处必改（其余逐字一致）：
--    1. 去掉文件自带的 `BEGIN; … COMMIT;` —— 迁移器**自己**在事务里执行本文件
--       （实测日志 `WARNING: there is already a transaction in progress`），
--       文件内的 COMMIT 会提前提交掉迁移器的事务；仓库里 63 个 up 迁移只有 1 个带 BEGIN。
--    2. §7.3 自检块里 `FOR cfg IN SELECT unnest(ARRAY[ … ])` 的括号少了一层
--       （psql 实测 `ERROR: mismatched parentheses at or near ";"`），
--       这里按标准嵌套逐行展开写，语义不变：3 条反例必须被拦住 + 1 条正例必须能入库。
-- ============================================================

-- ---------- ① 放开 assignee_type 枚举 ----------
-- 原约束来自 V0120 的列级 CHECK（PG 自动命名 biz_flow_node_assignee_type_check）
ALTER TABLE biz_flow_node DROP CONSTRAINT IF EXISTS biz_flow_node_assignee_type_check;

ALTER TABLE biz_flow_node
    ADD CONSTRAINT biz_flow_node_assignee_type_check
    CHECK (assignee_type IN ('USER','ROLE','INITIATOR','INITIATOR_SUPERIOR','DEPT_LEADER',
                             'TASK_OWNER','TASK_CREATOR','TASK_ASSIGNER','FORM_VAR','ADMIN',
                             'EXPRESSION'));

COMMENT ON COLUMN biz_flow_node.assignee_type IS
    '审批人来源：10 种静态来源 + EXPRESSION（动态表达式，规则树存 config.expression，见 EXPRESSION-DSL 规范）';

-- ---------- ② 表达式必须存在（库侧兜底，防「选了 EXPRESSION 却没规则」） ----------
-- 只判「有没有 / 条数是否在 1..16」，结构合法性由应用层 parse 保证（与 biz_flow_edge.condition_expr 同款分工）。
--
-- ⚠ 为什么必须写成 CASE，而不是 `A OR (B AND C)`：
--   ① PG 的 CHECK 在结果为 **NULL 时视为通过**（三值逻辑）—— `assignee_type='EXPRESSION'` 且 `config='{}'` 时
--      `jsonb_typeof(config->'expression')` 是 NULL，整条 CHECK 变 NULL ⇒ 脏数据直接放行（POC 实测：无规则的能入库）。
--   ② `AND` 不保证短路，`jsonb_array_length(非数组)` 会直接抛 `cannot get array length of a non-array`（变 500）。
--   CASE + `IS DISTINCT FROM` 同时解决这两点：逐支求值、每个分支都返回确定的 TRUE/FALSE。
ALTER TABLE biz_flow_node DROP CONSTRAINT IF EXISTS ck_biz_flow_node_expression;
ALTER TABLE biz_flow_node
    ADD CONSTRAINT ck_biz_flow_node_expression
    CHECK (
        assignee_type IS DISTINCT FROM 'EXPRESSION'
        OR CASE
               WHEN jsonb_typeof(config -> 'expression') IS DISTINCT FROM 'object' THEN FALSE
               WHEN jsonb_typeof(config -> 'expression' -> 'rules') IS DISTINCT FROM 'array' THEN FALSE
               WHEN jsonb_array_length(config -> 'expression' -> 'rules') NOT BETWEEN 1 AND 16 THEN FALSE
               ELSE TRUE
           END
    );

-- ---------- ③ 命中规则 id（审计：回答「为什么是他审我」） ----------
ALTER TABLE biz_flow_task ADD COLUMN IF NOT EXISTS assignee_rule_id VARCHAR(64);

COMMENT ON COLUMN biz_flow_task.assignee_rule_id IS
    '动态表达式命中的规则 id（如 r_big）；静态来源为 NULL。assignee_source 仍写底层来源（ROLE/USER/…），本列只补「命中了哪条规则」';

-- ---------- ④ 自检（空库也要能过：首次部署时可能一个流程定义都没有） ----------
DO $$
DECLARE
    has_kind BOOLEAN; has_expr BOOLEAN; has_rule BOOLEAN;
    v_id UUID; tmp_def UUID; cfg JSONB; blocked BOOLEAN;
BEGIN
    SELECT EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid = 'biz_flow_node'::regclass
                    AND conname = 'biz_flow_node_assignee_type_check'
                    AND pg_get_constraintdef(oid) LIKE '%EXPRESSION%') INTO has_kind;
    SELECT EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid = 'biz_flow_node'::regclass
                    AND conname = 'ck_biz_flow_node_expression') INTO has_expr;
    SELECT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_name = 'biz_flow_task'
                    AND column_name = 'assignee_rule_id') INTO has_rule;
    IF NOT has_kind THEN RAISE EXCEPTION 'V0136 自检失败：assignee_type 的 CHECK 未包含 EXPRESSION'; END IF;
    IF NOT has_expr THEN RAISE EXCEPTION 'V0136 自检失败：缺少 ck_biz_flow_node_expression'; END IF;
    IF NOT has_rule THEN RAISE EXCEPTION 'V0136 自检失败：biz_flow_task.assignee_rule_id 未创建'; END IF;

    -- 取一个可用的 version_id；空库就临时造一条（末尾删掉）
    SELECT id INTO v_id FROM biz_flow_def_version ORDER BY version_no LIMIT 1;
    IF v_id IS NULL THEN
        INSERT INTO biz_flow_def (flow_code, flow_name, biz_type)
        VALUES ('v0136_selftest', 'V0136 自检', 'TASK_ACCEPT') RETURNING id INTO tmp_def;
        INSERT INTO biz_flow_def_version (def_id, version_no, version_status, is_current)
        VALUES (tmp_def, 1, 'DRAFT', false) RETURNING id INTO v_id;
    END IF;

    -- 反例 ×3：无 expression / rules 为空 / rules 17 条 → 都必须被 CHECK 拦住
    FOR cfg IN SELECT unnest(ARRAY[
            '{}'::jsonb,
            '{"expression":{"version":1,"rules":[]}}'::jsonb,
            jsonb_build_object(
                'expression',
                jsonb_build_object(
                    'version', 1,
                    'rules', (
                        SELECT jsonb_agg(jsonb_build_object(
                                   'id', 'r' || g,
                                   'when', '{}'::jsonb,
                                   'then', '[{"type":"ADMIN"}]'::jsonb))
                          FROM generate_series(1, 17) AS g
                    )
                )
            )
        ])
    LOOP
        blocked := FALSE;
        BEGIN
            INSERT INTO biz_flow_node (version_id, node_key, node_name, node_type, assignee_type, config)
            VALUES (v_id, 'n_bad', '坏节点', 'APPROVAL', 'EXPRESSION', cfg);
        EXCEPTION WHEN check_violation THEN blocked := TRUE;
        END;
        IF NOT blocked THEN RAISE EXCEPTION 'V0136 自检失败：脏数据竟然能入库：%', left(cfg::text, 80); END IF;
    END LOOP;

    -- 正例：1 条规则必须能入库（否则就是拦过头），随后删掉
    INSERT INTO biz_flow_node (version_id, node_key, node_name, node_type, assignee_type, config)
    VALUES (v_id, 'n_ok', '自检正例', 'APPROVAL', 'EXPRESSION',
            '{"expression":{"version":1,"rules":[{"id":"r1","when":{"op":"cmp","field":"form.a","cmp":"eq","value":1},"then":[{"type":"ADMIN"}]}]}}'::jsonb);
    DELETE FROM biz_flow_node WHERE version_id = v_id AND node_key = 'n_ok';

    IF tmp_def IS NOT NULL THEN DELETE FROM biz_flow_def WHERE id = tmp_def; END IF;

    RAISE NOTICE 'V0136 自检通过：枚举放开 + 存在性/上限兜底 + assignee_rule_id + 3 条反例被拦 + 1 条正例可入库';
END $$;
