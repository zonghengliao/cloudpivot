-- ======================================================
-- V0335 回滚：恢复 title 原文 + sort_order 原值（MENU-FIX-1 / t_95bc00b0）
--
-- 说明：本迁移只改展示层（title / sort_order），回滚就是把 24 条 title 与 5 条
--       sort_order 还原成迁移前实测值。同样按 (name, path) 双键定位、带差异判断，可重放。
--       注意：回滚会把 22 条「功能说明式」超长 title 和政府引用一起放回来，
--       只在排障/对照时使用，生产不要回滚本卡。
--
-- 两条**故意不回滚**（回滚只会让生产变乱，不是「恢复原状」）：
--   * /flow/def（流程定义）：生产迁移前就是 6 字「审批流程定义」（当年手工改的），
--     迁移链里才是 20 字；回滚成 20 字等于把生产弄坏。
--   * /business/erp/purchase-order（采购订单 sort_order）：生产迁移前就是 10，
--     链里才是 40（与 供应商 撞）；回滚成 40 同样等于把生产弄坏。
-- ======================================================

-- ── 一、title 还原 ────────────────────────────────────────────────
UPDATE sys_menu SET title = '即时 / 月末汇率维护 + 按月批量录入（ARCH-7 §15.6.3 ②）', updated_at = now()
 WHERE deleted_at IS NULL AND name = '汇率管理' AND path = '/business/erp/exchange-rate'
   AND title IS DISTINCT FROM '即时 / 月末汇率维护 + 按月批量录入（ARCH-7 §15.6.3 ②）';

UPDATE sys_menu SET title = '客户协议价 + 阶梯/促销价维护（取价链见 ARCH-7 §15.3.3）', updated_at = now()
 WHERE deleted_at IS NULL AND name = '价格管理' AND path = '/business/erp/customer-price'
   AND title IS DISTINCT FROM '客户协议价 + 阶梯/促销价维护（取价链见 ARCH-7 §15.3.3）';

UPDATE sys_menu SET title = '采购退货：选入库单带明细 → 审批 → 库存回冲 + 应付红字 + 凭证', updated_at = now()
 WHERE deleted_at IS NULL AND name = '采购退货' AND path = '/business/erp/purchase-return'
   AND title IS DISTINCT FROM '采购退货：选入库单带明细 → 审批 → 库存回冲 + 应付红字 + 凭证';

UPDATE sys_menu SET title = '询价 → 多家报价 → 比价 → 定标 → 转采购订单（P2-01）', updated_at = now()
 WHERE deleted_at IS NULL AND name = '询比价' AND path = '/business/erp/price-inquiry'
   AND title IS DISTINCT FROM '询价 → 多家报价 → 比价 → 定标 → 转采购订单（P2-01）';

UPDATE sys_menu SET title = '币种主数据 + 本位币标记（唯一；ARCH-7 §15.6.3 ①）', updated_at = now()
 WHERE deleted_at IS NULL AND name = '币种管理' AND path = '/business/erp/currency'
   AND title IS DISTINCT FROM '币种主数据 + 本位币标记（唯一；ARCH-7 §15.6.3 ①）';

UPDATE sys_menu SET title = '销售目标维护 + 达成率看板（按订单金额 / 按已收款金额两个口径；ARCH-7 §15.8）', updated_at = now()
 WHERE deleted_at IS NULL AND name = '销售目标与预测' AND path = '/business/erp/sales-target'
   AND title IS DISTINCT FROM '销售目标维护 + 达成率看板（按订单金额 / 按已收款金额两个口径；ARCH-7 §15.8）';

UPDATE sys_menu SET title = '报表项维护（报表项 ↔ 科目映射 / 取数公式 / 排序）', updated_at = now()
 WHERE deleted_at IS NULL AND name = '报表项维护' AND path = '/business/erp/report/report-item'
   AND title IS DISTINCT FROM '报表项维护（报表项 ↔ 科目映射 / 取数公式 / 排序）';

UPDATE sys_menu SET title = '部门提需求 → 审批 → 转采购订单（支持多单合并采购）', updated_at = now()
 WHERE deleted_at IS NULL AND name = '采购申请' AND path = '/business/erp/purchase-requisition'
   AND title IS DISTINCT FROM '部门提需求 → 审批 → 转采购订单（支持多单合并采购）';

UPDATE sys_menu SET title = '跨单据联动规则（源单据 + 触发状态 → 白名单动作）', updated_at = now()
 WHERE deleted_at IS NULL AND name = '联动规则' AND path = '/flow/linkage'
   AND title IS DISTINCT FROM '跨单据联动规则（源单据 + 触发状态 → 白名单动作）';

UPDATE sys_menu SET title = '费用类型（科目 / 可抵扣 / 受预算 / 上限）', updated_at = now()
 WHERE deleted_at IS NULL AND name = '费用类型' AND path = '/business/erp/expense-type'
   AND title IS DISTINCT FROM '费用类型（科目 / 可抵扣 / 受预算 / 上限）';

UPDATE sys_menu SET title = '我的报销（新建 / 提交 / 撤回 / 作废）', updated_at = now()
 WHERE deleted_at IS NULL AND name = '我的报销' AND path = '/business/erp/expense'
   AND title IS DISTINCT FROM '我的报销（新建 / 提交 / 撤回 / 作废）';

UPDATE sys_menu SET title = '费用报销（报销单 / 待付款 / 费用类型）', updated_at = now()
 WHERE deleted_at IS NULL AND name = '费用报销' AND path = '/business/erp/expense'
   AND title IS DISTINCT FROM '费用报销（报销单 / 待付款 / 费用类型）';

UPDATE sys_menu SET title = '折旧计提（月度批次 / 凭证 / 红冲）', updated_at = now()
 WHERE deleted_at IS NULL AND name = '折旧计提' AND path = '/business/erp/depreciation'
   AND title IS DISTINCT FROM '折旧计提（月度批次 / 凭证 / 红冲）';

UPDATE sys_menu SET title = '预算管理（编制 / 执行 / 科目映射）', updated_at = now()
 WHERE deleted_at IS NULL AND name = '预算管理' AND path = '/business/erp/budget'
   AND title IS DISTINCT FROM '预算管理（编制 / 执行 / 科目映射）';

UPDATE sys_menu SET title = '资产分类（年限 / 残值率 / 科目）', updated_at = now()
 WHERE deleted_at IS NULL AND name = '资产分类' AND path = '/business/erp/asset-category'
   AND title IS DISTINCT FROM '资产分类（年限 / 残值率 / 科目）';

UPDATE sys_menu SET title = '预算执行（已用 / 可用 / 执行率）', updated_at = now()
 WHERE deleted_at IS NULL AND name = '预算执行' AND path = '/business/erp/budget/report'
   AND title IS DISTINCT FROM '预算执行（已用 / 可用 / 执行率）';

UPDATE sys_menu SET title = '资产卡片（建卡 / 转移 / 处置）', updated_at = now()
 WHERE deleted_at IS NULL AND name = '资产卡片' AND path = '/business/erp/asset'
   AND title IS DISTINCT FROM '资产卡片（建卡 / 转移 / 处置）';

UPDATE sys_menu SET title = '固定资产（卡片 / 分类 / 折旧）', updated_at = now()
 WHERE deleted_at IS NULL AND name = '固定资产' AND path = '/business/erp/asset'
   AND title IS DISTINCT FROM '固定资产（卡片 / 分类 / 折旧）';

UPDATE sys_menu SET title = '预算编制（部门 × 科目 × 期间）', updated_at = now()
 WHERE deleted_at IS NULL AND name = '预算编制' AND path = '/business/erp/budget'
   AND title IS DISTINCT FROM '预算编制（部门 × 科目 × 期间）';

UPDATE sys_menu SET title = '预算科目映射（采购 → 预算科目）', updated_at = now()
 WHERE deleted_at IS NULL AND name = '预算科目映射' AND path = '/business/erp/budget/mapping'
   AND title IS DISTINCT FROM '预算科目映射（采购 → 预算科目）';

UPDATE sys_menu SET title = '合同管理（采购/销售/服务/租赁）', updated_at = now()
 WHERE deleted_at IS NULL AND name = '合同管理' AND path = '/business/erp/contract'
   AND title IS DISTINCT FROM '合同管理（采购/销售/服务/租赁）';

UPDATE sys_menu SET title = '待付款（已审批未付清的报销单）', updated_at = now()
 WHERE deleted_at IS NULL AND name = '待付款' AND path = '/business/erp/expense/pay'
   AND title IS DISTINCT FROM '待付款（已审批未付清的报销单）';

UPDATE sys_menu SET title = '收款退款单：RR-yyyyMMdd-000001 / DRAFT-CONFIRMED-POSTED-CANCELLED；退款过账 借 1122 应收 贷 1002 银行', updated_at = now()
 WHERE deleted_at IS NULL AND name = '收款退款单' AND (path IS NULL OR path = '')
   AND title IS DISTINCT FROM '收款退款单：RR-yyyyMMdd-000001 / DRAFT-CONFIRMED-POSTED-CANCELLED；退款过账 借 1122 应收 贷 1002 银行';

UPDATE sys_menu SET title = '取价解析（5 级确定性取价：客户协议价 → 客户专属阶梯 → 通用阶梯 → 物料标准售价 → 无价）。path/component/可见性由价格页面卡落地后补', updated_at = now()
 WHERE deleted_at IS NULL AND name = '价格查询' AND (path IS NULL OR path = '')
   AND title IS DISTINCT FROM '取价解析（5 级确定性取价：客户协议价 → 客户专属阶梯 → 通用阶梯 → 物料标准售价 → 无价）。path/component/可见性由价格页面卡落地后补';

-- ── 二、sort_order 还原 ──────────────────────────────────────────
UPDATE sys_menu SET sort_order = 47, updated_at = now()
 WHERE deleted_at IS NULL AND name = 'business:erp:report:payable-aging' AND sort_order = 48;
UPDATE sys_menu SET sort_order = 48, updated_at = now()
 WHERE deleted_at IS NULL AND name = '报表项维护' AND path = '/business/erp/report/report-item' AND sort_order = 49;
UPDATE sys_menu SET sort_order = 70, updated_at = now()
 WHERE deleted_at IS NULL AND name = '销售退货单' AND path = '/business/erp/sales-return' AND sort_order = 72;
UPDATE sys_menu SET sort_order = 80, updated_at = now()
 WHERE deleted_at IS NULL AND name = '币种管理' AND path = '/business/erp/currency' AND sort_order = 82;
UPDATE sys_menu SET sort_order = 90, updated_at = now()
 WHERE deleted_at IS NULL AND name = '汇率管理' AND path = '/business/erp/exchange-rate' AND sort_order = 92;
