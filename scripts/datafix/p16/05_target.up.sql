-- ============================================================================
-- P16 · E2E-FIX-7 全库既有差异清账 —— ① 目标清单（class A：夹具 / 演练残留）
-- ----------------------------------------------------------------------------
-- 本文件只做两件事：
--   1. 建（或刷新）目标表 `datafix_p16_target`：一行 = 一个要整体清掉的 (物料, 仓库)；
--   2. 建视图 `datafix_p16_doomed_rows`：把「即将被删的每一行」摊平成 (表名, 行 jsonb)，
--      供 apply.sh --backup 导出成可精确还原的快照。
--
-- 为什么是「整体清 (物料, 仓库)」而不是按差额补：
--   这些组全部是 145 dev 库里的**夹具/演练残留**（详见每行 reason），共同形态是
--   「成本账 / 库位账 / 流水 / 序列号 四张表里只有一半被写」。补流水等于凭空造业务事实
--   （例如 W2B-M-SN 需要凭空造 2029 件入库），删干净才是诚实的处置；
--   真实数据的「差一点」走 30_restore_missing_txn.up.sql 的补流水路径。
--
-- 幂等：INSERT … ON CONFLICT DO UPDATE + 每次重算行数；重复执行不改变目标集合。
-- 门禁：20_purge 会逐条复核「目标物料没有任何已过账单据明细」「库位账行没被期初库存引用」，
--       任一不成立就整单回滚 —— 防止把业务数据当夹具删掉。
-- ============================================================================
\set ON_ERROR_STOP on

CREATE TABLE IF NOT EXISTS datafix_p16_target (
    material_code  text        NOT NULL,
    warehouse_code text        NOT NULL,
    klass          text        NOT NULL DEFAULT 'A',
    reason         text        NOT NULL,
    inv_rows       int,
    loc_rows       int,
    txn_rows       int,
    serial_rows    int,
    done_at        timestamptz,
    PRIMARY KEY (material_code, warehouse_code)
);

INSERT INTO datafix_p16_target (material_code, warehouse_code, reason) VALUES
 ('W1B-M-001','WH-W1B-LEAF',
  'W1B 分桶夹具：成本账「未分配桶」5 vs 库位账 A-01 3 + A-02 2。B1（按物料×仓库合计）相等所以长期是绿的，只有 B1b 逐桶红'),
 ('P1F2-M1','P1F2-WH',
  'P1F2 出库夹具残留：单据 OUT-20261109-000022 已被硬删，只剩 2 条 OUT 流水（-30）与两账 40；补流水等于凭空造 70 件入库'),
 ('SO14-M1','SO14-WH',
  'SO-14 挂账夹具残留：单据 OUT-20261109-000011 已被硬删，只剩 1 条 OUT 流水（-4）与两账 16；同上'),
 ('W2B-M-SN','WH-W2B-1',
  'W2B 序列号夹具：30 条 SERIAL_* 流水累计 2031 vs 账面 2（夹具只跑流水不重算账）'),
 ('W2C1-M-PLAIN','WH-W2C1',
  'W2C1 序列号夹具「按数量物料 7 件」：只写成本账 7，无流水、无库位账（E2E-FIX-6 已给夹具补库位账行，旧库不会自愈）'),
 ('W2C2-M-PLAIN','W2C2-WH-A',
  'W2C2 序列号夹具：只写成本账 7（未分配桶），无流水、无库位账'),
 ('W2C2-M-RT','W2C2-WH-B',
  'W2C2 退回夹具：成本账 1 + 1 件在库序列号，无流水、无库位账'),
 ('W2C2-M-SN','W2C2-WH-A',
  'W2C2 序列号夹具：成本账 3（未分配桶）vs 库位账 3（W2C2-A-01 桶）→ 逐桶错位；在库序列号 2 ≠ 账面 3（⑤ 一物一码违例）'),
 ('W3B-M-PLAIN','WH-W3B-1',
  'W3B 调拨/结算夹具：成本账 + 库位账各 100，无任何流水（P0-03 的 15_purge 清过同款，后续再跑夹具又长回来）'),
 ('W3B-M-PLAIN','WH-W3B-2',
  'W3B 调拨/结算夹具（调入仓）：成本账 + 库位账各 10，无流水'),
 ('W3B-M-SN','WH-W3B-1',
  'W3B 序列号夹具：成本账 + 库位账各 2 + 2 件在库序列号，无流水'),
 ('P2C1-M1','P2C1-WH',
  'P2C1 收货夹具：只有库位账 633，成本账与流水都没有（B1 破在「库位账多于成本账」一侧）'),
 ('P2C1-M2','P2C1-WH',
  'P2C1 收货夹具：只有库位账 13，成本账与流水都没有')
ON CONFLICT (material_code, warehouse_code)
DO UPDATE SET reason = EXCLUDED.reason, klass = EXCLUDED.klass;

-- 行数快照（每次运行刷新；清完应为 0）
UPDATE datafix_p16_target t SET
    inv_rows = (SELECT count(*) FROM biz_inventory i
                  JOIN biz_material m ON m.id = i.material_id
                  JOIN biz_warehouse w ON w.id = i.warehouse_id
                 WHERE m.material_code = t.material_code AND w.warehouse_code = t.warehouse_code),
    loc_rows = (SELECT count(*) FROM biz_inventory_location l
                  JOIN biz_material m ON m.id = l.material_id
                  JOIN biz_warehouse w ON w.id = l.warehouse_id
                 WHERE m.material_code = t.material_code AND w.warehouse_code = t.warehouse_code),
    txn_rows = (SELECT count(*) FROM biz_inventory_txn x
                  JOIN biz_material m ON m.id = x.material_id
                  JOIN biz_warehouse w ON w.id = x.warehouse_id
                 WHERE m.material_code = t.material_code AND w.warehouse_code = t.warehouse_code),
    serial_rows = (SELECT count(*) FROM biz_material_serial s
                     JOIN biz_material m ON m.id = s.material_id
                     JOIN biz_warehouse w ON w.id = s.warehouse_id
                    WHERE m.material_code = t.material_code AND w.warehouse_code = t.warehouse_code);

-- 即将被删的行（快照源）：四张表 + 孤儿库位账行
CREATE OR REPLACE VIEW datafix_p16_doomed_rows AS
SELECT 'biz_inventory' AS tbl, to_jsonb(i) AS row
  FROM biz_inventory i
  JOIN biz_material  m ON m.id = i.material_id
  JOIN biz_warehouse w ON w.id = i.warehouse_id
  JOIN datafix_p16_target t
    ON t.material_code = m.material_code AND t.warehouse_code = w.warehouse_code
UNION ALL
SELECT 'biz_inventory_location', to_jsonb(l)
  FROM biz_inventory_location l
  JOIN biz_material  m ON m.id = l.material_id
  JOIN biz_warehouse w ON w.id = l.warehouse_id
  JOIN datafix_p16_target t
    ON t.material_code = m.material_code AND t.warehouse_code = w.warehouse_code
UNION ALL
SELECT 'biz_inventory_txn', to_jsonb(x)
  FROM biz_inventory_txn x
  JOIN biz_material  m ON m.id = x.material_id
  JOIN biz_warehouse w ON w.id = x.warehouse_id
  JOIN datafix_p16_target t
    ON t.material_code = m.material_code AND t.warehouse_code = w.warehouse_code
UNION ALL
SELECT 'biz_material_serial', to_jsonb(s)
  FROM biz_material_serial s
  JOIN biz_material  m ON m.id = s.material_id
  JOIN biz_warehouse w ON w.id = s.warehouse_id
  JOIN datafix_p16_target t
    ON t.material_code = m.material_code AND t.warehouse_code = w.warehouse_code
UNION ALL
-- 孤儿库位账行：物料或仓库已在夹具清理时被硬删，库位账行留下来（形态判据，不写物料编码）
SELECT 'biz_inventory_location(孤儿)', to_jsonb(l)
  FROM biz_inventory_location l
 WHERE NOT EXISTS (SELECT 1 FROM biz_material  m WHERE m.id = l.material_id)
    OR NOT EXISTS (SELECT 1 FROM biz_warehouse w WHERE w.id = l.warehouse_id);

\echo '== P16 目标清单（class A）=='
SELECT material_code, warehouse_code, inv_rows, loc_rows, txn_rows, serial_rows,
       coalesce(to_char(done_at,'MM-DD HH24:MI'),'未处理') AS 状态
  FROM datafix_p16_target ORDER BY material_code, warehouse_code;

\echo '== P16 即将删除的行数（按表）=='
SELECT tbl, count(*) FROM datafix_p16_doomed_rows GROUP BY 1 ORDER BY 1;
