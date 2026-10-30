-- ============================================================
-- V0139：库存加库位维度（`biz_inventory.location_id`）+ 三列唯一索引 + 在途仓（系统仓）
--
-- 依据：
--   ① 卡面 W3a §二（TSK-202610-000681）：加 location_id / 唯一约束改三列 / 插在途仓种子
--   ② `docs/ERP-仓库管理-技术架构.md`（W0 设计）§2.2 仓库树四列、§2.4 两级账、
--      §5.1「biz_inventory 要不要加 location_id」、§6.4「在途的货放哪」
--   ③ W1a 已落地的真实结构（本迁移只 ALTER，不重建任何表）
--
-- ── 开工先查的真实结构（2026-10-30 实测，cloudpivot_dev）──────────────────
--   `biz_inventory`（10 列，**没有** deleted_at —— 卡面 §二① 的疑问在这里定案）：
--       id / material_id / warehouse_id / qty / locked_qty / avg_cost / amount
--       / last_txn_at / created_at / updated_at
--       唯一索引 `uk_biz_inv_mat_wh (material_id, warehouse_id)`
--       CHECK `ck_biz_inv_locked (locked_qty >= 0)`；**没有任何外键**
--   `biz_inventory_location`（库位级数量账，W1a 建）：
--       id / material_id / warehouse_id / location_id / qty / locked_qty / last_txn_at / created_at / updated_at
--       唯一索引 `uk_biz_inv_loc (material_id, warehouse_id, location_id) NULLS NOT DISTINCT`
--   `biz_warehouse`：树列叫 `warehouse_path` / `level` / `is_leaf`（**不是** `path`）；
--       `ck_biz_warehouse_root CHECK ((parent_id IS NULL) = (level = 1))`
--       `ck_biz_warehouse_type CHECK (warehouse_type IN ('NORMAL','RAW','FINISHED','SCRAP','IN_TRANSIT'))`
--       → **IN_TRANSIT 枚举早已存在**，本迁移不改枚举、不动 CHECK
--       `uk_biz_warehouse_code` 是**部分**唯一索引（WHERE deleted_at IS NULL）
--
-- ── 与卡面逐条对齐（差异全部写在这里，别当 bug）────────────────────────────
--   ① `biz_inventory.location_id` 的硬外键目标 = **`biz_inventory_location(id)`**（卡面原文）。
--      这是本仓既定口径：W2a/W2b 的「location_id」一律指**库位库存行 id**，不是 `biz_location.id`
--      （见 `business/erp/serial_model.rs` 模块头与 W2a 证据文档第 5 条）。本迁移沿用，不新造语义。
--   ② 三列唯一索引**不带** `WHERE deleted_at IS NULL`：实测 `biz_inventory` 没有 deleted_at 列
--      （卡面 §二① 提示的那条「已实测没有」是**对的**）。用 `COALESCE(location_id, 全零 uuid)` 把
--      NULL 折成一个可比较的值 —— 否则 PG 认为 NULL 彼此不相等，同物料同仓可以插任意多行 NULL 库位行。
--   ③ 在途仓种子：单行、固定 UUID、`warehouse_code='WH-TRANSIT'`、`warehouse_type='IN_TRANSIT'`、
--      level=1 / parent_id NULL / warehouse_path='/自己id/' / is_leaf=1（与 V0137 种子 WH01 同技法）。
--      幂等用 `WHERE NOT EXISTS (… deleted_at IS NULL)`（部分唯一索引不能用 `ON CONFLICT (warehouse_code)`）。
--   ④ **连带修复（同一个 push 的另一条 commit，不是本文件）**：三列索引顶掉 `uk_biz_inv_mat_wh` 后，
--      代码里两处 `ON CONFLICT (material_id, warehouse_id)` 的**推断目标就失效**（PG 42P10：
--      `there is no unique or exclusion constraint matching the ON CONFLICT specification`）：
--        · `business/erp/serial_model.rs::recompute_quantity`（**生产路径**，W2c-1 不变式①）
--        · `business/erp/serial_handler.rs` 里的真库测试夹具（测试路径）
--      处理：这两处的冲突目标改成 `ON CONFLICT (material_id, warehouse_id,
--      COALESCE(location_id, '00000000-0000-0000-0000-000000000000'::uuid))`。
--      语义不变：`recompute_quantity` 算的是「该物料该仓的全部在库件数」= **仓级汇总**，
--      落进 `location_id IS NULL` 这一桶（与 W0 §2.4「NULL = 未分配库位/整仓汇总」一致）。
--   ⑤ **迁移不可重放**：V0137 up 的自检 ③ 断言 `uk_biz_inv_mat_wh` 存在（V0137 第 746 行），
--      V0139 之后重放 V0137 up 会挂在那条断言上。这符合仓里既有口径（迁移只前进；
--      `schema_migrations` 的 checksum 也禁止改已应用的 up 文件）。
--
-- 回滚：`V0139__erp_inventory_location.down.sql`
--   （顺序由迁移器保证：V0140 down 先跑，然后才轮到本文件）
--   有「同物料同仓多库位行」时 down 会**中止**并报出冲突组合（无法退回两列唯一），
--   这是有意的安全阀：宁可让运维处理数据，也不静默丢库位维度。
--
-- ⚠ 文件内**不写** `BEGIN; … COMMIT;` —— 迁移器自己在事务里执行本文件（V0136 的坑）。
-- ============================================================

-- ============================================================
-- §1 `biz_inventory` 加库位维度
-- ============================================================

-- ---------- §1.1 加列（可空：NULL = 未分配库位/整仓汇总桶）----------
ALTER TABLE biz_inventory ADD COLUMN IF NOT EXISTS location_id UUID;

COMMENT ON COLUMN biz_inventory.location_id IS
    '库位库存行 id（指向 biz_inventory_location.id；NULL = 未分配库位，即该物料该仓的汇总桶）';

-- ---------- §1.2 加外键（命名 + 幂等；ADD COLUMN 的 IF NOT EXISTS 会连约束一起跳过）----------
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'fk_biz_inv_location_id'
                      AND conrelid = 'public.biz_inventory'::regclass) THEN
        ALTER TABLE biz_inventory
            ADD CONSTRAINT fk_biz_inv_location_id
            FOREIGN KEY (location_id) REFERENCES biz_inventory_location(id);
        RAISE NOTICE 'V0139 §1.2：已建外键 fk_biz_inv_location_id → biz_inventory_location(id)';
    ELSE
        RAISE NOTICE 'V0139 §1.2：外键 fk_biz_inv_location_id 已存在，跳过（幂等）';
    END IF;
END
$$;

-- ============================================================
-- §2 唯一约束：两列 → 三列（卡面 §二① 的关键决策）
--     旧：uk_biz_inv_mat_wh      (material_id, warehouse_id)
--     新：uk_biz_inv_mat_wh_loc  (material_id, warehouse_id, COALESCE(location_id, 全零 uuid))
--     代价与处置见文件头 ④（代码里 ON CONFLICT 的推断目标同步改）。
-- ============================================================
DROP INDEX IF EXISTS uk_biz_inv_mat_wh;

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_inv_mat_wh_loc
    ON biz_inventory (material_id, warehouse_id,
                      coalesce(location_id, '00000000-0000-0000-0000-000000000000'::uuid));

COMMENT ON INDEX uk_biz_inv_mat_wh_loc IS
    '同物料同仓同库位唯一；location_id 的 NULL（汇总桶）用全零 uuid 折叠，等价于 NULLS NOT DISTINCT 的效果';

-- 库位维度反查（按仓查该仓库所有库位的库存行；与 biz_inventory_location 的 idx_biz_inv_loc_wh 同形）
CREATE INDEX IF NOT EXISTS idx_biz_inv_location ON biz_inventory (warehouse_id, location_id);

-- ============================================================
-- §3 在途仓（系统仓库）种子
--     W0 §6.4：在途的货放**在途仓**（warehouse_type='IN_TRANSIT'，没有库位 → 靠 location_id IS NULL 桶）
-- ============================================================
INSERT INTO biz_warehouse (id, warehouse_code, warehouse_name, warehouse_type, status, sort_order,
                           remark, parent_id, level, warehouse_path, is_leaf)
SELECT v.id::uuid, v.warehouse_code, v.warehouse_name, 'IN_TRANSIT', 1, 99, v.remark,
       NULL, 1, '/' || v.id || '/', 1
  FROM (VALUES
        ('d1390001-0000-4000-8000-000000000001', 'WH-TRANSIT', '在途仓',
         '系统仓（V0139 种子）：仓库间调拨的「在途」库存归属，非真实物理仓；无库位，库存走 biz_inventory.location_id IS NULL 桶')
       ) AS v(id, warehouse_code, warehouse_name, remark)
 WHERE NOT EXISTS (SELECT 1 FROM biz_warehouse w
                    WHERE w.warehouse_code = v.warehouse_code AND w.deleted_at IS NULL);

-- 注：`biz_warehouse` 的表注释属 V0137（已应用，checksum 不可改），本迁移**不覆盖**它。

-- ============================================================
-- §4 自检（空库 / 有数据重放都必须过；只断结构 + 约束真的会拦，不依赖库里既有数据）
-- ============================================================
DO $$
DECLARE
    n            INT;
    v_uk         INT;
    n_keys       SMALLINT;
    n_fk         INT;
    n_seed       INT;
    blocked      BOOLEAN;
    v_prod       UUID := gen_random_uuid();
    v_mat        UUID := '00000000-0000-4000-8000-0000000003a1'::uuid;
    v_wh         UUID := '00000000-0000-4000-8000-0000000003a2'::uuid;
    v_loc1       UUID;
    v_loc2       UUID;
BEGIN
    -- ① location_id 列在，且可空（NULL = 汇总桶）
    SELECT count(*) INTO n
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_inventory'
       AND column_name = 'location_id' AND data_type = 'uuid' AND is_nullable = 'YES';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0139 自检失败：biz_inventory.location_id 不存在或不是可空 uuid（%）', n;
    END IF;

    -- ② 外键指向 biz_inventory_location（不是 biz_location）
    SELECT count(*) INTO n_fk
      FROM pg_constraint c
     WHERE c.conname = 'fk_biz_inv_location_id'
       AND c.conrelid = 'public.biz_inventory'::regclass
       AND c.contype = 'f'
       AND c.confrelid = 'public.biz_inventory_location'::regclass;
    IF n_fk <> 1 THEN
        RAISE EXCEPTION 'V0139 自检失败：fk_biz_inv_location_id 不存在或目标不是 biz_inventory_location';
    END IF;

    -- ③ 旧两列唯一索引必须消失，新三列唯一索引必须唯一 + 3 个键列 + 第 3 列是 COALESCE 表达式
    SELECT count(*) INTO n FROM pg_indexes
     WHERE schemaname = 'public' AND tablename = 'biz_inventory' AND indexname = 'uk_biz_inv_mat_wh';
    IF n <> 0 THEN
        RAISE EXCEPTION 'V0139 自检失败：旧索引 uk_biz_inv_mat_wh 仍存在';
    END IF;

    SELECT i.indisunique::int, i.indnkeyatts INTO v_uk, n_keys
      FROM pg_index i JOIN pg_class ic ON ic.oid = i.indexrelid
     WHERE ic.relname = 'uk_biz_inv_mat_wh_loc';
    IF v_uk IS NULL THEN
        RAISE EXCEPTION 'V0139 自检失败：新索引 uk_biz_inv_mat_wh_loc 不存在';
    END IF;
    IF v_uk <> 1 OR n_keys <> 3 THEN
        RAISE EXCEPTION 'V0139 自检失败：uk_biz_inv_mat_wh_loc 不是三列唯一索引（unique=% / keys=%）', v_uk, n_keys;
    END IF;

    SELECT count(*) INTO n
      FROM pg_indexes
     WHERE schemaname = 'public' AND indexname = 'uk_biz_inv_mat_wh_loc'
       AND indexdef ILIKE '%coalesce(location_id%';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0139 自检失败：uk_biz_inv_mat_wh_loc 的第 3 列不是 COALESCE(location_id, 全零 uuid) 表达式';
    END IF;

    -- ④ 在途仓种子：恰好 1 行，类型 IN_TRANSIT，根节点，路径自洽
    SELECT count(*) INTO n_seed
      FROM biz_warehouse
     WHERE warehouse_code = 'WH-TRANSIT' AND deleted_at IS NULL;
    IF n_seed <> 1 THEN
        RAISE EXCEPTION 'V0139 自检失败：在途仓 WH-TRANSIT 行数 = %（期望 1）', n_seed;
    END IF;

    SELECT count(*) INTO n
      FROM biz_warehouse
     WHERE warehouse_code = 'WH-TRANSIT' AND deleted_at IS NULL
       AND warehouse_type = 'IN_TRANSIT'
       AND parent_id IS NULL AND level = 1 AND is_leaf = 1
       AND warehouse_path = '/' || id::text || '/'
       AND id = 'd1390001-0000-4000-8000-000000000001'::uuid;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0139 自检失败：在途仓的类型/层级/路径/固定 UUID 不自洽';
    END IF;

    -- ⑤ 反例：仓库类型不在枚举内 → 必须被 ck_biz_warehouse_type 拦住（证明 CHECK 仍然只有 5 个值）
    blocked := FALSE;
    BEGIN
        INSERT INTO biz_warehouse (warehouse_code, warehouse_name, warehouse_type, level, warehouse_path)
        VALUES ('__W3A_BAD_TYPE__', '自检-坏类型仓', 'IN_TRANSIT_X', 1, '/x/');
    EXCEPTION WHEN check_violation THEN
        blocked := TRUE;
    END;
    IF NOT blocked THEN
        RAISE EXCEPTION 'V0139 自检失败：warehouse_type 非法值被放进库（ck_biz_warehouse_type 失效）';
    END IF;

    -- ⑥ 三列唯一索引真的生效（本轮自检数据用独立 UUID，跑完清场）
    INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty)
    VALUES (gen_random_uuid(), v_mat, v_wh, NULL, 0)
    RETURNING id INTO v_loc1;
    INSERT INTO biz_inventory_location (id, material_id, warehouse_id, location_id, qty)
    VALUES (gen_random_uuid(), v_mat, v_wh, v_prod, 0)
    RETURNING id INTO v_loc2;

    -- 正例：同物料同仓，库位 NULL 和库位 v_prod 各一行 → 允许
    INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty)
    VALUES (gen_random_uuid(), v_mat, v_wh, NULL, 1);
    INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty)
    VALUES (gen_random_uuid(), v_mat, v_wh, v_loc2, 2);

    -- 反例：同物料同仓同库位（NULL）再来一行 → 必须被唯一索引拦住
    blocked := FALSE;
    BEGIN
        INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty)
        VALUES (gen_random_uuid(), v_mat, v_wh, NULL, 9);
    EXCEPTION WHEN unique_violation THEN
        blocked := TRUE;
    END;
    IF NOT blocked THEN
        RAISE EXCEPTION 'V0139 自检失败：同物料同仓的 location_id IS NULL 行可重复插入（uk_biz_inv_mat_wh_loc 未生效）';
    END IF;

    -- 反例：同物料同仓同库位（非 NULL）再来一行 → 同样必须被拦住
    blocked := FALSE;
    BEGIN
        INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty)
        VALUES (gen_random_uuid(), v_mat, v_wh, v_loc2, 9);
    EXCEPTION WHEN unique_violation THEN
        blocked := TRUE;
    END;
    IF NOT blocked THEN
        RAISE EXCEPTION 'V0139 自检失败：同物料同仓同库位的行可重复插入（uk_biz_inv_mat_wh_loc 未生效）';
    END IF;

    -- 反例：location_id 指向不存在的库位库存行 → 必须被外键拦住
    blocked := FALSE;
    BEGIN
        INSERT INTO biz_inventory (id, material_id, warehouse_id, location_id, qty)
        VALUES (gen_random_uuid(), v_mat, v_wh, '00000000-0000-4000-8000-0000000fffff', 1);
    EXCEPTION WHEN foreign_key_violation THEN
        blocked := TRUE;
    END;
    IF NOT blocked THEN
        RAISE EXCEPTION 'V0139 自检失败：location_id 悬空引用被放进库（fk_biz_inv_location_id 失效）';
    END IF;

    -- 清场（自检数据一行不留）
    DELETE FROM biz_inventory          WHERE material_id = v_mat AND warehouse_id = v_wh;
    DELETE FROM biz_inventory_location WHERE material_id = v_mat AND warehouse_id = v_wh;

    RAISE NOTICE 'V0139 自检通过：location_id + FK / 三列唯一索引（含 NULL 折叠与不同库位共存）/ 在途仓种子 1 行 / 3 条反例拦截 / 自检数据已清场';
END
$$;
