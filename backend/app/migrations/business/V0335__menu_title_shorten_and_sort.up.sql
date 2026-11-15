-- ======================================================
-- V0335：菜单 title 改短 + sort_order 去重（卡 MENU-FIX-1 / t_95bc00b0）
--
-- 问题（生产 2026-11-08 实测）：
--   1) 22 条业务菜单的 title 被写成了「功能说明文档」，最长 38 字，侧栏换行/截断；
--      还夹着 `ARCH-7 §15.6.3`、`P2-01` 这类开发文档引用，终端用户看不懂。
--   2) 另有 1 条只在迁移链里是长名（/flow/def 流程定义，生产侧当年手工改短过、链里没写）；
--      再另有 2 条历史占位行（name=价格查询 / 收款退款单，path 为空、不进侧栏）
--      的 title 是更长的开发备注（79 / 84 字），属同一缺陷，一并收短。
--   3) 4 个父目录下存在重复 sort_order，导致侧栏顺序在刷新间不稳定。
--
-- 口径（本卡确定，后续卡照此办）：
--   * title 只写菜单名（2~6 字）；功能说明归页面/文档，不进 title。
--   * `name` 字段一律不动（它是菜单的唯一业务键，权限/路由都按它对齐），只改 title。
--   * 定位一律用 (name, path) 双键：`/business/erp/expense`、`/business/erp/asset`、
--     `/business/erp/budget` 三条 path 各自挂着「目录行(menu_type=1) + 页面行(menu_type=2)」，
--     只按 path 定会一次改掉两行（已实测：这三条 path 下各 2 行，非重复菜单）。
--
-- 幂等：每条 UPDATE 都带 `IS DISTINCT FROM`（或目标值差异）判断，
--       重复执行 0 行改动；末尾两个 DO 块在改完后自检，不通过直接 RAISE。
-- ======================================================

-- ── 一、title 改短（22 条 + /flow/def + 2 条占位行 = 25 条）────────────
UPDATE sys_menu SET title = '汇率管理', updated_at = now()
 WHERE deleted_at IS NULL AND name = '汇率管理' AND path = '/business/erp/exchange-rate'
   AND title IS DISTINCT FROM '汇率管理';

UPDATE sys_menu SET title = '客户价格', updated_at = now()
 WHERE deleted_at IS NULL AND name = '价格管理' AND path = '/business/erp/customer-price'
   AND title IS DISTINCT FROM '客户价格';

UPDATE sys_menu SET title = '采购退货', updated_at = now()
 WHERE deleted_at IS NULL AND name = '采购退货' AND path = '/business/erp/purchase-return'
   AND title IS DISTINCT FROM '采购退货';

UPDATE sys_menu SET title = '询比价', updated_at = now()
 WHERE deleted_at IS NULL AND name = '询比价' AND path = '/business/erp/price-inquiry'
   AND title IS DISTINCT FROM '询比价';

UPDATE sys_menu SET title = '币种管理', updated_at = now()
 WHERE deleted_at IS NULL AND name = '币种管理' AND path = '/business/erp/currency'
   AND title IS DISTINCT FROM '币种管理';

UPDATE sys_menu SET title = '销售目标', updated_at = now()
 WHERE deleted_at IS NULL AND name = '销售目标与预测' AND path = '/business/erp/sales-target'
   AND title IS DISTINCT FROM '销售目标';

UPDATE sys_menu SET title = '报表项维护', updated_at = now()
 WHERE deleted_at IS NULL AND name = '报表项维护' AND path = '/business/erp/report/report-item'
   AND title IS DISTINCT FROM '报表项维护';

UPDATE sys_menu SET title = '采购申请', updated_at = now()
 WHERE deleted_at IS NULL AND name = '采购申请' AND path = '/business/erp/purchase-requisition'
   AND title IS DISTINCT FROM '采购申请';

UPDATE sys_menu SET title = '联动规则', updated_at = now()
 WHERE deleted_at IS NULL AND name = '联动规则' AND path = '/flow/linkage'
   AND title IS DISTINCT FROM '联动规则';

UPDATE sys_menu SET title = '费用类型', updated_at = now()
 WHERE deleted_at IS NULL AND name = '费用类型' AND path = '/business/erp/expense-type'
   AND title IS DISTINCT FROM '费用类型';

UPDATE sys_menu SET title = '我的报销', updated_at = now()
 WHERE deleted_at IS NULL AND name = '我的报销' AND path = '/business/erp/expense'
   AND title IS DISTINCT FROM '我的报销';

UPDATE sys_menu SET title = '费用报销', updated_at = now()
 WHERE deleted_at IS NULL AND name = '费用报销' AND path = '/business/erp/expense'
   AND title IS DISTINCT FROM '费用报销';

UPDATE sys_menu SET title = '折旧计提', updated_at = now()
 WHERE deleted_at IS NULL AND name = '折旧计提' AND path = '/business/erp/depreciation'
   AND title IS DISTINCT FROM '折旧计提';

UPDATE sys_menu SET title = '预算管理', updated_at = now()
 WHERE deleted_at IS NULL AND name = '预算管理' AND path = '/business/erp/budget'
   AND title IS DISTINCT FROM '预算管理';

UPDATE sys_menu SET title = '资产分类', updated_at = now()
 WHERE deleted_at IS NULL AND name = '资产分类' AND path = '/business/erp/asset-category'
   AND title IS DISTINCT FROM '资产分类';

UPDATE sys_menu SET title = '预算执行', updated_at = now()
 WHERE deleted_at IS NULL AND name = '预算执行' AND path = '/business/erp/budget/report'
   AND title IS DISTINCT FROM '预算执行';

UPDATE sys_menu SET title = '资产卡片', updated_at = now()
 WHERE deleted_at IS NULL AND name = '资产卡片' AND path = '/business/erp/asset'
   AND title IS DISTINCT FROM '资产卡片';

UPDATE sys_menu SET title = '固定资产', updated_at = now()
 WHERE deleted_at IS NULL AND name = '固定资产' AND path = '/business/erp/asset'
   AND title IS DISTINCT FROM '固定资产';

UPDATE sys_menu SET title = '预算编制', updated_at = now()
 WHERE deleted_at IS NULL AND name = '预算编制' AND path = '/business/erp/budget'
   AND title IS DISTINCT FROM '预算编制';

UPDATE sys_menu SET title = '预算科目映射', updated_at = now()
 WHERE deleted_at IS NULL AND name = '预算科目映射' AND path = '/business/erp/budget/mapping'
   AND title IS DISTINCT FROM '预算科目映射';

UPDATE sys_menu SET title = '合同管理', updated_at = now()
 WHERE deleted_at IS NULL AND name = '合同管理' AND path = '/business/erp/contract'
   AND title IS DISTINCT FROM '合同管理';

UPDATE sys_menu SET title = '待付款', updated_at = now()
 WHERE deleted_at IS NULL AND name = '待付款' AND path = '/business/erp/expense/pay'
   AND title IS DISTINCT FROM '待付款';

-- 23 条标题收短 + 2 条历史占位行（path 为空，不进侧栏；title 是开发备注 79/84 字）
--
-- 第 23 条 /flow/def（流程定义）：生产侧当年是**手工**把 20 字 title 改成 6 字，
-- 迁移链里（V0121）仍是长的 —— 所以「新环境」比生产多一条超长 title。本迁移把这条
-- 也写进链里（目标值取生产的现值，生产侧执行即 0 行改动），新老环境就此收敛。
UPDATE sys_menu SET title = '审批流程定义', updated_at = now()
 WHERE deleted_at IS NULL AND name = '流程定义' AND path = '/flow/def'
   AND title IS DISTINCT FROM '审批流程定义';

UPDATE sys_menu SET title = '收款退款单', updated_at = now()
 WHERE deleted_at IS NULL AND name = '收款退款单' AND (path IS NULL OR path = '')
   AND title IS DISTINCT FROM '收款退款单';

UPDATE sys_menu SET title = '价格查询', updated_at = now()
 WHERE deleted_at IS NULL AND name = '价格查询' AND (path IS NULL OR path = '')
   AND title IS DISTINCT FROM '价格查询';

-- ── 二、sort_order 去重（4 组撞车，共挪 5 行）───────────────────────
-- 原则：主数据在前、业务单据在中、结算在后；只把撞车的往后挪，不重排其他菜单。
-- 财务（/business/erp/finance-mgmt）：
--   45 资产负债表 / 46 利润表 / 47 现金流量表 之后都是报表类，
--   故 应付账龄分析 47→48 紧跟报表组，维护类 报表项维护 48→49 垫在其后（原 48 让位）。
UPDATE sys_menu SET sort_order = 48, updated_at = now()
 WHERE deleted_at IS NULL AND name = 'business:erp:report:payable-aging' AND sort_order = 47;
UPDATE sys_menu SET sort_order = 49, updated_at = now()
 WHERE deleted_at IS NULL AND name = '报表项维护' AND path = '/business/erp/report/report-item' AND sort_order = 48;

-- 采购（/business/erp/purchase-mgmt）：迁移链里 采购订单(V0141) 与 供应商 都是 40，
-- 生产侧早就手工把 采购订单 提到 10（采购单据理应排第一），链里没写 →
-- 「新库」会多出一组重复顺序。本迁移把这条也补进链（生产侧执行 0 行改动）。
UPDATE sys_menu SET sort_order = 10, updated_at = now()
 WHERE deleted_at IS NULL AND name = '采购订单' AND path = '/business/erp/purchase-order' AND sort_order = 40;

-- 销售（/business/erp/sales-mgmt）：70 价格管理 / 80 佣金规则 / 90 佣金结算 保持不动，
-- 分别把 销售退货单 / 币种管理 / 汇率管理 往后挪 2（72 / 82 / 92）。
UPDATE sys_menu SET sort_order = 72, updated_at = now()
 WHERE deleted_at IS NULL AND name = '销售退货单' AND path = '/business/erp/sales-return' AND sort_order = 70;
UPDATE sys_menu SET sort_order = 82, updated_at = now()
 WHERE deleted_at IS NULL AND name = '币种管理' AND path = '/business/erp/currency' AND sort_order = 80;
UPDATE sys_menu SET sort_order = 92, updated_at = now()
 WHERE deleted_at IS NULL AND name = '汇率管理' AND path = '/business/erp/exchange-rate' AND sort_order = 90;

-- ── 三、自检 ───────────────────────────────────────────────────────
-- ① 本卡点名改的 25 条：title 必须等于目标值（行不存在则跳过，兼容新环境无占位行的情形）
DO $$
DECLARE
    bad int;
BEGIN
    WITH t(name, path, want) AS (VALUES
        ('汇率管理',       '/business/erp/exchange-rate',       '汇率管理'),
        ('价格管理',       '/business/erp/customer-price',      '客户价格'),
        ('采购退货',       '/business/erp/purchase-return',     '采购退货'),
        ('询比价',         '/business/erp/price-inquiry',       '询比价'),
        ('币种管理',       '/business/erp/currency',            '币种管理'),
        ('销售目标与预测', '/business/erp/sales-target',        '销售目标'),
        ('报表项维护',     '/business/erp/report/report-item',  '报表项维护'),
        ('采购申请',       '/business/erp/purchase-requisition', '采购申请'),
        ('联动规则',       '/flow/linkage',                     '联动规则'),
        ('费用类型',       '/business/erp/expense-type',        '费用类型'),
        ('我的报销',       '/business/erp/expense',             '我的报销'),
        ('费用报销',       '/business/erp/expense',             '费用报销'),
        ('折旧计提',       '/business/erp/depreciation',        '折旧计提'),
        ('预算管理',       '/business/erp/budget',              '预算管理'),
        ('资产分类',       '/business/erp/asset-category',      '资产分类'),
        ('预算执行',       '/business/erp/budget/report',       '预算执行'),
        ('资产卡片',       '/business/erp/asset',               '资产卡片'),
        ('固定资产',       '/business/erp/asset',               '固定资产'),
        ('预算编制',       '/business/erp/budget',              '预算编制'),
        ('预算科目映射',   '/business/erp/budget/mapping',      '预算科目映射'),
        ('合同管理',       '/business/erp/contract',            '合同管理'),
        ('待付款',         '/business/erp/expense/pay',         '待付款'),
        ('流程定义',       '/flow/def',                         '审批流程定义'),
        ('收款退款单',     '',                                  '收款退款单'),
        ('价格查询',       '',                                  '价格查询')
    )
    SELECT count(*) INTO bad
      FROM t
      JOIN sys_menu m ON m.deleted_at IS NULL AND m.name = t.name AND coalesce(m.path, '') = t.path
     WHERE m.title IS DISTINCT FROM t.want;

    IF bad > 0 THEN
        RAISE EXCEPTION 'V0335 自检失败：% 条点名菜单的 title 未落到目标值', bad;
    END IF;
END $$;

-- ② 目录(1)/页面(2) 层面不允许再有超长 title（12 字 = 侧栏单行上限的余量）
DO $$
DECLARE
    bad int;
    sample text;
BEGIN
    SELECT count(*) INTO bad FROM sys_menu
     WHERE deleted_at IS NULL AND menu_type IN (1, 2) AND length(title) > 12;
    IF bad > 0 THEN
        SELECT string_agg(format('%s(%s 字)', coalesce(path, name), length(title)), '; ')
          INTO sample
          FROM sys_menu
         WHERE deleted_at IS NULL AND menu_type IN (1, 2) AND length(title) > 12;
        RAISE EXCEPTION 'V0335 自检失败：% 条菜单 title 仍超 12 字 → %', bad, sample;
    END IF;
END $$;

-- ③ 同一父节点下页面(menu_type=2)的 sort_order 不得重复（顺序稳定）
DO $$
DECLARE
    bad int;
    sample text;
BEGIN
    SELECT count(*) INTO bad FROM (
        SELECT parent_id, sort_order FROM sys_menu
         WHERE deleted_at IS NULL AND menu_type = 2
         GROUP BY parent_id, sort_order HAVING count(*) > 1
    ) d;
    IF bad > 0 THEN
        SELECT string_agg(format('parent=%s sort=%s', parent_id, sort_order), '; ')
          INTO sample
          FROM (
            SELECT parent_id, sort_order FROM sys_menu
             WHERE deleted_at IS NULL AND menu_type = 2
             GROUP BY parent_id, sort_order HAVING count(*) > 1
          ) d;
        RAISE EXCEPTION 'V0335 自检失败：% 组父节点下 sort_order 仍重复 → %', bad, sample;
    END IF;
END $$;
