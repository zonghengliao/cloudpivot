-- =============================================================================
-- e2e07_fix1_flow_checks.sql —— E2E07-FIX-1 的「流程库事实」检查（只读）
--
-- 用法（在 145 / 任意环境）：
--   PGPASSWORD=... psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_dev -f scripts/verify/e2e07_fix1_flow_checks.sql
-- 输出：每行 `PASS|...` 或 `FAIL|...`；`FAIL` 行数 = 0 才算通过（配 e2e07_fix1_contract_any_probe.sh 用）。
--
-- 查什么
--   ① 快照自检：每个流程「当前版本」的 graph_snapshot 按 canonical 序列化后的 sha256
--      必须等于它发布时写死的 graph_hash（谁偷偷改了行/快照，这里立刻红；同时证明
--      V0359 现算的 hash 与本仓 Rust 口径一致）。
--   ② 合同审批：当前版本 = v2，n_pm / n_pm_hi 都是 ANY。
--   ③ 预算审批（E2E06-FIX-1 的成果）：当前版本仍是 v2、n_dept 仍是
--      「INITIATOR_SUPERIOR + ANY」，且**没有**被本次改动再发一版（回归）。
--   ④ 跨角色会签（COUNTERSIGN）必须仍然是 ALL —— 本卡只改「角色型审批」，不许误伤会签。
--   ⑤ 没有残留的中间态：本人演练产生的合同流程实例不允许停在 PENDING。
-- =============================================================================
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

WITH cur AS (
    SELECT d.flow_code, d.id AS def_id, v.id AS version_id, v.version_no, v.graph_hash, v.graph_snapshot
      FROM biz_flow_def d
      JOIN biz_flow_def_version v ON v.id = d.current_version_id
     WHERE d.deleted_at IS NULL AND d.status = 'PUBLISHED'
),
node AS (
    SELECT c.flow_code, n.node_key, n.node_type, n.assignee_type, n.approval_mode, n.superior_levels
      FROM cur c JOIN biz_flow_node n ON n.version_id = c.version_id
),
facts AS (
    SELECT
      -- ① 快照自检
      (SELECT count(*) FROM cur c
        WHERE encode(sha256(convert_to(pg_temp.canonical_json(c.graph_snapshot), 'UTF8')), 'hex')
              IS DISTINCT FROM btrim(c.graph_hash)) AS snapshot_mismatch,
      -- ② 合同审批
      (SELECT version_no FROM cur WHERE flow_code = 'contract_approve') AS contract_version,
      (SELECT count(*) FROM node WHERE flow_code = 'contract_approve'
          AND node_key IN ('n_pm','n_pm_hi') AND approval_mode = 'ANY') AS contract_any_nodes,
      -- ③ 预算审批（回归）
      (SELECT version_no FROM cur WHERE flow_code = 'budget_approve') AS budget_version,
      (SELECT count(*) FROM biz_flow_def_version v JOIN biz_flow_def d ON d.id = v.def_id
        WHERE d.flow_code = 'budget_approve') AS budget_versions,
      (SELECT count(*) FROM node WHERE flow_code = 'budget_approve' AND node_key = 'n_dept'
          AND node_type = 'APPROVAL' AND assignee_type = 'INITIATOR_SUPERIOR' AND approval_mode = 'ANY') AS budget_dept_shape,
      -- ④ 会签节点必须还是 ALL
      (SELECT count(*) FROM node WHERE node_type = 'COUNTERSIGN' AND approval_mode = 'ALL') AS countersign_all,
      (SELECT count(*) FROM node WHERE node_type = 'COUNTERSIGN') AS countersign_total,
      -- ⑤ 本人演练是否留下中间态
      (SELECT count(*) FROM biz_flow_instance i
        WHERE i.flow_code = 'contract_approve' AND i.status = 'PENDING'
          AND i.biz_id IN (SELECT id FROM biz_contract WHERE contract_name LIKE 'E2E07FIX1-ANY-%')
          AND i.created_at > now() - interval '2 hours') AS my_pending_instances
)
SELECT res || ' | ' || label || ' | 实际=' || val || ' | 期望=' || expect AS line
  FROM (
    SELECT 1 AS ord,
           '① 快照自检 canonical(snapshot)==graph_hash' AS label,
           snapshot_mismatch::text AS val, '0' AS expect,
           CASE WHEN snapshot_mismatch = 0 THEN 'PASS' ELSE 'FAIL' END AS res FROM facts
    UNION ALL SELECT 2, '② 合同审批当前版本号', contract_version::text, '2',
           CASE WHEN contract_version = 2 THEN 'PASS' ELSE 'FAIL' END FROM facts
    UNION ALL SELECT 3, '② 合同 n_pm/n_pm_hi 都是 ANY 的节点数', contract_any_nodes::text, '2',
           CASE WHEN contract_any_nodes = 2 THEN 'PASS' ELSE 'FAIL' END FROM facts
    UNION ALL SELECT 4, '③ 预算审批当前版本号（E2E06-FIX-1 之后 = 2，本次未再发版）',
           budget_version::text, '2',
           CASE WHEN budget_version = 2 THEN 'PASS' ELSE 'FAIL' END FROM facts
    UNION ALL SELECT 5, '③ 预算审批版本总数（= 2）', budget_versions::text, '2',
           CASE WHEN budget_versions = 2 THEN 'PASS' ELSE 'FAIL' END FROM facts
    UNION ALL SELECT 6, '③ 预算 n_dept = APPROVAL/INITIATOR_SUPERIOR/ANY 的节点数', budget_dept_shape::text, '1',
           CASE WHEN budget_dept_shape = 1 THEN 'PASS' ELSE 'FAIL' END FROM facts
    UNION ALL SELECT 7, '④ COUNTERSIGN 节点仍是 ALL 的数量（= 全部会签节点）',
           countersign_all::text, countersign_total::text,
           CASE WHEN countersign_all = countersign_total THEN 'PASS' ELSE 'FAIL' END FROM facts
    UNION ALL SELECT 8, '⑤ 本人演练残留的 PENDING 合同实例数', my_pending_instances::text, '0',
           CASE WHEN my_pending_instances = 0 THEN 'PASS' ELSE 'FAIL' END FROM facts
) t
ORDER BY ord;
