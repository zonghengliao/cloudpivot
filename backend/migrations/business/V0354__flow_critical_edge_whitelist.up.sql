-- ============================================================================
-- V0354：主链路「必需边」巡检白名单 —— biz_flow_critical_edge（卡 t_c99b63cd / E2E-FIX-1）
--
-- # 为什么需要这张表
--
--   2026-11-09 03:34（E2E-02 演练期间）有人把 `biz_flow_state_edge` 里
--   `STOCK_OUT DRAFT→POSTED` 改成 `enabled = false`。后果：
--     · 发货过账 409「流转 DRAFT → POSTED 已被状态机配置禁用」
--     · **整条销售链断在发货**（订单 → 发货 → 出库 → 应收全卡住）
--     · 没有任何巡检发现这条边被关；恢复靠演练的人手工改库
--
--   代码侧的真相是 `backend/src/business/shared/state_machine.rs::CRITICAL_EDGES`
--   （禁用要 `confirm_critical = true`，运行时错误文案用它的 `impact`）。
--   但巡检脚本是 bash + psql，**读不到 Rust 常量**，所以把同一份清单种进库：
--     · `scripts/verify/flow_critical_edges.sh` 只读巡检：任何一条 critical 边
--       `enabled = false` → 报警（rc=1），**不自动改回**（避免把人工加严的合法动作覆盖掉）；
--     · 真库单测 `state_machine::tests::critical_edges_match_db_table` 逐行比对
--       本表与 `CRITICAL_EDGES`，漂移（漏一条 / 多一条 / impact 改字）一律红。
--
-- # 为什么建表而不是给 `biz_flow_state_edge` 加 `is_critical` 列
--
--   两者语义不同：
--     `biz_flow_state_edge` = **运维配了什么**（可能根本没有行，边在走 BASE_EDGES 默认行为）；
--     `biz_flow_critical_edge` = **产品说这条边不能关**（与配置行无关，边不存在配置行时
--                                也必须被巡检看见）。
--   合成一列会漏掉「base 边还没被配置过」的那一半 —— 而 E2E-02 的坑正是「配置行存在且被关」，
--   两者必须能分别查。另外这张表是**只读清单**，谁都不许在应用里写它（没有 handler / 没有
--   菜单入口），只有迁移能改 —— 想改必须加新迁移 + 同步改 Rust 常量（单测会挡漂移）。
--
-- # 清单口径（与 CRITICAL_EDGES 逐行一致，共 25 条）
--
--   `impact` 是**给用户看的业务后果**，被三处共用，所以措辞固定成从句：
--     ① 禁用的 400 报文：「这是主链路必需边（STOCK_OUT DRAFT→POSTED），禁用它会让{impact}」
--     ② 运行时 409 报文：「{impact}：该操作已被系统配置禁用，请联系系统管理员」
--     ③ 巡检报警：「{entity} {from}→{to} 被禁用 → {impact}」
--   因此 `impact` **不带句尾标点**（句号 / 逗号会被拼进句子中间）。
--
--   卡面原话里 `STOCK_TRANSFER DRAFT → DONE` 在 BASE_EDGES 里不存在（调拨必须
--   `DRAFT→PENDING→APPROVED→IN_TRANSIT→DONE`），按真实边表补成四段；
--   `SALES_ORDER` 在库里的实体值是 `SALE_ORDER`（`DocKind::SaleOrder::as_str()`），
--   按代码口径写，否则巡检按不存在的实体名查，永远 0 行 —— 那是「巡检绿但瞎」。
--
-- # 幂等
--
--   `CREATE TABLE IF NOT EXISTS` + `INSERT ... ON CONFLICT DO NOTHING`：重复执行无副作用。
--   **不要**在 down 里删表数据以外的东西；down 直接 DROP TABLE（这张表是纯白名单，
--   应用代码不读它落业务数据，回滚不影响任何单据）。
-- ============================================================================

CREATE TABLE IF NOT EXISTS biz_flow_critical_edge (
    entity      varchar(32) NOT NULL,
    from_status varchar(32) NOT NULL,
    to_status   varchar(32) NOT NULL,
    impact      varchar(128) NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (entity, from_status, to_status)
);

COMMENT ON TABLE biz_flow_critical_edge IS
    '主链路必需边白名单（只读巡检清单）：被禁用的功能边，巡检报警用；真相在 state_machine::CRITICAL_EDGES';
COMMENT ON COLUMN biz_flow_critical_edge.impact IS
    '业务后果（给用户看，例：销售出库无法直接过账）；不带句尾标点，会被拼进「禁用它会让{impact}」';

INSERT INTO biz_flow_critical_edge (entity, from_status, to_status, impact) VALUES
    -- 销售出库（E2E-02 断链的那一条）
    ('STOCK_OUT',      'DRAFT',          'PENDING_POST',    '销售出库无法提交过账审批'),
    ('STOCK_OUT',      'PENDING_POST',   'POSTED',          '销售出库无法过账'),
    ('STOCK_OUT',      'DRAFT',          'POSTED',          '销售出库无法直接过账'),
    -- 采购收货
    ('STOCK_IN',       'DRAFT',          'PENDING_POST',    '采购收货无法提交过账审批'),
    ('STOCK_IN',       'PENDING_POST',   'POSTED',          '采购收货无法过账'),
    ('STOCK_IN',       'DRAFT',          'POSTED',          '采购收货无法直接过账'),
    -- 采购订单
    ('PURCHASE_ORDER', 'DRAFT',          'SUBMITTED',       '采购订单无法提交审批'),
    ('PURCHASE_ORDER', 'SUBMITTED',      'APPROVED',        '采购订单无法审批通过（后续收货全部卡住）'),
    ('PURCHASE_ORDER', 'APPROVED',       'PARTIAL_RECEIVED','采购订单无法继续收货'),
    ('PURCHASE_ORDER', 'APPROVED',       'RECEIVED',        '采购订单无法收货完成'),
    -- 销售订单
    ('SALE_ORDER',     'DRAFT',          'SUBMITTED',       '销售订单无法提交审批'),
    ('SALE_ORDER',     'SUBMITTED',      'APPROVED',        '销售订单无法审批通过（后续发货全部卡住）'),
    ('SALE_ORDER',     'APPROVED',       'PARTIAL_OUT',     '销售订单无法部分出库'),
    ('SALE_ORDER',     'APPROVED',       'OUTBOUND',        '销售订单无法出库完成'),
    ('SALE_ORDER',     'PARTIAL_OUT',    'OUTBOUND',        '销售订单无法出库完成'),
    -- 发货单
    ('SHIPMENT',       'DRAFT',          'PENDING_POST',    '发货单无法提交待过账'),
    ('SHIPMENT',       'PENDING_POST',   'POSTED',          '发货单无法过账'),
    -- 库存调拨（DRAFT→DONE 不是一条边，按真实四段补全）
    ('STOCK_TRANSFER', 'DRAFT',          'PENDING',         '库存调拨无法提交审批'),
    ('STOCK_TRANSFER', 'PENDING',        'APPROVED',        '库存调拨无法审批通过'),
    ('STOCK_TRANSFER', 'APPROVED',       'IN_TRANSIT',      '库存调拨无法发货'),
    ('STOCK_TRANSFER', 'IN_TRANSIT',     'DONE',            '库存调拨无法收货完成'),
    -- 盘点
    ('STOCK_TAKE',     'DRAFT',          'PENDING_POST',    '盘点单无法提交审批'),
    ('STOCK_TAKE',     'PENDING_POST',   'POSTED',          '盘点单无法过账'),
    -- 凭证（记账链：关掉它总账与报表一条不进）
    ('VOUCHER',        'DRAFT',          'AUDITED',         '凭证无法审核'),
    ('VOUCHER',        'AUDITED',        'POSTED',          '凭证无法记账（总账与报表一条不进）')
ON CONFLICT (entity, from_status, to_status) DO NOTHING;

-- 自检：清单必须正好 25 条（与 CRITICAL_EDGES 同长）。少一条 = 漏种的边巡检看不见。
DO $$
DECLARE
    n_rows integer;
BEGIN
    SELECT count(*) INTO n_rows FROM biz_flow_critical_edge;
    IF n_rows <> 25 THEN
        RAISE EXCEPTION 'V0354 自检失败：biz_flow_critical_edge 应为 25 行，实际 %', n_rows;
    END IF;
END $$;
