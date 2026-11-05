-- ============================================================
-- 00_install_compat.sql —— 巡检兼容层安装（幂等，可重复执行）
--
-- 目的：让「同一条巡检 SQL」在两种 schema 阶段都能直接跑：
--   A 旧表阶段（当前生产）：biz_purchase_order 无 contract_id/purchase_type，biz_contract 不存在
--   B V0193 之后：合同表与采购单合同三列已就位
--
-- 手法：巡检 SQL 不直接引用业务表列，而是引用 di 命名空间下的三个视图；
--   本脚本用 catalog 探测决定视图的两种定义（直列 / jsonb 兜底 / 空壳）。
--   视图定义在 di schema，不污染 public，也不会被误认为业务表。
--
-- 可重复执行：CREATE OR REPLACE VIEW（两阶段列名/类型一致，无需 DROP，也不会连带删掉依赖视图）；
-- 不新建表、不写业务数据。
-- 回滚：DROP SCHEMA di CASCADE;
-- ============================================================

BEGIN;

CREATE SCHEMA IF NOT EXISTS di;
COMMENT ON SCHEMA di IS '数据一致性巡检辅助对象（视图只读）：不属于业务模型，可随时重建';

-- ---------- ① 能力探测 ----------
CREATE OR REPLACE VIEW di.di_capability AS
SELECT
    (to_regclass('public.biz_contract') IS NOT NULL)                       AS has_contract_table,
    EXISTS (SELECT 1 FROM information_schema.columns
             WHERE table_schema='public' AND table_name='biz_purchase_order'
               AND column_name='contract_id')                              AS has_po_contract_id,
    EXISTS (SELECT 1 FROM information_schema.columns
             WHERE table_schema='public' AND table_name='biz_purchase_order'
               AND column_name='purchase_type')                            AS has_po_purchase_type,
    (to_regclass('public.biz_contract_party_change') IS NOT NULL)          AS has_party_change_ledger,
    -- 合同维度巡检（DI-01..DI-05）就绪条件：合同表 + 采购单两列
    (to_regclass('public.biz_contract') IS NOT NULL
     AND EXISTS (SELECT 1 FROM information_schema.columns
                  WHERE table_schema='public' AND table_name='biz_purchase_order'
                    AND column_name IN ('contract_id','purchase_type')
                  GROUP BY table_name HAVING count(*) = 2))                AS contract_checks_ready;

COMMENT ON VIEW di.di_capability IS
  '巡检能力探测：contract_checks_ready=false 时 DI-01..DI-05 必须记 SKIPPED，不能记 PASS（0 行 ≠ 无缺陷）';

-- ---------- ② 采购单视图：合同两列按 schema 阶段二选一 ----------
DO $do$
DECLARE
    has_cols BOOLEAN;
BEGIN
    SELECT EXISTS (SELECT 1 FROM information_schema.columns
                    WHERE table_schema='public' AND table_name='biz_purchase_order'
                      AND column_name IN ('contract_id','purchase_type')
                    GROUP BY table_name HAVING count(*) = 2)
      INTO has_cols;

    IF has_cols THEN
        EXECUTE $sql$
            CREATE OR REPLACE VIEW di.v_po AS
            SELECT o.id, o.order_no, o.supplier_id, o.supplier_name_snapshot,
                   o.status, o.order_date, o.purchase_type,
                   o.contract_id, o.contract_no_snapshot,
                   o.created_at, o.updated_at, o.deleted_at, FALSE AS via_jsonb
              FROM biz_purchase_order o
        $sql$;
    ELSE
        -- 旧表阶段：用 to_jsonb 取「不存在的列」→ NULL，解析期不报错
        EXECUTE $sql$
            CREATE OR REPLACE VIEW di.v_po AS
            SELECT o.id, o.order_no, o.supplier_id, NULL::varchar(200) AS supplier_name_snapshot,
                   o.status, o.order_date,
                   (to_jsonb(o)->>'purchase_type')::varchar(8)   AS purchase_type,
                   (to_jsonb(o)->>'contract_id')::uuid           AS contract_id,
                   (to_jsonb(o)->>'contract_no_snapshot')::varchar(32) AS contract_no_snapshot,
                   o.created_at, o.updated_at, o.deleted_at, TRUE AS via_jsonb
              FROM biz_purchase_order o
        $sql$;
    END IF;
END
$do$;

COMMENT ON VIEW di.v_po IS
  '采购单巡检视图：合同两列在旧表阶段返回 NULL（via_jsonb=t）；V0193 落地后为真实列（via_jsonb=f）。巡检 SQL 只认本视图';

-- ---------- ③ 合同视图：无表时给空壳（JOIN 恒 0 行，不报错）----------
DO $do$
BEGIN
    IF to_regclass('public.biz_contract') IS NOT NULL THEN
        EXECUTE $sql$
            CREATE OR REPLACE VIEW di.v_contract AS
            SELECT c.id, c.contract_no, c.contract_name, c.contract_type,
                   c.party_b_type, c.party_b_id, c.party_b_name,
                   c.status, c.sign_date, c.start_date, c.end_date,
                   c.created_at, c.updated_at, c.deleted_at
              FROM biz_contract c
        $sql$;
    ELSE
        EXECUTE $sql$
            CREATE OR REPLACE VIEW di.v_contract AS
            SELECT NULL::uuid AS id, NULL::varchar(32) AS contract_no,
                   NULL::varchar(200) AS contract_name, NULL::varchar(16) AS contract_type,
                   NULL::varchar(16) AS party_b_type, NULL::uuid AS party_b_id,
                   NULL::varchar(200) AS party_b_name, NULL::varchar(16) AS status,
                   NULL::date AS sign_date, NULL::date AS start_date, NULL::date AS end_date,
                   NULL::timestamptz AS created_at, NULL::timestamptz AS updated_at,
                   NULL::timestamptz AS deleted_at
             WHERE FALSE
        $sql$;
    END IF;
END
$do$;

COMMENT ON VIEW di.v_contract IS
  '合同巡检视图：V0193 之前是「零行空壳」，因此依赖它的巡检天然 0 行 —— 调用方必须用 di.di_capability 判 SKIPPED';

-- ---------- ④ 合同乙方变更台账视图 ----------
-- 台账来源优先级：① biz_contract_party_change（推荐随 V0195 增建）② 审批实例（CONTRACT/AMEND 已通过）
-- 两者都无 ⇒ 空视图 ⇒ DI-01 对任何偏移一律判「无台账 = P0 疑似篡改」（保守，宁多报不放过）
DO $do$
DECLARE
    parts TEXT[] := ARRAY[]::TEXT[];
    ddl   TEXT;
BEGIN
    IF to_regclass('public.biz_contract_party_change') IS NOT NULL THEN
        parts := array_append(parts, $sql$
            SELECT contract_id, changed_at, old_party_b_id, new_party_b_id, 'LEDGER'::text AS src
              FROM biz_contract_party_change
        $sql$::text);
    END IF;

    IF to_regclass('public.biz_flow_instance') IS NOT NULL
       AND to_regclass('public.biz_contract') IS NOT NULL THEN
        parts := array_append(parts, $sql$
            SELECT i.biz_id AS contract_id, i.ended_at AS changed_at,
                   NULL::uuid AS old_party_b_id, NULL::uuid AS new_party_b_id, 'FLOW'::text AS src
              FROM biz_flow_instance i
             WHERE i.deleted_at IS NULL
               AND i.status = 'APPROVED'
               AND i.biz_type = 'CONTRACT'
               AND i.ended_at IS NOT NULL
        $sql$::text);
    END IF;

    IF array_length(parts, 1) IS NULL THEN
        ddl := $sql$
            CREATE OR REPLACE VIEW di.v_party_change AS
            SELECT NULL::uuid AS contract_id, NULL::timestamptz AS changed_at,
                   NULL::uuid AS old_party_b_id, NULL::uuid AS new_party_b_id,
                   NULL::text AS src
             WHERE FALSE
        $sql$;
    ELSE
        ddl := 'CREATE OR REPLACE VIEW di.v_party_change AS ' || array_to_string(parts, ' UNION ALL ');
    END IF;

    EXECUTE ddl;
END
$do$;

COMMENT ON VIEW di.v_party_change IS
  '合同乙方变更台账（合同 id + 变更时刻）：DI-01 靠它把「合法历史快照」与「事后篡改」分开；空视图 = 无台账 = 一律按 P0 处理';

-- ---------- ⑤ 固定资产视图：V0229 之前给空壳（JOIN 恒 0 行，不报错）----------
-- 为什么要这一层：`biz_fixed_asset.supplier_name` 是**快照名**（建卡时取主档名），
-- 是 DI-26「快照名 vs 现名」的四类来源之一；但该表 V0229 才建，
-- 巡检 SQL 直接引用它会让 V0229 之前的库 `--install-views` 整体失败。
DO $do$
BEGIN
    IF to_regclass('public.biz_fixed_asset') IS NOT NULL THEN
        EXECUTE $sql$
            CREATE OR REPLACE VIEW di.v_asset AS
            SELECT a.id, a.asset_no, a.supplier_id, a.supplier_name,
                   a.created_at, a.updated_at, a.deleted_at
              FROM biz_fixed_asset a
        $sql$;
    ELSE
        EXECUTE $sql$
            CREATE OR REPLACE VIEW di.v_asset AS
            SELECT NULL::uuid AS id, NULL::varchar(32) AS asset_no,
                   NULL::uuid AS supplier_id, NULL::varchar(200) AS supplier_name,
                   NULL::timestamptz AS created_at, NULL::timestamptz AS updated_at,
                   NULL::timestamptz AS deleted_at
             WHERE FALSE
        $sql$;
    END IF;
END
$do$;

COMMENT ON VIEW di.v_asset IS
  '固定资产巡检视图：V0229 之前是「零行空壳」，DI-26 的资产快照分支因此天然 0 行（不是通过，是还没这张表）';

COMMIT;
