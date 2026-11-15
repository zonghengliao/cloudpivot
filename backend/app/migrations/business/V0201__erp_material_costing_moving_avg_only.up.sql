-- ============================================================
-- V0201：成本法收敛 —— 只允许移动加权平均（MOVING_AVG）
--
-- 依据（照做，不自行发挥）：
--   `docs/architecture/ARCH-3-模块勾稽关系审计.md`
--     · §5 DC-10（P0）  成本法与用户选择不符：`costing_method` 三个取值，只有移动加权有实现
--     · §6 R-P0-5       值域收敛为 `["MOVING_AVG"]`；`ck_biz_material_costing` 同步收紧
--     · 附录 D.5        存量排查 SQL（`costing_method <> 'MOVING_AVG'` 的物料清单）
--
-- # 为什么收紧（而不是「补上 FIFO 实现」）
--   消费方实测 **0 处**：`stockin_write.rs` / `transfer_settle.rs` / `serial_model.rs` 全文
--   不读 `costing_method`，入库 / 调拨 / 序列号路径一律 upsert `avg_cost`（移动加权）。
--   也就是说用户选 FIFO / STANDARD 时，系统静默按移动加权算 —— 「成本口径与选择不符且
--   毫无提示」是比「没有 FIFO」更坏的缺陷（账算错了，用户还以为按 FIFO 算的）。
--   完整 FIFO 需要「批次成本层」（`biz_material_batch` + 出库按批次核销），属专项；
--   本迁移只保证「选了不会算错」：值域收敛到唯一实现。
--
-- # 三处收口（缺一处就还能绕过）
--   ① DTO：`dto::COSTING_METHODS` 收敛为 `["MOVING_AVG"]` + 专门文案（400 + 10001，
--      说清 FIFO / STANDARD 在路线图里，而不是让人以为漏配）；
--   ② DB：本文件的 `ck_biz_material_costing` 收紧为单值 —— 绕过接口直接写库同样被拒；
--   ③ 前端：主数据「成本法」下拉只剩一项 + 灰字说明（与 DTO 文案同口径）。
--
-- # 存量（附录 D.5 的排查结果）
--   上线前实测：prod `cloudpivot` / dev+Test `cloudpivot_dev` 三库 `biz_material` 全表
--   `costing_method` 只有 `MOVING_AVG`（prod 7 行 = 5 有效 + 2 软删），**命中 0 行**。
--   S2 的 UPDATE 因此是 no-op（幂等：可重复执行）；若在别的环境命中，S1 会先把
--   被改写的物料编码逐条播报到迁移日志（留痕，供人工核对 / 必要时手工改回）。
--
-- # 三条写法纪律（照抄 V0137 / V0198 / V0199 头注释）
--   ① 文件内**不写** `BEGIN; … COMMIT;` —— 迁移器自己在事务里跑；
--   ② 约束重建用 `DROP CONSTRAINT IF EXISTS` + `ADD CONSTRAINT`（existence-guard 见 S3），
--      值域写死在 `CASE … ELSE FALSE END` 风格里的更严写法：`costing_method = 'MOVING_AVG'`
--      单值比较天然兜住 NULL（`NULL = …` 判 NULL → PG 判定通过，故下面用 `IS TRUE` 兜底）；
--   ③ 迁移**不带 CONCURRENTLY**；`biz_material` 量级 <10 万，改 CHECK 只做全表校验扫描，
--      不加锁表重写（PG 的 ADD CONSTRAINT ... CHECK 会取 SHARE ROW EXCLUSIVE 锁，秒级）。
--
-- # 幂等
--   `DROP CONSTRAINT IF EXISTS` + `ADD CONSTRAINT`（无条件重建，结果一致）；
--   回填 UPDATE 自带 `IS DISTINCT FROM` 过滤。up 重复执行是 no-op。
-- ============================================================

-- ---------- S1 存量体检（只读；先播报，再动手） ----------
DO $$
DECLARE
    retired_total bigint;
    retired_alive bigint;
    detail        text;
BEGIN
    SELECT count(*) INTO retired_total
      FROM biz_material WHERE costing_method <> 'MOVING_AVG';
    SELECT count(*) INTO retired_alive
      FROM biz_material WHERE costing_method <> 'MOVING_AVG' AND deleted_at IS NULL;

    SELECT coalesce(string_agg(material_code || '=' || costing_method, ', ' ORDER BY material_code), '（无）')
      INTO detail
      FROM biz_material WHERE costing_method <> 'MOVING_AVG';

    RAISE NOTICE 'S1 存量体检：costing_method <> MOVING_AVG 的物料 % 行（其中有效 % 行）；明细：%',
                 retired_total, retired_alive, detail;
END $$;

-- ---------- S2 回填：非移动加权 → MOVING_AVG（幂等；实测 0 行 = no-op） ----------
-- 值域收敛后这些行**无法再被编辑**（编辑会 400），留着只会让主数据「看得见改不了」，
-- 且它们的历史成本本来就是按移动加权算的 —— 回填只是把口径对齐成事实。
UPDATE biz_material
   SET costing_method = 'MOVING_AVG',
       updated_at     = now()
 WHERE costing_method IS DISTINCT FROM 'MOVING_AVG';

-- ---------- S3 收紧值域：ck_biz_material_costing 只允许 MOVING_AVG ----------
-- V0137 建表时是 `IN ('MOVING_AVG','FIFO','STANDARD')`；这里 DROP + 重建（CHECK 无 ALTER 语法）。
ALTER TABLE biz_material
    DROP CONSTRAINT IF EXISTS ck_biz_material_costing;

ALTER TABLE biz_material
    ADD CONSTRAINT ck_biz_material_costing
    CHECK ((costing_method = 'MOVING_AVG') IS TRUE);

-- ---------- S4 注释：把「只有移动加权」写进库，别让下一个人又从 DDL 猜 ----------
COMMENT ON COLUMN biz_material.costing_method IS
    '计价方式（V0201 收敛）：本版本仅支持移动加权平均 MOVING_AVG（成本由 avg_cost 承载）。FIFO / STANDARD 需批次成本层，属路线图能力，值域由 ck_biz_material_costing 钉死为单值';

-- ---------- S5 迁移内自检：结构 + 钉死零违反 + 别的约束没被误删 ----------
DO $$
DECLARE
    con_def     text;
    violation   bigint;
    keep_count  bigint;
    alive_total bigint;
BEGIN
    SELECT pg_get_constraintdef(oid) INTO con_def
      FROM pg_constraint
     WHERE conrelid = 'biz_material'::regclass AND conname = 'ck_biz_material_costing';
    IF con_def IS NULL THEN
        RAISE EXCEPTION 'S3 自检失败：ck_biz_material_costing 不存在';
    END IF;
    IF con_def LIKE '%FIFO%' OR con_def LIKE '%STANDARD%' THEN
        RAISE EXCEPTION 'S3 自检失败：约束里仍有 FIFO / STANDARD，实际定义 = %', con_def;
    END IF;
    IF con_def NOT LIKE '%MOVING_AVG%' THEN
        RAISE EXCEPTION 'S3 自检失败：约束里没有 MOVING_AVG，实际定义 = %', con_def;
    END IF;

    SELECT count(*) INTO violation
      FROM biz_material WHERE (costing_method = 'MOVING_AVG') IS NOT TRUE;
    IF violation > 0 THEN
        RAISE EXCEPTION 'S5 自检失败：% 行违反 ck_biz_material_costing（回填没跑干净）', violation;
    END IF;

    -- 同表的其余 4 条业务 CHECK 必须还在（DROP CONSTRAINT 只该动 costing 这一条）
    SELECT count(*) INTO keep_count
      FROM pg_constraint
     WHERE conrelid = 'biz_material'::regclass
       AND conname IN ('ck_biz_material_type', 'ck_biz_material_safety', 'ck_biz_material_tax');
    IF keep_count <> 3 THEN
        RAISE EXCEPTION 'S5 自检失败：同表的 type/safety/tax 三条 CHECK 只剩 % / 3 条', keep_count;
    END IF;

    SELECT count(*) INTO alive_total FROM biz_material WHERE deleted_at IS NULL;
    RAISE NOTICE 'S5 自检通过：ck_biz_material_costing = 单值 MOVING_AVG；违反=0；同表其余 3 条 CHECK 完好；有效物料 % 行全部为移动加权',
                 alive_total;
END $$;
