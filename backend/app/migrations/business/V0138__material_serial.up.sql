-- ============================================================
-- V0138：一物一码（B）—— 单品序列号表 `biz_material_serial` + `biz_material` 两列
--        云枢 TSK-202610-000678（批次 D · W2a）· 2026-10-30
--
-- 本文件交付（卡面 §一 / §二 逐字落地）：
--   ① 新表 `biz_material_serial`：一件货一行，`serial_no` 全局唯一 + 状态机 + 来源单据追溯
--   ② `biz_material` 加两列：`serial_required BOOLEAN`（一物一码开关）/
--      `external_sn VARCHAR(128)`（厂商序列号，与内部 SN 并存）
--   ③ 4 个部分索引（唯一号 / 物料 / 仓库×库位 / 状态，全部带 `deleted_at IS NULL`）
--      + 状态枚举 CHECK（DB 层拦住非法 status）
--   ④ 空库自检（结构齐备 + 非法 status 真被拦 + 唯一号真能拦重复）
-- 回滚：`V0138__material_serial.down.sql`（先删表、再删列，附带零残留断言）
-- 依赖：V0137（`biz_material` / `biz_warehouse` / `biz_inventory_location` / `biz_seq` 已建）
--
-- ── 与 W0 设计（`docs/ERP-仓库管理-技术架构.md` §2.5 + 架构师草稿
--    `erp_wh/migrations/V0138__erp_material_serial.up.sql`）的 6 处差异 ────────
-- 卡面是契约源（后端注解 / `business/erp/dto.rs` / 前端都按卡面写），因此**按卡面落地**，
-- 差异逐条记账，免得 W2b（model/handler）照 W0 写代码时对不上：
--   ① `external_sn` 落点：卡面 = `biz_material`（物料级厂商序列号）；W0 = `biz_material_serial`（单品外部码）。
--      本卡按卡面放物料表；单品级的厂商码若要，只能在 W2b 里另立列/表（不在本卡）。
--   ② `serial_required` 类型：卡面 `BOOLEAN`；W0 `SMALLINT 0/1`。
--      仓内 `business/erp/dto.rs::MaterialSaveReq.serial_required` 已经是 `Option<bool>`、W1c 的
--      物料接口按布尔出入参 → 卡面口径正确，DB 落 `BOOLEAN NOT NULL DEFAULT FALSE`。
--   ③ 状态集：卡面 3 态 `IN_STOCK / OUTBOUND / SCRAPPED`；W0 4 态（多 `TRANSIT` 在途）。
--      在途是 V0139 调拨域的语义，本卡不预埋；将来加状态 = 重建这条 CHECK（见 down 注释）。
--   ④ 软删：卡面有 `deleted_at`（审计五件套一致）+ 唯一索引带 `WHERE deleted_at IS NULL`
--      → **软删后同号可重用**；W0 声明「单品身份永久、无软删」。按卡面（卡面验收⑥ 就要求重用）。
--   ⑤ `location_id`：卡面 `REFERENCES biz_inventory_location(id)`（硬外键）；W0 弱引用 `biz_location.id`。
--      硬外键的代价：库位库存行被序列号引用时删不掉（要先把货移走/软删序列号）—— 这是卡面取舍，
--      按卡面建；`biz_material` / `biz_warehouse` 两条 FK 同理（V0137 的惯例是弱引用，本表是例外）。
--   ⑥ `biz_seq(seq_type='SN')` 取号规则（W0 §3.1 Q5）由本迁移一并种入（§3）：
--      W2b 生成 `SN-yyyyMMdd-6位` 必须有这条规则；幂等（按业务键判重），不越过「本卡只建表」的边界。
--
-- ── 状态机（W2b 应用层负责，DB 只管枚举）───────────────────────────────────
--   `IN_STOCK → OUTBOUND → SCRAPPED` 单向不可逆：`OUTBOUND` 不能回 `IN_STOCK`（要回库 = 新入库新序列号）。
--   DB 层只拦「不在枚举里的值」，不拦「合法值之间的非法跳转」—— 因为『不可逆』需要读到旧行才知道
--   是从哪来的，属写路径的业务规则；本卡不写触发器（红线：不加业务代码），由 W2b 的 model 层把关。
--
-- ── 三条写法纪律（V0136 / V0137 同款，别改）────────────────────────────────
-- 1. 文件内**不写** `BEGIN; … COMMIT;` —— 迁移器自己在事务里执行本文件。
-- 2. 全部 DDL 幂等：`CREATE TABLE / INDEX IF NOT EXISTS`、`ADD COLUMN IF NOT EXISTS`，
--    约束走 DO 块按 `pg_constraint` 判重（`ADD CONSTRAINT` 没有 IF NOT EXISTS）。
-- 3. CHECK 一律写成「显式布尔 / 枚举等值」而不是 `A OR (B AND C)`：PG 的三值逻辑里
--    CHECK 结果为 NULL 视为通过，弱约束会静默放行。
-- ============================================================


-- ============================================================
-- §1 `biz_material` 加两列（卡面 §二）
-- ============================================================
ALTER TABLE biz_material ADD COLUMN IF NOT EXISTS serial_required BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE biz_material ADD COLUMN IF NOT EXISTS external_sn     VARCHAR(128);

COMMENT ON COLUMN biz_material.serial_required IS
    '是否强制一物一码：TRUE 入库/出库必须逐件登记序列号（biz_material_serial，一物一码）/ FALSE 按数量管理';
COMMENT ON COLUMN biz_material.external_sn IS
    '厂商序列号（厂商 SN / IMEI / 防伪码），与内部 SN（biz_material_serial.serial_no）并存；可空';

-- 一物一码物料是少数，但「哪些物料要扫码」是主数据页的高频筛选条件 → 部分索引只收 TRUE 行
CREATE INDEX IF NOT EXISTS idx_biz_material_serial_req ON biz_material (serial_required)
    WHERE serial_required = TRUE AND deleted_at IS NULL;

-- V0137 §5.9 的本意是「V0138 落地后把 M-W1A-004 标成一物一码」，但它在列不存在时跳过、
-- 而列的**类型由本卡决定**（BOOLEAN）；它那句 `serial_required <> 1` 在 BOOLEAN 列上会报
-- `operator does not exist: boolean <> integer`（PG 18.6 实测）——只在「重放 V0137 up」时才会走到，
-- 正常路径（V0137 先于 V0138）永远是跳过分支。所以这里按它的意图补一次精确标记（幂等）。
UPDATE biz_material SET serial_required = TRUE
 WHERE material_code = 'M-W1A-004' AND serial_required IS DISTINCT FROM TRUE;


-- ============================================================
-- §2 单品序列号 `biz_material_serial`（卡面 §一）
-- ============================================================
CREATE TABLE IF NOT EXISTS biz_material_serial (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),                    -- 主键
    serial_no     VARCHAR(64)  NOT NULL,                                         -- 系统内码 SN-yyyyMMdd-000001，全局唯一（软删后可重用）
    material_id   UUID         NOT NULL REFERENCES biz_material(id),             -- 物料（卡面：硬外键）
    warehouse_id  UUID         REFERENCES biz_warehouse(id),                     -- 当前/最后所在仓库（出库后保留最后一次位置）
    location_id   UUID         REFERENCES biz_inventory_location(id),            -- 当前所在库位（指向库位库存明细行，可空）
    batch_no      VARCHAR(64),                                                   -- 批次号（可选）
    status        VARCHAR(16)  NOT NULL,                                         -- IN_STOCK 在库 / OUTBOUND 已出库 / SCRAPPED 已报废
    inbound_at    TIMESTAMPTZ,                                                   -- 首次入库时间
    outbound_at   TIMESTAMPTZ,                                                   -- 出库/报废时间
    source_type   VARCHAR(32),                                                   -- 来源单据类型：PURCHASE / TRANSFER / PRODUCE / RETURN
    source_id     UUID,                                                          -- 来源单据 id（弱引用，跨批次不建外键）

    -- 审计五件套（与 biz_material / biz_warehouse / biz_task 等 biz_ 表一致）
    created_at    TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,               -- 创建时间（= 登记时间）
    updated_at    TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,               -- 更新时间
    create_by     UUID,                                                          -- 创建人（弱引用 sys_user.id）
    update_by     UUID,                                                          -- 更新人（弱引用 sys_user.id）
    deleted_at    TIMESTAMPTZ,                                                   -- 软删时间（NULL = 未删；软删后同号可重用）

    -- 状态枚举：非法 status 必须在 DB 层就被拒（卡面验收 ④）
    CONSTRAINT ck_biz_material_serial_status CHECK (status IN ('IN_STOCK','OUTBOUND','SCRAPPED'))
);

COMMENT ON TABLE  biz_material_serial IS
    '单品序列号（一物一码 B）：一件货一行；序列号全局唯一，状态机 IN_STOCK→OUTBOUND→SCRAPPED 由应用层保证';
COMMENT ON COLUMN biz_material_serial.serial_no IS
    '系统内码 SN-yyyyMMdd-6位流水（服务端由 biz_seq(seq_type=SN) 原子取号）；全局唯一，软删后可重用';
COMMENT ON COLUMN biz_material_serial.status IS
    'IN_STOCK 在库 / OUTBOUND 已出库 / SCRAPPED 已报废（单向不可逆，OUTBOUND 不回 IN_STOCK）';
COMMENT ON COLUMN biz_material_serial.warehouse_id IS
    '当前/最后所在仓库；出库/报废后保留最后一次位置便于溯源 → 查「在库」必须带 status=''IN_STOCK''';
COMMENT ON COLUMN biz_material_serial.source_type IS
    '来源单据类型：PURCHASE 采购入库 / TRANSFER 调拨 / PRODUCE 生产入库 / RETURN 退货入库';
COMMENT ON COLUMN biz_material_serial.deleted_at IS
    '软删时间；唯一索引带 WHERE deleted_at IS NULL，所以软删后同一个号可以重新登记';

-- 索引（卡面 §一 逐条；全部部分索引，软删行不进索引）
-- ① 序列号全局唯一：扫码枪扫出来就是一条记录
CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_material_serial_no
    ON biz_material_serial (serial_no) WHERE deleted_at IS NULL;
-- ② 「这台机器的所有序列号」/ 物料维度的在库件数
CREATE INDEX IF NOT EXISTS idx_biz_material_serial_material
    ON biz_material_serial (material_id) WHERE deleted_at IS NULL;
-- ③ 某仓某库位的单品清单（拣货 / 盘点主查询）
CREATE INDEX IF NOT EXISTS idx_biz_material_serial_wh_loc
    ON biz_material_serial (warehouse_id, location_id) WHERE deleted_at IS NULL;
-- ④ 状态维度（在库 / 已出库 / 已报废 列表与巡检）
CREATE INDEX IF NOT EXISTS idx_biz_material_serial_status
    ON biz_material_serial (status) WHERE deleted_at IS NULL;

-- 约束幂等兜底：如果这张表是别的迁移/人工先建的（`CREATE TABLE IF NOT EXISTS` 会跳过整块），
-- 状态 CHECK 可能缺 → 这里补上（同名已在则跳过）。
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1
                     FROM pg_constraint
                    WHERE conname = 'ck_biz_material_serial_status'
                      AND conrelid = 'public.biz_material_serial'::regclass) THEN
        ALTER TABLE biz_material_serial ADD CONSTRAINT ck_biz_material_serial_status
            CHECK (status IN ('IN_STOCK','OUTBOUND','SCRAPPED'));
        RAISE NOTICE 'V0138：补建 ck_biz_material_serial_status（表已存在但约束缺失）';
    END IF;
END
$$;


-- ============================================================
-- §3 SN 取号规则（W0 §3.1 Q5 / §0.2 偏差 5：6 位流水，按天滚动）
--    流水按天滚动（period_key=yyyyMMdd），6 位 = 单日 999,999 件上限
--    幂等：按业务键 seq_type 判重（唯一索引带 WHERE deleted_at IS NULL，
--    普通 ON CONFLICT (seq_type) 推不出这个部分索引，PG 会直接报错 → 用 NOT EXISTS）
-- ============================================================
INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, current_no, step, separator, status, remark)
SELECT 'd1380000-0000-4000-8000-000000000001'::uuid, 'SN', '单品序列号', 'SN', 'yyyyMMdd', '', 0, 1, '-', 1,
       '一物一码单品序列号：格式 SN-yyyyMMdd-000001，按天滚动；批量入库用 current_no + step*N 一次取 N 个连号'
 WHERE NOT EXISTS (SELECT 1 FROM biz_seq WHERE seq_type = 'SN' AND deleted_at IS NULL);


-- ============================================================
-- §4 自检（空库必须也能过：只断言结构与「约束真的会拦」，不依赖库里已有数据）
--    自检用的临时物料/序列号在本块末尾清场，跑完库里零残留。
-- ============================================================
DO $$
DECLARE
    n_col    INT;
    n_idx    INT;
    n_chk    INT;
    n_seq    INT;
    blocked  BOOLEAN;
    v_mat    UUID := '00000000-0000-4000-8000-0000000d1380';
    v_id     UUID;
    v_cnt    INT;
BEGIN
    -- ① 表在 + 16 个列齐（id/serial_no/material_id/warehouse_id/location_id/batch_no/status/
    --    inbound_at/outbound_at/source_type/source_id + 审计五件套）
    SELECT count(*) INTO n_col
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_material_serial'
       AND column_name IN ('id','serial_no','material_id','warehouse_id','location_id','batch_no','status',
                           'inbound_at','outbound_at','source_type','source_id',
                           'created_at','updated_at','create_by','update_by','deleted_at');
    IF n_col <> 16 THEN
        RAISE EXCEPTION 'V0138 自检失败：biz_material_serial 列不全（% / 16）', n_col;
    END IF;

    -- ② biz_material 两列 + 类型/可空性正确（serial_required 非空、external_sn 可空且 128 长）
    SELECT count(*) INTO n_col
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'biz_material'
       AND (   (column_name = 'serial_required' AND data_type = 'boolean' AND is_nullable = 'NO')
            OR (column_name = 'external_sn'     AND data_type = 'character varying' AND is_nullable = 'YES'
                AND character_maximum_length = 128));
    IF n_col <> 2 THEN
        RAISE EXCEPTION 'V0138 自检失败：biz_material.serial_required / external_sn 类型或可空性不符（% / 2）', n_col;
    END IF;

    -- ③ 4 个索引在位（卡面点名）
    SELECT count(*) INTO n_idx
      FROM pg_indexes
     WHERE schemaname = 'public'
       AND indexname IN ('uk_biz_material_serial_no','idx_biz_material_serial_material',
                         'idx_biz_material_serial_wh_loc','idx_biz_material_serial_status');
    IF n_idx <> 4 THEN
        RAISE EXCEPTION 'V0138 自检失败：索引缺失（% / 4）', n_idx;
    END IF;

    -- ④ 状态 CHECK 在位，且真的是枚举（读 pg_get_constraintdef，不只看约束名）
    SELECT count(*) INTO n_chk
      FROM pg_constraint
     WHERE conname = 'ck_biz_material_serial_status'
       AND conrelid = 'public.biz_material_serial'::regclass
       AND contype = 'c'
       AND pg_get_constraintdef(oid) LIKE '%IN_STOCK%OUTBOUND%SCRAPPED%';
    IF n_chk <> 1 THEN
        RAISE EXCEPTION 'V0138 自检失败：ck_biz_material_serial_status 缺失或口径不对（% / 1）', n_chk;
    END IF;

    -- ⑤ SN 取号规则在位（恰好 1 行启用）
    SELECT count(*) INTO n_seq FROM biz_seq WHERE seq_type = 'SN' AND deleted_at IS NULL;
    IF n_seq <> 1 THEN
        RAISE EXCEPTION 'V0138 自检失败：SN 取号规则行数异常（% / 1）', n_seq;
    END IF;

    -- ⑥ 反例 1：非法 status → 必须被 CHECK 拦住（卡面验收 ④ 在迁移期先验一遍）
    --    硬外键要求 material_id 真实存在 → 先建一条临时物料（unit_id 是弱引用，不需要真单位）
    INSERT INTO biz_material (id, material_code, material_name, unit_id)
    VALUES (v_mat, '__W2A_ST_MAT__', '自检-序列号物料', gen_random_uuid())
        ON CONFLICT (id) DO NOTHING;

    blocked := FALSE;
    BEGIN
        INSERT INTO biz_material_serial (serial_no, material_id, status)
        VALUES ('SN-ST-SELFTEST-INVALID', v_mat, 'INVALID');
    EXCEPTION WHEN check_violation THEN
        blocked := TRUE;
    END;
    IF NOT blocked THEN
        RAISE EXCEPTION 'V0138 自检失败：非法 status 被放行（ck_biz_material_serial_status 失效）';
    END IF;

    -- ⑦ 正例：在库单品能入库
    INSERT INTO biz_material_serial (serial_no, material_id, status, inbound_at, source_type)
    VALUES ('SN-ST-SELFTEST-000001', v_mat, 'IN_STOCK', CURRENT_TIMESTAMP, 'PURCHASE')
    RETURNING id INTO v_id;

    -- ⑧ 反例 2：同一 serial_no 插第二条 → 必须被唯一索引拦住（卡面验收 ⑤）
    blocked := FALSE;
    BEGIN
        INSERT INTO biz_material_serial (serial_no, material_id, status)
        VALUES ('SN-ST-SELFTEST-000001', v_mat, 'IN_STOCK');
    EXCEPTION WHEN unique_violation THEN
        blocked := TRUE;
    END;
    IF NOT blocked THEN
        RAISE EXCEPTION 'V0138 自检失败：重复 serial_no 被放行（uk_biz_material_serial_no 失效）';
    END IF;

    -- ⑨ 正例：软删后同号可再插（部分唯一索引的口径；卡面验收 ⑥）
    UPDATE biz_material_serial SET deleted_at = CURRENT_TIMESTAMP WHERE id = v_id;
    INSERT INTO biz_material_serial (serial_no, material_id, status, inbound_at)
    VALUES ('SN-ST-SELFTEST-000001', v_mat, 'IN_STOCK', CURRENT_TIMESTAMP);

    SELECT count(*) INTO v_cnt FROM biz_material_serial WHERE material_id = v_mat;
    IF v_cnt <> 2 THEN
        RAISE EXCEPTION 'V0138 自检失败：软删后同号重插的行数异常（% / 2）', v_cnt;
    END IF;

    -- 清场（自检不留数据）
    DELETE FROM biz_material_serial WHERE material_id = v_mat;
    DELETE FROM biz_material        WHERE id = v_mat;

    RAISE NOTICE 'V0138 自检通过：表/16 列/4 索引/状态 CHECK/SN 取号规则齐备；非法 status 被拦、重复号被拦、软删后同号可重用';
END
$$;
