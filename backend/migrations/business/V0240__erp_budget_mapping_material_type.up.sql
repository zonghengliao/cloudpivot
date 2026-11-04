-- ============================================================================
-- V0240__erp_budget_mapping_material_type.up.sql
--
-- 【t_ce73ae26】预算科目映射存不进 MATERIAL_TYPE：校验要 ref_id、解析只认 ref_code、
--              硬塞占位 ref_id 又撞 uk_biz_bsm_target
--
-- # 一、现象（HEAD df74a5f，源码 + 前端真机）
--   `POST /api/business/erp/budget/mapping` 带 `map_type=MATERIAL_TYPE` + `ref_code=RAW`
--   必然 400「MATERIAL_TYPE 映射必须给 ref_id」；补一个占位 `ref_id` 虽能过校验，但
--     ① 解析侧 `budget_model::resolve_for_po` 的 MATERIAL_TYPE 分支只比 `ref_code`
--        （`ref_code.as_deref() == Some(kind)`），占位 id 存进去也不会命中；
--     ② 同部门第二条 MATERIAL_TYPE 直接撞 `uk_biz_bsm_target (map_type, ref_id, dept)`。
--   ⇒ 「物料类型 → 预算科目」在旧 DDL + 旧校验的组合下**没有任何合法写法**，
--      只能靠前端显式拦住（`mappingSaveBlocker`，本卡一并撤掉）。
--
-- # 二、为什么改索引（方案 B）而不是给物料类型造 id（方案 A）
--   物料类型**不是一张表**：它是 `biz_material.material_type` 上的字符串枚举
--   （`ck_biz_material_type`，V0187 由 4 值扩到 7 值 —— RAW / SEMI / FINISHED /
--    PACKAGING / CONSUMABLE / OFFICE / SERVICE），全库没有 `biz_material_type` 表、
--   也没有 uuid。走方案 A 要先造一张主数据表 + 改物料落库形态 + 回填存量物料，
--   那是另一个功能（主数据化），不是这个缺陷的修复面。
--   方案 B 保留既有语义：`ref_code` 就是类型编码，把 DDL 与 DTO 校验改成同一套口径。
--
-- # 三、本文件改什么（只动约束与索引，不增删列、不改任何业务数据）
--   ① `ck_biz_bsm_ref`：旧版只有两分支「DEFAULT 免 ref_id / 其余必须 ref_id」，
--      对 MATERIAL_TYPE 强制 ref_id 就是「存不进去」的第一道墙。新增三档：
--        DEFAULT             : ref_id IS NULL
--        MATERIAL_TYPE       : ref_id IS NULL 且 ref_code 非空（类型编码）
--        MATERIAL / WAREHOUSE: ref_id NOT NULL（物料 id / 仓库 id）
--      `ref_code` 对 MATERIAL / WAREHOUSE 仍可空（只做展示与对账，与旧行为一致）。
--   ② `uk_biz_bsm_target`：目标键从 `(map_type, ref_id, dept)` 改成
--        `(map_type, COALESCE(ref_id::text, upper(btrim(ref_code)), ''),
--                    COALESCE(dept_id, '000…000'::uuid))`
--      即「有目标 id 用 id，没有 id 用归一化后的编码」，dept 的 NULL（全公司）用零 uuid 占位。
--      `COALESCE(..., '')` 的第三个参数不能省：DEFAULT 行 ref_id 与 ref_code 都是 NULL，
--      少了它键里仍有 NULL，而 Postgres 的唯一索引把 NULL 当互不相同 —— 那么「同部门两条
--      DEFAULT」照样能插进去（实测过：漏掉 `''` 时第二次 POST DEFAULT 返回 200）。
--      语义变化三条，都要知道：
--        a. MATERIAL_TYPE 的目标唯一性落在 ref_code 上 —— 同一部门同一类型只能一条（符合预期，
--           前端重复新建会拿到 409 可读文案，而不是 23505 裸错）；
--        b. 顺带堵上旧索引的两个洞：旧键里 ref_id 为 NULL 时 Postgres 把行视为互不相同，
--           于是 DEFAULT 映射可以在同一部门重复建任意多条；新键用 COALESCE 填成非空后，
--           同一部门只能有一条 DEFAULT（全公司兜底本来就该唯一）；
--        c. 编码在索引表达式里做了 `upper(btrim(...))` —— 配置侧存 'raw' 也挡得住与 'RAW'
--           的重复，应用层不需要先「查重再插」（仍会查，见 handler 的 409）。
--   ③ 归一化只用于**比较口径**（索引表达式 + 应用层写入），**不改已有行的大小写**：
--      展示与对账仍是用户当初填的原文。
--
-- # 四、既有行安全性（自检失败即中止升级，整文件单事务回滚）
--   a. 新 CHECK：按三档组合扫全表，任一行不满足就 RAISE，并打印涉及的 map_type；
--   b. 新唯一键：先按新表达式 group by 找重复，有重复就 RAISE 并列出前 5 组
--      —— 升级不会静默删用户的配置行，宁可让人工合并后再上；
--   c. 末尾断言约束与索引都在、且 uk 的键表达式与预期一致（正则匹配 pg_get_indexdef）。
--   实况：99 的 cloudpivot_* 库与 144 的 cloudpivot / cloudpivot_dev / cloudpivot_test
--   在 V0227 之后**还没有任何种子行**（本卡实测 `relation … does not exist` 或 0 行），
--   所以三步自检在现有环境上都是 0 行命中。
--
-- # 五、回滚
--   down 恢复 V0227 的 `ck_biz_bsm_ref` 与 `uk_biz_bsm_target`。
--   ⚠ 回滚前必须先处理 MATERIAL_TYPE 行（ref_id 为 NULL 会违反旧 ck），
--     down 里先按「ref_id IS NULL 的非 DEFAULT 行」RAISE 拦一道，不静默删配置。
-- ============================================================================

-- ---------- ① 新 CHECK 的既有行自检（先检查，后改约束） ----------
DO $$
DECLARE
    bad    bigint;
    kinds  text;
BEGIN
    SELECT count(*), string_agg(DISTINCT map_type, ', ')
      INTO bad, kinds
      FROM biz_budget_subject_map
     WHERE NOT (
              (map_type = 'DEFAULT' AND ref_id IS NULL)
           OR (map_type = 'MATERIAL_TYPE'
               AND ref_id IS NULL AND ref_code IS NOT NULL AND btrim(ref_code) <> '')
           OR (map_type IN ('MATERIAL', 'WAREHOUSE') AND ref_id IS NOT NULL)
          );
    IF bad > 0 THEN
        RAISE EXCEPTION
            'V0240 自检失败：% 条映射行不满足新的 ck_biz_bsm_ref（涉及 map_type=%）。'
            '先用 SELECT id, map_type, ref_id, ref_code, dept_id FROM biz_budget_subject_map 核对后再升级',
            bad, coalesce(kinds, '(空)');
    END IF;
END $$;

-- ---------- ② 新唯一键的重复自检（不替用户合并配置） ----------
-- 用 EXISTS 自比较而不是 GROUP BY：键是函数表达式，分组时还要把原始列一起带上，
-- 写出来又长又容易写错；映射表是运维配置（几十行），O(n²) 完全无所谓。
DO $$
DECLARE
    dup    bigint;
    sample text;
BEGIN
    SELECT count(*), string_agg(k, ' | ')
      INTO dup, sample
      FROM (
            SELECT DISTINCT
                   m.map_type || '/' ||
                   coalesce(m.ref_id::text, upper(btrim(m.ref_code)), '∅') || '/' ||
                   coalesce(m.dept_id::text, 'ALL') AS k
              FROM biz_budget_subject_map m
             WHERE m.deleted_at IS NULL
               AND EXISTS (
                    SELECT 1
                      FROM biz_budget_subject_map o
                     WHERE o.deleted_at IS NULL
                       AND o.id <> m.id
                       AND o.map_type = m.map_type
                       AND coalesce(o.ref_id::text, upper(btrim(o.ref_code)), '∅')
                         = coalesce(m.ref_id::text, upper(btrim(m.ref_code)), '∅')
                       AND coalesce(o.dept_id, '00000000-0000-0000-0000-000000000000'::uuid)
                         = coalesce(m.dept_id, '00000000-0000-0000-0000-000000000000'::uuid)
                   )
             LIMIT 5
           ) AS d;
    IF dup > 0 THEN
        RAISE EXCEPTION
            'V0240 自检失败：新唯一键 (map_type, coalesce(ref_id, upper(btrim(ref_code))), dept) '
            '下有 % 组重复映射（%）；升级不会替你删配置，请先人工合并',
            dup, sample;
    END IF;
END $$;

-- ---------- ③ 重写 ck_biz_bsm_ref ----------
ALTER TABLE biz_budget_subject_map DROP CONSTRAINT IF EXISTS ck_biz_bsm_ref;
ALTER TABLE biz_budget_subject_map
    ADD CONSTRAINT ck_biz_bsm_ref CHECK (
        (map_type = 'DEFAULT' AND ref_id IS NULL)
     OR (map_type = 'MATERIAL_TYPE'
         AND ref_id IS NULL AND ref_code IS NOT NULL AND btrim(ref_code) <> '')
     OR (map_type IN ('MATERIAL', 'WAREHOUSE') AND ref_id IS NOT NULL)
    );

-- ---------- ④ 重建 uk_biz_bsm_target ----------
DROP INDEX IF EXISTS uk_biz_bsm_target;
CREATE UNIQUE INDEX uk_biz_bsm_target
    ON biz_budget_subject_map (
        map_type,
        COALESCE(ref_id::text, upper(btrim(ref_code)), ''),
        COALESCE(dept_id, '00000000-0000-0000-0000-000000000000'::uuid)
    )
    WHERE deleted_at IS NULL;

-- ---------- ⑤ 列语义写进注释（后来人看 information_schema 就能看懂） ----------
COMMENT ON COLUMN biz_budget_subject_map.ref_id IS
    '目标 id：MATERIAL=物料 id / WAREHOUSE=仓库 id / MATERIAL_TYPE 与 DEFAULT 恒为 NULL（物料类型是字符串枚举，没有独立 id）';
COMMENT ON COLUMN biz_budget_subject_map.ref_code IS
    '目标编码：MATERIAL_TYPE=物料类型编码（7 值枚举，比较口径 upper(btrim()) 不区分大小写）；MATERIAL/WAREHOUSE=展示用编码，可空';
COMMENT ON INDEX uk_biz_bsm_target IS
    '目标唯一：同一 map_type 下「同一目标 + 同一部门（NULL=全公司）」只能一条；目标键 = coalesce(ref_id, upper(btrim(ref_code)))';

-- ---------- ⑥ 断言（改完必须成立，否则整文件回滚） ----------
DO $$
DECLARE
    idx_def   text;
    has_ck    boolean;
BEGIN
    SELECT pg_get_indexdef(i.indexrelid)
      INTO idx_def
      FROM pg_index i
      JOIN pg_class c ON c.oid = i.indexrelid
     WHERE c.relname = 'uk_biz_bsm_target';
    IF idx_def IS NULL THEN
        RAISE EXCEPTION 'V0240 自检失败：uk_biz_bsm_target 不存在';
    END IF;
    IF idx_def NOT LIKE '%upper(btrim%' OR idx_def NOT LIKE '%ref_id%' THEN
        RAISE EXCEPTION
            'V0240 自检失败：uk_biz_bsm_target 的键表达式没换成 coalesce(ref_id, upper(btrim(ref_code)))，实际=%',
            idx_def;
    END IF;

    SELECT count(*) > 0
      INTO has_ck
      FROM pg_constraint
     WHERE conname = 'ck_biz_bsm_ref'
       AND conrelid = 'biz_budget_subject_map'::regclass;
    IF NOT has_ck THEN
        RAISE EXCEPTION 'V0240 自检失败：ck_biz_bsm_ref 不存在';
    END IF;

    RAISE NOTICE 'V0240 ✓ MATERIAL_TYPE 可落库：ck_biz_bsm_ref 三档 + uk_biz_bsm_target 目标键 = coalesce(ref_id, upper(btrim(ref_code)))';
END $$;
