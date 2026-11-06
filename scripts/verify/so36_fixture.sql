-- ======================================================
-- SO-36 佣金计提 E2E 夹具（**只用于隔离库 cloudpivot_so36**，由 so36_curl_evidence.sh 调用）
--
-- 为什么要 SQL 夹具而不是全走 API：销售链上游（订单 → 发货过账 → 出库 → 应收）需要
-- 物料 / 仓库 / 单位 / 库存 / 凭证规则一整套前置数据，那几段链路由 SO-04/SO-10/SO-14
-- 各自的卡验证过（含真库 e2e），本卡要验证的是「**佣金**按已收款计提是否正确」，
-- 因此把上游结果直接摆成事实：订单 + 发货 + 出库 + 应收（含已收金额）。
--
-- 场景矩阵（一张表看全）：
--
-- | 场景 | 销售员 | 订单/应收 | 已收 | 规则 | 期望 |
-- |------|--------|-----------|------|------|------|
-- | A 规则优先级 | SP1 | SO-A / AR-A 1000（M1 600 + M2 400） | 400 | SP1 20% + M1 10% + 全局 5% | 基数 400，佣金 80（20% 胜出）|
-- | B 封顶 | SP2 | SO-B / AR-B 1000（M1 1000） | 1000 | M1 10% 封顶 60 | 基数 1000，佣金 60（封顶生效）|
-- | C 未收款不计 | SP1 | SO-C / AR-C 500 | 0 | — | 不出现任何明细 |
-- | D 退货/退款冲回 | SP2 | AR-B 已收 1000 → 600 | −400 | M1 10% 封顶 60 | 基数 −400，佣金 −40 |
--
-- 幂等：先按 `__SO36` 前缀清场再插，可反复执行。
-- ======================================================
\set ON_ERROR_STOP on

BEGIN;

-- ---------- 0. 清场（隔离库专用；顺序按外键） ----------
DELETE FROM biz_commission_item
 WHERE settlement_id IN (SELECT id FROM biz_commission_settlement);
DELETE FROM biz_commission_settlement;
DELETE FROM biz_commission_rule;
DELETE FROM biz_receivable  WHERE ar_no    LIKE '__SO36%';
DELETE FROM biz_stock_out   WHERE out_no   LIKE '__SO36%';
DELETE FROM biz_shipment    WHERE ship_no  LIKE '__SO36%';
DELETE FROM biz_sales_order WHERE so_no    LIKE '__SO36%';
DELETE FROM biz_material    WHERE material_code LIKE 'SO36-%';
DELETE FROM biz_customer    WHERE code = 'SO36-C1';
DELETE FROM biz_warehouse   WHERE warehouse_code = 'SO36-WH';
DELETE FROM biz_unit        WHERE unit_code = 'SO36-PC';
DELETE FROM sys_user        WHERE username LIKE 'so36_sp%';

-- ---------- 1. 主数据：单位 / 仓库 / 客户 ----------
INSERT INTO biz_unit (id, unit_code, unit_name, is_base, base_unit_id, rate, status,
                      sort_order, is_system, precision)
VALUES ('e3600000-0000-4000-8000-000000000001', 'SO36-PC', '个(SO36)', 1, NULL, 1, 1, 0, FALSE, 2);

INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_type, allow_negative,
                           status, sort_order, level, warehouse_path, is_leaf)
VALUES ('e3600000-0000-4000-8000-000000000002', 'SO36-WH', 'SO36 测试仓', 'NORMAL', 0,
        1, 0, 1, '/SO36-WH', 1);

INSERT INTO biz_customer (id, code, name, credit_limit, status, credit_days, remark)
VALUES ('e3600000-0000-4000-8000-000000000003', 'SO36-C1', 'SO36 佣金测试客户', 0, 'ACTIVE', 30,
        'SO-36 佣金 E2E 夹具');

-- ---------- 2. 两个销售员（口令哈希 / user_type 复制现有账号，不新建凭据） ----------
INSERT INTO sys_user (id, username, password, real_name, status, user_type, token_version,
                      login_fail_count, is_locked, must_change_password, dept_id)
SELECT 'e3600000-0000-4000-8000-000000000011', 'so36_sp1', password, 'SO36 销售员甲', 1,
       user_type, 1, 0, FALSE, FALSE, dept_id
  FROM sys_user WHERE deleted_at IS NULL ORDER BY created_at LIMIT 1;

INSERT INTO sys_user (id, username, password, real_name, status, user_type, token_version,
                      login_fail_count, is_locked, must_change_password, dept_id)
SELECT 'e3600000-0000-4000-8000-000000000012', 'so36_sp2', password, 'SO36 销售员乙', 1,
       user_type, 1, 0, FALSE, FALSE, dept_id
  FROM sys_user WHERE deleted_at IS NULL ORDER BY created_at LIMIT 1;

-- ---------- 3. 物料（M1 走物料级规则 + 封顶；M2 只吃全局 / 销售员规则） ----------
INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type, status)
VALUES ('e3600000-0000-4000-8000-000000000021', 'SO36-M1', 'SO36 物料一', 'e3600000-0000-4000-8000-000000000001', 'FINISHED', 1),
       ('e3600000-0000-4000-8000-000000000022', 'SO36-M2', 'SO36 物料二', 'e3600000-0000-4000-8000-000000000001', 'RAW', 1);

-- ---------- 4. 订单 / 发货 / 出库 / 应收（四跳链路，佣金计提主查询路径） ----------
-- 4.1 SO-A（SP1；M1 600 + M2 400 = 1000）
INSERT INTO biz_sales_order (id, so_no, customer_id, order_date, status,
                             total_amount, total_tax, total_with_tax, salesperson_id)
VALUES ('e3600000-0000-4000-8000-000000000031', '__SO36-SO-A__',
        'e3600000-0000-4000-8000-000000000003', DATE '2026-11-02', 'APPROVED',
        1000, 0, 1000, 'e3600000-0000-4000-8000-000000000011');
INSERT INTO biz_sales_order_item (so_id, line_no, material_id, qty, price, amount, tax_rate, tax_amount, delivered_qty)
VALUES ('e3600000-0000-4000-8000-000000000031', 1, 'e3600000-0000-4000-8000-000000000021', 60, 10, 600, 0, 0, 60),
       ('e3600000-0000-4000-8000-000000000031', 2, 'e3600000-0000-4000-8000-000000000022', 40, 10, 400, 0, 0, 40);

-- 4.2 SO-B（SP2；M1 1000）
INSERT INTO biz_sales_order (id, so_no, customer_id, order_date, status,
                             total_amount, total_tax, total_with_tax, salesperson_id)
VALUES ('e3600000-0000-4000-8000-000000000032', '__SO36-SO-B__',
        'e3600000-0000-4000-8000-000000000003', DATE '2026-11-03', 'APPROVED',
        1000, 0, 1000, 'e3600000-0000-4000-8000-000000000012');
INSERT INTO biz_sales_order_item (so_id, line_no, material_id, qty, price, amount, tax_rate, tax_amount, delivered_qty)
VALUES ('e3600000-0000-4000-8000-000000000032', 1, 'e3600000-0000-4000-8000-000000000021', 100, 10, 1000, 0, 0, 100);

-- 4.3 SO-C（SP1；一分未收 —— 必须不计佣）
INSERT INTO biz_sales_order (id, so_no, customer_id, order_date, status,
                             total_amount, total_tax, total_with_tax, salesperson_id)
VALUES ('e3600000-0000-4000-8000-000000000033', '__SO36-SO-C__',
        'e3600000-0000-4000-8000-000000000003', DATE '2026-11-04', 'APPROVED',
        500, 0, 500, 'e3600000-0000-4000-8000-000000000011');
INSERT INTO biz_sales_order_item (so_id, line_no, material_id, qty, price, amount, tax_rate, tax_amount, delivered_qty)
VALUES ('e3600000-0000-4000-8000-000000000033', 1, 'e3600000-0000-4000-8000-000000000021', 50, 10, 500, 0, 0, 50);

-- 4.4 发货（一张订单一次发货；stock_out.source_id = shipment.id，source_type='SALES'）
INSERT INTO biz_shipment (id, ship_no, so_id, customer_id, warehouse_id, ship_date, status, total_qty, stock_out_id)
VALUES ('e3600000-0000-4000-8000-000000000041', '__SO36-SH-A__', 'e3600000-0000-4000-8000-000000000031',
        'e3600000-0000-4000-8000-000000000003', 'e3600000-0000-4000-8000-000000000002', DATE '2026-11-02', 'POSTED', 100,
        'e3600000-0000-4000-8000-000000000051'),
       ('e3600000-0000-4000-8000-000000000042', '__SO36-SH-B__', 'e3600000-0000-4000-8000-000000000032',
        'e3600000-0000-4000-8000-000000000003', 'e3600000-0000-4000-8000-000000000002', DATE '2026-11-03', 'POSTED', 100,
        'e3600000-0000-4000-8000-000000000052'),
       ('e3600000-0000-4000-8000-000000000043', '__SO36-SH-C__', 'e3600000-0000-4000-8000-000000000033',
        'e3600000-0000-4000-8000-000000000003', 'e3600000-0000-4000-8000-000000000002', DATE '2026-11-04', 'POSTED', 50,
        'e3600000-0000-4000-8000-000000000053');

INSERT INTO biz_stock_out (id, out_no, out_type, warehouse_id, out_date, status, total_qty,
                           total_amount, source_type, source_id)
VALUES ('e3600000-0000-4000-8000-000000000051', '__SO36-OUT-A__', 'OTHER', 'e3600000-0000-4000-8000-000000000002',
        DATE '2026-11-02', 'POSTED', 100, 0, 'SALES', 'e3600000-0000-4000-8000-000000000041'),
       ('e3600000-0000-4000-8000-000000000052', '__SO36-OUT-B__', 'OTHER', 'e3600000-0000-4000-8000-000000000002',
        DATE '2026-11-03', 'POSTED', 100, 0, 'SALES', 'e3600000-0000-4000-8000-000000000042'),
       ('e3600000-0000-4000-8000-000000000053', '__SO36-OUT-C__', 'OTHER', 'e3600000-0000-4000-8000-000000000002',
        DATE '2026-11-04', 'POSTED', 50, 0, 'SALES', 'e3600000-0000-4000-8000-000000000043');

-- 4.5 应收（AR-A 已收 400 / AR-B 已收 1000 / AR-C 已收 0）
INSERT INTO biz_receivable (id, ar_no, customer_id, source_type, source_id, source_no, currency_code,
                            amount, received_amount, unsettled_amount, credit_days, due_date,
                            period_code, status, settled_at, remark)
VALUES ('e3600000-0000-4000-8000-000000000061', '__SO36-AR-A__', 'e3600000-0000-4000-8000-000000000003',
        'SALES_OUT', 'e3600000-0000-4000-8000-000000000051', '__SO36-OUT-A__', 'CNY',
        1000, 400, 600, 30, DATE '2026-12-02', '2026-11', 'PARTIAL', NULL, 'SO-36 夹具'),
       ('e3600000-0000-4000-8000-000000000062', '__SO36-AR-B__', 'e3600000-0000-4000-8000-000000000003',
        'SALES_OUT', 'e3600000-0000-4000-8000-000000000052', '__SO36-OUT-B__', 'CNY',
        1000, 1000, 0, 30, DATE '2026-12-03', '2026-11', 'SETTLED', now(), 'SO-36 夹具'),
       ('e3600000-0000-4000-8000-000000000063', '__SO36-AR-C__', 'e3600000-0000-4000-8000-000000000003',
        'SALES_OUT', 'e3600000-0000-4000-8000-000000000053', '__SO36-OUT-C__', 'CNY',
        500, 0, 500, 30, DATE '2026-12-04', '2026-11', 'OPEN', NULL, 'SO-36 夹具：一分未收');

-- ---------- 5. 佣金规则（三条，专测优先级 + 封顶） ----------
INSERT INTO biz_commission_rule (id, rule_name, scope_type, scope_id, calc_type, rate,
                                 min_amount, max_amount, effective_from, status, remark)
VALUES ('e3600000-0000-4000-8000-000000000071', 'SO36 全局 5%', 'ALL', NULL, 'PERCENT', 5, 0, NULL,
        DATE '2026-01-01', 'ACTIVE', 'SO-36 夹具：全局兜底'),
       ('e3600000-0000-4000-8000-000000000072', 'SO36 物料一 10% 封顶 60', 'MATERIAL_CATEGORY',
        'e3600000-0000-4000-8000-000000000021', 'PERCENT', 10, 0, 60,
        DATE '2026-01-01', 'ACTIVE',
        'SO-36 夹具：物料级（基础版无分类表，scope_id = biz_material.id）+ 单行封顶 60'),
       ('e3600000-0000-4000-8000-000000000073', 'SO36 销售员甲 20%', 'SALESPERSON',
        'e3600000-0000-4000-8000-000000000011', 'PERCENT', 20, 0, NULL,
        DATE '2026-01-01', 'ACTIVE', 'SO-36 夹具：销售员级，必须压过物料级与全局');

-- ---------- 6. 放开 admin 的「首登必须改密」（新库首启会给 admin 置 must_change_password） ----------
-- 隔离实例专用：真实环境改密是为了安全，这里只是让 curl 留证能打到业务接口。
UPDATE sys_user SET must_change_password = FALSE WHERE username = 'admin';

COMMIT;

-- 探针：夹具就位
SELECT 'customers' AS what, count(*) FROM biz_customer WHERE code = 'SO36-C1'
UNION ALL SELECT 'orders', count(*) FROM biz_sales_order WHERE so_no LIKE '__SO36%'
UNION ALL SELECT 'receivables', count(*) FROM biz_receivable WHERE ar_no LIKE '__SO36%'
UNION ALL SELECT 'rules', count(*) FROM biz_commission_rule WHERE rule_name LIKE 'SO36%'
UNION ALL SELECT 'salespersons', count(*) FROM sys_user WHERE username LIKE 'so36_sp%';
