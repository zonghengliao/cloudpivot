-- ============================================================================
-- V0367__flow_all_role_approval_to_any_multiversion.up.sql
--
-- 【t_626aaa8a / V0359-FIX · P0 生产隐患】「角色型审批」被配成全员会签（ALL）
--   —— 多版本流程支持版（V0359 的修正迁移，**不改 V0359**）
--
-- # 一、为什么需要这个迁移（2026-11-09 生产部署失败复盘）
--
--   V0359（E2E07-FIX-1）的口径是对的：`APPROVAL + ROLE + ALL` = 把「岗位任一人签字」
--   误配成「岗位上所有人都点头」⇒ 角色里只要有人停用/离职未摘角色，单据永久卡
--   PENDING_APPROVAL，没有兜底、没有待办、界面无按钮可点。
--
--   但 V0359 的实现假设「目标流程只有 v1、当前版本就是最高版本」：
--       INSERT ... VALUES (c_v2, c_def, c_v1_no + 1, ...)   -- c_v1_no = 当前版本号
--   生产（144 / cloudpivot）实际是**多版本**：
--       po_approve: v1 ARCHIVED / v2 PUBLISHED / v3 DRAFT / v4 DRAFT
--   于是 V0359 对 po_approve 想发 v3 → 撞已存在的 DRAFT v3 →
--       ERROR: duplicate key value violates unique constraint "uk_biz_flow_def_version"
--       DETAIL: Key (def_id, version_no)=(…,3) already exists.
--   整个迁移事务回滚 ⇒ contract_approve v2 / payable_pay v3 白改 ⇒ 服务起不来
--   （FATAL: 服务启动失败: 配置错误: 数据库迁移失败）。应急处理是把 V0359 手工记为已应用
--   （schema_migrations.source = 'manual-skip-deploy-incident'），但它**没真跑**，
--   所以生产上「岗位审批仍是全员会签」的隐患还在。
--
--   本迁移（V0367）= V0359 的修正版，语义不变、目标清单不变，只把「发版」改成
--   **多版本安全**的写法：
--       SELECT COALESCE(MAX(version_no), 0) + 1 INTO v_next_no
--         FROM biz_flow_def_version WHERE def_id = c_def;
--   po_approve 因此发 v5，不撞 v3/v4 的 DRAFT。
--
-- # 二、目标清单：从「硬编码 7 个节点」改为「按形状动态扫描」
--
--   V0359 用 `_fix_target` 硬编码了 7 个 (flow_code, node_key)。但既然生产上 V0359
--   一次都没跑成，扫描口径就应该按**判据**（形状）来，而不是按当时的清单：
--
--       当前版本（is_current = TRUE AND version_status = 'PUBLISHED'）
--       里仍有节点满足 node_type = 'APPROVAL' AND assignee_type = 'ROLE'
--                              AND approval_mode = 'ALL'
--
--   12 个流程 / 17 个节点（144 备份副本实测）：asset_disposal.n_gm、
--   budget_approve.n_gm、contract_approve.n_gm/n_pm/n_pm_hi、payable_pay.n_dept、
--   po_approve.n_dept/n_pm、pr_approve.n_pm、prt_approve.n_pm、so_approve.n_gm、
--   stock_in_cancel.n_review、stock_in_post.n_wh、stock_take_post.n_fin/n_gm/n_wh、
--   stock_transfer.n_approve。
--   （V0359 清单只覆盖其中 5 个流程 —— 因为 V0359 起草时只扫了 145 dev 的 8 个流程。）
--
--   `COUNTERSIGN + ROLE + ALL`（6 个：budget_approve.n_fin_gm/n_gm_fd、expense.n_board、
--   payable_pay.n_board、po_approve.n_board、voucher_reverse.n_cs）**一律不动**：
--   COUNTERSIGN 的业务语义就是「多个不同角色各投一票」，全员签是正确配置。
--   判据：`node_type = 'APPROVAL'` 才是「一个人签字即可」；要多人共识必须显式建 COUNTERSIGN。
--
-- # 三、改法（已发布版本只读 ⇒ 归档旧版 → 发新版）
--
--   `flow/handler.rs` 红线 1：已发布版本不可改。本迁移走「归档 vN → 发 v(MAX+1)」：
--     · vN 的行 / 快照 / graph_hash **逐字节不动**（只把 is_current 置 FALSE、
--       version_status 置 ARCHIVED）；
--     · v(MAX+1) 是新版本行 + 新快照 + 新 hash + 整份复制节点/边/审批人绑定；
--     · 只把 APPROVAL+ROLE+ALL 的节点 approval_mode 改成 ANY，
--       assignee_type / assignees / 超时 / 兜底 / 边 / 阈值 **一个字不动**。
--     · 新快照与 hash 不是手写的：由旧快照在库内按 node_key 打补丁（只改
--       approval_mode），再用与 Rust `graph::snapshot_json()` / `graph_hash()`
--       同一套规则（递归按键字典序 COLLATE "C" + 紧凑无空白 + sha256）现算；
--       打补丁前先证明 `sha256(canonical(旧快照)) == 旧 graph_hash` —— 对不上就
--       NOTICE 点名跳过（**不抛异常**，缺口留给人在流程编辑器里改）。
--
--   与 V0359 的两处实现差异（都是修正，不是口径变化）：
--     ① 版本号 = MAX(version_no) + 1（全库该流程的最大版本，含 DRAFT）—— 这是本卡的核心；
--     ② 复制节点行时**补齐了 fallback_user_id / fallback_role_id / subprocess_def_id**
--        三列（V0359 的列清单漏了它们，会让新版本的行与快照 JSON 不一致；
--        当前 17 个目标节点上这三列都是 NULL，但复制后仍必须与快照同构）。
--
-- # 四、幂等（重复执行 = 0 变更，可安全重跑）
--
--   * 处理完的流程，其新的 PUBLISHED 版本里已经没有 APPROVAL+ROLE+ALL 节点
--     ⇒ 第二次执行时目标清单为空 ⇒ 0 个流程受影响（NOTICE 报数）；
--   * 快照与 hash 对不齐的流程 ⇒ NOTICE 点名跳过，不抛异常；
--   * 节点在 JSON 快照里形状与库里不一致（客户改过图）⇒ 该节点跳过并说明形状；
--   * 全程无 `INSERT ... ON CONFLICT`：靠「先判形状、再发版」保证不重复发版。
--
-- # 五、回滚
--   `V0367__flow_all_role_approval_to_any_multiversion.down.sql`：删掉本迁移发的版本
--   （change_note 以 'V0367：' 开头），把该流程剩余版本里**版本号最高且已归档**的那一版
--   复位为 PUBLISHED + is_current；有审批单跑在被删版本上则**拒绝回滚**
--   （删版本会留下指向空气的审批单），与 V0251 / V0355 / V0359 同口径。
-- ============================================================================

-- ---------- 0) canonical JSON（与 graph.rs::write_canonical 逐字同义，沿用 V0359） ----------
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

-- ---------- 1) 逐流程：归档旧版 → 发 MAX+1 版（只把目标节点的 approval_mode 改成 ANY） ----------
DO $$
DECLARE
    t           record;
    c_def       uuid;
    c_ver       uuid;
    c_ver_no    int;
    c_hash      text;
    c_snap      jsonb;
    c_new_snap  jsonb;
    c_new_hash  text;
    c_new_ver   uuid;
    v_next_no   int;
    v_keys      text[];          -- 库里判定的目标节点 key
    v_pkeys     text[] := '{}';  -- 在 JSON 快照里复核过、且已打补丁的 key
    v_idx       bigint;
    v_node      jsonb;
    v_patched   int;
    v_touched   int := 0;
    v_flows     int := 0;
    n_nodes     int;
    n_edges     int;
    n_bind      int;
    n_cur       int;
    n_live      int;
    n_aff       int;
BEGIN
    FOR t IN
        SELECT d.id AS def_id, d.flow_code, v.id AS ver_id
          FROM biz_flow_def d
          JOIN biz_flow_def_version v
            ON v.def_id = d.id AND v.is_current AND v.version_status = 'PUBLISHED'
         WHERE d.deleted_at IS NULL
           AND EXISTS (
                 SELECT 1 FROM biz_flow_node n
                  WHERE n.version_id = v.id
                    AND n.node_type = 'APPROVAL'
                    AND n.assignee_type = 'ROLE'
                    AND n.approval_mode = 'ALL')
         ORDER BY d.flow_code
    LOOP
        c_def := t.def_id;
        c_ver := t.ver_id;
        v_pkeys := '{}';
        v_patched := 0;

        SELECT v.version_no, btrim(v.graph_hash), v.graph_snapshot
          INTO c_ver_no, c_hash, c_snap
          FROM biz_flow_def_version v WHERE v.id = c_ver;

        -- 序列化口径校准：canonical(旧快照) 的 sha256 必须等于库里的 graph_hash，
        -- 否则说明快照被直接改过 / 含本函数不支持的形状 —— 跳过，不抛异常。
        IF c_hash IS NULL OR c_snap IS NULL
           OR encode(sha256(convert_to(pg_temp.canonical_json(c_snap), 'UTF8')), 'hex')
              IS DISTINCT FROM c_hash
        THEN
            RAISE NOTICE 'V0367 %：跳过 —— canonical(v% 快照) 与库里的 graph_hash 不一致（快照被直接改过）。请在流程编辑器里另存草稿 → 把这些节点的签名模式改成「或签」→ 发布',
                t.flow_code, c_ver_no;
            CONTINUE;
        END IF;

        -- 目标节点 key（库侧判定）
        SELECT array_agg(n.node_key ORDER BY n.node_key) INTO v_keys
          FROM biz_flow_node n
         WHERE n.version_id = c_ver
           AND n.node_type = 'APPROVAL'
           AND n.assignee_type = 'ROLE'
           AND n.approval_mode = 'ALL';

        IF v_keys IS NULL THEN
            RAISE NOTICE 'V0367 %：当前版本 v% 没有 APPROVAL+ROLE+ALL 节点（已修正），跳过', t.flow_code, c_ver_no;
            CONTINUE;
        END IF;

        -- 在快照 JSON 里逐节点复核形状并打补丁（只改 approval_mode）
        c_new_snap := c_snap;
        FOR v_idx, v_node IN
            SELECT x.ord, x.node
              FROM jsonb_array_elements(c_snap -> 'nodes') WITH ORDINALITY AS x(node, ord)
             WHERE (x.node ->> 'key') = ANY (v_keys)
             ORDER BY x.ord
        LOOP
            IF (v_node ->> 'approval_mode') IS DISTINCT FROM 'ALL' THEN
                RAISE NOTICE 'V0367 %.%：快照里 approval_mode = %（不是 ALL），跳过该节点',
                    t.flow_code, v_node ->> 'key', coalesce(v_node ->> 'approval_mode', '<null>');
                CONTINUE;
            END IF;
            IF (v_node ->> 'node_type') IS DISTINCT FROM 'APPROVAL'
               OR (v_node ->> 'assignee_type') IS DISTINCT FROM 'ROLE'
            THEN
                RAISE NOTICE 'V0367 %.%：快照形状是 % / %（不是 APPROVAL / ROLE），保持原样 —— 会签节点不动',
                    t.flow_code, v_node ->> 'key',
                    coalesce(v_node ->> 'node_type', '<null>'),
                    coalesce(v_node ->> 'assignee_type', '<null>');
                CONTINUE;
            END IF;
            c_new_snap := jsonb_set(c_new_snap, ARRAY['nodes', (v_idx - 1)::text],
                                    jsonb_set(v_node, '{approval_mode}', '"ANY"'::jsonb));
            v_pkeys   := array_append(v_pkeys, v_node ->> 'key');
            v_patched := v_patched + 1;
        END LOOP;

        IF v_patched = 0 THEN
            RAISE NOTICE 'V0367 %：快照里没有可打补丁的节点（与库里的行不一致？），跳过', t.flow_code;
            CONTINUE;
        END IF;

        -- ★ 核心修正：版本号 = 该流程全库最大版本 + 1（含 DRAFT），不硬编码
        SELECT COALESCE(MAX(version_no), 0) + 1 INTO v_next_no
          FROM biz_flow_def_version WHERE def_id = c_def;

        c_new_hash := encode(sha256(convert_to(pg_temp.canonical_json(c_new_snap), 'UTF8')), 'hex');
        c_new_ver  := gen_random_uuid();
        v_touched  := v_touched + v_patched;
        v_flows    := v_flows + 1;

        SELECT count(*) INTO n_nodes FROM biz_flow_node WHERE version_id = c_ver;
        SELECT count(*) INTO n_edges FROM biz_flow_edge WHERE version_id = c_ver;

        -- ① 旧版退位归档（行 / 快照 / hash 逐字节不动）
        --    顺序不能反：uk_biz_flow_def_version_current 保证一个流程只有一行 is_current
        UPDATE biz_flow_def_version
           SET is_current = FALSE, version_status = 'ARCHIVED', updated_at = now()
         WHERE id = c_ver;

        -- ② 新版版本行（version_no = MAX+1）
        INSERT INTO biz_flow_def_version
            (id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
             node_count, edge_count, change_note, published_at, published_by, created_at, updated_at)
        VALUES
            (c_new_ver, c_def, v_next_no, 'PUBLISHED', TRUE, c_new_hash, c_new_snap,
             (SELECT count(*) FROM jsonb_array_elements(c_new_snap -> 'nodes')),
             (SELECT count(*) FROM jsonb_array_elements(c_new_snap -> 'edges')),
             'V0367：' || v_patched || ' 个「角色型审批」节点由全员会签（ALL）改为或签（ANY）'
             || '（APPROVAL 节点的业务语义是「岗位任一人签字即可」；跨角色会签节点 COUNTERSIGN 不动）',
             now(), NULL, now(), now());

        -- ③ 节点行：整份复制（含 fallback / subprocess 列）再按 v_pkeys 打补丁
        INSERT INTO biz_flow_node
            (id, version_id, node_key, node_name, node_type, assignee_type, superior_levels,
             form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
             allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
             max_remind_times, cc_on_approved, position_x, position_y, sort_order, config, created_at,
             fallback_user_id, fallback_role_id, subprocess_def_id)
        SELECT gen_random_uuid(), c_new_ver, node_key, node_name, node_type, assignee_type, superior_levels,
               form_var_key, approval_mode, approve_ratio, reject_veto, empty_assignee, self_approval,
               allow_robot, join_mode, fork_mode, timeout_hours, timeout_action, remind_before_hours,
               max_remind_times, cc_on_approved, position_x, position_y, sort_order, config, now(),
               fallback_user_id, fallback_role_id, subprocess_def_id
          FROM biz_flow_node WHERE version_id = c_ver;

        UPDATE biz_flow_node
           SET approval_mode = 'ANY'
         WHERE version_id = c_new_ver
           AND node_key = ANY (v_pkeys)
           AND node_type = 'APPROVAL'
           AND assignee_type = 'ROLE'
           AND approval_mode = 'ALL';
        GET DIAGNOSTICS n_aff = ROW_COUNT;
        IF n_aff <> v_patched THEN
            RAISE EXCEPTION 'V0367 % 自检失败：行更新 % 条，快照打补丁 % 个（行与快照不一致，已中止）',
                t.flow_code, n_aff, v_patched;
        END IF;

        -- ④ 边：按 node_key 重指到新节点（条件表达式 / 优先级 / 兜底标记原样）
        INSERT INTO biz_flow_edge
            (id, version_id, edge_key, from_node_id, to_node_id, condition_expr,
             condition_label, priority, is_default, created_at)
        SELECT gen_random_uuid(), c_new_ver, e.edge_key, nf2.id, nt2.id,
               CASE WHEN e.condition_expr = 'null'::jsonb THEN NULL ELSE e.condition_expr END,
               e.condition_label, e.priority, e.is_default, now()
          FROM biz_flow_edge e
          JOIN biz_flow_node nf ON nf.id = e.from_node_id
          JOIN biz_flow_node nt ON nt.id = e.to_node_id
          JOIN biz_flow_node nf2 ON nf2.version_id = c_new_ver AND nf2.node_key = nf.node_key
          JOIN biz_flow_node nt2 ON nt2.version_id = c_new_ver AND nt2.node_key = nt.node_key
         WHERE e.version_id = c_ver;

        -- ⑤ 审批人绑定：全部照抄（本迁移只改签名模式，不动审批人来源）
        INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order, created_at)
        SELECT n2.id, a.assignee_type, a.ref_id, a.sort_order, now()
          FROM biz_flow_node_assignee a
          JOIN biz_flow_node n1 ON n1.id = a.node_id
          JOIN biz_flow_node n2 ON n2.version_id = c_new_ver AND n2.node_key = n1.node_key
         WHERE n1.version_id = c_ver;

        -- ⑥ 定义指向新版本
        UPDATE biz_flow_def
           SET current_version_id = c_new_ver, current_version_no = v_next_no, updated_at = now()
         WHERE id = c_def;

        -- ---------- 自检 ----------
        SELECT count(*) INTO n_bind FROM biz_flow_node_assignee a JOIN biz_flow_node n ON n.id = a.node_id
         WHERE n.version_id = c_new_ver;

        IF (SELECT count(*) FROM biz_flow_node WHERE version_id = c_new_ver) <> n_nodes THEN
            RAISE EXCEPTION 'V0367 %.v% 自检失败：新版节点数 != 旧版（旧版 %）', t.flow_code, v_next_no, n_nodes;
        END IF;
        IF (SELECT count(*) FROM biz_flow_edge WHERE version_id = c_new_ver) <> n_edges THEN
            RAISE EXCEPTION 'V0367 %.v% 自检失败：新版边数 != 旧版（旧版 %）', t.flow_code, v_next_no, n_edges;
        END IF;
        IF EXISTS (SELECT 1 FROM biz_flow_node
                    WHERE version_id = c_new_ver AND node_key = ANY (v_pkeys)
                      AND node_type = 'APPROVAL' AND assignee_type = 'ROLE' AND approval_mode = 'ALL') THEN
            RAISE EXCEPTION 'V0367 %.v% 自检失败：新版里仍有 APPROVAL + ROLE + ALL 的节点', t.flow_code, v_next_no;
        END IF;
        -- 行与快照一致：新版 graph_hash 必须等于「库内重新 canonical 化的快照」的 sha256
        IF (SELECT encode(sha256(convert_to(pg_temp.canonical_json(graph_snapshot), 'UTF8')), 'hex')
              FROM biz_flow_def_version WHERE id = c_new_ver) IS DISTINCT FROM c_new_hash THEN
            RAISE EXCEPTION 'V0367 %.v% 自检失败：新版 graph_snapshot 与 graph_hash 不一致', t.flow_code, v_next_no;
        END IF;
        -- 同一流程只能有一个 is_current，且定义指向它
        SELECT count(*) INTO n_cur FROM biz_flow_def_version WHERE def_id = c_def AND is_current;
        IF n_cur <> 1 THEN
            RAISE EXCEPTION 'V0367 % 自检失败：有 % 个 is_current 版本（必须 1）', t.flow_code, n_cur;
        END IF;
        IF (SELECT current_version_id FROM biz_flow_def WHERE id = c_def) IS DISTINCT FROM c_new_ver THEN
            RAISE EXCEPTION 'V0367 % 自检失败：定义未指向新版本', t.flow_code;
        END IF;
        -- 版本号唯一（uk_biz_flow_def_version 的显式复核，便于排障时一眼看到）
        IF EXISTS (SELECT 1 FROM biz_flow_def_version
                    WHERE def_id = c_def AND version_no = v_next_no AND id <> c_new_ver) THEN
            RAISE EXCEPTION 'V0367 % 自检失败：v% 版本号已被占用', t.flow_code, v_next_no;
        END IF;
        -- 旧版一行没动
        IF (SELECT count(*) FROM biz_flow_node WHERE version_id = c_ver) <> n_nodes
           OR (SELECT btrim(graph_hash) FROM biz_flow_def_version WHERE id = c_ver) IS DISTINCT FROM c_hash THEN
            RAISE EXCEPTION 'V0367 %.v% 自检失败：旧版的行 / hash 被改动了（已发布版本必须只读）', t.flow_code, c_ver_no;
        END IF;
        -- 旧实例继续读自己的版本，不受影响（仅统计）
        SELECT count(*) INTO n_live FROM biz_flow_instance WHERE version_id = c_ver;
        RAISE NOTICE 'V0367 %：v% 已发布（改 % 个节点 [%] / hash=% / 节点 % / 边 % / 绑定 % 行；历史实例跑在旧版 v% 上 % 张，继续可用）',
            t.flow_code, v_next_no, v_patched, array_to_string(v_pkeys, ','), c_new_hash,
            n_nodes, n_edges, n_bind, c_ver_no, n_live;
    END LOOP;

    IF v_flows = 0 THEN
        RAISE NOTICE 'V0367 完成：0 个流程需要处理（PUBLISHED 当前版本里已无 APPROVAL+ROLE+ALL 节点）—— 幂等';
    ELSE
        RAISE NOTICE 'V0367 完成：共处理 % 个流程 / % 个「角色型审批被配成全员会签」节点', v_flows, v_touched;
    END IF;
END $$;

-- ---------- 2) 收口自检：PUBLISHED 当前版本里不允许再出现 APPROVAL + ROLE + ALL ----------
DO $$
DECLARE
    n_bad  int;
    v_list text;
BEGIN
    SELECT count(*), string_agg(d.flow_code || '.' || n.node_key, ', ' ORDER BY d.flow_code, n.node_key)
      INTO n_bad, v_list
      FROM biz_flow_def d
      JOIN biz_flow_def_version v ON v.def_id = d.id AND v.is_current AND v.version_status = 'PUBLISHED'
      JOIN biz_flow_node n ON n.version_id = v.id
     WHERE d.deleted_at IS NULL
       AND n.node_type = 'APPROVAL' AND n.assignee_type = 'ROLE' AND n.approval_mode = 'ALL';

    IF n_bad > 0 THEN
        -- 不抛异常：抛异常会挡住整个升级（与 V0355 / V0359 同口径）。
        -- 缺口留给人在流程编辑器里改完再发版，之后 `scripts/verify/flow_countersign_invariants.sh` 会继续报警。
        RAISE WARNING 'V0367 收口自检：PUBLISHED 当前版本里仍有 % 个 APPROVAL+ROLE+ALL 节点（%）。'
                      '若这些是本迁移「跳过」的（快照与 hash 不一致 / 客户改过图），请按 NOTICE 指引在流程编辑器里改成「或签」再发版',
                      n_bad, v_list;
    ELSE
        RAISE NOTICE 'V0367 收口自检通过：PUBLISHED 当前版本里 APPROVAL+ROLE+ALL = 0';
    END IF;
END $$;
