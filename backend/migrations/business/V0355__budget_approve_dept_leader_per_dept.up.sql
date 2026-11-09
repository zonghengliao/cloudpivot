-- ============================================================================
-- V0355__budget_approve_dept_leader_per_dept.up.sql
--
-- 【t_1fafa2ae / E2E06-FIX-1 · 问题 2】预算审批的「部门主管」节点是**全局角色会签**：
-- 任何部门的预算都要全部 DEPT_LEADER 会签，且角色里有一个坏号就永远批不完。
--
-- # 一、现象（145 / cloudpivot_dev，E2E-06 演练实测）
--   `budget_approve` v1 的 `n_dept`：assignee_type=ROLE、角色 DEPT_LEADER、approval_mode=ALL。
--   ① 部门维度错配：A 部门编的预算，B 部门主管也得各投一票（谁都能否掉别人部门的预算）；
--   ② 永久死锁：会签要求**全员**签 —— 角色成员里只要有一个不可用（停用 / 密码坏号 /
--      离职未摘角色），节点永远凑不齐票，预算停 SUBMITTED、额度永不生效，
--      整套预算控制（占用 / 超支拦截）跟着失效。
--      实测样本：共享库 p3a_0002 的 `password` 是 1 字节占位符 `'x'`（登录 500），
--      它被种在全局 DEPT_LEADER 里 → E2E-06 只能临时重置其哈希才把审批链推通。
--
-- # 二、改法（只改 `n_dept`，其余节点 / 边 / 阈值一个字不动）
--   v1 : n_dept = APPROVAL / ROLE[DEPT_LEADER] / approval_mode=ALL（会签，候选=全体主管）
--   v2 : n_dept = APPROVAL / INITIATOR_SUPERIOR(superior_levels=1) / approval_mode=ANY
--        → 候选 = **发起人所在部门的 leader_id**（一人），一人同意即通过。
--
--   ## 为什么是 INITIATOR_SUPERIOR 而不是 DEPT_LEADER
--   `DEPT_LEADER` 的语义是「`biz_flow_node_assignee` 里**指定部门**的 leader_id」
--   （assignee.rs 顶部来源表），部门是**发布期写死**的 —— 一张流程接所有部门，
--   写死部门就等于「A 部门的预算永远派 A 的负责人」，还是错配。
--   而 R14（`graph::check_assignees`）要求 `DEPT_LEADER` 是**可枚举来源**，
--   必须落部门行；空行会被发布校验拒掉。所以「随发起人变化的部门」只能由
--   `INITIATOR_SUPERIOR` 表达：`superior_levels=1` = 沿 `dept_path` 取**发起人本级**
--   部门的 leader_id（`assignee.rs::load_superior`，index = levels-1 = 0 = 本级）。
--   同一口径已被 `expense_approve.n_sup`（V0243「直属上级审批」）用过，不是新机制。
--
--   ## 为什么要 ANY
--   单候选（一个部门一个负责人）时 ANY / ALL 判定等价，写 ANY 是为了：
--   ① `empty_assignee=FALLBACK_ADMIN` 兜出多个超管时（发起人部门没设负责人 /
--      部门负责人被内控排除）**一人同意即可**，不会重演「超管会签死锁」；
--   ② 节点上显式写着「或签」，运维看图不会误以为是会签（V0339 之后
--      APPROVAL 的 NULL 语义是 ANY，但字面 NULL 读起来两可）。
--
--   ## 没改的东西（重要）
--   * `empty_assignee` 仍是 FALLBACK_ADMIN（部门没负责人 → 退回超管，防卡死）；
--   * `self_approval` 仍是 ALLOW（真正的否决在 `budget_bridge::guard_self_approval` 403+65019，
--     和「制单人不可自审」的 `flow::internal_control` 落 SKIPPED 留痕，两处都没动）；
--   * n_fin / n_fin_adj / n_fin_gm / n_gm / n_gm_fd / 条件分流 / 50 万阈值 全部原样。
--
-- # 三、怎么改的（已发布版本只读）
--   `flow/handler.rs` 红线 1：已发布版本不可改。本迁移走「归档 v1 → 发 v2」：
--   v1 的行 / 快照 / graph_hash **逐字节不动**（只把 is_current 置 FALSE、
--   version_status 置 ARCHIVED），v2 是一份新行 + 新快照 + 新 hash。
--   v2 的快照与 hash **不是手写的**：由 v1 的 `graph_snapshot` 在库内打补丁
--   （只替换 n_dept 那一个节点）后，用与 Rust `graph::snapshot_json()` /
--   `graph_hash()` **同一套规则**（递归按键字典序 + 紧凑无空白 + sha256）现算。
--   本文件自带 canonical 序列化函数，并在自检里先证明：
--      canonical(v1 快照) 的 sha256 == v1 的 graph_hash
--   （对得上 = 序列化口径与 Rust 一致，v2 的 hash 可信；对不上直接炸迁移）。
--
-- # 四、幂等 / 跳过
--   * 该流程已有 version_no ≥ 2 → 整段跳过（客户自己发过新版，绝不覆盖）；
--   * 没有 n_dept 节点 / n_dept 不是 ROLE[DEPT_LEADER] → NOTICE 跳过（客户改过图）；
--   * 没有 budget_approve 流程 → NOTICE 跳过（纯净业务库未接预算域）。
--   重复执行 = 0 变更。
--
-- # 五、回滚
--   `V0355__budget_approve_dept_leader_per_dept.down.sql`：删 v2、v1 复位为当前版本；
--   有审批单跑在 v2 上则**拒绝回滚**（删版本会留下指向空气的审批单），与 V0251 同口径。
-- ============================================================================

-- ---------- 0) canonical JSON（与 graph.rs::write_canonical 逐字同义） ----------
-- 存在的唯一理由：v2 的 graph_hash 必须在库内现算，否则客户环境的 v1 快照与我仓种子
-- 不一致时只能跳过（那是 V0251 的取舍）。有了它，任何 v1 形状都能安全升级。
CREATE OR REPLACE FUNCTION pg_temp.canonical_json(v jsonb) RETURNS text
LANGUAGE plpgsql IMMUTABLE AS $fn$
DECLARE
    out   text;
    k     text;
    item  jsonb;
    first boolean := true;
BEGIN
    CASE jsonb_typeof(v)
    WHEN 'object' THEN
        out := '{';
        -- COLLATE "C"：Rust 的 String Ord 是字节序；库默认 collation 会把 '_' 当弱标点
        -- 排到字母后面（`allow_robot` vs `allowRobot` 这种键会换序 → hash 漂移）。
        FOR k, item IN SELECT e.key, e.value FROM jsonb_each(v) e ORDER BY e.key COLLATE "C" LOOP
            IF NOT first THEN out := out || ','; END IF;
            first := false;
            out := out || to_jsonb(k)::text || ':' || pg_temp.canonical_json(item);
        END LOOP;
        RETURN out || '}';
    WHEN 'array' THEN
        out := '[';
        FOR item IN SELECT a.value FROM jsonb_array_elements(v) a LOOP
            IF NOT first THEN out := out || ','; END IF;
            first := false;
            out := out || pg_temp.canonical_json(item);
        END LOOP;
        RETURN out || ']';
    ELSE
        -- 标量：jsonb 的 text 形式与 serde_json 一致（数字 / 布尔 / null / 字符串转义）
        RETURN v::text;
    END CASE;
END $fn$;

-- ---------- 1) budget_approve：n_dept 改成「发起人部门主管 + 或签」----------
DO $$
DECLARE
    c_dept_role CONSTANT uuid := '20000000-0000-4000-8000-0000000f0001';   -- DEPT_LEADER
    c_v2 CONSTANT uuid := '20000000-0000-4000-8000-0000000c022b';
    c_def       uuid;
    c_v1        uuid;
    c_v1_no     int;
    c_v1_hash   text;
    c_snap      jsonb;
    c_node      jsonb;
    c_node_idx  int;
    c_v2_snap   jsonb;
    c_v2_hash   text;
    n_nodes     int;
    n_edges     int;
    n_bind      int;
    n_pending   int;
    n_live      int;
BEGIN
    SELECT d.id, v.id, v.version_no, btrim(v.graph_hash), v.graph_snapshot
      INTO c_def, c_v1, c_v1_no, c_v1_hash, c_snap
      FROM biz_flow_def d
      JOIN biz_flow_def_version v ON v.id = d.current_version_id
     WHERE d.flow_code = 'budget_approve' AND d.deleted_at IS NULL;

    IF c_def IS NULL THEN
        RAISE NOTICE 'V0355 budget_approve：本库没有该流程（未接预算域 / 已软删），跳过';
        RETURN;
    END IF;

    -- 幂等：客户已发过 v2+ → 整段不动
    IF EXISTS (SELECT 1 FROM biz_flow_def_version WHERE def_id = c_def AND version_no >= 2) THEN
        RAISE NOTICE 'V0355 budget_approve：已有 version_no ≥ 2，跳过（绝不覆盖客户发布过的图）';
        RETURN;
    END IF;

    -- n_dept 定位（节点数组按 key 排序，取下标）
    SELECT t.ord INTO c_node_idx
      FROM jsonb_array_elements(c_snap -> 'nodes') WITH ORDINALITY AS t(node, ord)
     WHERE t.node ->> 'key' = 'n_dept';
    IF c_node_idx IS NULL THEN
        RAISE NOTICE 'V0355 budget_approve：图里没有 n_dept 节点（客户改过图），跳过 —— 请在流程编辑器里「另存草稿 → 改节点 → 发布」';
        RETURN;
    END IF;
    c_node := c_snap -> 'nodes' -> (c_node_idx - 1);

    -- 只改「全局角色会签」这一种形状；别的形状（已是部门主管 / 已改过）不动
    IF (c_node ->> 'node_type') IS DISTINCT FROM 'APPROVAL'
       OR (c_node ->> 'assignee_type') IS DISTINCT FROM 'ROLE'
       OR NOT EXISTS (
            SELECT 1
              FROM jsonb_array_elements(coalesce(c_node -> 'assignees', '[]'::jsonb)) AS a(item)
             WHERE (a.item ->> 'ref_id') = c_dept_role::text
          )
    THEN
        RAISE NOTICE 'V0355 budget_approve：n_dept 当前形状是 % / %（不是 ROLE[DEPT_LEADER]），跳过',
            coalesce(c_node ->> 'node_type', '<null>'), coalesce(c_node ->> 'assignee_type', '<null>');
        RETURN;
    END IF;

    -- 序列化口径自检：canonical(v1 快照) 的 sha256 必须等于 v1 的 graph_hash
    IF encode(sha256(convert_to(pg_temp.canonical_json(c_snap), 'UTF8')), 'hex') IS DISTINCT FROM c_v1_hash THEN
        RAISE EXCEPTION 'V0355 budget_approve 自检失败：canonical 序列化口径与 Rust graph_hash 不一致（v1 hash=%，实算=%）',
            c_v1_hash, encode(sha256(convert_to(pg_temp.canonical_json(c_snap), 'UTF8')), 'hex');
    END IF;

    -- 打补丁：只替换 n_dept 这个节点
    c_node := jsonb_set(c_node, '{assignee_type}',    '"INITIATOR_SUPERIOR"'::jsonb);
    c_node := jsonb_set(c_node, '{approval_mode}',    '"ANY"'::jsonb);
    c_node := jsonb_set(c_node, '{assignees}',        '[]'::jsonb);
    c_node := jsonb_set(c_node, '{superior_levels}',  '1'::jsonb);
    c_v2_snap := jsonb_set(c_snap, ARRAY['nodes', (c_node_idx - 1)::text], c_node);
    c_v2_hash := encode(sha256(convert_to(pg_temp.canonical_json(c_v2_snap), 'UTF8')), 'hex');

    SELECT count(*) INTO n_nodes FROM biz_flow_node WHERE version_id = c_v1;
    SELECT count(*) INTO n_edges FROM biz_flow_edge WHERE version_id = c_v1;

    -- ① v1 退位归档（行 / 快照 / hash 逐字节不动）
    --    顺序不能反：uk_biz_flow_def_version_current 保证一个流程只有一行 is_current
    UPDATE biz_flow_def_version
       SET is_current = FALSE, version_status = 'ARCHIVED', updated_at = now()
     WHERE id = c_v1;

    -- ② v2 版本行
    INSERT INTO biz_flow_def_version
        (id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
         node_count, edge_count, change_note, published_at, published_by, created_at, updated_at)
    VALUES
        (c_v2, c_def, 2, 'PUBLISHED', TRUE, c_v2_hash, c_v2_snap,
         (SELECT count(*) FROM jsonb_array_elements(c_v2_snap -> 'nodes')),
         (SELECT count(*) FROM jsonb_array_elements(c_v2_snap -> 'edges')),
         'E2E06-FIX-1：部门主管节点由「全局 DEPT_LEADER 全员会签」改为「发起人所在部门负责人 + 或签」（部门维度对齐、一人同意即通过；其余节点 / 边 / 阈值不动）',
         now(), NULL, now(), now());

    -- ③ 节点：整份复制再按 node_key 打补丁
    INSERT INTO biz_flow_node
        (id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
         form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
         allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
         max_remind_times, cc_on_approved, position_x, position_y, sort_order, config, created_at)
    SELECT gen_random_uuid(), c_v2, node_key, node_name, node_type, assignee_type, superior_levels,
           form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
           allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
           max_remind_times, cc_on_approved, position_x, position_y, sort_order, config, now()
      FROM biz_flow_node WHERE version_id = c_v1;

    UPDATE biz_flow_node
       SET assignee_type = 'INITIATOR_SUPERIOR', approval_mode = 'ANY', superior_levels = 1
     WHERE version_id = c_v2 AND node_key = 'n_dept';

    -- ④ 边：按 node_key 重指到新节点（条件表达式 / 优先级 / 兜底标记原样）
    INSERT INTO biz_flow_edge
        (id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
         condition_label, priority, is_default, created_at)
    SELECT gen_random_uuid(), c_v2, e.edge_key, nf2.id, nt2.id,
           CASE WHEN e.condition_expr = 'null'::jsonb THEN NULL ELSE e.condition_expr END,
           e.condition_label, e.priority, e.is_default, now()
      FROM biz_flow_edge e
      JOIN biz_flow_node nf ON nf.id = e.from_node_id
      JOIN biz_flow_node nt ON nt.id = e.to_node_id
      JOIN biz_flow_node nf2 ON nf2.version_id = c_v2 AND nf2.node_key = nf.node_key
      JOIN biz_flow_node nt2 ON nt2.version_id = c_v2 AND nt2.node_key = nt.node_key
     WHERE e.version_id = c_v1;

    -- ⑤ 审批人绑定：除 n_dept（改为运行时按发起人部门解析）外全部照抄
    INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order, created_at)
    SELECT n2.id, a.assignee_type, a.ref_id, a.sort_order, now()
      FROM biz_flow_node_assignee a
      JOIN biz_flow_node n1 ON n1.id = a.node_id
      JOIN biz_flow_node n2 ON n2.version_id = c_v2 AND n2.node_key = n1.node_key
     WHERE n1.version_id = c_v1
       AND n1.node_key <> 'n_dept';

    -- ⑥ 定义指向新版本
    UPDATE biz_flow_def
       SET current_version_id = c_v2, current_version_no = 2, updated_at = now()
     WHERE id = c_def;

    -- ---------- 自检 ----------
    SELECT count(*) INTO n_bind FROM biz_flow_node_assignee a JOIN biz_flow_node n ON n.id = a.node_id
     WHERE n.version_id = c_v2;

    IF (SELECT count(*) FROM biz_flow_node WHERE version_id = c_v2) <> n_nodes THEN
        RAISE EXCEPTION 'V0355 自检失败：v2 节点数 != v1（v1=%）', n_nodes;
    END IF;
    IF (SELECT count(*) FROM biz_flow_edge WHERE version_id = c_v2) <> n_edges THEN
        RAISE EXCEPTION 'V0355 自检失败：v2 边数 != v1（v1=%）', n_edges;
    END IF;
    IF (SELECT count(*) FROM biz_flow_node
         WHERE version_id = c_v2 AND node_key = 'n_dept'
           AND assignee_type = 'INITIATOR_SUPERIOR' AND approval_mode = 'ANY' AND superior_levels = 1) <> 1 THEN
        RAISE EXCEPTION 'V0355 自检失败：v2 的 n_dept 没改成 INITIATOR_SUPERIOR / ANY';
    END IF;
    IF EXISTS (SELECT 1 FROM biz_flow_node_assignee a JOIN biz_flow_node n ON n.id = a.node_id
                WHERE n.version_id = c_v2 AND n.node_key = 'n_dept') THEN
        RAISE EXCEPTION 'V0355 自检失败：v2 的 n_dept 还挂着部门行（应改为运行时解析）';
    END IF;
    -- 行与快照一致：v2 的 graph_hash 必须等于「库内重新 canonical 化的快照」的 sha256
    IF (SELECT encode(sha256(convert_to(pg_temp.canonical_json(graph_snapshot), 'UTF8')), 'hex')
          FROM biz_flow_def_version WHERE id = c_v2) IS DISTINCT FROM c_v2_hash THEN
        RAISE EXCEPTION 'V0355 自检失败：v2 的 graph_snapshot 与 graph_hash 不一致';
    END IF;
    -- 同一流程只能有一个 is_current
    SELECT count(*) INTO n_pending FROM biz_flow_def_version WHERE def_id = c_def AND is_current;
    IF n_pending <> 1 THEN
        RAISE EXCEPTION 'V0355 自检失败：budget_approve 有 % 个 is_current 版本（必须 1）', n_pending;
    END IF;
    -- v1 一行没动
    IF (SELECT count(*) FROM biz_flow_node WHERE version_id = c_v1) <> n_nodes
       OR (SELECT btrim(graph_hash) FROM biz_flow_def_version WHERE id = c_v1) IS DISTINCT FROM c_v1_hash THEN
        RAISE EXCEPTION 'V0355 自检失败：v1 的行 / hash 被改动了（已发布版本必须只读）';
    END IF;
    -- 有实例挂在 v1 上不影响（v1 归档后旧实例继续读自己的版本），这里只做统计
    SELECT count(*) INTO n_live FROM biz_flow_instance WHERE version_id = c_v1;
    RAISE NOTICE 'V0355 budget_approve：v2 已发布（hash=% / 节点 % / 边 % / 审批人绑定 % 行；历史实例跑在 v1 上 % 张，继续可用）',
        c_v2_hash, n_nodes, n_edges, n_bind, n_live;
END $$;
