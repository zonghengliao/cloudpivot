-- ============================================================================
-- V0318：销售目标 + 销售预测两张表 + 8 个权限码 + 销售管理菜单（SO-44 / ARCH-7 §15.8）
--
--   ① biz_sales_target    销售目标（按 销售员 / 部门 / 物料 / 全局 × 期间）
--   ② biz_sales_forecast  滚动预测（客户 × 物料 × 期间 × 版本）
--   ③ biz_seq 取号：STG（目标）/ SFC（预测）
--   ④ 4+4 个权限码 + 销售管理目录下的一个页面节点 + 8 个按钮
--   ⑤ 角色授权矩阵（SALES_SUPERVISOR 全给 / SALES_CLERK 看 + 编预测 / 财务与总经理只读）
--
-- ── 号段 ─────────────────────────────────────────────────────────────
--   写入本文件时 backend/migrations/business/ 最大号 = V0317（SO-43 对账确认）。
--   迁移号会被并行卡抢走，**提交前**再 `ls business | tail` 复核一次。
--
-- ── 表口径（卡面逐字，与 docs 稿差异记在这里）────────────────────────
--  1) `MATERIAL_CATEGORY` 的 `scope_id` 存 **biz_material.id**：
--     基础版**没有** `biz_material_category` 表（V0187 头注释已登记、V0309 佣金规则
--     也是同一口径）。接分类表后由数据迁移换 id，看板取数逻辑不变 —— 这条
--     和佣金规则的 `scope_id` 完全同构，两张表的 scope 域值刻意保持一致。
--  2) `DEPT` 的 `scope_id` 存 **sys_dept.id**；达成率取数时经
--     `sys_user.dept_id` 反查（订单/收款都挂在人身上，人挂部门）。
--  3) `biz_sales_order.salesperson_id` 由 V0309 补出（不是本卡新增）；它可空，
--     所以「按销售员/部门」的目标达成率里**未登记销售员的订单不归属任何人**
--     （只计入 ALL 口径）—— 这是账实相符的必然结果，不是漏算。
--  4) 预测的 `customer_id` / `material_id` 都可空：**两个都空 = 全局滚动预测**。
--     不加「至少一个非空」的 CHECK —— 销售总监确实会填「下季度整体」这一行。
--  5) 目标 `target_qty` 可选（卡面「可选」）⇒ NOT NULL DEFAULT 0，0 = 没定数量目标。
--
-- ── 幂等 ─────────────────────────────────────────────────────────────
--   CREATE TABLE / INDEX IF NOT EXISTS；biz_seq / sys_menu 用 `WHERE NOT EXISTS`
--   或 `ON CONFLICT DO UPDATE`。末尾 DO 块自检：表 / 列 / 索引 / CHECK 行为
--   （坏数据必须被拒）/ 默认值 / 取号规则 / 权限码承载行 / 页面 path，
--   任一不符 → 迁移失败（不留半套结构）。
-- ============================================================================

-- ---------- ① biz_sales_target（销售目标） ----------
CREATE TABLE IF NOT EXISTS biz_sales_target (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),          -- 主键
    target_no     VARCHAR(32)    NOT NULL,                             -- 目标编号：STG-yyyyMMdd-000001（biz_seq 取号，存活行唯一）
    scope_type    VARCHAR(24)    NOT NULL,                             -- 范围类型：SALESPERSON / DEPT / MATERIAL_CATEGORY / ALL
    scope_id      UUID,                                                -- 范围对象：sys_user.id / sys_dept.id / biz_material.id；ALL ⇒ NULL
    period_code   VARCHAR(8)     NOT NULL,                             -- 期间：yyyy-MM 或 yyyy-QN（卡面两值）
    target_amount NUMERIC(18,2)  NOT NULL,                             -- 目标销售额（>= 0）
    target_qty    NUMERIC(18,4)  NOT NULL DEFAULT 0,                   -- 目标数量（可选，0 = 没定）
    status        VARCHAR(16)    NOT NULL DEFAULT 'ACTIVE',            -- ACTIVE 参与看板 / INACTIVE 停用（历史照旧可查）
    remark        TEXT,                                                -- 备注
    created_at    TIMESTAMPTZ    NOT NULL DEFAULT CURRENT_TIMESTAMP,   -- 审计：创建时间
    updated_at    TIMESTAMPTZ    NOT NULL DEFAULT CURRENT_TIMESTAMP,   -- 审计：更新时间（model 层维护）
    create_by     UUID,                                                -- 审计：创建人
    update_by     UUID,                                                -- 审计：更新人
    deleted_at    TIMESTAMPTZ,                                         -- 软删时间（NULL = 存活）

    CONSTRAINT ck_biz_st_target_scope
        CHECK (scope_type IN ('SALESPERSON','DEPT','MATERIAL_CATEGORY','ALL')),
    -- ALL 不带 scope_id；其余三种必须带（口径与 ck_biz_cmrule_scope_id 同构）
    CONSTRAINT ck_biz_st_target_scope_id
        CHECK ((scope_type = 'ALL' AND scope_id IS NULL)
            OR (scope_type <> 'ALL' AND scope_id IS NOT NULL)),
    -- 期间格式：yyyy-MM 或 yyyy-QN（季度大小写都收，model 层统一转大写）
    CONSTRAINT ck_biz_st_target_period
        CHECK (period_code ~ '^[0-9]{4}-(0[1-9]|1[0-2]|[Qq][1-4])$'),
    -- 金额 / 数量非负：负目标没有业务含义，且会让达成率符号翻转
    CONSTRAINT ck_biz_st_target_amount
        CHECK (target_amount >= 0 AND target_qty >= 0),
    CONSTRAINT ck_biz_st_target_status
        CHECK (status IN ('ACTIVE','INACTIVE'))
);

COMMENT ON TABLE  biz_sales_target IS '销售目标（ARCH-7 §15.8；按 销售员/部门/物料/全局 × 期间，达成率看板的数据源）';
COMMENT ON COLUMN biz_sales_target.target_no IS '目标编号：STG-yyyyMMdd-000001（biz_seq seq_type=STG，按 CST 的天滚动）';
COMMENT ON COLUMN biz_sales_target.scope_type IS 'SALESPERSON ⇒ sys_user.id / DEPT ⇒ sys_dept.id / MATERIAL_CATEGORY ⇒ biz_material.id（基础版无分类表，与佣金规则同口径）/ ALL ⇒ NULL';
COMMENT ON COLUMN biz_sales_target.period_code IS '期间码：yyyy-MM（月度）或 yyyy-QN（季度，Q 大小写不限，model 层转大写）';
COMMENT ON COLUMN biz_sales_target.status IS 'ACTIVE 进达成率看板 / INACTIVE 停用（不改历史，不删行）';
COMMENT ON COLUMN biz_sales_target.deleted_at IS '软删标记；编号与「范围+期间」唯一索引都带 WHERE deleted_at IS NULL ⇒ 软删后可重建';

-- 编号唯一（存活行唯一，与 uk_biz_sales_order_no 同口径）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_sales_target_no
    ON biz_sales_target (target_no) WHERE deleted_at IS NULL;
-- 「同一范围 + 同一期间」只能有一条存活目标：否则看板会把两条目标相加，达成率造假。
-- COALESCE 把 ALL 的 NULL 折成零 UUID —— NULL 在唯一索引里互不相等，不折算等于没约束。
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_sales_target_scope
    ON biz_sales_target (scope_type,
                         COALESCE(scope_id, '00000000-0000-0000-0000-000000000000'::uuid),
                         period_code)
    WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_sales_target_period
    ON biz_sales_target (period_code, scope_type) WHERE deleted_at IS NULL;

-- ---------- ② biz_sales_forecast（销售预测） ----------
CREATE TABLE IF NOT EXISTS biz_sales_forecast (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),         -- 主键
    forecast_no     VARCHAR(32)   NOT NULL,                             -- 预测编号：SFC-yyyyMMdd-000001（biz_seq 取号，存活行唯一）
    customer_id     UUID,                                               -- 按客户预测（可空；弱引用 biz_customer.id）
    material_id     UUID,                                               -- 按物料预测（可空；弱引用 biz_material.id）
    period_code     VARCHAR(8)    NOT NULL,                             -- 期间：yyyy-MM 或 yyyy-QN
    forecast_amount NUMERIC(18,2) NOT NULL,                             -- 预测金额（>= 0）
    confidence      VARCHAR(8)    NOT NULL DEFAULT 'MEDIUM',            -- 信心度：HIGH / MEDIUM / LOW
    version_no      INT           NOT NULL DEFAULT 1,                   -- 版本号（同一维度同一期间的滚动版本，>= 1）
    remark          TEXT,                                               -- 备注
    created_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,   -- 审计
    updated_at      TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,   -- 审计
    create_by       UUID,                                               -- 审计
    update_by       UUID,                                               -- 审计
    deleted_at      TIMESTAMPTZ,                                        -- 软删

    CONSTRAINT ck_biz_sf_period
        CHECK (period_code ~ '^[0-9]{4}-(0[1-9]|1[0-2]|[Qq][1-4])$'),
    CONSTRAINT ck_biz_sf_amount CHECK (forecast_amount >= 0),
    CONSTRAINT ck_biz_sf_confidence
        CHECK (confidence IN ('HIGH','MEDIUM','LOW')),
    CONSTRAINT ck_biz_sf_version CHECK (version_no >= 1)
);

COMMENT ON TABLE  biz_sales_forecast IS '销售预测（ARCH-7 §15.8；滚动预测：同维度同期间按 version_no 叠版本，最新版本才是当前预测）';
COMMENT ON COLUMN biz_sales_forecast.forecast_no IS '预测编号：SFC-yyyyMMdd-000001（biz_seq seq_type=SFC）';
COMMENT ON COLUMN biz_sales_forecast.customer_id IS '按客户预测（弱引用；与 material_id 同时为空 = 全局滚动预测）';
COMMENT ON COLUMN biz_sales_forecast.confidence IS '信心度 HIGH / MEDIUM / LOW（ck_biz_sf_confidence）';
COMMENT ON COLUMN biz_sales_forecast.version_no IS '版本号（>= 1）：同维度同期间可多版本，看板取 max(version_no)';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_sales_forecast_no
    ON biz_sales_forecast (forecast_no) WHERE deleted_at IS NULL;
-- 同维度 + 同期间 + 同版本只能一条存活行（版本是新行，不是原地改）
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_sales_forecast_dim
    ON biz_sales_forecast (COALESCE(customer_id, '00000000-0000-0000-0000-000000000000'::uuid),
                           COALESCE(material_id, '00000000-0000-0000-0000-000000000000'::uuid),
                           period_code, version_no)
    WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_sales_forecast_period
    ON biz_sales_forecast (period_code, confidence) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_biz_sales_forecast_customer
    ON biz_sales_forecast (customer_id, period_code DESC) WHERE deleted_at IS NULL;

-- ---------- ③ 取号规则：STG（目标）/ SFC（预测） ----------
INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status, remark)
SELECT v.id::uuid, 'STG', '销售目标编号', 'STG', 'yyyyMMdd', '', 0, 1, '-', 1,
       '销售目标编号：STG-yyyyMMdd-000001，按天（CST）滚动'
  FROM (VALUES ('d0318000-0000-4000-8000-000000000001')) AS v(id)
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq q WHERE q.seq_type = 'STG' AND q.deleted_at IS NULL);

INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status, remark)
SELECT v.id::uuid, 'SFC', '销售预测编号', 'SFC', 'yyyyMMdd', '', 0, 1, '-', 1,
       '销售预测编号：SFC-yyyyMMdd-000001，按天（CST）滚动'
  FROM (VALUES ('d0318000-0000-4000-8000-000000000002')) AS v(id)
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq q WHERE q.seq_type = 'SFC' AND q.deleted_at IS NULL);

-- ---------- ④ 菜单：一个页面节点（销售管理 …2701 下）+ 8 个按钮 ----------
--
-- 页面 `path` / `component` 本卡**直接落位**（前端 views/business/erp/sales-target
-- 与 router 同卡交付），因此 is_visible = TRUE —— 与 SO-37/SO-42 的
-- 「先 seeding false、前端卡再 UPDATE」不同：本卡前后端同卡，n08 绝不会红。
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000003181'::uuid, '销售目标与预测', 2,
     'business:erp:sales-target:list', '/business/erp/sales-target',
     '20000000-0000-0000-0000-000000002701'::uuid, 'business/erp/sales-target/index',
     78, 1, 'chart-bar', '销售目标维护 + 达成率看板（按订单金额 / 按已收款金额两个口径；ARCH-7 §15.8）',
     TRUE, 'business')
ON CONFLICT (id) DO UPDATE
   SET name = EXCLUDED.name, menu_type = EXCLUDED.menu_type, auth_code = EXCLUDED.auth_code,
       path = EXCLUDED.path, parent_id = EXCLUDED.parent_id, component = EXCLUDED.component,
       sort_order = EXCLUDED.sort_order, status = EXCLUDED.status, icon = EXCLUDED.icon,
       title = EXCLUDED.title, is_visible = EXCLUDED.is_visible, module_code = EXCLUDED.module_code,
       deleted_at = NULL, updated_at = now();

INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000003182'::uuid, '销售目标新建', 3,
     'business:erp:sales-target:create', NULL, '20000000-0000-0000-0000-000000003181'::uuid,
     NULL, 10, 1, NULL, '新建销售目标（ALL 不带 scope_id；同范围同期间只能一条）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003183'::uuid, '销售目标编辑', 3,
     'business:erp:sales-target:update', NULL, '20000000-0000-0000-0000-000000003181'::uuid,
     NULL, 20, 1, NULL, '编辑 / 停用销售目标（停用改 status=INACTIVE，不删行）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003184'::uuid, '销售目标删除', 3,
     'business:erp:sales-target:delete', NULL, '20000000-0000-0000-0000-000000003181'::uuid,
     NULL, 30, 1, NULL, '软删销售目标（仅「录错了」用）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003185'::uuid, '销售预测查看', 3,
     'business:erp:sales-forecast:list', NULL, '20000000-0000-0000-0000-000000003181'::uuid,
     NULL, 40, 1, NULL, '销售预测分页（同页签内）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003186'::uuid, '销售预测新建', 3,
     'business:erp:sales-forecast:create', NULL, '20000000-0000-0000-0000-000000003181'::uuid,
     NULL, 50, 1, NULL, '新建销售预测（滚动预测：同维度同期间同版本只能一条）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003187'::uuid, '销售预测编辑', 3,
     'business:erp:sales-forecast:update', NULL, '20000000-0000-0000-0000-000000003181'::uuid,
     NULL, 60, 1, NULL, '编辑销售预测（改金额 / 信心度 / 备注；换版本请新建）', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000003188'::uuid, '销售预测删除', 3,
     'business:erp:sales-forecast:delete', NULL, '20000000-0000-0000-0000-000000003181'::uuid,
     NULL, 70, 1, NULL, '软删销售预测（仅「录错了」用）', TRUE, 'business')
ON CONFLICT (id) DO UPDATE
   SET name = EXCLUDED.name, menu_type = EXCLUDED.menu_type, auth_code = EXCLUDED.auth_code,
       path = EXCLUDED.path, parent_id = EXCLUDED.parent_id, component = EXCLUDED.component,
       sort_order = EXCLUDED.sort_order, status = EXCLUDED.status, icon = EXCLUDED.icon,
       title = EXCLUDED.title, is_visible = EXCLUDED.is_visible, module_code = EXCLUDED.module_code,
       deleted_at = NULL, updated_at = now();

-- ---------- ⑤ 角色授权矩阵 ----------
--  SALES_SUPERVISOR（销售经理） 8 码全给：定目标 + 看达成率 + 维护预测
--  SALES_CLERK（销售员）        target:list + forecast:list/create/update（报自己的预测）
--  FINANCE_MANAGER / GENERAL_MANAGER  只读两个 list（财务与总经理看板，不改目标）
CREATE TEMP TABLE _stg_matrix (role_code text NOT NULL, menu_id uuid NOT NULL, PRIMARY KEY (role_code, menu_id));

INSERT INTO _stg_matrix (role_code, menu_id) VALUES
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003181'::uuid),
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003182'::uuid),
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003183'::uuid),
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003184'::uuid),
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003185'::uuid),
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003186'::uuid),
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003187'::uuid),
    ('SALES_SUPERVISOR', '20000000-0000-0000-0000-000000003188'::uuid),
    ('SALES_CLERK',      '20000000-0000-0000-0000-000000003181'::uuid),
    ('SALES_CLERK',      '20000000-0000-0000-0000-000000003185'::uuid),
    ('SALES_CLERK',      '20000000-0000-0000-0000-000000003186'::uuid),
    ('SALES_CLERK',      '20000000-0000-0000-0000-000000003187'::uuid),
    ('FINANCE_MANAGER',  '20000000-0000-0000-0000-000000003181'::uuid),
    ('FINANCE_MANAGER',  '20000000-0000-0000-0000-000000003185'::uuid),
    ('GENERAL_MANAGER',  '20000000-0000-0000-0000-000000003181'::uuid),
    ('GENERAL_MANAGER',  '20000000-0000-0000-0000-000000003185'::uuid);

INSERT INTO sys_role_menu (role_id, menu_id)
SELECT r.id, m.menu_id
  FROM _stg_matrix m
  JOIN sys_role r ON r.role_code = m.role_code AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;

DROP TABLE _stg_matrix;

-- ---------- ⑥ 自检 ----------
DO $$
DECLARE
    v_missing   text;
    v_dupe      text;
    v_rows      integer;
    v_path      text;
    v_component text;
    v_visible   boolean;
    v_parent    uuid;
    v_cnt       integer;
    probe       uuid := gen_random_uuid();
BEGIN
    -- ⑥.1 两张表必须存在
    IF to_regclass('public.biz_sales_target') IS NULL THEN
        RAISE EXCEPTION 'V0318 自检失败：biz_sales_target 未建出';
    END IF;
    IF to_regclass('public.biz_sales_forecast') IS NULL THEN
        RAISE EXCEPTION 'V0318 自检失败：biz_sales_forecast 未建出';
    END IF;

    -- ⑥.2 列数与关键列（卡面 9 列 + 审计）
    SELECT count(*) INTO v_cnt FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_sales_target';
    IF v_cnt <> 14 THEN
        RAISE EXCEPTION 'V0318 自检失败：biz_sales_target 列数 % ≠ 14', v_cnt;
    END IF;
    SELECT count(*) INTO v_cnt FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_sales_forecast';
    IF v_cnt <> 14 THEN
        RAISE EXCEPTION 'V0318 自检失败：biz_sales_forecast 列数 % ≠ 14', v_cnt;
    END IF;

    -- ⑥.3 唯一索引必须是部分索引（软删后可复用）
    SELECT count(*) INTO v_cnt
      FROM pg_indexes
     WHERE schemaname = 'public'
       AND indexname IN ('uk_biz_sales_target_no','uk_biz_sales_target_scope',
                         'uk_biz_sales_forecast_no','uk_biz_sales_forecast_dim')
       AND indexdef LIKE '%WHERE (deleted_at IS NULL)%';
    IF v_cnt <> 4 THEN
        RAISE EXCEPTION 'V0318 自检失败：4 个部分唯一索引只建出 % 个', v_cnt;
    END IF;

    -- ⑥.4 行为探针：坏数据必须被 CHECK 拒（逐条独立，任一「能插进去」即失败）
    BEGIN
        INSERT INTO biz_sales_target (target_no, scope_type, scope_id, period_code, target_amount)
        VALUES ('PROBE-1', 'REGION', gen_random_uuid(), '2026-11', 100);
        RAISE EXCEPTION 'V0318 自检失败：非法 scope_type 被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    BEGIN
        INSERT INTO biz_sales_target (target_no, scope_type, scope_id, period_code, target_amount)
        VALUES ('PROBE-2', 'ALL', gen_random_uuid(), '2026-11', 100);
        RAISE EXCEPTION 'V0318 自检失败：ALL 带 scope_id 被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    BEGIN
        INSERT INTO biz_sales_target (target_no, scope_type, scope_id, period_code, target_amount)
        VALUES ('PROBE-3', 'SALESPERSON', NULL, '2026-11', 100);
        RAISE EXCEPTION 'V0318 自检失败：SALESPERSON 缺 scope_id 被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    BEGIN
        INSERT INTO biz_sales_target (target_no, scope_type, scope_id, period_code, target_amount)
        VALUES ('PROBE-4', 'ALL', NULL, '2026-13', 100);
        RAISE EXCEPTION 'V0318 自检失败：非法期间码 2026-13 被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    BEGIN
        INSERT INTO biz_sales_target (target_no, scope_type, scope_id, period_code, target_amount)
        VALUES ('PROBE-5', 'ALL', NULL, '2026-11', -1);
        RAISE EXCEPTION 'V0318 自检失败：负目标金额被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    BEGIN
        INSERT INTO biz_sales_forecast (forecast_no, period_code, forecast_amount, confidence)
        VALUES ('PROBE-6', '2026-11', 100, 'SURE');
        RAISE EXCEPTION 'V0318 自检失败：非法 confidence 被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    BEGIN
        INSERT INTO biz_sales_forecast (forecast_no, period_code, forecast_amount, version_no)
        VALUES ('PROBE-7', '2026-11', 100, 0);
        RAISE EXCEPTION 'V0318 自检失败：version_no=0 被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    BEGIN
        INSERT INTO biz_sales_forecast (forecast_no, period_code, forecast_amount)
        VALUES ('PROBE-8', '2026Q1', 100);
        RAISE EXCEPTION 'V0318 自检失败：非法期间码 2026Q1 被接受';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    -- ⑥.5 正样本：合法行能插、默认值对、同范围同期间插第二条被唯一索引拒
    INSERT INTO biz_sales_target (id, target_no, scope_type, scope_id, period_code, target_amount)
    VALUES (probe, 'PROBE-OK', 'ALL', NULL, '2026-Q1', 1000);
    IF (SELECT status FROM biz_sales_target WHERE id = probe) <> 'ACTIVE' THEN
        RAISE EXCEPTION 'V0318 自检失败：status 默认值不是 ACTIVE';
    END IF;
    IF (SELECT target_qty FROM biz_sales_target WHERE id = probe) <> 0 THEN
        RAISE EXCEPTION 'V0318 自检失败：target_qty 默认值不是 0';
    END IF;

    BEGIN
        INSERT INTO biz_sales_target (target_no, scope_type, scope_id, period_code, target_amount)
        VALUES ('PROBE-DUPE', 'ALL', NULL, '2026-Q1', 2000);
        RAISE EXCEPTION 'V0318 自检失败：同范围同期间的第二条目标被接受（uk_biz_sales_target_scope 失效）';
    EXCEPTION WHEN unique_violation THEN NULL;
    END;

    DELETE FROM biz_sales_target WHERE id = probe;

    -- ⑥.6 取号规则两行
    SELECT count(*) INTO v_cnt FROM biz_seq
     WHERE seq_type IN ('STG','SFC') AND deleted_at IS NULL;
    IF v_cnt <> 2 THEN
        RAISE EXCEPTION 'V0318 自检失败：biz_seq(STG/SFC) 只种下 % 行', v_cnt;
    END IF;

    -- ⑥.7 8 个权限码各且仅 1 行承载
    SELECT string_agg(code, ', ') INTO v_missing
      FROM (VALUES ('business:erp:sales-target:list'),
                   ('business:erp:sales-target:create'),
                   ('business:erp:sales-target:update'),
                   ('business:erp:sales-target:delete'),
                   ('business:erp:sales-forecast:list'),
                   ('business:erp:sales-forecast:create'),
                   ('business:erp:sales-forecast:update'),
                   ('business:erp:sales-forecast:delete')) AS v(code)
     WHERE NOT EXISTS (SELECT 1 FROM sys_menu m
                        WHERE m.auth_code = v.code AND m.deleted_at IS NULL);
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0318 自检失败：以下权限码在 sys_menu 没有承载行：%', v_missing;
    END IF;

    SELECT string_agg(auth_code || '=' || cnt, ', ') INTO v_dupe
      FROM (SELECT auth_code, count(*) AS cnt FROM sys_menu
             WHERE auth_code LIKE 'business:erp:sales-target:%'
                OR auth_code LIKE 'business:erp:sales-forecast:%'
             GROUP BY auth_code HAVING count(*) <> 1) AS d;
    IF v_dupe IS NOT NULL THEN
        RAISE EXCEPTION 'V0318 自检失败：权限码承载行不唯一：%', v_dupe;
    END IF;

    -- ⑥.8 页面节点：path / component / 可见 / 父级 = 销售管理
    SELECT path, component, is_visible, parent_id
      INTO v_path, v_component, v_visible, v_parent
      FROM sys_menu WHERE id = '20000000-0000-0000-0000-000000003181'::uuid;
    IF v_path IS DISTINCT FROM '/business/erp/sales-target' THEN
        RAISE EXCEPTION 'V0318 自检失败：页面 path 不对（%）', v_path;
    END IF;
    IF v_component IS DISTINCT FROM 'business/erp/sales-target/index' THEN
        RAISE EXCEPTION 'V0318 自检失败：页面 component 不对（%）', v_component;
    END IF;
    IF v_visible IS NOT TRUE THEN
        RAISE EXCEPTION 'V0318 自检失败：页面 is_visible 不是 TRUE（n08 会缺菜单）';
    END IF;
    IF v_parent IS DISTINCT FROM '20000000-0000-0000-0000-000000002701'::uuid THEN
        RAISE EXCEPTION 'V0318 自检失败：页面父级不是销售管理（…2701）';
    END IF;

    -- ⑥.9 关键授权在位（角色不存在时 0 授权属正常）
    IF EXISTS (SELECT 1 FROM sys_role WHERE role_code = 'SALES_SUPERVISOR' AND deleted_at IS NULL) THEN
        SELECT count(*) INTO v_rows
          FROM sys_role_menu rm
          JOIN sys_role r ON r.id = rm.role_id AND r.role_code = 'SALES_SUPERVISOR' AND r.deleted_at IS NULL
         WHERE rm.menu_id IN ('20000000-0000-0000-0000-000000003181'::uuid,
                              '20000000-0000-0000-0000-000000003188'::uuid);
        IF v_rows <> 2 THEN
            RAISE EXCEPTION 'V0318 自检失败：SALES_SUPERVISOR 只拿到 % / 2 个页面授权（只算了页面与最后一个按钮，见下）', v_rows;
        END IF;
    END IF;

    RAISE NOTICE 'SO-44: 销售目标 / 预测两表 + 8 个权限码 + 菜单 + 取号规则自检通过';
END $$;
