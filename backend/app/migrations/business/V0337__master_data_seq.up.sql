-- =============================================================================
-- V0337 主数据编码序列（卡 CODE-01 · 自动编码地基）
-- =============================================================================
--
-- 目标：给「供应商 / 客户 / 物料 / 仓库 / 资产分类 / 费用类型」六类主数据
--       各配一条**纯流水、不带日期、不按期间重置**的取号规则。
--
-- ── 与卡面的三处差异（先核了真实表结构，照卡面抄会直接失败）──────────────
--
-- ① 卡面写 `date_pattern = NULL`；真实列是
--      date_pattern VARCHAR(8) NOT NULL DEFAULT 'yyyyMMdd'
--      CHECK (date_pattern IN ('yyyyMMdd','yyyyMM','none'))          （V0137 §1.1）
--    ⇒ NOT NULL + 白名单，**写 NULL 必然 23502/23514 报错**。
--    库里本来就用 `'none'` 表达「编号不带日期段」，本迁移照它落地；语义与卡面
--    「date_pattern 为空」一致，验收口径改成 `date_pattern = 'none'`。
--
-- ② 卡面写 `ON CONFLICT (seq_type) DO NOTHING`；真实唯一约束是**部分唯一索引**
--      uk_biz_seq_type ON biz_seq (seq_type) WHERE deleted_at IS NULL       （V0137 §1.1）
--    只写列名推不出该索引 → 报 42P10「no unique or exclusion constraint matching」。
--    ⇒ 必须写 `ON CONFLICT (seq_type) WHERE deleted_at IS NULL DO NOTHING`（逐字对应谓词）。
--
-- ③ 位数（4 位 SUP/CUS/MAT、2 位 WH/AC/ET）在卡面没有落点：只写「加 step 或新列
--    pad_width」。本迁移**新增 `pad_width`**（step 是步长，语义不同，借用会把
--    「一次跳 N 号」和「补零位数」混成一个字段）。默认 4，CHECK 1..12。
--    取号器 `erp::seq` 读它补零；流水超过位数时按实际位数输出、**不截断**。
--
-- ── 为什么 period_key = 'GLOBAL' ────────────────────────────────────────
-- 主数据是**档案**：编码永久唯一，不按天/月重置（对比单据 PO/IN 的 `yyyyMMdd`）。
-- 取号器按「同一 period_key 内累加」，写成 'GLOBAL' 就等于「永不清零」。
--
-- ── 幂等 ───────────────────────────────────────────────────────────────
-- 重复执行：ADD COLUMN IF NOT EXISTS / 约束在 DO 块里判 pg_constraint /
-- INSERT ... ON CONFLICT（部分索引谓词）DO NOTHING。第二遍跑 = 0 行插入、无报错。
--
-- ── 与已有种子的冲突（重要，交付给下游卡）───────────────────────────────
-- V0137 已种入一个示例供应商 `biz_supplier.supplier_code = 'SUP-0001'`。
-- 所以「取号 SUP-0001 → 插入供应商」在老库里会撞唯一约束；`erp::seq` 的
-- `next_master_code_free` 会**自动跳过已占用的号**（下次给出 SUP-0002）。
-- 本迁移**不**因此抬高 current_no：current_no 只表示「本序列发到第几号」，
-- 老数据的编码不一定来自序列（企业自建编码体系同理）。

-- ---------- §1 位数列 ----------
ALTER TABLE biz_seq ADD COLUMN IF NOT EXISTS pad_width SMALLINT NOT NULL DEFAULT 4;

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_seq_pad_width') THEN
        ALTER TABLE biz_seq
            ADD CONSTRAINT ck_biz_seq_pad_width CHECK (pad_width BETWEEN 1 AND 12);
    END IF;
END $$;

COMMENT ON COLUMN biz_seq.pad_width IS
    '流水段最小位数：不足左补 0（4 ⇒ SUP-0001 / 2 ⇒ WH-01）；超过位数按实际输出、不截断';

-- ---------- §2 六条主数据序列 ----------
INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key,
                     current_no, step, separator, status, pad_width, remark)
VALUES
    (gen_random_uuid(), 'SUP', '供应商编码',   'SUP', 'none', 'GLOBAL', 0, 1, '-', 1, 4,
     '主数据编码（CODE-01）：SUP-0001，纯流水不带日期，永久唯一'),
    (gen_random_uuid(), 'CUS', '客户编码',     'CUS', 'none', 'GLOBAL', 0, 1, '-', 1, 4,
     '主数据编码（CODE-01）：CUS-0001，纯流水不带日期，永久唯一'),
    (gen_random_uuid(), 'MAT', '物料编码',     'MAT', 'none', 'GLOBAL', 0, 1, '-', 1, 4,
     '主数据编码（CODE-01）：MAT-0001，纯流水不带日期，永久唯一'),
    (gen_random_uuid(), 'WH',  '仓库编码',     'WH',  'none', 'GLOBAL', 0, 1, '-', 1, 2,
     '主数据编码（CODE-01）：WH-01，纯流水不带日期，永久唯一（2 位）'),
    (gen_random_uuid(), 'AC',  '资产分类编码', 'AC',  'none', 'GLOBAL', 0, 1, '-', 1, 2,
     '主数据编码（CODE-01）：AC-01，纯流水不带日期，永久唯一（2 位）'),
    (gen_random_uuid(), 'ET',  '费用类型编码', 'ET',  'none', 'GLOBAL', 0, 1, '-', 1, 2,
     '主数据编码（CODE-01）：ET-01，纯流水不带日期，永久唯一（2 位）')
ON CONFLICT (seq_type) WHERE deleted_at IS NULL DO NOTHING;

-- ---------- §3 自检（不符合就整条迁移回滚） ----------
DO $$
DECLARE
    v_bad   text;
    v_total int;
BEGIN
    SELECT count(*) INTO v_total
      FROM biz_seq
     WHERE seq_type IN ('SUP','CUS','MAT','WH','AC','ET') AND deleted_at IS NULL;

    SELECT string_agg(e.seq_type || ' → 实际[' ||
                      coalesce(s.prefix, '缺行') || '/' || coalesce(s.date_pattern, '-') || '/' ||
                      coalesce(s.period_key, '-') || '/' || coalesce(s.pad_width::text, '-') || ']',
                      ', ')
      INTO v_bad
      FROM (VALUES
            ('SUP','SUP','none','GLOBAL',4),
            ('CUS','CUS','none','GLOBAL',4),
            ('MAT','MAT','none','GLOBAL',4),
            ('WH', 'WH', 'none','GLOBAL',2),
            ('AC', 'AC', 'none','GLOBAL',2),
            ('ET', 'ET', 'none','GLOBAL',2)
           ) AS e(seq_type, prefix, date_pattern, period_key, pad_width)
      LEFT JOIN biz_seq s
             ON s.seq_type = e.seq_type AND s.deleted_at IS NULL
     WHERE s.id IS NULL
        OR s.prefix       <> e.prefix
        OR s.date_pattern <> e.date_pattern
        OR s.period_key   <> e.period_key
        OR s.pad_width    <> e.pad_width
        OR s.status       <> 1;

    IF v_bad IS NOT NULL THEN
        RAISE EXCEPTION 'V0337 自检失败：主数据取号规则不符 → %', v_bad;
    END IF;
    IF v_total <> 6 THEN
        RAISE EXCEPTION 'V0337 自检失败：主数据取号规则应为 6 条，实际 % 条', v_total;
    END IF;

    RAISE NOTICE 'V0337 自检通过：6 条主数据取号规则就位（date_pattern=none / period_key=GLOBAL / pad_width 4:4:4:2:2:2）';
END $$;
