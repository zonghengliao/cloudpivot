-- ======================================================
-- V0187 回滚：标准基础数据预置（ERP-S1）
--
-- 只删本迁移种下的东西：
--   ① 结算方式字典（7 项 + 类型 pay_method）
--   ② 物料分类 CHECK 收回 4 值（**若已有物料用了新值 → 中止**，不静默丢分类）
--   ③ 25 个新增计量单位（**被物料引用 → 中止**，让调用方先换单位）
--   ④ `biz_unit.is_system` / `biz_unit.precision` 两列 + 索引 + 约束
--
-- 保留：PCS / KG / BOX 三行（V0137 种子，本来就在库里）；
--       客户自建单位 / 客户改名单位一行不动。
-- ======================================================

-- ======================================================
-- ① 结算方式字典
-- ======================================================
DELETE FROM sys_dict_item i
 USING sys_dict_type t
 WHERE i.dict_type_id = t.id
   AND t.dict_code = 'pay_method';

DELETE FROM sys_dict_type WHERE dict_code = 'pay_method';

-- ======================================================
-- ② 物料分类 CHECK 收回 4 值
--    新值上已有数据 → 中止（回滚不该把「包装物」类物料悄悄变成非法行）
-- ======================================================
DO $$
DECLARE
    n_new INT;
BEGIN
    SELECT count(*) INTO n_new
      FROM biz_material
     WHERE deleted_at IS NULL
       AND material_type IN ('PACKAGING', 'CONSUMABLE', 'OFFICE');
    IF n_new > 0 THEN
        RAISE EXCEPTION 'V0187 回滚中止：有 % 个物料用了新增分类（PACKAGING/CONSUMABLE/OFFICE），先把它们改回 RAW/SEMI/FINISHED/SERVICE', n_new;
    END IF;
END $$;

ALTER TABLE biz_material DROP CONSTRAINT IF EXISTS ck_biz_material_type;
ALTER TABLE biz_material
    ADD CONSTRAINT ck_biz_material_type
    CHECK (material_type IN ('RAW', 'SEMI', 'FINISHED', 'SERVICE'));

-- ======================================================
-- ③ 25 个新增计量单位
-- ======================================================
DO $$
DECLARE
    n_ref INT;
BEGIN
    SELECT count(*) INTO n_ref
      FROM biz_unit u
     WHERE u.deleted_at IS NULL
       AND u.unit_code IN ('SET','PACK','PAIR','DOZEN','G','T','LB','M','CM','MM','KM','FT',
                           'M2','M3','L','ML','GAL','ROLL','BAG','BOTTLE','CAN','TUBE',
                           'HOUR','DAY','MONTH')
       AND EXISTS (SELECT 1 FROM biz_material m WHERE m.unit_id = u.id AND m.deleted_at IS NULL);
    IF n_ref > 0 THEN
        RAISE EXCEPTION 'V0187 回滚中止：有 % 个预置单位已被物料引用，先换单位再回滚', n_ref;
    END IF;
END $$;

DELETE FROM biz_unit
 WHERE unit_code IN ('SET','PACK','PAIR','DOZEN','G','T','LB','M','CM','MM','KM','FT',
                     'M2','M3','L','ML','GAL','ROLL','BAG','BOTTLE','CAN','TUBE',
                     'HOUR','DAY','MONTH');

-- ======================================================
-- ④ 两列 + 索引 + 约束
-- ======================================================
DROP INDEX IF EXISTS idx_biz_unit_is_system;
ALTER TABLE biz_unit DROP CONSTRAINT IF EXISTS ck_biz_unit_precision;
ALTER TABLE biz_unit DROP COLUMN IF EXISTS precision;
ALTER TABLE biz_unit DROP COLUMN IF EXISTS is_system;
