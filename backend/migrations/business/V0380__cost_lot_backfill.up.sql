-- ============================================================================
-- V0380：成本核算升级 · 存量批号回填 biz_lot（ARCH-9 §7.2「biz_lot 回填」）
--
-- 卡：t_191cf205（COST-B1-7【成本】迁移 V0376/0377/0379）—— B1-7 第三件
-- 依据：docs/architecture/ARCH-9-成本核算升级设计.md §7.2（回填 SQL 三处来源 + 待清理清单）、
--       §3.1 ①（biz_lot DDL）、§7.1（设计稿把这块叫 V0379__cost_lot_backfill）、
--       实证脚本 docs/evidence/2026-11-10-arch9-3-cost-ddl/01_cost_ddl_check.sql。
--
-- # 为什么需要回填（现场事实，不是"感觉缺"）
--   库里原先**没有**批次主数据，批号只是三处自由文本列：
--     biz_stock_in_item.lot_no      VARCHAR(64)  （入库明细，V0145）
--     biz_stock_out_item.lot_no     VARCHAR(64)  （出库明细，V0203）
--     biz_material_serial.batch_no  VARCHAR(64)  （序列号，V0138）
--   V0370 才建了 biz_lot。存量单据里的批号如果不在 biz_lot 里，"按批号查库存/查成本"
--   就查不到历史批次（分批法的批次锚点缺失）。本迁移把三处来源里的**不同 (物料, 批号)**
--   收进 biz_lot。
--
-- # 口径
--   · 三处来源 UNION（去重），同一 (物料, 批号) 只建一行：来源优先级 STOCK_IN > STOCK_OUT > SERIAL
--     （入库是最原始的发生点；只在 remark 里记来源，供人工确认来源类型与效期）
--   · 批号一律 **btrim** 后落库（' AB-01 ' 与 'AB-01' 不该是两个批次）
--   · 落库形态：source_type='MANUAL'（不知道是采购还是自产，交人工确认）、
--              source_doc_type='BACKFILL'（**回滚的唯一抓手**：down 只删这一类）
--   · 存量行与已存在的活 biz_lot 冲突 ⇒ 保留已有的（ON CONFLICT DO NOTHING），不覆盖客户维护过的主数据
--   · **不建物理外键**：存量批号质量不可控（V0370 文件头已定此口径），错值靠清单 + 巡检
--
-- # 不合规清单（设计 §7.2）
--   三类：① 首尾含空格 ② 超长（>64 字节语义，btrim 后） ③ 物料不存在或已被软删
--   处理：**逐条 RAISE NOTICE 打到迁移日志**（明细最多 200 行 + 合计行），**不阻断迁移**
--   —— 客户环境里这些脏数据是历史遗留，阻断就等于"这个版本升不上去"。
--   注意：含空格的批号回填时会 btrim，所以 biz_lot 里是干净值；清单报的是**源表原始值**，
--   便于运维去源单据上改（设计 §7.2 的清单 SQL 查的是 biz_lot，回填后永远查不出空格，
--   本迁移改进为查源表，见文件末「偏差」）。
--
-- # 幂等
--   同一 (material_id, btrim(lot_no)) 第二次执行必落 ON CONFLICT DO NOTHING ⇒ 0 行插入。
--   前置一致性断言（§3）也保证第二次跑不会因为"已存在"而误报。
--
-- # 回滚
--   V0380.down.sql：DELETE FROM biz_lot WHERE source_doc_type='BACKFILL' AND source_type='MANUAL'
--   （只删回填行，一行不碰业务/客户自建的批次）；带护栏：若这些批次已被成本对象引用，仍删但打
--   WARNING 点名数量（回填行不是业务事实来源，删掉只是回到"没有批次主数据"的旧状态）。
-- ============================================================================

-- ----------------------------------------------------------------------------
-- §0 前置护栏：需要的表 / 列都在（缺就是前面迁移没跑全，别在这里瞎猜）
-- ----------------------------------------------------------------------------
DO $$
DECLARE
    missing TEXT;
BEGIN
    SELECT string_agg(w.what, '; ') INTO missing FROM (
        SELECT 'biz_lot' AS what WHERE to_regclass('public.biz_lot') IS NULL
        UNION ALL
        SELECT 'biz_stock_in_item.lot_no' WHERE NOT EXISTS (
            SELECT 1 FROM information_schema.columns
             WHERE table_schema='public' AND table_name='biz_stock_in_item' AND column_name='lot_no')
        UNION ALL
        SELECT 'biz_stock_out_item.lot_no' WHERE NOT EXISTS (
            SELECT 1 FROM information_schema.columns
             WHERE table_schema='public' AND table_name='biz_stock_out_item' AND column_name='lot_no')
        UNION ALL
        SELECT 'biz_material_serial.batch_no' WHERE NOT EXISTS (
            SELECT 1 FROM information_schema.columns
             WHERE table_schema='public' AND table_name='biz_material_serial' AND column_name='batch_no')
        UNION ALL
        SELECT 'biz_material' WHERE to_regclass('public.biz_material') IS NULL
    ) w;
    IF missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0380 拒绝执行：缺少依赖（%）。请确认 V0137/V0138/V0145/V0203/V0370 都已应用', missing;
    END IF;
END $$;

-- ----------------------------------------------------------------------------
-- §1 待清理清单：三类不合规批号 → 打日志（不阻断）
-- ----------------------------------------------------------------------------
DO $$
DECLARE
    r        RECORD;
    n_total  INT := 0;
    n_shown  INT := 0;
    n_space  INT := 0;
    n_long   INT := 0;
    n_mat    INT := 0;
BEGIN
    FOR r IN
        WITH src AS (
            SELECT material_id, lot_no AS raw_no, 'STOCK_IN' AS from_tbl
              FROM biz_stock_in_item WHERE lot_no IS NOT NULL AND btrim(lot_no) <> ''
            UNION ALL
            SELECT material_id, lot_no, 'STOCK_OUT'
              FROM biz_stock_out_item WHERE lot_no IS NOT NULL AND btrim(lot_no) <> ''
            UNION ALL
            SELECT material_id, batch_no, 'SERIAL'
              FROM biz_material_serial
             WHERE batch_no IS NOT NULL AND btrim(batch_no) <> '' AND deleted_at IS NULL
        )
        SELECT s.material_id,
               s.raw_no,
               string_agg(DISTINCT s.from_tbl, '/' ORDER BY s.from_tbl) AS from_tbl,
               CASE WHEN bool_or(s.raw_no <> btrim(s.raw_no)) THEN '首尾含空格' END AS e1,
               CASE WHEN length(btrim(s.raw_no)) > 64 THEN '超长' END AS e2,
               CASE WHEN NOT EXISTS (SELECT 1 FROM biz_material m
                                      WHERE m.id = s.material_id AND m.deleted_at IS NULL)
                    THEN '物料不存在或已软删' END AS e3
          FROM src s
         GROUP BY s.material_id, s.raw_no
        HAVING bool_or(s.raw_no <> btrim(s.raw_no))
            OR length(btrim(s.raw_no)) > 64
            OR NOT EXISTS (SELECT 1 FROM biz_material m WHERE m.id = s.material_id AND m.deleted_at IS NULL)
         ORDER BY s.material_id, s.raw_no
    LOOP
        n_total := n_total + 1;
        IF r.e1 IS NOT NULL THEN n_space := n_space + 1; END IF;
        IF r.e2 IS NOT NULL THEN n_long  := n_long  + 1; END IF;
        IF r.e3 IS NOT NULL THEN n_mat   := n_mat   + 1; END IF;
        IF n_shown < 200 THEN
            RAISE NOTICE 'V0380 待清理批号 #%：物料=% 批号=[%] 来源=% 问题=%',
                n_total, r.material_id, r.raw_no, r.from_tbl,
                concat_ws('/', r.e1, r.e2, r.e3);
            n_shown := n_shown + 1;
        END IF;
    END LOOP;

    RAISE NOTICE 'V0380 不合规批号合计 % 条（含空格 % / 超长 % / 物料失联 %；明细打印 % 行，上限 200）'
                 '—— 不阻断迁移，请进运维报告跟进源单据',
        n_total, n_space, n_long, n_mat, n_shown;
END $$;

-- ----------------------------------------------------------------------------
-- §2 回填：三处来源 → biz_lot（每 (物料, btrim(批号)) 一行）
-- ----------------------------------------------------------------------------
DO $$
DECLARE
    n_ins INT;
BEGIN
    WITH src AS (
        SELECT material_id, btrim(lot_no) AS lot_no, 1 AS prio, 'STOCK_IN' AS from_tbl
          FROM biz_stock_in_item WHERE lot_no IS NOT NULL AND btrim(lot_no) <> ''
        UNION ALL
        SELECT material_id, btrim(lot_no), 2, 'STOCK_OUT'
          FROM biz_stock_out_item WHERE lot_no IS NOT NULL AND btrim(lot_no) <> ''
        UNION ALL
        SELECT material_id, btrim(batch_no), 3, 'SERIAL'
          FROM biz_material_serial
         WHERE batch_no IS NOT NULL AND btrim(batch_no) <> '' AND deleted_at IS NULL
    ), one AS (
        -- 同一 (物料, 批号) 只留一行；取优先级最高的来源做 remark 溯源
        SELECT DISTINCT ON (material_id, lot_no) material_id, lot_no, from_tbl
          FROM src
         ORDER BY material_id, lot_no, prio
    )
    INSERT INTO biz_lot (lot_no, material_id, source_type, source_doc_type, remark)
    SELECT lot_no, material_id, 'MANUAL', 'BACKFILL',
           '存量批号回填（来源：' || from_tbl || '），请确认来源类型与效期'
      FROM one
    ON CONFLICT (material_id, lot_no) WHERE deleted_at IS NULL DO NOTHING;

    GET DIAGNOSTICS n_ins = ROW_COUNT;
    RAISE NOTICE 'V0380：本次回填 % 行（已存在的活批次不覆盖；第二次执行应为 0）', n_ins;
END $$;

-- ----------------------------------------------------------------------------
-- §3 自检
--   ① 每个源 (物料, 批号) 在 biz_lot 都有活行 —— 回填没漏，且幂等重放后依然成立
--   ② 回填行的批号都是 btrim 后的干净值（源表空格已归一）
--   ③ 回填行形态固定（source_type='MANUAL' / source_doc_type='BACKFILL'），down 才抓得住
--   任一不符 → 迁移失败（这三条都是"算错了"级别，不能只告警）
-- ----------------------------------------------------------------------------
DO $$
DECLARE
    n_miss  INT;
    n_dirty INT;
    n_shape INT;
    sample  TEXT;
BEGIN
    WITH src AS (
        SELECT material_id, btrim(lot_no) AS lot_no
          FROM biz_stock_in_item WHERE lot_no IS NOT NULL AND btrim(lot_no) <> ''
        UNION
        SELECT material_id, btrim(lot_no)
          FROM biz_stock_out_item WHERE lot_no IS NOT NULL AND btrim(lot_no) <> ''
        UNION
        SELECT material_id, btrim(batch_no)
          FROM biz_material_serial
         WHERE batch_no IS NOT NULL AND btrim(batch_no) <> '' AND deleted_at IS NULL
    )
    SELECT count(*), string_agg(s.lot_no, ', ') INTO n_miss, sample
      FROM src s
     WHERE NOT EXISTS (SELECT 1 FROM biz_lot l
                        WHERE l.material_id = s.material_id AND l.lot_no = s.lot_no
                          AND l.deleted_at IS NULL);
    IF n_miss > 0 THEN
        RAISE EXCEPTION 'V0380 自检失败：% 个源批号没有落进 biz_lot（例：%）', n_miss, sample;
    END IF;

    SELECT count(*) INTO n_dirty FROM biz_lot
     WHERE source_doc_type = 'BACKFILL' AND deleted_at IS NULL
       AND lot_no <> btrim(lot_no);
    IF n_dirty > 0 THEN
        RAISE EXCEPTION 'V0380 自检失败：% 条回填批号含首尾空格（应已 btrim）', n_dirty;
    END IF;

    SELECT count(*) INTO n_shape FROM biz_lot
     WHERE source_doc_type = 'BACKFILL' AND deleted_at IS NULL
       AND (source_type <> 'MANUAL' OR coalesce(remark, '') NOT LIKE '存量批号回填（来源：%');
    IF n_shape > 0 THEN
        RAISE EXCEPTION 'V0380 自检失败：% 条回填行的形态不对（source_type 应为 MANUAL、remark 应带来源）', n_shape;
    END IF;

    RAISE NOTICE 'V0380：批号回填自检通过（源批号全部落地、批号已归一、回填行形态固定）';
END $$;
