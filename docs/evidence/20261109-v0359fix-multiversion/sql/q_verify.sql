\pset pager off
CREATE OR REPLACE FUNCTION pg_temp.canonical_json(v jsonb) RETURNS text
LANGUAGE plpgsql IMMUTABLE AS $fn$
DECLARE out text; k text; item jsonb; first boolean := true;
BEGIN
    CASE jsonb_typeof(v)
    WHEN 'object' THEN
        out := '{';
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
        RETURN v::text;
    END CASE;
END $fn$;

\echo === ① 版本列表（全部流程，按 flow_code/version_no） ===
SELECT d.flow_code, v.version_no, v.version_status, v.is_current, d.current_version_no,
       (v.id = d.current_version_id) AS def_points_here,
       (SELECT count(*) FROM biz_flow_node n WHERE n.version_id=v.id AND n.node_type='APPROVAL' AND n.assignee_type='ROLE' AND n.approval_mode='ALL') AS approval_role_all,
       (SELECT count(*) FROM biz_flow_node n WHERE n.version_id=v.id AND n.node_type='COUNTERSIGN' AND n.approval_mode='ALL') AS countersign_all
  FROM biz_flow_def d JOIN biz_flow_def_version v ON v.def_id=d.id
 WHERE d.deleted_at IS NULL ORDER BY d.flow_code, v.version_no;
\echo === ② is_current 唯一性（每个流程必须恰好 1 行） ===
SELECT count(*) AS flows_with_wrong_current FROM (
  SELECT d.id, count(v.id) FILTER (WHERE v.is_current) AS c
    FROM biz_flow_def d JOIN biz_flow_def_version v ON v.def_id=d.id
   WHERE d.deleted_at IS NULL GROUP BY d.id HAVING count(v.id) FILTER (WHERE v.is_current) <> 1) x;
\echo === ③ po_approve 节点 approval_mode 前后对比（旧版 v2 vs 新版 v5） ===
SELECT n.node_key, n.node_type, n.assignee_type, n.approval_mode, v.version_no, v.is_current
  FROM biz_flow_def d
  JOIN biz_flow_def_version v ON v.def_id=d.id AND v.version_no IN (2,5)
  JOIN biz_flow_node n ON n.version_id=v.id
 WHERE d.flow_code='po_approve' AND n.node_type IN ('APPROVAL','COUNTERSIGN')
 ORDER BY n.node_key, v.version_no;
\echo === ④ COUNTERSIGN 节点（必须一字不动） ===
SELECT d.flow_code, v.version_no, n.node_key, n.approval_mode, v.is_current
  FROM biz_flow_def d JOIN biz_flow_def_version v ON v.def_id=d.id
  JOIN biz_flow_node n ON n.version_id=v.id AND n.node_type='COUNTERSIGN'
 WHERE d.deleted_at IS NULL ORDER BY d.flow_code, v.version_no, n.node_key;
\echo === ⑤ 目标节点：旧版仍 ALL / 新版变 ANY（同 node_key 对比，取 po_approve / contract_approve / payable_pay） ===
SELECT d.flow_code, n.node_key,
       max(CASE WHEN v.is_current THEN n.approval_mode END) AS new_mode,
       max(CASE WHEN NOT v.is_current AND v.version_status <> 'DRAFT' THEN n.approval_mode END) AS prev_published_mode
  FROM biz_flow_def d
  JOIN biz_flow_def_version v ON v.def_id=d.id
  JOIN biz_flow_node n ON n.version_id=v.id AND n.node_type='APPROVAL' AND n.assignee_type='ROLE'
 WHERE d.deleted_at IS NULL AND d.flow_code IN ('po_approve','contract_approve','payable_pay','pr_approve','prt_approve')
 GROUP BY d.flow_code, n.node_key ORDER BY d.flow_code, n.node_key;
\echo === ⑥ 收口：PUBLISHED 当前版本里 APPROVAL+ROLE+ALL 数量（必须 0） ===
SELECT count(*) AS bad FROM biz_flow_def d
  JOIN biz_flow_def_version v ON v.def_id=d.id AND v.is_current AND v.version_status='PUBLISHED'
  JOIN biz_flow_node n ON n.version_id=v.id
 WHERE d.deleted_at IS NULL AND n.node_type='APPROVAL' AND n.assignee_type='ROLE' AND n.approval_mode='ALL';
\echo === ⑦ DRAFT 版本未被改动（po_approve v3/v4 / stock_take_post v2） ===
SELECT d.flow_code, v.version_no, v.version_status, v.is_current,
       (SELECT count(*) FROM biz_flow_node n WHERE n.version_id=v.id AND n.node_type='APPROVAL' AND n.assignee_type='ROLE' AND n.approval_mode='ALL') AS draft_all
  FROM biz_flow_def d JOIN biz_flow_def_version v ON v.def_id=d.id
 WHERE d.flow_code IN ('po_approve','stock_take_post') AND v.version_status='DRAFT' ORDER BY 1,2;
\echo === ⑧ 新版图完整性：节点/边/绑定数 与 node_count/edge_count 声明一致 ===
SELECT d.flow_code, v.version_no,
       v.node_count, (SELECT count(*) FROM biz_flow_node n WHERE n.version_id=v.id) AS nodes_actual,
       v.edge_count, (SELECT count(*) FROM biz_flow_edge e WHERE e.version_id=v.id) AS edges_actual,
       (SELECT count(*) FROM biz_flow_node_assignee a JOIN biz_flow_node n ON n.id=a.node_id WHERE n.version_id=v.id) AS binds
  FROM biz_flow_def d JOIN biz_flow_def_version v ON v.def_id=d.id AND v.is_current
 WHERE d.deleted_at IS NULL ORDER BY d.flow_code;
\echo === ⑨ 全部版本快照 hash 自洽（mismatched 应为 0） ===
WITH f AS (
  SELECT pg_temp.canonical_json(v.graph_snapshot) AS c, btrim(v.graph_hash) AS h, v.id
    FROM biz_flow_def_version v JOIN biz_flow_def d ON d.id=v.def_id WHERE d.deleted_at IS NULL)
SELECT count(*) AS total,
       count(*) FILTER (WHERE c IS NULL) AS null_snapshot,
       count(*) FILTER (WHERE c IS NOT NULL AND encode(sha256(convert_to(c,'UTF8')),'hex') IS DISTINCT FROM h) AS mismatched
  FROM f;
