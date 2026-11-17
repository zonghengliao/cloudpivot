-- ============================================================================
-- V0451：框架协议「订单引用」—— 采购单挂协议（M4-08）
--
--   ① biz_purchase_order       + framework_agreement_id（弱引用单头）
--                              + framework_agreement_no_snapshot（协议号快照）
--   ② biz_purchase_order_item  + framework_agreement_item_id（弱引用物料范围行）
--   ③ 两个部分索引（只有挂了协议的行走索引，存量零星采购不付代价）
--   ④ 末尾 DO 块自检（列 / 可空性 / 部分索引谓词 / 存量行不被改动 / 弱引用无外键 /
--      探针清理）
--
-- ── 卡面与本卡边界（M4-08：订单引用，校验价格/数量，超限拒绝）─────────────
--   本迁移只加「引用列」，不写校验逻辑（校验在 `framework_agreement_ref.rs`），
--   不加一行 `sys_menu` / 一个权限码 / 一条路由 / 一行前端 —— 页面属 M4-11。
--   执行量统计属 M4-09、系统单测属 M4-10。
--
-- ── 号段核对（动手前 `ls backend/app/migrations/*/ | grep -oE 'V[0-9]{4}' | sort -u | tail`）
--   写入本文件时全局最大号 = V0450（框架协议表结构），V0451 空闲。
--
-- ── 为什么是「单头挂协议 + 明细挂协议行」两层 ────────────────────────────
--   一份协议对一家供应商（V0450 头注释 4），所以「这张单用的是哪份协议」是**单头**属性：
--   一张采购单不可能同时按两份框架协议的价格结算。
--   「这一行对应协议里的哪个物料」才是**明细**属性 —— 它既是价格判据的取值来源，
--   也是执行量回写（`executed_qty +=`）的落点。明细只存 item id、不重复存 material_id：
--   协议行的物料是建单时按 `(agreement_id, material_id)` 查出来的（`uk_biz_fwai_material`），
--   落成外键式引用才能让「协议后来改了物料范围」这件事在历史单上查得出来。
--
-- ── 为什么两列都可空（NULL = 不挂协议）─────────────────────────────────
--   框架协议是**可选**的采购方式：零星采购（SPOT）、紧急采购、协议外物料都必须能下单。
--   把列写成 NOT NULL 等于「没有协议就不许买」，那是另一种业务（且现实里没人这么干）。
--   所以：NULL = 本单不引用协议（**不校验价格/数量**）；非空 = 必须过 M4-08 的三道校验
--   （协议可用 / 单价不超协议价 × (1+容差) / 累计数量不超上限）。
--   引用列可空 和 「挂了协议就必须校验」并不矛盾 —— 校验在写入路径上做，不在库层
--   （跨表、跨单据，CHECK 表达不出来，见 V0450 头注释 2/3）。
--
-- ── 为什么是弱引用（不加外键），以及为什么存协议号快照 ──────────────────
--   与 `contract_id` / `contract_no_snapshot`（V0198）逐字同口径：
--   · 不加外键：协议是软删表（`deleted_at`），外键拦不住软删；而 `ON DELETE CASCADE`
--     会让「删协议」把历史采购单的明细一起删掉 —— 那是账，不是配置。
--   · 存协议号快照：协议号是**唯一且不再变**的业务单号，历史单显示「当时按哪份协议下的单」
--     不该依赖协议表还在不在（约定 13：审计/历史类存快照）。展示侧要现名/现状态时
--     再 join `biz_framework_agreement`，两列并存不是冗余。
--   名称/状态/价格条款**不**落快照：那些会变（协议能改、能停用），落下来就成了一份
--   永远不更新的假快照；详情现查现显（M4-08 的 detail 出参就是这么做的）。
--
-- ── 幂等与回滚 ─────────────────────────────────────────────────────────
--   ALTER TABLE ... ADD COLUMN IF NOT EXISTS / CREATE INDEX IF NOT EXISTS；
--   自检只读 + 一对探针（插一张探针单，验完立刻删）。
--   回滚见 V0451__framework_agreement_order_ref.down.sql：**有单据挂了协议就拒绝回滚**
--   （drop column 等于把「这张单按哪份协议下的」抹掉，历史就查不回来了）。
-- ============================================================================

-- ---------- ① 采购单单头：引用框架协议 ----------
ALTER TABLE biz_purchase_order ADD COLUMN IF NOT EXISTS framework_agreement_id UUID;
ALTER TABLE biz_purchase_order ADD COLUMN IF NOT EXISTS framework_agreement_no_snapshot VARCHAR(32);

COMMENT ON COLUMN biz_purchase_order.framework_agreement_id IS
    '框架协议（弱引用 biz_framework_agreement.id；NULL = 本单不挂协议，不做价格/数量校验）。M4-08';
COMMENT ON COLUMN biz_purchase_order.framework_agreement_no_snapshot IS
    '协议号快照（FWA-yyyyMMdd-000001；建单时从协议带出，历史单不依赖协议表是否还在）。M4-08';

-- ---------- ② 采购单明细：引用协议物料行（价格判据 + 执行量回写的落点） ----------
ALTER TABLE biz_purchase_order_item ADD COLUMN IF NOT EXISTS framework_agreement_item_id UUID;

COMMENT ON COLUMN biz_purchase_order_item.framework_agreement_item_id IS
    '框架协议物料范围行（弱引用 biz_framework_agreement_item.id；按 (协议, 物料) 由服务端带出，客户端不许传）。M4-08';

-- ---------- ③ 索引：只有挂了协议的单走索引（存量零星采购不付代价） ----------
CREATE INDEX IF NOT EXISTS idx_biz_po_fwa
    ON biz_purchase_order (framework_agreement_id)
 WHERE framework_agreement_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_biz_poi_fwa_item
    ON biz_purchase_order_item (framework_agreement_item_id)
 WHERE framework_agreement_item_id IS NOT NULL;

-- ---------- ④ 自检 ----------
DO $$
DECLARE
    v_cnt    integer;
    v_before bigint;
    v_after  bigint;
    v_missing text;
    probe_order uuid := gen_random_uuid();
    probe_item  uuid := gen_random_uuid();
BEGIN
    -- ④.1 三列必须都在（迁移改名 / 写漏一处时这里先炸，而不是等 handler 运行期）
    SELECT string_agg(c, ', ') INTO v_missing
      FROM (VALUES ('framework_agreement_id'), ('framework_agreement_no_snapshot')) AS v(c)
     WHERE NOT EXISTS (SELECT 1 FROM information_schema.columns
                        WHERE table_schema = 'public' AND table_name = 'biz_purchase_order'
                          AND column_name = v.c);
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0451 自检失败：biz_purchase_order 缺列：%', v_missing;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                    WHERE table_schema = 'public' AND table_name = 'biz_purchase_order_item'
                      AND column_name = 'framework_agreement_item_id') THEN
        RAISE EXCEPTION 'V0451 自检失败：biz_purchase_order_item 缺列 framework_agreement_item_id';
    END IF;

    -- ④.2 三列都必须可空（NULL = 不挂协议；写成 NOT NULL 等于「没有协议不许采购」）
    SELECT count(*) INTO v_cnt FROM information_schema.columns
     WHERE table_schema = 'public'
       AND ((table_name = 'biz_purchase_order'
             AND column_name IN ('framework_agreement_id', 'framework_agreement_no_snapshot'))
         OR (table_name = 'biz_purchase_order_item' AND column_name = 'framework_agreement_item_id'))
       AND is_nullable = 'YES';
    IF v_cnt <> 3 THEN
        RAISE EXCEPTION 'V0451 自检失败：三个引用列里只有 % / 3 个可空（引用必须是可选的）', v_cnt;
    END IF;

    -- ④.3 两个索引必须存在，且都是带谓词的部分索引（否则存量行白付索引代价）
    SELECT count(*) INTO v_cnt
      FROM pg_indexes
     WHERE schemaname = 'public'
       AND indexname IN ('idx_biz_po_fwa', 'idx_biz_poi_fwa_item')
       AND indexdef LIKE '%WHERE%IS NOT NULL%';
    IF v_cnt <> 2 THEN
        RAISE EXCEPTION 'V0451 自检失败：两个引用索引只建出 % / 2 个（或不是部分索引）', v_cnt;
    END IF;

    -- ④.4 弱引用：指向不存在的协议 / 协议行也必须写得进去（无外键，历史单不因协议被删而失效）
    v_before := (SELECT count(*) FROM biz_purchase_order WHERE framework_agreement_id IS NOT NULL);
    INSERT INTO biz_purchase_order
        (id, order_no, supplier_id, order_date, warehouse_id, framework_agreement_id,
         framework_agreement_no_snapshot)
    VALUES (probe_order, 'PO-19990101-900451', gen_random_uuid(), DATE '1999-01-01',
            gen_random_uuid(), gen_random_uuid(), 'FWA-19990101-900451');
    INSERT INTO biz_purchase_order_item
        (id, order_id, line_no, material_id, unit_id, qty, price, framework_agreement_item_id)
    VALUES (probe_item, probe_order, 1, gen_random_uuid(), gen_random_uuid(), 1, 1, gen_random_uuid());

    -- ④.5 存量行一行都不许被这次迁移改动（加列是纯加列，不回填、不重写）
    v_after := (SELECT count(*) FROM biz_purchase_order WHERE framework_agreement_id IS NOT NULL);
    IF v_after <> v_before + 1 THEN
        RAISE EXCEPTION 'V0451 自检失败：加列改动了存量采购单的引用列（前 % / 后 %）', v_before, v_after;
    END IF;
    SELECT count(*) INTO v_cnt FROM biz_purchase_order_item WHERE framework_agreement_item_id IS NOT NULL;
    IF v_cnt <> 1 THEN
        RAISE EXCEPTION 'V0451 自检失败：加列改动了存量明细的引用列（非空 % 行，期望 1 = 探针本身）', v_cnt;
    END IF;

    -- ④.6 探针清干净（明细靠 ON DELETE CASCADE 跟着走）
    DELETE FROM biz_purchase_order WHERE id = probe_order;
    SELECT count(*) INTO v_cnt FROM biz_purchase_order_item WHERE id = probe_item;
    IF v_cnt <> 0 THEN
        RAISE EXCEPTION 'V0451 自检失败：删探针单后明细还留着（ON DELETE CASCADE 失效）';
    END IF;
    SELECT count(*) INTO v_cnt FROM biz_purchase_order WHERE order_no = 'PO-19990101-900451';
    IF v_cnt <> 0 THEN
        RAISE EXCEPTION 'V0451 自检失败：探针单没清干净（% 行残留）', v_cnt;
    END IF;

    RAISE NOTICE 'M4-08/V0451: 采购单挂框架协议（单头 framework_agreement_id + 明细 framework_agreement_item_id）自检通过';
END $$;
