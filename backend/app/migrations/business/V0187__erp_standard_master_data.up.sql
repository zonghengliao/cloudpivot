-- ======================================================
-- V0187：标准基础数据预置（ERP-S1）—— 计量单位 25 个 + 物料分类 7 值 + 结算方式字典
--
-- 依据：kanban 卡 t_eee6774a（老板提问：「有些标准数据是不是可以提前录入？比如科目，计量单位」）
--   科目由 ERP-R1（V0183）补齐 56 个；本卡补计量单位、物料分类、结算方式。
--
-- ── 动手前的实测现状（卡面是草图，实际表结构以本节为准）────────────────────
--   ① `biz_unit` 只有 3 行（PCS 个 / KG 千克 / BOX 箱，V0137 种子），
--      且**没有** `precision` 列、**没有** `is_system` 列 —— 卡面假设有 precision，
--      实际没有 ⇒ 第 ① 步先补两列，再种数据（硬红线 4：先查表结构再写迁移）。
--   ② `biz_unit` 的唯一索引是**部分唯一索引**
--        `uk_biz_unit_code ON biz_unit (unit_code) WHERE deleted_at IS NULL`
--      ⇒ `ON CONFLICT` 必须带上索引谓词：
--        `ON CONFLICT (unit_code) WHERE deleted_at IS NULL DO NOTHING`。
--        写成裸的 `ON CONFLICT (unit_code)` 会报
--        `there is no unique or exclusion constraint matching the ON CONFLICT specification`
--        （部分索引不算「与 (unit_code) 匹配的唯一约束」），整条迁移失败。
--   ③ `biz_material` **没有** `category` 列，全库也**没有** `biz_material_category` 表
--      （`select * from pg_tables where tablename like '%categ%'` 零行）。
--      物料分类的真身是 `material_type`：`dto::MATERIAL_TYPES` + V0137 的
--      `ck_biz_material_type CHECK (material_type IN ('RAW','SEMI','FINISHED','SERVICE'))`。
--      ⇒ 卡面要的 7 类正好是「4 值 + 包装物 / 低值易耗品 / 办公用品」，
--        本迁移把枚举扩到 7 值（`dto::MATERIAL_TYPES` 与前端选项同步改）。
--   ④ `biz_payment.pay_method` 是**自由文本**（VARCHAR，无枚举约束、无字典类型），
--      前端 `payable/index.vue` 硬编码 4 个选项（银行转账/支票/现金/承兑汇票）。
--      ⇒ 本迁移种字典类型 `pay_method`（7 项，item_value 就用中文原串，
--        与既有自由文本数据同形，前端下拉切到字典时不用做值映射）。
--   ⑤ `biz_warehouse.warehouse_type` 枚举已是 5 值（NORMAL/RAW/FINISHED/SCRAP/IN_TRANSIT），
--      原料仓 / 成品仓 / 不良品仓 / 在途仓**已是标准值**，且生产已有
--      WH01 总仓 / WH02 分仓 / WH-TRANSIT 在途仓（在途仓正被调拨单据引用）。
--      ⇒ 卡面 §二③ 的「仓库类型」不重复造仓库、不造未落库的枚举值（造了就是死数据）。
--
-- ── 幂等与「不覆盖客户修改」（硬红线 2 / 3）────────────────────────────────
--   · 单位：`ON CONFLICT (unit_code) WHERE deleted_at IS NULL DO NOTHING`
--           ⇒ 客户改过的名字 / 折算率 / 状态一律保留（撞码即跳过）。
--   · 历史 3 行的 `is_system` 只在「编码 + 名称仍与 V0137 种子逐字一致」时回填：
--     客户把「千克」改成「公斤」后，本迁移**不会**把它标成系统预置（仍视为自建）。
--   · 物料分类只扩 CHECK 枚举（加值不改值），不动任何已有行。
--   · 字典：类型按 `dict_code` 幂等，项按 `(dict_type_id, item_value)` 幂等。
--
-- ── 单位精度（`precision`）口径 ───────────────────────────────────────────
--   重量 / 长度 / 面积体积 = 3 位小数（克、毫米级业务需要）；数量 / 包装 / 时间 = 0 位。
--   卡面只写了「重量/长度 3 位、个数 0 位」，面积体积与时间按同一逻辑归类并在自检块断言。
--
-- ── 新单位的 `is_base` / `rate` ──────────────────────────────────────────
--   全部 `is_base = 1`（基础单位，`rate = 1`，`base_unit_id = NULL`）——仓库里没有
--   权威的折算关系（箱=12个 是 V0137 已有的 BOX→PCS），不臆造 1 G = 0.001 KG 这类
--   客户各不相同的折算率；真要折算由客户在页面上自己建辅助单位。
-- ======================================================

-- ======================================================
-- ① `biz_unit` 补两列：`is_system`（系统预置标记）/ `precision`（小数位数）
-- ======================================================
ALTER TABLE biz_unit ADD COLUMN IF NOT EXISTS is_system BOOLEAN  NOT NULL DEFAULT false;
ALTER TABLE biz_unit ADD COLUMN IF NOT EXISTS precision SMALLINT NOT NULL DEFAULT 0;

COMMENT ON COLUMN biz_unit.is_system IS
    '是否系统预置（ERP-S1）：true = 预置标准值，不允许删除、只能停用；客户自建 = false';
COMMENT ON COLUMN biz_unit.precision IS
    '数量小数位数（0–6）：个数 0 位，重量 / 长度 / 面积体积 3 位；仅供录入与展示时定标';

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_unit_precision'
    ) THEN
        ALTER TABLE biz_unit
            ADD CONSTRAINT ck_biz_unit_precision CHECK (precision BETWEEN 0 AND 6);
    END IF;
END $$;

-- 预置标记参与「单位页筛选 / 删除拦截」，建一个部分索引（只索引系统预置行，体积可忽略）
CREATE INDEX IF NOT EXISTS idx_biz_unit_is_system
    ON biz_unit (is_system) WHERE is_system AND deleted_at IS NULL;

-- ======================================================
-- ② 标准计量单位（28 个编码：3 个已存在 → 实际新增 25 个）
--
--   排序值分段：数量 10–60 / 重量 110–140 / 长度 210–250 / 面积体积 310–350
--               / 包装 410–450 / 时间 510–530（客户自建单位排序值随便填，不会撞的唯一键只有编码）
-- ======================================================
INSERT INTO biz_unit (unit_code, unit_name, is_base, rate, status, sort_order, is_system, precision)
SELECT v.unit_code, v.unit_name, 1, 1, 1, v.sort_order, true, v.precision
  FROM (VALUES
        -- 数量类（0 位小数）
        ('PCS',    '个',   10,  0),
        ('SET',    '套',   20,  0),
        ('BOX',    '箱',   30,  0),
        ('PACK',   '包',   40,  0),
        ('PAIR',   '双',   50,  0),
        ('DOZEN',  '打',   60,  0),
        -- 重量类（3 位小数）
        ('KG',     '千克', 110, 3),
        ('G',      '克',   120, 3),
        ('T',      '吨',   130, 3),
        ('LB',     '磅',   140, 3),
        -- 长度类（3 位小数）
        ('M',      '米',   210, 3),
        ('CM',     '厘米', 220, 3),
        ('MM',     '毫米', 230, 3),
        ('KM',     '千米', 240, 3),
        ('FT',     '英尺', 250, 3),
        -- 面积 / 体积（3 位小数）
        ('M2',     '平方米', 310, 3),
        ('M3',     '立方米', 320, 3),
        ('L',      '升',     330, 3),
        ('ML',     '毫升',   340, 3),
        ('GAL',    '加仑',   350, 3),
        -- 包装类（0 位小数）
        ('ROLL',   '卷',   410, 0),
        ('BAG',    '袋',   420, 0),
        ('BOTTLE', '瓶',   430, 0),
        ('CAN',    '罐',   440, 0),
        ('TUBE',   '支',   450, 0),
        -- 时间类（0 位小数）
        ('HOUR',   '小时', 510, 0),
        ('DAY',    '天',   520, 0),
        ('MONTH',  '月',   530, 0)
       ) AS v(unit_code, unit_name, sort_order, precision)
ON CONFLICT (unit_code) WHERE deleted_at IS NULL DO NOTHING;

-- ======================================================
-- ③ 历史 3 行（V0137 种子）补 `is_system` / `precision`
--
--   **只在「编码 + 名称仍与种子逐字一致」时回填**：客户若已改名（千克 → 公斤），
--   本语句的条件不成立，该行继续是 `is_system = false`（视为客户自建，可删），
--   这是「不覆盖已有数据」（硬红线 3）的直接实现。
-- ======================================================
UPDATE biz_unit
   SET is_system = true, precision = 3, updated_at = now()
 WHERE deleted_at IS NULL
   AND is_system = false
   AND (unit_code, unit_name) IN (('KG', '千克'));

UPDATE biz_unit
   SET is_system = true, updated_at = now()
 WHERE deleted_at IS NULL
   AND is_system = false
   AND (unit_code, unit_name) IN (('PCS', '个'), ('BOX', '箱'));

-- ======================================================
-- ④ 物料分类：`ck_biz_material_type` 4 值 → 7 值（加值不改值，已有行零影响）
--
--   RAW 原材料 / SEMI 半成品 / FINISHED 产成品 / PACKAGING 包装物
--   / CONSUMABLE 低值易耗品 / OFFICE 办公用品 / SERVICE 服务
--   后端 `dto::MATERIAL_TYPES` 与前端 `MaterialType` / `MATERIAL_TYPE_OPTIONS` 必须同步
--   （不同步的表现：接口 400「material_type 不在允许范围内」，或前端选项里选不到新值）。
-- ======================================================
ALTER TABLE biz_material DROP CONSTRAINT IF EXISTS ck_biz_material_type;
ALTER TABLE biz_material
    ADD CONSTRAINT ck_biz_material_type
    CHECK (material_type IN ('RAW', 'SEMI', 'FINISHED', 'PACKAGING', 'CONSUMABLE', 'OFFICE', 'SERVICE'));

-- ======================================================
-- ⑤ 结算方式字典（`pay_method` 7 项）
--
--   `item_value` 直接用中文原串：`biz_payment.pay_method` 是自由文本列，历史与前端
--   存的都是「银行转账」这类中文；字典若用 CASH / BANK 之类代码，前端还得做值映射，
--   老数据也会对不上。status=1、现金为默认项。
-- ======================================================
INSERT INTO sys_dict_type (dict_code, dict_name, description, status, sort_order)
VALUES ('pay_method', '结算方式', 'ERP-S1 预置：现金 / 银行转账 / 承兑汇票 / 支票 / 微信 / 支付宝 / 赊账', 1, 100)
ON CONFLICT (dict_code) WHERE deleted_at IS NULL DO NOTHING;

INSERT INTO sys_dict_item (dict_type_id, item_value, item_label, sort_order, is_default, status)
SELECT t.id, v.item_value, v.item_label, v.sort_order, v.is_default, 1
  FROM sys_dict_type t
  CROSS JOIN (VALUES
        ('现金',     '现金',     10, true),
        ('银行转账', '银行转账', 20, false),
        ('承兑汇票', '承兑汇票', 30, false),
        ('支票',     '支票',     40, false),
        ('微信',     '微信',     50, false),
        ('支付宝',   '支付宝',   60, false),
        ('赊账',     '赊账',     70, false)
       ) AS v(item_value, item_label, sort_order, is_default)
 WHERE t.dict_code = 'pay_method'
   AND t.deleted_at IS NULL
ON CONFLICT (dict_type_id, item_value) WHERE deleted_at IS NULL DO NOTHING;

-- ======================================================
-- ⑥ 迁移自检（幂等：只断言「结果形态」，不假设库里原有几行）
-- ======================================================
DO $$
DECLARE
    n_units      INT;
    n_dup        INT;
    n_sys        INT;
    n_pcs_prec   INT;
    n_kg_prec    INT;
    n_types      INT;
    n_items      INT;
    n_bad_prec   INT;
    expect_codes TEXT[] := ARRAY[
        'PCS','SET','BOX','PACK','PAIR','DOZEN',
        'KG','G','T','LB',
        'M','CM','MM','KM','FT',
        'M2','M3','L','ML','GAL',
        'ROLL','BAG','BOTTLE','CAN','TUBE',
        'HOUR','DAY','MONTH'];
    n_expect     INT;
BEGIN
    -- ① 单位总数（卡面验收口径：≥ 24）
    SELECT count(*) INTO n_units FROM biz_unit WHERE deleted_at IS NULL;
    IF n_units < 24 THEN
        RAISE EXCEPTION 'V0187 自检失败：标准单位只有 % 个（期望 ≥ 24）', n_units;
    END IF;

    -- ② 无重复编码（卡面验证 SQL）
    SELECT count(*) INTO n_dup
      FROM (SELECT unit_code FROM biz_unit WHERE deleted_at IS NULL
             GROUP BY unit_code HAVING count(*) > 1) d;
    IF n_dup > 0 THEN
        RAISE EXCEPTION 'V0187 自检失败：有 % 个单位编码重复（唯一索引被绕过）', n_dup;
    END IF;

    -- ③ 28 个标准编码逐个在位（客户删掉其中一个 → 迁移现场就该炸，而不是悄悄少一个）
    SELECT count(*) INTO n_expect
      FROM biz_unit WHERE deleted_at IS NULL AND unit_code = ANY(expect_codes);
    IF n_expect <> array_length(expect_codes, 1) THEN
        RAISE EXCEPTION 'V0187 自检失败：标准单位只到位 % / % 个',
            n_expect, array_length(expect_codes, 1);
    END IF;

    -- ④ 除「客户改过名的旧种子行」外，标准编码都必须是 is_system = true
    SELECT count(*) INTO n_sys
      FROM biz_unit WHERE deleted_at IS NULL AND unit_code = ANY(expect_codes) AND is_system;
    IF n_sys < array_length(expect_codes, 1) - 1 THEN
        RAISE EXCEPTION 'V0187 自检失败：标准单位里只有 % 个标了 is_system（最多允许 1 个因客户改名而未标）', n_sys;
    END IF;

    -- ⑤ 精度口径：数量类 0 位、重量 / 长度 / 面积体积 3 位
    SELECT count(*) INTO n_pcs_prec
      FROM biz_unit WHERE deleted_at IS NULL AND unit_code IN ('PCS','SET','BOX','PACK','PAIR','DOZEN',
            'ROLL','BAG','BOTTLE','CAN','TUBE','HOUR','DAY','MONTH') AND precision <> 0;
    IF n_pcs_prec > 0 THEN
        RAISE EXCEPTION 'V0187 自检失败：% 个「个数 / 包装 / 时间」类单位的 precision 不是 0', n_pcs_prec;
    END IF;

    SELECT count(*) INTO n_kg_prec
      FROM biz_unit WHERE deleted_at IS NULL
       AND unit_code IN ('KG','G','T','LB','M','CM','MM','KM','FT','M2','M3','L','ML','GAL')
       AND precision <> 3;
    IF n_kg_prec > 0 THEN
        RAISE EXCEPTION 'V0187 自检失败：% 个「重量 / 长度 / 面积体积」类单位的 precision 不是 3', n_kg_prec;
    END IF;

    SELECT count(*) INTO n_bad_prec
      FROM biz_unit WHERE deleted_at IS NULL AND (precision < 0 OR precision > 6);
    IF n_bad_prec > 0 THEN
        RAISE EXCEPTION 'V0187 自检失败：% 个单位的 precision 越界（ck_biz_unit_precision 失效）', n_bad_prec;
    END IF;

    -- ⑥ 物料分类 7 值（CHECK 里 7 个枚举都在；已有行的值仍然合法）
    SELECT count(*) INTO n_types
      FROM pg_constraint
     WHERE conname = 'ck_biz_material_type'
       AND pg_get_constraintdef(oid) LIKE '%PACKAGING%'
       AND pg_get_constraintdef(oid) LIKE '%CONSUMABLE%'
       AND pg_get_constraintdef(oid) LIKE '%OFFICE%';
    IF n_types <> 1 THEN
        RAISE EXCEPTION 'V0187 自检失败：ck_biz_material_type 没扩到 7 值';
    END IF;

    -- ⑦ 结算方式字典：类型 1 个 + 项 7 个
    SELECT count(*) INTO n_types
      FROM sys_dict_type WHERE dict_code = 'pay_method' AND deleted_at IS NULL;
    IF n_types <> 1 THEN
        RAISE EXCEPTION 'V0187 自检失败：字典类型 pay_method 没种上（实际 % 行）', n_types;
    END IF;

    SELECT count(*) INTO n_items
      FROM sys_dict_item i
      JOIN sys_dict_type t ON t.id = i.dict_type_id
     WHERE t.dict_code = 'pay_method' AND t.deleted_at IS NULL AND i.deleted_at IS NULL;
    IF n_items <> 7 THEN
        RAISE EXCEPTION 'V0187 自检失败：结算方式字典项应为 7 个（实际 %）', n_items;
    END IF;

    RAISE NOTICE 'V0187 自检通过：单位 % 个（其中标准 % 个、系统预置 % 个）、物料分类 7 值、结算方式 7 项',
        n_units, array_length(expect_codes, 1), n_sys;
END $$;
