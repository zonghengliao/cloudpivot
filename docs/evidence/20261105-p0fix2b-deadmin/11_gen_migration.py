#!/usr/bin/env python3
"""生成 V0251 迁移：三条流程发 v2（审批人来源 ADMIN → 业务角色）。

快照 / hash 均取自「应用内真实发布接口」在隔离库上产出的结果（build_v2_report.json
+ snap_v2_*.json），迁移只负责把同一张图种进新库/生产，**不手算 hash**。
"""
import json, pathlib

EV = pathlib.Path("/opt/cloudpivot/docs/evidence/20261105-p0fix2b-deadmin")
rep = json.loads((EV / "build_v2_report.json").read_text())
MIG = pathlib.Path("/opt/cloudpivot/backend/migrations/business")

ROLE = {
    "DEPT_LEADER": "20000000-0000-4000-8000-0000000f0001",
    "FINANCE_MANAGER": "20000000-0000-4000-8000-0000000f0005",
    "FINANCE_DIRECTOR": "20000000-0000-4000-8000-0000000f0006",
    "GENERAL_MANAGER": "20000000-0000-4000-8000-0000000f0003",
    "WAREHOUSE_MANAGER": "20000000-0000-4000-8000-0000000f0004",
}

# 每个流程的补丁：node_key -> (assignee_type, empty_assignee|None, node_name|None, [(role_code, ord)], 追加自审策略不变)
FLOWS = {
    "payable_pay": {
        "comment": "应付付款：部门主管 / 财务经理 / 财务总监+总经理会签",
        "nodes": {
            "n_dept": {"type": "ROLE", "empty": None, "name": None,
                        "bind": [("DEPT_LEADER", 1)]},
            "n_fin": {"type": "ROLE", "empty": None, "name": None,
                       "bind": [("FINANCE_MANAGER", 1)]},
            "n_board": {"type": "ROLE", "empty": None, "name": None,
                         "bind": [("FINANCE_DIRECTOR", 1), ("GENERAL_MANAGER", 2)]},
        },
        "seed_hash": "8341eac726949864d2a9fdb562a34154d8b7eb611a265fc6adc2c1c9e8e5c1c5",
        "alt_hashes": [],
    },
    "stock_in_cancel": {
        "comment": "收货单作废复核：仓管主管",
        "nodes": {
            # V0248（P1-FIX-1）已把该节点改成 ROLE+WAREHOUSE_MANAGER+ERROR 的库，
            # 这里保持 ERROR 不变（空角色宁可发起失败，也不把复核悄悄派回超管）。
            "n_review": {"type": "ROLE", "empty": "ERROR", "name": None,
                          "bind": [("WAREHOUSE_MANAGER", 1)]},
        },
        "seed_hash": "78d34c4557630fd8a57c45f901b427649db06ee380baf1868d257813f6e4e56c",
        "alt_hashes": ["0267661a1b7d37ebc94a3e8859eecde63173325f57d3949970dcbdd4f8b9b5a0"],
    },
    "stock_transfer": {
        "comment": "库存调拨审批：仓管主管（节点名同时改「仓管主管审批」）",
        "nodes": {
            "n_approve": {"type": "ROLE", "empty": None, "name": "仓管主管审批",
                           "bind": [("WAREHOUSE_MANAGER", 1)]},
        },
        "seed_hash": "e1947064684e326c4ad4248e354d0215f1a3f3a7f668fb467aefe854adbd90da",
        "alt_hashes": [],
    },
}

V2_ID = {  # 固定 uuid（不沿用应用生成的 now_v7，便于幂等/取证）
    "payable_pay": "20000000-0000-4000-8000-0000002b0001",
    "stock_in_cancel": "20000000-0000-4000-8000-0000002b0002",
    "stock_transfer": "20000000-0000-4000-8000-0000002b0003",
}


def hash_list(flow):
    spec = FLOWS[flow]
    vals = [spec["seed_hash"]] + spec["alt_hashes"]
    return ", ".join("'" + v + "'" for v in vals)


def sql_flow(flow):
    spec = FLOWS[flow]
    r = rep[flow]
    snap = (EV / f"snap_v2_{flow}.json").read_text().strip()
    assert snap.startswith("{") and snap.endswith("}"), flow
    pub = r["publish"]
    node_ops = []
    bind_rows = []
    for key, patch in spec["nodes"].items():
        sets = [f"assignee_type = '{patch['type']}'"]
        if patch["empty"]:
            sets.append(f"empty_assignee = '{patch['empty']}'")
        if patch["name"]:
            sets.append(f"node_name = '{patch['name']}'")
        node_ops.append(
            "    UPDATE biz_flow_node SET " + ", ".join(sets)
            + f" WHERE version_id = c_v2 AND node_key = '{key}';"
        )
        for code, ord_ in patch["bind"]:
            bind_rows.append(f"        ('{key}', '{ROLE[code]}'::uuid, {ord_})")
    bind_sql = ",\n".join(bind_rows)
    n_nodes = len(spec["nodes"])
    total_bind = len(bind_rows)
    return f"""
-- ---------------------------------------------------------------------------
-- {flow}：{spec['comment']}
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    c_def       uuid;
    c_v1        uuid;
    c_v1_hash   text;
    c_v2  CONSTANT uuid := '{V2_ID[flow]}';
    c_new_hash  CONSTANT text := '{pub['graph_hash']}';
    c_old_hash  text;
    c_snapshot  CONSTANT text := $snap${snap}$snap$;
    n_v1_nodes  int;
    n_v2_nodes  int;
    n_bind      int;
    n_admin     int;
BEGIN
    SELECT id, current_version_id INTO c_def, c_v1
      FROM biz_flow_def
     WHERE flow_code = '{flow}' AND deleted_at IS NULL;
    IF c_def IS NULL THEN
        RAISE NOTICE 'V0251 {flow}：本库没有该流程（客户未种 / 已软删），跳过';
        RETURN;
    END IF;

    -- 幂等：已经发过 v2（含客户自己发过）就整段不动
    IF EXISTS (SELECT 1 FROM biz_flow_def_version WHERE def_id = c_def AND version_no >= 2) THEN
        RAISE NOTICE 'V0251 {flow}：已有 version_no ≥ 2，跳过（绝不覆盖客户发布过的图）';
        RETURN;
    END IF;

    -- 只在自己种子的 v1 上动手（hash 对不上 = 客户改过图）
    SELECT btrim(graph_hash) INTO c_v1_hash FROM biz_flow_def_version WHERE id = c_v1;
    IF c_v1_hash IS NULL OR c_v1_hash NOT IN ({hash_list(flow)}) THEN
        RAISE NOTICE 'V0251 {flow}：v1 hash = %，不是我仓种子口径，跳过', coalesce(c_v1_hash, '<null>');
        RETURN;
    END IF;
    c_old_hash := c_v1_hash;

    SELECT count(*) INTO n_v1_nodes FROM biz_flow_node WHERE version_id = c_v1;

    -- ① v1 先退位归档（**只改状态与当前标记**；行 / 快照 / graph_hash 逐字节不动）
    --    顺序不能反：uk_biz_flow_def_version_current 保证一个流程只有一行 is_current
    UPDATE biz_flow_def_version
       SET is_current = FALSE, version_status = 'ARCHIVED', updated_at = now()
     WHERE id = c_v1;

    -- ② 新版本行（PUBLISHED + is_current）
    INSERT INTO biz_flow_def_version
        (id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot,
         node_count, edge_count, change_note, published_at, published_by, created_at, updated_at)
    VALUES
        (c_v2, c_def, 2, 'PUBLISHED', TRUE, c_new_hash, c_snapshot::jsonb,
         {pub['node_count']}, {pub['edge_count']},
         'P0-FIX-2B-B 去 ADMIN 化：审批人来源由「全部超管」改为业务角色（不含其它语义改动）',
         now(), NULL, now(), now());

    -- ② 节点：整份复制再按 node_key 打补丁（金额分级 / reject_veto / self_approval / join_mode 一律不动）
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

{chr(10).join(node_ops)}

    -- ③ 边：按 node_key 重指到新节点
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

    -- ④ 审批人绑定（业务角色，不是「超管」这个身份）
    INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order, created_at)
    SELECT n.id, 'ROLE', b.ref_id, b.ord, now()
      FROM (VALUES
{bind_sql}
           ) AS b(node_key, ref_id, ord)
      JOIN biz_flow_node n ON n.version_id = c_v2 AND n.node_key = b.node_key;

    -- ⑤ 定义指向新版本（v1 已在 ① 退位）
    UPDATE biz_flow_def
       SET current_version_id = c_v2, current_version_no = 2, updated_at = now()
     WHERE id = c_def;

    -- ⑥ 自检
    SELECT count(*) INTO n_v2_nodes FROM biz_flow_node WHERE version_id = c_v2;
    IF n_v2_nodes <> n_v1_nodes OR n_v2_nodes <> {pub['node_count']} THEN
        RAISE EXCEPTION 'V0251 {flow} 自检失败：节点数 v1=% v2=%（期望 {pub['node_count']}）', n_v1_nodes, n_v2_nodes;
    END IF;
    SELECT count(*) INTO n_bind FROM biz_flow_node_assignee a JOIN biz_flow_node n ON n.id = a.node_id
     WHERE n.version_id = c_v2;
    IF n_bind <> {total_bind} THEN
        RAISE EXCEPTION 'V0251 {flow} 自检失败：审批人绑定 % 行（期望 {total_bind}）', n_bind;
    END IF;
    SELECT count(*) INTO n_admin FROM biz_flow_node
     WHERE version_id = c_v2 AND assignee_type = 'ADMIN';
    IF n_admin <> 0 THEN
        RAISE EXCEPTION 'V0251 {flow} 自检失败：v2 里还有 % 个 ADMIN 节点', n_admin;
    END IF;
    IF (SELECT btrim(graph_hash) FROM biz_flow_def_version WHERE id = c_v1) IS DISTINCT FROM c_old_hash THEN
        RAISE EXCEPTION 'V0251 {flow} 自检失败：v1 的 graph_hash 被改动了';
    END IF;
    IF (SELECT count(*) FROM biz_flow_node WHERE version_id = c_v1) <> n_v1_nodes THEN
        RAISE EXCEPTION 'V0251 {flow} 自检失败：v1 的节点行数变了';
    END IF;
    RAISE NOTICE 'V0251 {flow}：v2 已发布（hash=%）', c_new_hash;
END $$;
"""


def sql_down_flow(flow):
    return f"""
DO $$
DECLARE
    c_def uuid;
    c_v1  uuid;
    c_v2  CONSTANT uuid := '{V2_ID[flow]}';
    n_live int;
BEGIN
    SELECT id INTO c_def FROM biz_flow_def WHERE flow_code = '{flow}' AND deleted_at IS NULL;
    IF c_def IS NULL OR NOT EXISTS (SELECT 1 FROM biz_flow_def_version WHERE id = c_v2) THEN
        RAISE NOTICE 'V0251 down {flow}：没有 v2，跳过';
        RETURN;
    END IF;
    -- v1 必须按 version_no=1 取：此刻 def.current_version_id 指向 v2
    SELECT id INTO c_v1 FROM biz_flow_def_version WHERE def_id = c_def AND version_no = 1;
    SELECT count(*) INTO n_live FROM biz_flow_instance WHERE version_id = c_v2;
    IF n_live > 0 THEN
        RAISE EXCEPTION 'V0251 down {flow} 拒绝回滚：% 张审批单跑在 v2 上（删版本会留下指向空气的审批单）', n_live;
    END IF;
    DELETE FROM biz_flow_node_assignee WHERE node_id IN (SELECT id FROM biz_flow_node WHERE version_id = c_v2);
    DELETE FROM biz_flow_edge WHERE version_id = c_v2;
    DELETE FROM biz_flow_node WHERE version_id = c_v2;
    DELETE FROM biz_flow_def_version WHERE id = c_v2;
    UPDATE biz_flow_def_version SET is_current = TRUE, version_status = 'PUBLISHED', updated_at = now() WHERE id = c_v1;
    UPDATE biz_flow_def SET current_version_id = c_v1, current_version_no = 1, updated_at = now() WHERE id = c_def;
    RAISE NOTICE 'V0251 down {flow}：已回滚到 v1';
END $$;
"""


UP_HEAD = """-- ============================================================================
-- V0251__flow_deadmin_assignee_roles.up.sql
--
-- 【P0-FIX-2B-B / t_1e6a8431】审批流「去 ADMIN 化」：5 个 assignee_type='ADMIN' 的节点
-- 改成**业务角色**（ROLE），并按流程发布新版本（v1 退位归档，行/快照/hash 逐字节不动）。
--
-- 为什么必须单独修
-- ----------------
-- `assignee_type='ADMIN'` 的语义是「持有 SUPER_ADMIN 的所有人」（flow/assignee.rs 的映射表），
-- 生产今天有 4 个超管（admin / henry.liao / laoliu / laoliu_test），于是：
--   payable_pay.n_dept  「部门主管审批」       → 4 个超管
--   payable_pay.n_fin   「财务经理审批」       → 4 个超管
--   payable_pay.n_board 「财务总监+总经理会签」→ **4 个超管会签**（与财务总监/总经理无关）
--   stock_in_cancel.n_review「作废复核」      → 4 个超管
--   stock_transfer.n_approve「超管审批」      → 4 个超管
-- 「摘超管」= 同时改这 5 个节点的审批人集合：先把超管从 4 摘到 1，`n_board` 的会签就静默
-- 变成 admin 单人审批 —— 降低复核强度且没人会发现。所以顺序定死：**先去 ADMIN 化，后摘超管**。
--
-- 改法（逐节点，角色 id 见括号）
-- -----------------------------
--   payable_pay.n_dept      → ROLE[DEPT_LEADER]                         empty=FALLBACK_ADMIN
--   payable_pay.n_fin       → ROLE[FINANCE_MANAGER]                     empty=FALLBACK_ADMIN
--   payable_pay.n_board     → ROLE[FINANCE_DIRECTOR, GENERAL_MANAGER]   empty=FALLBACK_ADMIN，approval_mode=ALL 会签 **不动**
--   stock_in_cancel.n_review→ ROLE[WAREHOUSE_MANAGER]                   empty=ERROR（沿用 V0248）
--   stock_transfer.n_approve→ ROLE[WAREHOUSE_MANAGER]，节点名改「仓管主管审批」  empty=FALLBACK_ADMIN
--
-- 两条必须知道的口径
-- ------------------
-- 1. **多角色节点是候选并集**：`assignee.rs::resolve` 把节点绑定的所有 ROLE 成员并成一个候选集
--    再过 finalize。所以 `n_board` 在 FINANCE_DIRECTOR 0 成员时**不会**落 FALLBACK_ADMIN，
--    而是「会签人数 = 总经理那几个人」。本迁移不解决「财务总监岗位空缺」（那是组织问题）。
-- 2. **stock_in_cancel 的 empty_assignee 保持 ERROR**（与 V0248 / P1-FIX-1 一致），
--    与卡面「一律 FALLBACK_ADMIN」的差异是有意的：作废复核若落 FALLBACK_ADMIN，在单超管库上
--    兜出来的正好是发起人本人，`self_approval=FORBID` 会当场 422 —— 那正是 P1-FIX-1 修掉的缺陷。
--    其余 4 个节点按卡面 FALLBACK_ADMIN（角色没人时退回超管，防「角色没人 ⇒ 单据卡死」）。
--
-- 快照与 hash 从哪来（**不手算**）
-- -------------------------------
-- 先在隔离副本库上走**应用内真实发布接口**（另存草稿 → 改图 → 发布），由
-- `graph.snapshot_json()` / `graph.graph_hash()` 生成 `graph_snapshot` 与 `graph_hash`；
-- 本迁移只把同一张图种进新库（证据：docs/evidence/20261105-p0fix2b-deadmin/）。
--
-- 幂等 / 安全
-- -----------
-- · 只在「本组织该流程仍是 v1（version_no = 1）+ v1 hash 是我仓种子」时动手；
--   客户发过 v2 / 改过图 → NOTICE 跳过，绝不覆盖；
-- · v1 的行、快照、graph_hash 一律不动（只改 is_current / version_status）；
-- · 回滚见 .down.sql（有单据跑在 v2 上则拒绝回滚）。
-- ============================================================================
"""

if __name__ == "__main__":
    up = UP_HEAD + "".join(sql_flow(f) for f in FLOWS)
    down = ("-- V0251 down：删掉三条流程的 v2，v1 复位为当前版本（有实例跑在 v2 上则拒绝）\n"
            + "".join(sql_down_flow(f) for f in FLOWS))
    (MIG / "V0251__flow_deadmin_assignee_roles.up.sql").write_text(up)
    (MIG / "V0251__flow_deadmin_assignee_roles.down.sql").write_text(down)
    print("up bytes", len(up), "down bytes", len(down))
