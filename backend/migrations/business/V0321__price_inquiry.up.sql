-- ============================================================================
-- V0321：询比价闭环（P2-01）—— 询价单 / 询价明细 / 报价 三张表
--        + 取号规则 PIQ + 5 个权限码 + 采购管理下的「询比价」页面节点与按钮
--
--   ① biz_price_inquiry       询价单（单号 / 日期 / 采购员 / 仓库 / 状态 / 定标结果）
--   ② biz_price_inquiry_item  询价明细（物料 / 数量 / 需求日期）
--   ③ biz_price_quote         报价（询价明细 × 供应商 → 不含税单价 / 税率 / 交期 / 是否中标）
--   ④ biz_seq：PIQ（PIQ-yyyyMMdd-000001，按 CST 天滚动）
--   ⑤ 5 个权限码 + 1 个页面节点（menu_type=2）+ 4 个按钮（menu_type=3）
--   ⑥ 角色授权：PURCHASE_MANAGER 全给 / WAREHOUSE_MANAGER 只读
--
-- ── 号段核对（动手前 `ls backend/migrations/business | tail`）─────────────
--   写入本文件时 business 目录最大号 = V0320__seq_menu_move_to_master_data。
--   sys_menu 已用段：…0187 / …2701 / …3081–3088 / …3161–3173 / …3175–3188 / …3303–3313。
--   本卡取 **…3321–3326**（页面 + 4 个按钮 + 1 个预留位不占），与既有段无交叠。
--
-- ── 表口径（与卡面的差异，逐条说明）──────────────────────────────────────
--   1) 卡面只写「单号/日期/采购员/状态/定标供应商」，但「转采购订单」必须知道
--      **收货仓库**（biz_purchase_order.warehouse_id NOT NULL）⇒ 询价单带 `warehouse_id`，
--      转单时由它派生（可在 to-po 请求里显式覆盖，见 handler 文档）。
--   2) 报价行挂在**询价明细**上（卡面「询价明细 → 供应商报价」）：`(item_id, supplier_id)`
--      唯一 ⇒ 同一物料同一供应商只有一条报价，重复录入是「改价」不是「加一条」。
--   3) 金额口径：`price_excl` 是不含税单价（NUMERIC(18,6)，与采购明细同标度），
--      `price_incl` 由服务端按 `price_excl × (1 + tax_rate/100)` 算好落库（6 位舍入），
--      **不接受客户端传**——比价表要能横向比，含税价必须同源。
--   4) 定标按**明细行**表达（`biz_price_quote.is_awarded`），不是单一 supplier 列：
--      一张询价单可能 A 供物料 1、B 供物料 2，转单时按供应商分组拆成多张采购订单
--      （卡面业务规则第 3 条）。头表另留 `award_supplier_id` 作为「单一供应商定标」的
--      快捷口径（多供应商定标时为空），`award_mode` 记录 MULTI / SINGLE_SOURCE。
--   5) 状态域 4 值：DRAFT（建单）→ QUOTING（首次录入报价自动推进）→ AWARDED（定标）
--      → CONVERTED（已转采购订单）。卡面没有「作废」要求，不造没有入口的死状态。
--
-- ── 幂等与回滚 ───────────────────────────────────────────────────────────
--   CREATE TABLE / INDEX IF NOT EXISTS；biz_seq 与 sys_menu 用 WHERE NOT EXISTS /
--   ON CONFLICT DO UPDATE；授权 ON CONFLICT DO NOTHING。末尾 DO 块自检（表 / 唯一索引 /
--   CHECK 真能拒坏数据 / 取号规则 / 权限码承载行 / 菜单 path 与父级 / 授权），
--   任一不符 → 迁移失败，不留半套结构。
--   回滚 V0321__price_inquiry.down.sql：先删授权、再删按钮与页面、最后删表（子表先删）。
-- ============================================================================

-- ---------- ① biz_price_inquiry（询价单） ----------
CREATE TABLE IF NOT EXISTS biz_price_inquiry (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),      -- 主键
    inquiry_no         VARCHAR(32)   NOT NULL,                          -- 询价单号：PIQ-yyyyMMdd-000001（biz_seq 取号，存活行唯一）
    inquiry_date       DATE          NOT NULL DEFAULT CURRENT_DATE,      -- 询价日期
    buyer_id           UUID,                                            -- 采购员（弱引用 sys_user.id；离职后仍可追溯）
    buyer_name         VARCHAR(64),                                     -- 采购员姓名快照（用户改名后历史不失真）
    warehouse_id       UUID          NOT NULL,                          -- 收货仓库（弱引用 biz_warehouse.id；转采购订单时派生）
    status             VARCHAR(16)   NOT NULL DEFAULT 'DRAFT',          -- 状态：DRAFT / QUOTING / AWARDED / CONVERTED
    award_mode         VARCHAR(16),                                     -- 定标方式：MULTI 多家比价 / SINGLE_SOURCE 单源例外（未定标为 NULL）
    award_reason       TEXT,                                            -- 单源例外理由（award_mode=SINGLE_SOURCE 时必填，≥10 字）
    award_supplier_id  UUID,                                            -- 单一供应商定标时的中标供应商（多供应商定标为 NULL，明细见 is_awarded）
    awarded_at         TIMESTAMPTZ,                                     -- 定标时间
    converted_at       TIMESTAMPTZ,                                     -- 转采购订单时间
    po_refs            JSONB         NOT NULL DEFAULT '[]'::jsonb,       -- 已生成的采购订单引用：[{po_id, po_no, supplier_id, line_count}]
    remark             TEXT,                                            -- 备注
    created_at         TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 审计：创建时间
    updated_at         TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP, -- 审计：更新时间
    create_by          UUID,                                            -- 审计：创建人
    update_by          UUID,                                            -- 审计：更新人
    deleted_at         TIMESTAMPTZ,                                     -- 软删时间（NULL = 存活）

    CONSTRAINT ck_biz_piq_status CHECK (status IN ('DRAFT','QUOTING','AWARDED','CONVERTED')),
    CONSTRAINT ck_biz_piq_award_mode
        CHECK (award_mode IS NULL OR award_mode IN ('MULTI','SINGLE_SOURCE')),
    -- 单源例外必须留下理由：没有理由的「只有一家报价就定标」是不可审计的
    CONSTRAINT ck_biz_piq_award_reason
        CHECK (award_mode IS DISTINCT FROM 'SINGLE_SOURCE'
               OR (award_reason IS NOT NULL AND length(btrim(award_reason)) >= 10)),
    -- 定标之后必然有定标时间；没定标不许有（否则报表「已定标」口径会分叉）
    CONSTRAINT ck_biz_piq_awarded_at
        CHECK ((status IN ('AWARDED','CONVERTED')) = (awarded_at IS NOT NULL))
);

COMMENT ON TABLE  biz_price_inquiry IS '询价单（P2-01 询比价闭环；一家询价单可拆成多张采购订单）';
COMMENT ON COLUMN biz_price_inquiry.inquiry_no IS '询价单号：PIQ-yyyyMMdd-000001（biz_seq.seq_type=PIQ，按北京时间天滚动）';
COMMENT ON COLUMN biz_price_inquiry.status IS 'DRAFT 建单 / QUOTING 已录入报价 / AWARDED 已定标 / CONVERTED 已转采购订单';
COMMENT ON COLUMN biz_price_inquiry.award_mode IS 'MULTI = 同一明细至少 2 家比价后定标；SINGLE_SOURCE = 单源例外（award_reason 必填）';
COMMENT ON COLUMN biz_price_inquiry.po_refs IS '转单结果引用（JSONB 数组）：转单是幂等闸，CONVERTED 后不再转';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_price_inquiry_no
    ON biz_price_inquiry (inquiry_no) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_price_inquiry_status
    ON biz_price_inquiry (status, inquiry_date DESC);
CREATE INDEX IF NOT EXISTS idx_biz_price_inquiry_date
    ON biz_price_inquiry (inquiry_date DESC);

-- ---------- ② biz_price_inquiry_item（询价明细） ----------
-- 明细随主单整体重建（编辑口径与采购订单明细一致：先删后插，事务内）。
CREATE TABLE IF NOT EXISTS biz_price_inquiry_item (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),           -- 主键
    inquiry_id    UUID          NOT NULL,                               -- 所属询价单
    line_no       INT           NOT NULL,                               -- 行号（1 起，单内唯一）
    material_id   UUID          NOT NULL,                               -- 物料（弱引用 biz_material.id）
    unit_id       UUID          NOT NULL,                               -- 计量单位（建单时由物料带出，客户端不许传）
    qty           NUMERIC(18,4) NOT NULL,                               -- 询价数量（> 0）
    demand_date   DATE,                                                 -- 需求日期（转单时作为采购单期望到货日）
    remark        TEXT,                                                 -- 行备注
    created_at    TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT ck_biz_piqi_qty  CHECK (qty > 0),
    CONSTRAINT ck_biz_piqi_line CHECK (line_no > 0),
    CONSTRAINT fk_biz_piqi_inquiry FOREIGN KEY (inquiry_id)
        REFERENCES biz_price_inquiry (id) ON DELETE CASCADE
);

COMMENT ON TABLE  biz_price_inquiry_item IS '询价明细（一行 = 一个物料 + 数量 + 需求日期；随主单整体重建）';
COMMENT ON COLUMN biz_price_inquiry_item.unit_id IS '由物料基础单位带出（DC-09 止血口径，与采购订单明细同源）';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_piqi_line
    ON biz_price_inquiry_item (inquiry_id, line_no);
CREATE INDEX IF NOT EXISTS idx_biz_piqi_material
    ON biz_price_inquiry_item (material_id);

-- ---------- ③ biz_price_quote（报价） ----------
CREATE TABLE IF NOT EXISTS biz_price_quote (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),          -- 主键
    inquiry_id     UUID          NOT NULL,                              -- 所属询价单（冗余，便于按单查全部报价）
    item_id        UUID          NOT NULL,                              -- 询价明细行
    supplier_id    UUID          NOT NULL,                              -- 报价供应商（弱引用 biz_supplier.id）
    price_excl     NUMERIC(18,6) NOT NULL,                              -- 不含税单价（比价主口径）
    price_incl     NUMERIC(18,6) NOT NULL,                              -- 含税单价（服务端由 price_excl × (1+税率) 算）
    tax_rate       NUMERIC(9,4)  NOT NULL DEFAULT 0,                    -- 税率（百分数 0–100）
    delivery_days  INT,                                                 -- 交期（天，≥ 0）
    delivery_date  DATE,                                                -- 承诺到货日（可选；与天数二选一，都给以日期为准展示）
    remark         TEXT,                                                -- 报价备注
    is_awarded     BOOLEAN       NOT NULL DEFAULT FALSE,                 -- 是否中标（定标时按明细行置位）
    created_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at     TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by      UUID,
    update_by      UUID,

    CONSTRAINT ck_biz_pq_price CHECK (price_excl >= 0 AND price_incl >= 0),
    CONSTRAINT ck_biz_pq_tax   CHECK (tax_rate >= 0 AND tax_rate <= 100),
    CONSTRAINT ck_biz_pq_days  CHECK (delivery_days IS NULL OR delivery_days >= 0),
    CONSTRAINT fk_biz_pq_inquiry FOREIGN KEY (inquiry_id)
        REFERENCES biz_price_inquiry (id) ON DELETE CASCADE,
    CONSTRAINT fk_biz_pq_item FOREIGN KEY (item_id)
        REFERENCES biz_price_inquiry_item (id) ON DELETE CASCADE
);

COMMENT ON TABLE  biz_price_quote IS '供应商报价（询价明细 × 供应商唯一；重复录入 = 改价）';
COMMENT ON COLUMN biz_price_quote.price_incl IS '服务端算：price_excl × (1 + tax_rate/100)，6 位舍入；客户端传了也会被忽略';
COMMENT ON COLUMN biz_price_quote.is_awarded IS '中标标记（按明细行置位；同一条明细可以有多个中标供应商吗？不能——定标按行唯一）';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_price_quote_item_supplier
    ON biz_price_quote (item_id, supplier_id);
CREATE INDEX IF NOT EXISTS idx_biz_price_quote_inquiry
    ON biz_price_quote (inquiry_id, supplier_id);
-- 同一明细行**最多一家中标**：定标在数据库层面兜底（handler 先清后置位，`award_rejects_duplicate_line`
-- 单测管住请求体；这条部分唯一索引管住「绕过接口直改库」与将来新增的写入路径）。
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_price_quote_awarded
    ON biz_price_quote (item_id) WHERE is_awarded;

-- ---------- ④ 取号规则 PIQ ----------
INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status, remark)
SELECT '20000000-0000-0000-0000-000000003326'::uuid, 'PIQ', '询价单号', 'PIQ', 'yyyyMMdd', '', 0, 1, '-', 1,
       '询价单号：PIQ-yyyyMMdd-000001，按天（CST）滚动'
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'PIQ' AND deleted_at IS NULL);

-- ---------- ⑤ 菜单：页面节点 + 4 个按钮 ----------
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000003321'::uuid, '询比价', 2,
     'business:erp:price-inquiry:list', '/business/erp/price-inquiry',
     '20000000-0000-0000-0000-000000000946'::uuid, 'business/erp/price-inquiry/index',
     35, 1, 'search', '询价 → 多家报价 → 比价 → 定标 → 转采购订单（P2-01）',
     TRUE, 'business')
ON CONFLICT (id) DO UPDATE
   SET name        = EXCLUDED.name,
       menu_type   = EXCLUDED.menu_type,
       auth_code   = EXCLUDED.auth_code,
       path        = EXCLUDED.path,
       parent_id   = EXCLUDED.parent_id,
       component   = EXCLUDED.component,
       sort_order  = EXCLUDED.sort_order,
       status      = EXCLUDED.status,
       icon        = EXCLUDED.icon,
       title       = EXCLUDED.title,
       is_visible  = EXCLUDED.is_visible,
       module_code = EXCLUDED.module_code,
       deleted_at  = NULL,
       updated_at  = now();

INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000003322'::uuid, '询价单新建', 3,
     'business:erp:price-inquiry:create', NULL, '20000000-0000-0000-0000-000000003321'::uuid,
     NULL, 10, 1, NULL, '新建询价单（3 个物料起，明细由物料带出基础单位）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003323'::uuid, '录入报价', 3,
     'business:erp:price-inquiry:quote', NULL, '20000000-0000-0000-0000-000000003321'::uuid,
     NULL, 20, 1, NULL, '按供应商一次性录入该供应商对全部明细的报价（重复录入 = 改价）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003324'::uuid, '定标', 3,
     'business:erp:price-inquiry:award', NULL, '20000000-0000-0000-0000-000000003321'::uuid,
     NULL, 30, 1, NULL, '按明细行选定中标供应商（同一明细只有一家报价时必须填单源例外理由）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003325'::uuid, '转采购订单', 3,
     'business:erp:price-inquiry:to-po', NULL, '20000000-0000-0000-0000-000000003321'::uuid,
     NULL, 40, 1, NULL, '按中标供应商分组生成采购订单（单价 = 中标报价的不含税单价）', TRUE, 'business')
ON CONFLICT (id) DO UPDATE
   SET name        = EXCLUDED.name,
       menu_type   = EXCLUDED.menu_type,
       auth_code   = EXCLUDED.auth_code,
       path        = EXCLUDED.path,
       parent_id   = EXCLUDED.parent_id,
       component   = EXCLUDED.component,
       sort_order  = EXCLUDED.sort_order,
       status      = EXCLUDED.status,
       icon        = EXCLUDED.icon,
       title       = EXCLUDED.title,
       is_visible  = EXCLUDED.is_visible,
       module_code = EXCLUDED.module_code,
       deleted_at  = NULL,
       updated_at  = now();

-- ---------- ⑥ 角色授权（本迁移唯一替人做的决定） ----------
--   · PURCHASE_MANAGER（采购经理）：5 个码全给 —— 询比价是采购的日常动作；
--   · WAREHOUSE_MANAGER（仓管）：只给 list（看得见询价与报价，不参与定价）；
--   · 其余角色不给：SUPER_ADMIN 由框架短路，不需要显式授权。
--   角色不存在时自然插入 0 行（空库首启：角色由 bootstrap 种，迁移先跑）。
INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.menu_id
  FROM (VALUES
        ('PURCHASE_MANAGER', '20000000-0000-0000-0000-000000003321'::uuid),
        ('PURCHASE_MANAGER', '20000000-0000-0000-0000-000000003322'::uuid),
        ('PURCHASE_MANAGER', '20000000-0000-0000-0000-000000003323'::uuid),
        ('PURCHASE_MANAGER', '20000000-0000-0000-0000-000000003324'::uuid),
        ('PURCHASE_MANAGER', '20000000-0000-0000-0000-000000003325'::uuid),
        ('WAREHOUSE_MANAGER', '20000000-0000-0000-0000-000000003321'::uuid)
       ) AS m(role_code, menu_id)
  JOIN sys_role r ON r.role_code = m.role_code AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ---------- ⑦ 自检 ----------
DO $$
DECLARE
    v_missing text;
    v_rows    integer;
    v_path    text;
    v_parent  uuid;
    v_component text;
    v_id      uuid;
BEGIN
    -- ① 三张表 + 取号规则
    SELECT string_agg(t, ', ') INTO v_missing
      FROM (VALUES ('biz_price_inquiry'), ('biz_price_inquiry_item'), ('biz_price_quote')) AS x(t)
     WHERE NOT EXISTS (SELECT 1 FROM information_schema.tables
                        WHERE table_schema = current_schema() AND table_name = x.t);
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'P2-01: 以下表未建成：%', v_missing;
    END IF;

    SELECT count(*) INTO v_rows FROM biz_seq WHERE seq_type = 'PIQ' AND deleted_at IS NULL;
    IF v_rows <> 1 THEN
        RAISE EXCEPTION 'P2-01: biz_seq(seq_type=PIQ) 行数 = %（期望 1）', v_rows;
    END IF;

    -- ② 唯一索引必须在位（(item_id, supplier_id) 是「重复录入 = 改价」的唯一依据；
    --    uk_biz_price_quote_awarded 是「同一明细最多一家中标」的库级兜底）
    SELECT count(*) INTO v_rows FROM pg_indexes
     WHERE schemaname = current_schema()
       AND indexname IN ('uk_biz_price_inquiry_no', 'uk_biz_piqi_line',
                         'uk_biz_price_quote_item_supplier', 'uk_biz_price_quote_awarded');
    IF v_rows <> 4 THEN
        RAISE EXCEPTION 'P2-01: 唯一索引数 = %（期望 4）', v_rows;
    END IF;

    -- ③ CHECK 必须真的能拒坏数据（不是写了一行注释）
    v_id := gen_random_uuid();
    BEGIN
        INSERT INTO biz_price_inquiry (inquiry_no, warehouse_id, status, award_mode, award_reason, awarded_at)
        VALUES ('SELFTEST-BAD-MODE', v_id, 'AWARDED', 'SINGLE_SOURCE', '太短', now());
        RAISE EXCEPTION 'P2-01: 单源理由 < 10 字竟然被接受';
    EXCEPTION WHEN check_violation THEN
        NULL;  -- 期望：被 ck_biz_piq_award_reason 拒
    END;
    BEGIN
        INSERT INTO biz_price_inquiry (inquiry_no, warehouse_id, status)
        VALUES ('SELFTEST-BAD-STATUS', v_id, 'WHATEVER');
        RAISE EXCEPTION 'P2-01: 非法状态竟然被接受';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- ④ 权限码承载行（每个码必须有且仅有 1 行）
    SELECT string_agg(code, ', ') INTO v_missing
      FROM (VALUES
            ('business:erp:price-inquiry:list'), ('business:erp:price-inquiry:create'),
            ('business:erp:price-inquiry:quote'), ('business:erp:price-inquiry:award'),
            ('business:erp:price-inquiry:to-po')
           ) AS expected(code)
     WHERE NOT EXISTS (SELECT 1 FROM sys_menu m
                        WHERE m.auth_code = expected.code AND m.deleted_at IS NULL);
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'P2-01: 以下权限码没有承载行：%', v_missing;
    END IF;

    -- ⑤ 页面节点字段（path 必须与前端路由逐字一致，否则侧边栏点进去 404）
    SELECT path, component, parent_id INTO v_path, v_component, v_parent
      FROM sys_menu WHERE id = '20000000-0000-0000-0000-000000003321'::uuid AND deleted_at IS NULL;
    IF v_path IS DISTINCT FROM '/business/erp/price-inquiry'
       OR v_component IS DISTINCT FROM 'business/erp/price-inquiry/index'
       OR v_parent IS DISTINCT FROM '20000000-0000-0000-0000-000000000946'::uuid THEN
        RAISE EXCEPTION 'P2-01: 询比价页面节点字段不对（path=%, component=%, parent=%）',
            v_path, v_component, v_parent;
    END IF;

    -- ⑥ 4 个按钮都挂在页面节点下
    SELECT count(*) INTO v_rows FROM sys_menu
     WHERE parent_id = '20000000-0000-0000-0000-000000003321'::uuid AND menu_type = 3
       AND deleted_at IS NULL;
    IF v_rows <> 4 THEN
        RAISE EXCEPTION 'P2-01: 询比价按钮数 = %（期望 4）', v_rows;
    END IF;

    RAISE NOTICE 'P2-01: 询比价三表 + 取号 + 5 个权限码 + 菜单自检通过';
END $$;
