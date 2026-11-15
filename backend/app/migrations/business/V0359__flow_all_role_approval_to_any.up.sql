-- ============================================================================
-- V0359__flow_all_role_approval_to_any.up.sql
--
-- 【t_8733a7a0 / E2E07-FIX-1 · P2 系统性】「角色型审批」被配成全员会签（ALL）
-- ⇒ 角色里只要有一个成员不可用（停用 / 坏号 / 离职未摘角色），节点永远凑不齐票，
--    单据永久停在 PENDING_APPROVAL：没有超时兜底、没有告警、界面上也没有可点的按钮。
--
-- # 一、为什么会出现（V0339 的副作用，不是谁写错了图）
--   `biz_flow_node.approval_mode` 历史口径是「NULL = 全员签」。V0339（P3-06）把默认
--   改成「NULL = 或签」，但为了不静默降低审批强度，对**存量**按候选人数做了冻结：
--   候选人数 ≥ 2 ⇒ 写 `ALL`。于是所有「派给某个岗位角色」的普通单签节点
--   （`node_type = 'APPROVAL'` + `assignee_type = 'ROLE'`，角色当时有 ≥2 名成员）
--   全被冻成了 `ALL` —— 采购经理 / 部门主管 / 财务总监这些**岗位**变成了「委员会」。
--
--   145（cloudpivot_dev）实测（本卡扫描，8 个 PUBLISHED 流程共 13 个 ALL 节点）：
--     · APPROVAL + ROLE + ALL（**本迁移要改的错配**）7 个：
--         contract_approve.n_pm / n_pm_hi（PURCHASE_MANAGER，3 人）
--         po_approve.n_dept（DEPT_LEADER，3 人）/ n_pm（PURCHASE_MANAGER，3 人）
--         pr_approve.n_pm、prt_approve.n_pm（PURCHASE_MANAGER，3 人）
--         payable_pay.n_dept（DEPT_LEADER，3 人）
--     · COUNTERSIGN + ALL（**跨角色共识，本来就该全员签，不动**）6 个：
--         budget_approve.n_fin_gm / n_gm_fd、expense.n_board、payable_pay.n_board、
--         po_approve.n_board、voucher_reverse.n_cs
--       （这些是「财务经理 + 总经理」这种**不同角色各投一票**的会签，语义正确）
--
-- # 二、判据（为什么 APPROVAL + ROLE + ALL 一定是错配）
--   `node_type = 'APPROVAL'` 的业务语义是「这道关由**一个人**签字即可」；
--   要多人共识必须显式建成 `COUNTERSIGN`（建图期 R6/R17 与发布期 R23 已区分）。
--   所以：APPROVAL 节点上写 ALL = 把「岗位审批」误读成「全员决议」。
--   业务上「采购经理审批」= 采购经理岗位上**任一人**审，不是「三个采购经理都点头」。
--
--   E2E06-FIX-1（V0355）对预算 `n_dept` 已经按这个口径改成 ANY，本迁移是**同一口径的
--   全系统铺开**（按本卡 ① 的扫描清单逐条评估，不搞一刀切：COUNTERSIGN 一律不动）。
--
-- # 三、改法（已发布版本只读 ⇒ 归档旧版 → 发新版）
--   `flow/handler.rs` 红线 1：已发布版本不可改。本迁移走「归档 vN → 发 v(N+1)」：
--   vN 的行 / 快照 / graph_hash **逐字节不动**（只把 is_current 置 FALSE、
--   version_status 置 ARCHIVED），v(N+1) 是新行 + 新快照 + 新 hash。
--   新快照与 hash **不是手写的**：由旧快照在库内按 node_key 打补丁（只改
--   `approval_mode`），再用与 Rust `graph::snapshot_json()` / `graph_hash()` 同一套规则
--   （递归按键字典序 + 紧凑无空白 + sha256）现算。本文件自带 canonical 序列化函数，
--   并在每段自检里先证明 `canonical(旧快照) 的 sha256 == 旧 graph_hash`
--   （对得上 = 序列化口径与 Rust 一致 ⇒ 新 hash 可信；对不上直接跳过并点名，不抛异常）。
--
--   只改 `approval_mode`：
--     · `assignee_type` / `assignees`（谁审）**一个字不动** —— 审批人来源与部门归属
--       是另外两件事（部门维度错配见 V0355 与 E2E06-FIX-1 的处理），本卡只管「会不会卡死」；
--     · `timeout_hours` / `timeout_action` / `empty_assignee` / 边 / 阈值 全部原样。
--
-- # 四、幂等 / 跳过（重复执行 = 0 变更）
--   * 目标节点在当前版本里已经不是 `ALL`（之前已改 / 客户自己改过）⇒ 该流程 NOTICE 跳过；
--   * 流程不存在 / 已软删 ⇒ NOTICE 跳过；
--   * canonical(快照) 与 graph_hash 对不上（快照被直接改过）⇒ NOTICE 点名跳过，
--     **不抛异常**（抛异常会挡住整个升级；缺口留给人在流程编辑器里改完再发版）；
--   * 节点形状不是 APPROVAL + ROLE ⇒ 该节点跳过并说明当前形状。
--
-- # 五、回滚
--   `V0359__flow_all_role_approval_to_any.down.sql`：删掉本迁移发的版本、旧版复位为当前；
--   有审批单跑在新版上则**拒绝回滚**（删版本会留下指向空气的审批单），与 V0251 / V0355 同口径。
-- ============================================================================

-- ---------- 0) canonical JSON（与 graph.rs::write_canonical 逐字同义） ----------
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
        -- 排到字母后面（`allow_robot` vs `allowRobot` 会换序 → hash 漂移）。
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

-- ---------- 1) 目标清单：APPROVAL + ROLE + ALL 的节点（见文件头 §一 的评估） ----------
-- DROP IF EXISTS：本文件可能在同一事务里被跑两次（往返预演 / 重放证明），
-- ON COMMIT DROP 的临时表在事务内仍会存在，不先删会撞「already exists」。
DROP TABLE IF EXISTS _fix_target;
CREATE TEMP TABLE _fix_target (flow_code text NOT NULL, node_key text NOT NULL) ON COMMIT DROP;
INSERT INTO _fix_target (flow_code, node_key) VALUES
    ('contract_approve', 'n_pm'),
    ('contract_approve', 'n_pm_hi'),
    ('po_approve',       'n_dept'),
    ('po_approve',       'n_pm'),
    ('pr_approve',       'n_pm'),
    ('prt_approve',      'n_pm'),
    ('payable_pay',      'n_dept');

-- ---------- 2) 逐流程：归档旧版 → 发新版（只把目标节点的 approval_mode 改成 ANY） ----------
DO $$
DECLARE
    t           record;
    c_def       uuid;
    c_v1        uuid;
    c_v1_no     int;
    c_v1_hash   text;
    c_snap      jsonb;
    c_new_snap  jsonb;
    c_new_hash  text;
    c_v2        uuid;
    v_keys      text[];
    v_patched   int := 0;
    v_touched   int := 0;
    v_idx       int;
    v_node      jsonb;
    v_shape     text;
    n_nodes     int;
    n_edges     int;
    n_bind      int;
    n_pending   int;
    n_live      int;
BEGIN
    FOR t IN
        SELECT flow_code, array_agg(node_key ORDER BY node_key) AS keys
          FROM _fix_target GROUP BY flow_code ORDER BY flow_code
    LOOP
        v_keys    := t.keys;
        v_patched := 0;
        c_def := NULL; c_v1 := NULL; c_v1_no := NULL; c_v1_hash := NULL; c_snap := NULL;

        SELECT d.id, v.id, v.version_no, btrim(v.graph_hash), v.graph_snapshot
          INTO c_def, c_v1, c_v1_no, c_v1_hash, c_snap
          FROM biz_flow_def d
          JOIN biz_flow_def_version v ON v.id = d.current_version_id
         WHERE d.flow_code = t.flow_code AND d.deleted_at IS NULL;

        IF c_def IS NULL THEN
            RAISE NOTICE 'V0359 %：本库没有该流程（未接该业务域 / 已软删），跳过', t.flow_code;
            CONTINUE;
        END IF;

        -- 序列化口径校准 + 快照自洽性
        IF c_v1_hash IS NULL
           OR encode(sha256(convert_to(pg_temp.canonical_json(c_snap), 'UTF8')), 'hex') IS DISTINCT FROM c_v1_hash
        THEN
            RAISE NOTICE 'V0359 %：跳过 —— canonical(v% 快照)=% 与库里的 graph_hash=% 不一致（快照被直接改过 / 含本函数不支持的形状）。请在流程编辑器里另存草稿 → 把这些节点的签名模式改成「或签」→ 发布',
                t.flow_code, c_v1_no,
                coalesce(encode(sha256(convert_to(pg_temp.canonical_json(c_snap), 'UTF8')), 'hex'), '<null>'),
                coalesce(c_v1_hash, '<null>');
            CONTINUE;
        END IF;

        -- 打补丁（在内存里做，先判断有没有需要改的）
        c_new_snap := c_snap;
        FOREACH v_idx IN ARRAY (SELECT coalesce(array_agg(ord ORDER BY ord), '{}')
                                  FROM jsonb_array_elements(c_snap -> 'nodes') WITH ORDINALITY AS x(node, ord)
                                 WHERE (x.node ->> 'key') = ANY (v_keys))
        LOOP
            v_node  := c_snap -> 'nodes' -> (v_idx - 1);
            v_shape := coalesce(v_node ->> 'node_type', '<null>') || ' / '
                       || coalesce(v_node ->> 'assignee_type', '<null>') || ' / '
                       || coalesce(v_node ->> 'approval_mode', '<null>');
            IF (v_node ->> 'approval_mode') IS DISTINCT FROM 'ALL' THEN
                RAISE NOTICE 'V0359 %.%：当前 approval_mode = %（不是 ALL，已修正或客户改过），跳过该节点',
                    t.flow_code, v_node ->> 'key', coalesce(v_node ->> 'approval_mode', '<null>');
                CONTINUE;
            END IF;
            IF (v_node ->> 'node_type') IS DISTINCT FROM 'APPROVAL'
               OR (v_node ->> 'assignee_type') IS DISTINCT FROM 'ROLE'
            THEN
                -- 形状不符：多半是客户改过图（COUNTERSIGN 会签是正确配置，绝不能改）
                RAISE NOTICE 'V0359 %.%：形状是 %（不是 APPROVAL / ROLE），保持原样 —— 会签节点不动',
                    t.flow_code, v_node ->> 'key', v_shape;
                CONTINUE;
            END IF;
            c_new_snap := jsonb_set(c_new_snap, ARRAY['nodes', (v_idx - 1)::text],
                                    jsonb_set(v_node, '{approval_mode}', '"ANY"'::jsonb));
            v_patched := v_patched + 1;
        END LOOP;

        IF v_patched = 0 THEN
            RAISE NOTICE 'V0359 %：没有需要改的节点（幂等 / 已是或签），跳过', t.flow_code;
            CONTINUE;
        END IF;

        c_new_hash := encode(sha256(convert_to(pg_temp.canonical_json(c_new_snap), 'UTF8')), 'hex');
        c_v2       := gen_random_uuid();
        v_touched  := v_touched + v_patched;

        SELECT count(*) INTO n_nodes FROM biz_flow_node  WHERE version_id = c_v1;
        SELECT count(*) INTO n_edges FROM biz_flow_edge  WHERE version_id = c_v1;

        -- ① 旧版退位归档（行 / 快照 / hash 逐字节不动）
        --    顺序不能反：uk_biz_flow_def_version_current 保证一个流程只有一行 is_current
        UPDATE biz_flow_def_version
           SET is_current = FALSE, version_status = 'ARCHIVED', updated_at = now()
         WHERE id = c_v1;

        -- ② 新版版本行
        INSERT INTO biz_flow_def_version
            (id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
             node_count, edge_count, change_note, published_at, published_by, created_at, updated_at)
        VALUES
            (c_v2, c_def, c_v1_no + 1, 'PUBLISHED', TRUE, c_new_hash, c_new_snap,
             (SELECT count(*) FROM jsonb_array_elements(c_new_snap -> 'nodes')),
             (SELECT count(*) FROM jsonb_array_elements(c_new_snap -> 'edges')),
             'E2E07-FIX-1：' || v_patched || ' 个「角色型审批」节点由全员会签（ALL）改为或签（ANY）'
             || '（APPROVAL 节点的业务语义是「岗位任一人签字即可」；跨角色会签节点 COUNTERSIGN 不动）',
             now(), NULL, now(), now());

        -- ③ 节点行：整份复制再按 node_key 打补丁
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
           SET approval_mode = 'ANY'
         WHERE version_id = c_v2
           AND node_key = ANY (v_keys)
           AND node_type = 'APPROVAL'
           AND assignee_type = 'ROLE'
           AND approval_mode = 'ALL';

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

        -- ⑤ 审批人绑定：全部照抄（本迁移只改签名模式，不动审批人来源）
        INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order, created_at)
        SELECT n2.id, a.assignee_type, a.ref_id, a.sort_order, now()
          FROM biz_flow_node_assignee a
          JOIN biz_flow_node n1 ON n1.id = a.node_id
          JOIN biz_flow_node n2 ON n2.version_id = c_v2 AND n2.node_key = n1.node_key
         WHERE n1.version_id = c_v1;

        -- ⑥ 定义指向新版本
        UPDATE biz_flow_def
           SET current_version_id = c_v2, current_version_no = c_v1_no + 1, updated_at = now()
         WHERE id = c_def;

        -- ---------- 自检 ----------
        SELECT count(*) INTO n_bind FROM biz_flow_node_assignee a JOIN biz_flow_node n ON n.id = a.node_id
         WHERE n.version_id = c_v2;

        IF (SELECT count(*) FROM biz_flow_node WHERE version_id = c_v2) <> n_nodes THEN
            RAISE EXCEPTION 'V0359 %.% 自检失败：新版节点数 != 旧版（旧版 %）', t.flow_code, c_v1_no, n_nodes;
        END IF;
        IF (SELECT count(*) FROM biz_flow_edge WHERE version_id = c_v2) <> n_edges THEN
            RAISE EXCEPTION 'V0359 %.% 自检失败：新版边数 != 旧版（旧版 %）', t.flow_code, c_v1_no, n_edges;
        END IF;
        IF EXISTS (SELECT 1 FROM biz_flow_node
                    WHERE version_id = c_v2 AND node_key = ANY (v_keys)
                      AND node_type = 'APPROVAL' AND assignee_type = 'ROLE' AND approval_mode = 'ALL') THEN
            RAISE EXCEPTION 'V0359 %.% 自检失败：新版里仍有 APPROVAL + ROLE + ALL 的节点', t.flow_code, c_v1_no;
        END IF;
        -- 行与快照一致：新版 graph_hash 必须等于「库内重新 canonical 化的快照」的 sha256
        IF (SELECT encode(sha256(convert_to(pg_temp.canonical_json(graph_snapshot), 'UTF8')), 'hex')
              FROM biz_flow_def_version WHERE id = c_v2) IS DISTINCT FROM c_new_hash THEN
            RAISE EXCEPTION 'V0359 %.% 自检失败：新版 graph_snapshot 与 graph_hash 不一致', t.flow_code, c_v1_no;
        END IF;
        -- 同一流程只能有一个 is_current
        SELECT count(*) INTO n_pending FROM biz_flow_def_version WHERE def_id = c_def AND is_current;
        IF n_pending <> 1 THEN
            RAISE EXCEPTION 'V0359 % 自检失败：有 % 个 is_current 版本（必须 1）', t.flow_code, n_pending;
        END IF;
        -- 旧版一行没动
        IF (SELECT count(*) FROM biz_flow_node WHERE version_id = c_v1) <> n_nodes
           OR (SELECT btrim(graph_hash) FROM biz_flow_def_version WHERE id = c_v1) IS DISTINCT FROM c_v1_hash THEN
            RAISE EXCEPTION 'V0359 %.% 自检失败：旧版的行 / hash 被改动了（已发布版本必须只读）', t.flow_code, c_v1_no;
        END IF;
        -- 旧实例继续读自己的版本，不受影响（仅统计）
        SELECT count(*) INTO n_live FROM biz_flow_instance WHERE version_id = c_v1;
        RAISE NOTICE 'V0359 %：v% 已发布（改 % 个节点 / hash=% / 节点 % / 边 % / 绑定 % 行；历史实例跑在旧版上 % 张，继续可用）',
            t.flow_code, c_v1_no + 1, v_patched, c_new_hash, n_nodes, n_edges, n_bind, n_live;
    END LOOP;

    RAISE NOTICE 'V0359 完成：共修正 % 个「角色型审批被配成全员会签」节点', v_touched;
END $$;

-- ---------- 3) 收口自检：PUBLISHED 流程里不允许再出现 APPROVAL + ROLE + ALL ----------
DO $$
DECLARE
    n_bad int;
    v_list text;
BEGIN
    SELECT count(*), string_agg(d.flow_code || '.' || n.node_key, ', ' ORDER BY d.flow_code, n.node_key)
      INTO n_bad, v_list
      FROM biz_flow_def d
      JOIN biz_flow_def_version v ON v.def_id = d.id AND v.is_current
      JOIN biz_flow_node n ON n.version_id = v.id
     WHERE d.deleted_at IS NULL AND d.status = 'PUBLISHED'
       AND n.node_type = 'APPROVAL' AND n.assignee_type = 'ROLE' AND n.approval_mode = 'ALL';

    IF n_bad > 0 THEN
        -- 不抛异常：抛异常会挡住整个升级（与 V0355 同口径）。缺口留给人在流程编辑器里改完再发版，
        -- 之后 `scripts/verify/flow_countersign_invariants.sh` 会继续报警直到改掉。
        RAISE WARNING 'V0359 收口自检：PUBLISHED 流程里仍有 % 个 APPROVAL+ROLE+ALL 节点（%）。'
                      '若这些是本迁移「跳过」的（快照与 hash 不一致 / 客户改过图），请按 NOTICE 指引在流程编辑器里改成「或签」再发版',
                      n_bad, v_list;
    ELSE
        RAISE NOTICE 'V0359 收口自检通过：PUBLISHED 流程里 APPROVAL+ROLE+ALL = 0';
    END IF;
END $$;
