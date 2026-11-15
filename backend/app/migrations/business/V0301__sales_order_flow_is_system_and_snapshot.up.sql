-- ============================================================================
-- V0301__sales_order_flow_is_system_and_snapshot.up.sql
--
-- 【SO-23 衍生卡 t_1dadde81 · BLOCKER 收口】把 `so_approve`（销售订单审批（标准·金额分级））
-- 的两处缺陷一次修完。**必须一起修**：单独修 A 会把红挪到 pflow1a，单独修 B 没有意义
-- （B 只有在该流程被当成「标准流程」之后才会被遍历）。
--
-- # 缺陷 A：V0281 的种子漏写 `is_system`（erp_invariants H6 BLOCKER / H5 WARN）
--
--   `V0281__sales_order_flow.up.sql:88` 的种子语句列清单止于 `owner_dept_id`，没有
--   `is_system` ⇒ 吃 `BOOLEAN NOT NULL DEFAULT FALSE`（列由 V0193 加）。之后没有任何
--   补偿迁移。后果不是报错，而是**静默少一张卡**：`flow_model::templates()` 的默认口径
--   （`TemplateScope::Standard` → `AND d.is_system`）把它过滤掉，前端模板库看不到
--   「销售订单审批（标准·金额分级）」，客户也无从启用。
--
--   同错法已经犯过 5 次（V0209 / V0211 / V0228 / V0231 + 本条的 V0281），修法照 V0220 /
--   V0233：**新开一条迁移**按 flow_code 补标。已应用的迁移存内容指纹
--   （nik-db `migrate::history::checksum`），改一个字下次启动就 ChecksumMismatch 拒绝启动
--   （AGENTS.md 约定 6：已应用迁移不可改），所以历史文件是只读记录。
--
-- # 缺陷 B：v1 的 `graph_snapshot` 边序不是 canonical 顺序（存量版本不可重放）
--
--   把 A 修好后 `so_approve` 立刻落进 `backend/tests/pflow1a.rs::publish_regression_keeps_graph_hash`
--   的遍历（该用例取 `is_system=TRUE AND status='PUBLISHED'` 的存量流程，要求「另存草稿 →
--   重新发布」后 `graph_hash` 逐字节不变）。实测当场红：
--
--     so_approve 重新发布后 graph_hash 变了
--       da3c2eed…fa9a2（v1 存的） → 97fe9377…6e80（从行重算的）
--
--   根因**不是图改过**，是**序列化口径**：`FlowGraph::snapshot()` 要求
--   `nodes` 按 key、`edges` 按 (from,to,key) 升序，而 V0281 种下的快照边序是**按 key
--   字母序**（e_finance_mid 打头）。快照内容与行数据逐列一致（node 全等、edge 集合相等），
--   只是数组顺序不同 ⇒ `canonical_json` 出来的字节不同 ⇒ hash 不同 ⇒ 存量 PUBLISHED
--   版本的 hash 无法由当前序列化器重放（同时 `flow::maintain` 的每日自检会报 RowsEdited：
--   `docs/evidence/20261106-so08-shipment/full-suite-attribution.md` 等文档里那条常红就是这个）。
--
--   本迁移用**应用的 canonical 序列化结果**重写 v1 的 `graph_snapshot` + `graph_hash`
--   （文本取自发布路径 `FlowGraph::snapshot_json()` 的产物，不是手抄实测 hash），
--   使存量版本可重放。没有改任何业务语义：节点行、边行一个字节都没动。
--
-- # 不做的事
--
--   · 不改 V0281（指纹校验）；不改节点/边行（图内容本来就对）；
--   · 不动 `status` / `current_version_id` / 版本号：这不是流程改造，是把「已发布的那一份」
--     按应用自己的口径重新序列化一次。
--   · 不按名字模糊匹配打标（客户另存为的副本可以自己起名，按名字改等于越权）。
--
-- # 守卫（别覆盖别人的配置）
--
--   两处写操作都带精确判据：打标只认 `flow_code = 'so_approve'` **且** 种子 id；改图只认
--   `graph_hash` 仍是 V0281 种下的 `da3c2eed…`（客户若已重新发布 / 手工改过图，整段跳过）。
--
-- # 号段
--
--   `backend/migrations/business/` 本工作仓最大 V0297；**bare（origin/main）在 DEPLOY-SYNC
--   t_c3087afe 合仓时把发票号段顺延到 V0298–V0300**，因此取 **V0301**（跨仓全局唯一，
--   两个仓都不会撞号）。动手前已实测：`git ls-tree -r origin/main backend/migrations/`
--   的 V 号最大 = V0300，本仓无 V0298+。
--
-- 回滚：`V0301__sales_order_flow_is_system_and_snapshot.down.sql`
-- ============================================================================

DO $$
DECLARE
    -- `so_approve` 的种子 id（V0281 种下的那一条）。用 id 而不是只按 flow_code：
    -- 若某客户的库里 V0281 的种子因角色 UUID 不匹配被整条跳过、又自建了同名 flow_code，
    -- 只按 flow_code 会把人家的流程标成系统预置（越权且翻不回来）。
    c_def        CONSTANT uuid := '20000000-0000-4000-8000-0000000d0281';
    c_ver        CONSTANT uuid := '20000000-0000-4000-8000-0000000d0282';
    -- V0281 种下时的 graph_hash（边按 key 字母序的旧序列化产物）
    c_seed_hash  CONSTANT text := 'da3c2eeda2593b0e3a0b362dd31ec2c79505ba3efe0053028896d05e5b2fa9a2';
    -- 按当前 canonical 序列化（边按 (from,to,key) 升序）重算的 graph_hash
    c_fixed_hash CONSTANT text := '97fe937752c881c0c9b36f7aa8e2a410c2891a4327711fde96fcbf16a0bc6e80';
    -- 同一张图的 canonical 快照文本（`FlowGraph::snapshot()` → `snapshot_json()` 的产物）
    c_snapshot   CONSTANT jsonb := $snapshot${"edges":[{"condition":{"cmp":"lt","field":"form.amount","op":"cmp","value":10000},"from":"c_low","is_default":false,"key":"e_low","label":"金额 < 10000","priority":0,"to":"e"},{"condition":null,"from":"c_low","is_default":true,"key":"e_low_else","label":"金额 ≥ 10000","priority":10,"to":"n_finance"},{"condition":{"cmp":"lt","field":"form.amount","op":"cmp","value":100000},"from":"c_mid","is_default":false,"key":"e_mid","label":"10000 ≤ 金额 < 100000","priority":0,"to":"e"},{"condition":null,"from":"c_mid","is_default":true,"key":"e_mid_else","label":"金额 ≥ 100000","priority":10,"to":"n_gm"},{"condition":null,"from":"n_finance","is_default":false,"key":"e_finance_mid","label":null,"priority":0,"to":"c_mid"},{"condition":null,"from":"n_gm","is_default":false,"key":"e_gm_end","label":null,"priority":0,"to":"e"},{"condition":null,"from":"n_sales","is_default":false,"key":"e_sales_low","label":null,"priority":0,"to":"c_low"},{"condition":null,"from":"s","is_default":false,"key":"e_start","label":null,"priority":0,"to":"n_sales"}],"nodes":[{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"c_low","max_remind_times":3,"name":"金额分级①（< 1 万）","node_type":"CONDITION","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":3,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":420,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"c_mid","max_remind_times":3,"name":"金额分级②（< 10 万）","node_type":"CONDITION","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":5,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":860,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"OR","key":"e","max_remind_times":3,"name":"结束","node_type":"END","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":7,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":1300,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0005","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_finance","max_remind_times":2,"name":"财务经理审批","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":4,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":640,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0003","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_gm","max_remind_times":2,"name":"总经理审批","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":6,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":48,"x":1080,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0007","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"FALLBACK_ADMIN","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_sales","max_remind_times":2,"name":"销售主管审批","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"ALLOW","sort_order":2,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":220,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"s","max_remind_times":3,"name":"开始","node_type":"START","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":1,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":0,"y":0}],"schema_version":1}$snapshot$::jsonb;

    n_flag    bigint;
    n_snap    bigint;
    n_bad     bigint;
    v_snap    jsonb;
    v_nodes   int;
    v_edges   int;
BEGIN
    -- ① 打标：补偿 V0281 种子漏写的 `is_system`（幂等：已是 TRUE 的行不参与）
    UPDATE biz_flow_def
       SET is_system  = TRUE,
           updated_at = now()
     WHERE flow_code = 'so_approve'
       AND id = c_def
       AND deleted_at IS NULL
       AND NOT is_system;
    GET DIAGNOSTICS n_flag = ROW_COUNT;

    -- ② 快照按应用的 canonical 序列化重排（边按 from,to,key 升序），hash 同步重算。
    --    守卫：只有「仍是 V0281 种下那一份」才改；客户重新发布 / 手工改过图就跳过。
    UPDATE biz_flow_def_version
       SET graph_snapshot = c_snapshot,
           graph_hash     = c_fixed_hash,
           updated_at     = now()
     WHERE id = c_ver
       AND def_id = c_def
       AND version_no = 1
       AND btrim(graph_hash) = c_seed_hash;
    GET DIAGNOSTICS n_snap = ROW_COUNT;

    -- ③ 自检一（fail-closed）：旧 hash 不许还留着 —— 那就是「该改没改成」
    IF EXISTS (
        SELECT 1 FROM biz_flow_def_version
         WHERE id = c_ver AND btrim(graph_hash) = c_seed_hash
    ) THEN
        RAISE EXCEPTION
            'V0301 自检失败：so_approve v1 仍是种子快照 hash（%），canonical 重排没有生效',
            c_seed_hash;
    END IF;

    -- ④ 自检二：定义行（存在的话）必须已经是系统预置
    IF EXISTS (
        SELECT 1 FROM biz_flow_def
         WHERE id = c_def AND deleted_at IS NULL AND NOT is_system
    ) THEN
        RAISE EXCEPTION 'V0301 自检失败：so_approve 仍是 is_system=FALSE（打标没生效）';
    END IF;

    -- ⑤ 自检三：**本迁移写下的那一份**必须「可重放」——
    --    节点按 key 升序、边按 (from,to,key) 升序，且计数与 node_count / edge_count 一致。
    --    这三条正是 `FlowGraph::snapshot()` 的确定性前提；顺序一歪，hash 就不可由行重算。
    --    判据里带 hash：客户已重新发布 / 手工改图（hash 既不是种子也不是本迁移写的）时
    --    整段跳过 —— 不越权校验别人的图，也不拿它当失败理由卡住启动。
    IF EXISTS (
        SELECT 1 FROM biz_flow_def_version WHERE id = c_ver AND btrim(graph_hash) = c_fixed_hash
    ) THEN
        SELECT v.graph_snapshot, v.node_count, v.edge_count
          INTO v_snap, v_nodes, v_edges
          FROM biz_flow_def_version v
         WHERE v.id = c_ver;

        IF coalesce((v_snap ->> 'schema_version')::int, 0) <> 1 THEN
            RAISE EXCEPTION 'V0301 自检失败：快照 schema_version 不是 1（%）',
                coalesce(v_snap ->> 'schema_version', 'null');
        END IF;

        SELECT count(*) INTO n_bad
          FROM (
            SELECT ord,
                   row_number() OVER (
                       ORDER BY (item ->> 'from'), (item ->> 'to'), (item ->> 'key')
                   ) AS expected_ord
              FROM jsonb_array_elements(v_snap -> 'edges') WITH ORDINALITY AS t(item, ord)
          ) s
         WHERE ord <> expected_ord;
        IF n_bad > 0 THEN
            RAISE EXCEPTION
                'V0301 自检失败：快照 edges 不是 canonical 顺序（% 条位置不对，应按 from,to,key 升序）',
                n_bad;
        END IF;

        SELECT count(*) INTO n_bad
          FROM (
            SELECT ord,
                   row_number() OVER (ORDER BY (item ->> 'key')) AS expected_ord
              FROM jsonb_array_elements(v_snap -> 'nodes') WITH ORDINALITY AS t(item, ord)
          ) s
         WHERE ord <> expected_ord;
        IF n_bad > 0 THEN
            RAISE EXCEPTION 'V0301 自检失败：快照 nodes 不是按 key 升序（% 条位置不对）', n_bad;
        END IF;

        IF jsonb_array_length(v_snap -> 'nodes') <> v_nodes
           OR jsonb_array_length(v_snap -> 'edges') <> v_edges THEN
            RAISE EXCEPTION
                'V0301 自检失败：快照节点/边数（%/%）与 node_count/edge_count（%/%）不一致',
                jsonb_array_length(v_snap -> 'nodes'), jsonb_array_length(v_snap -> 'edges'),
                v_nodes, v_edges;
        END IF;
    END IF;

    -- ⑥ 自检四（库内兜底，与 erp_invariants H5 同口径）：全库不许再有
    --    「名字自称『标准』却 is_system=FALSE」的行 —— 下一个漏写 is_system 的种子
    --    会在它之后的下一次启动里炸（迁移失败 → 启动失败），而不是等用户发现少卡。
    SELECT count(*) INTO n_bad
      FROM biz_flow_def
     WHERE deleted_at IS NULL
       AND NOT is_system
       AND flow_name LIKE '%标准%';
    IF n_bad > 0 THEN
        RAISE EXCEPTION
            'V0301 自检失败：仍有 % 条流程名字含「标准」却 is_system=FALSE —— '
            '几乎可以断定是某个种子迁移漏写了 is_system，请新开一条迁移按 flow_code 补标',
            n_bad;
    END IF;

    RAISE NOTICE 'V0301：so_approve 打标 % 行，快照 canonical 重排 % 行（% → %）',
        n_flag, n_snap, left(c_seed_hash, 8), left(c_fixed_hash, 8);
END $$;
