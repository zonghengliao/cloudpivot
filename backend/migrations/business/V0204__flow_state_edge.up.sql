-- ============================================================
-- V0204：状态机边配置 `biz_flow_state_edge`（【D1】流程配置化 · 阶段 3）
--
-- 依据：backend/src/business/shared/state_machine.rs
--       （BASE_EDGES 硬约束白名单 + 运行时合并 effective_edges）
--
-- # 迁移号
--
--   卡面写的是「新表迁移 V0201（先复核号段空着）」。**核对后 V0201 已被占用**
--   （`V0201__erp_material_costing_moving_avg_only`），当前最大号是 V0203
--   （`V0203__erp_stock_out`，出库单），故本卡取下一个空号 **V0204**。
--   号段复核命令：`ls backend/migrations/business | grep -oP 'V\d+' | sort -u | tail -3`
--
-- # 这张表是什么（第 2 层）
--
--   第 1 层在代码里：`state_machine::Entity::base_edges()` 的 BASE_EDGES（硬约束白名单，
--   永远存在、永远不能被配置删掉）。
--   第 2 层是本表：**只能从 BASE_EDGES 里挑**，作用是「加严」——
--     · enabled = false          → 禁用一条 **optional** 边（mandatory 边禁用无效，会打 WARN）
--     · requires_reason = true   → 走这条边必须填原因（只能加，不能减）
--     · label                    → 改显示文案（纯展示）
--     · auth_code                → 追加权限码要求（收紧谁能操作这条边）
--   第 3 层在运行时：`effective_edges()` 把两层合并；本表读失败 / 表不存在 / 数据脏
--   一律**退化到 BASE_EDGES**（fail-safe，宁可回到旧行为也不能让单据流转崩掉）。
--
-- # 为什么不允许「添加 BASE_EDGES 里没有的边」
--
--   本表只是**挑子集 + 加约束**的载体，不是「边表」的真相来源：
--   真相永远是代码里的 BASE_EDGES。脏行（BASE 里没有的 (from,to)、未知 entity）
--   在运行时会**被忽略并打 WARN**，API 写入时直接 400 拒绝。
--
-- # 为什么唯一约束是 `(entity, from_status, to_status)` 且删除走软删
--
--   一条边在库里只该有一行配置（重复行会让「到底用哪个 label」变得不可判）。
--   软删（`deleted_at`）+ 唯一约束的组合：`upsert` 时 `ON CONFLICT … DO UPDATE
--   SET deleted_at = NULL` 即可「复活」旧行，不需要为「删了再配」另开路径。
--   存活行一律 `deleted_at IS NULL`（读路径全部带这个条件）。
--
-- # 留痕
--
--   本表**不是**审计记录：谁在什么时候把哪条边改成什么，写在 `biz_status_log`
--   （`entity_type = 'FLOW_EDGE_CFG'`，只增不改）。改配置的入口在代码里：
--   `state_machine::upsert_edge_config` / `delete_edge_config`，
--   都要 SUPER_ADMIN，且写前 / 写后各留一行快照。
--
-- # 幂等
--
--   CREATE TABLE / INDEX / CONSTRAINT IF NOT EXISTS；不写 BEGIN/COMMIT
--   （迁移器自己在事务里执行）。**不种任何数据**：空表 = 全部使用 BASE_EDGES。
-- ============================================================

CREATE TABLE IF NOT EXISTS biz_flow_state_edge (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    -- 实体类型：与 shared/state_machine.rs 的 Entity::as_str 逐字一致
    -- （TASK 之类不在状态机注册表里的值会被运行时忽略 + WARN）
    entity          varchar(32) NOT NULL,
    from_status     varchar(32) NOT NULL,
    to_status       varchar(32) NOT NULL,
    -- false = 禁用这条（仅对 optional 边生效；mandatory 边禁用会被忽略）
    enabled         boolean     NOT NULL DEFAULT true,
    -- true = 走这条边必须填原因（只能加严）
    requires_reason boolean     NOT NULL DEFAULT false,
    -- 显示文案（空 = 用 BASE_EDGES 的 default_label）
    label           varchar(64),
    -- 追加的权限码要求（空 = 不追加）
    auth_code       varchar(64),
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    create_by       uuid,
    update_by       uuid,
    -- 软删：配置「删掉」= 回到 BASE_EDGES 行为（不是删除边本身）
    deleted_at      timestamptz,
    CONSTRAINT uk_flow_state_edge UNIQUE (entity, from_status, to_status)
);

COMMENT ON TABLE biz_flow_state_edge IS
    '状态机边配置（只能是 BASE_EDGES 的子集，且只能加严）：运行时可禁用 optional 边 / 追加必填原因 / 改文案 / 追加权限码；读失败一律退化到代码里的 BASE_EDGES。改配置需 SUPER_ADMIN，留痕写 biz_status_log(entity_type=FLOW_EDGE_CFG)。';

-- 读路径：按实体取存活行（运行时合并用）
CREATE INDEX IF NOT EXISTS idx_flow_state_edge_entity
    ON biz_flow_state_edge (entity)
    WHERE deleted_at IS NULL;

-- ------------------------------------------------------------
-- 结构自检（迁移期断言，失败即整笔回滚）
-- ------------------------------------------------------------
DO $$
DECLARE
    n_col   integer;
    n_uk    integer;
    n_idx   integer;
    n_rows  integer;
BEGIN
    SELECT count(*) INTO n_col FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_flow_state_edge'
       AND column_name IN ('id', 'entity', 'from_status', 'to_status', 'enabled',
                           'requires_reason', 'label', 'auth_code', 'created_at',
                           'updated_at', 'create_by', 'update_by', 'deleted_at');
    IF n_col <> 13 THEN
        RAISE EXCEPTION 'V0204 自检失败：biz_flow_state_edge 列数应为 13，实际 %', n_col;
    END IF;

    SELECT count(*) INTO n_uk FROM pg_constraint
     WHERE conname = 'uk_flow_state_edge'
       AND conrelid = 'biz_flow_state_edge'::regclass;
    IF n_uk <> 1 THEN
        RAISE EXCEPTION 'V0204 自检失败：缺唯一约束 uk_flow_state_edge（防止同一条边两行配置）';
    END IF;

    SELECT count(*) INTO n_idx FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_flow_state_edge'
       AND indexname = 'idx_flow_state_edge_entity';
    IF n_idx <> 1 THEN
        RAISE EXCEPTION 'V0204 自检失败：缺索引 idx_flow_state_edge_entity';
    END IF;

    -- 空表是刻意的：默认行为逐字等于改动前（全部使用 BASE_EDGES）
    SELECT count(*) INTO n_rows FROM biz_flow_state_edge;
    IF n_rows <> 0 THEN
        RAISE NOTICE 'V0204：biz_flow_state_edge 已有 % 行配置（沿用）', n_rows;
    END IF;

    RAISE NOTICE 'V0204 完成：biz_flow_state_edge 建表 + 唯一约束 + 存活行索引，初始 0 行（全部使用 BASE_EDGES）';
END $$;
