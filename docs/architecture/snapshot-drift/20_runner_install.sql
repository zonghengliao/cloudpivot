-- ============================================================
-- 20_runner_install.sql —— 巡检执行层的「登记与视图」安装（幂等，随发版一次性执行）
--
-- 面向实现卡（本卡只出设计）：下面只是对象定义；每轮巡检跑的是 21_run_once.sql。
-- ============================================================

BEGIN;

-- ============================================================
-- ① 巡检项登记表（作为视图存在：它是设计事实，不是业务数据，不该有第二份存储）
--    needs_contract = true 的项，在合同 schema 未就位时必须记 SKIPPED（0 行 ≠ 无缺陷）
-- ============================================================
CREATE OR REPLACE VIEW di.v_check_registry AS
SELECT * FROM (VALUES
  ('DI-01','采购单供应商快照 vs 合同乙方',  'P0/P1/INFO','PURCHASE_ORDER', true,  'biz_contract.party_b_id'),
  ('DI-02','采购单引用合同已软删',          'P0/P1',     'PURCHASE_ORDER', true,  'biz_contract.deleted_at'),
  ('DI-03','采购单 contract_id 悬空',       'P0',        'PURCHASE_ORDER', true,  'biz_contract.id'),
  ('DI-04','CONTRACT 采购缺 contract_id',   'P1',        'PURCHASE_ORDER', true,  'ck_biz_po_contract_pair'),
  ('DI-05','SPOT 采购缺 supplier_id',       'P1',        'PURCHASE_ORDER', true,  'supplier_id NOT NULL'),
  ('DI-06','入库单供应商 vs 来源采购单供应商','P0/P1',     'STOCK_IN',       false, 'biz_purchase_order.supplier_id'),
  ('DI-07','入库单来源采购单已软删',        'P0/P1',     'STOCK_IN',       false, 'biz_purchase_order.deleted_at'),
  ('DI-08','入库单来源采购单悬空',          'P0',        'STOCK_IN',       false, 'biz_purchase_order.id'),
  ('DI-09','采购入库缺 supplier_id',        'P1',        'STOCK_IN',       false, 'ck_biz_si_supplier'),
  ('DI-10','应付供应商 vs 入库单供应商',    'P0',        'PAYABLE',        false, 'biz_stock_in.supplier_id'),
  ('DI-11','应付来源入库单悬空/已删',       'P0',        'PAYABLE',        false, 'biz_stock_in.id'),
  ('DI-12','采购单供应商主档悬空/已删',     'P0/P1',     'PURCHASE_ORDER', false, 'biz_supplier.deleted_at'),
  ('DI-13','同一合同下快照互斥',            'P1',        'CONTRACT',       true,  'biz_contract.party_b_id'),
  ('DI-14','结构护栏缺失（巡检自身可信度）','P0',        'SYSTEM',         false, 'pg_constraint')
) AS t(issue_code, check_name, priority_rule, entity_type, needs_contract, truth_source);

COMMENT ON VIEW di.v_check_registry IS
  '巡检项清单（设计事实）：issue_code / 名称 / 分级规则 / 主体类型 / 是否依赖合同 schema / 真相来源';

CREATE OR REPLACE VIEW di.v_check_state AS
SELECT r.issue_code, r.check_name, r.priority_rule, r.entity_type, r.truth_source,
       CASE WHEN r.needs_contract AND NOT c.contract_checks_ready
            THEN 'SKIPPED' ELSE 'CHECKED' END AS run_state
  FROM di.v_check_registry r
 CROSS JOIN di.di_capability c;

COMMENT ON VIEW di.v_check_state IS
  '本轮各巡检项的执行状态：SKIPPED 的项既不写入结果，也不参与收敛（否则 schema 未就绪会误清历史缺陷）';

-- ============================================================
-- ② 本轮全量缺陷集（14 条巡检项的并集；合同类在未就绪阶段天然 0 行）
-- ============================================================
CREATE OR REPLACE VIEW di.v_drift_now AS
SELECT * FROM di.c_di01 WHERE (SELECT contract_checks_ready FROM di.di_capability)
UNION ALL SELECT * FROM di.c_di02 WHERE (SELECT contract_checks_ready FROM di.di_capability)
UNION ALL SELECT * FROM di.c_di03 WHERE (SELECT contract_checks_ready FROM di.di_capability)
UNION ALL SELECT * FROM di.c_di04 WHERE (SELECT contract_checks_ready FROM di.di_capability)
UNION ALL SELECT * FROM di.c_di05 WHERE (SELECT contract_checks_ready FROM di.di_capability)
UNION ALL SELECT * FROM di.c_di06
UNION ALL SELECT * FROM di.c_di07
UNION ALL SELECT * FROM di.c_di08
UNION ALL SELECT * FROM di.c_di09
UNION ALL SELECT * FROM di.c_di10
UNION ALL SELECT * FROM di.c_di11
UNION ALL SELECT * FROM di.c_di12
UNION ALL SELECT * FROM di.c_di13 WHERE (SELECT contract_checks_ready FROM di.di_capability)
UNION ALL SELECT * FROM di.c_di14;

COMMENT ON VIEW di.v_drift_now IS
  '本轮全量未决缺陷（只读）：运维可直接 select * 复核；定时任务用它一条语句落库';

COMMIT;
