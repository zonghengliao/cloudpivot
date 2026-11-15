-- ============================================================
-- V0201 down：放宽 ck_biz_material_costing（三种取值时代）
--
-- # 回滚分档（与 V0198 / V0199 down 同口径）
--   档 A（本文件实际执行）：约束重建为 V0137 的三值域
--     `IN ('MOVING_AVG','FIFO','STANDARD')` + 注释回退到 V0201 之前（无列注释）。
--     **结构无损**：只是把「能存什么值」放宽回去，不动任何业务数据。
--   档 B（本文件**不做**，必须人工决定）：回填不可逆 —— 见下。
--
-- # ⚠️ 可逆性说明（重要，别指望 down 能把数据改回来）
--   up 的 S2 把 `costing_method <> 'MOVING_AVG'` 的行**改写成 MOVING_AVG**，原值没有随行保存。
--   PG 没有「列级历史」，本迁移也刻意**不建归档表**（ERP 已有 30 张表，为一次实测 0 行的
--   回填再加一张表不划算）。因此：
--     · 本 down 只恢复「能写 FIFO / STANDARD」的能力，被回填过的物料**仍然是 MOVING_AVG**；
--     · 需要复原那些行时，取值来源只有 up 迁移时 S1 播报的迁移日志（NOTICE
--       `S1 存量体检：… 明细：<material_code>=<旧值>, …`），按日志人工改回。
--   **实测口径**：上线前三库（prod `cloudpivot` / dev+Test `cloudpivot_dev`）S1 命中 0 行，
--   故本次 down 在现网是**零数据损失**的回滚；这条说明是为「以后在别的环境跑过 up」留的。
--
-- # 执行顺序
--   DROP 新约束 → ADD 回三值约束 → 清列注释 → 反向自检。
--   与 V0200 之前的结构一致（V0137 建的正是这个三值约束名与形态）。
-- ============================================================

ALTER TABLE biz_material
    DROP CONSTRAINT IF EXISTS ck_biz_material_costing;

ALTER TABLE biz_material
    ADD CONSTRAINT ck_biz_material_costing
    CHECK (costing_method IN ('MOVING_AVG', 'FIFO', 'STANDARD'));

-- 注释回退：V0137 建列时没有 COMMENT ON COLUMN，这里如实清空，
-- 避免 down 之后库里留着「指向已不存在语义」的注释（V0201 才说「只有移动加权」）。
COMMENT ON COLUMN biz_material.costing_method IS NULL;

-- ---------- 反向自检（down 必须自己证明自己干净） ----------
DO $$
DECLARE
    con_def    text;
    keep_count bigint;
    probe_ok   boolean := false;
BEGIN
    SELECT pg_get_constraintdef(oid) INTO con_def
      FROM pg_constraint
     WHERE conrelid = 'biz_material'::regclass AND conname = 'ck_biz_material_costing';
    IF con_def IS NULL THEN
        RAISE EXCEPTION 'down 自检失败：ck_biz_material_costing 未重建';
    END IF;
    IF con_def NOT LIKE '%FIFO%' OR con_def NOT LIKE '%STANDARD%' OR con_def NOT LIKE '%MOVING_AVG%' THEN
        RAISE EXCEPTION 'down 自检失败：约束未回到三值域，实际定义 = %', con_def;
    END IF;

    -- 放开后的值域必须真的能写：探一行 FIFO 再删掉（biz_material 无外键，故用「插-删」验证，
    -- 若 CHECK 仍然拦 FIFO，会在子事务里抛 check_violation）。
    DELETE FROM biz_material WHERE id = '02010000-0000-4000-8000-00000000d0d0';
    BEGIN
        INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type,
                                  safety_stock, costing_method, tax_rate, status)
        VALUES ('02010000-0000-4000-8000-00000000d0d0', 'D0201-DOWN-PROBE', 'down 自检探针',
                '02010000-0000-4000-8000-000000000001', 'RAW', 0, 'FIFO', 0, 1);
        probe_ok := true;
    EXCEPTION
        WHEN check_violation THEN
            probe_ok := false;
    END;
    DELETE FROM biz_material WHERE id = '02010000-0000-4000-8000-00000000d0d0';

    IF NOT probe_ok THEN
        RAISE EXCEPTION 'down 自检失败：约束放宽后 FIFO 仍被 CHECK 拒绝';
    END IF;
    IF EXISTS (SELECT 1 FROM biz_material WHERE id = '02010000-0000-4000-8000-00000000d0d0') THEN
        RAISE EXCEPTION 'down 自检失败：探针行没删掉（幂等被破坏）';
    END IF;

    SELECT count(*) INTO keep_count
      FROM pg_constraint
     WHERE conrelid = 'biz_material'::regclass
       AND conname IN ('ck_biz_material_type', 'ck_biz_material_safety', 'ck_biz_material_tax');
    IF keep_count <> 3 THEN
        RAISE EXCEPTION 'down 自检失败：同表的 type/safety/tax 三条 CHECK 只剩 % / 3 条', keep_count;
    END IF;

    RAISE NOTICE '回滚完成（V0201）：ck_biz_material_costing 已回到三值域 MOVING_AVG / FIFO / STANDARD（FIFO 写入探针已通过并清理），列注释已清空；业务数据一行未动（被 up 回填过的行**不会**恢复原值，见文件头「可逆性说明」）';
END $$;
