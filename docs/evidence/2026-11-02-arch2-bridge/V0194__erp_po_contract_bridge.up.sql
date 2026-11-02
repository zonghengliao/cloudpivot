-- ============================================================
-- V0194：采购单↔合同勾稽 + 入库单供应商快照（ARCH-2 · 设计稿，由实现卡拷入 backend/migrations/business/）
--
-- 本文件是设计文档（§2–§4 模型 + §8 迁移步骤）的 DDL 原文，并且**已在本机真库跑过**（含自检与回滚），
-- 证据：docs/evidence/2026-11-02-arch2-bridge/（apply.log / rollback.log / bench_q1.log）
--
-- 迁移号：V0194（开工实测：仓库最大 V0192；V0193 已被「合同表」卡占用（设计稿
--         docs/evidence/2026-11-02-arch2-contract/V0193__erp_contract.up.sql），故本卡顺延 V0194）
--
-- # 本迁移只做四件事
--   ① biz_purchase_order 加合同四列（contract_id / purchase_type / contract_no_snapshot /
--      supplier_name_snapshot）+ 两个 CHECK（purchase_type 枚举、合同与类型**双向钉死**）
--   ② biz_stock_in 只改语义注释 + 加一条「非采购入库不得带供应商」的 CHECK
--   ③ 两个部分索引（合同维度、无源入库口径监控）
--   ④ 自检：列 / 约束 / 索引齐了没 + 5 条反例真的被拦 + 2 条正例真的能进 + 幂等重放
--
-- ── 与 V0193（合同表卡）的边界与去重 ────────────────────────────────────────
-- ① **不建** biz_contract：那是 V0193 的地盘。本文件对 biz_contract 只做**弱引用**（不建外键），
--    所以本迁移在「biz_contract 还不存在」的库上也能独立执行通过（自检块用 to_regclass 兜底）。
--    两卡谁先落地都对，后落地的 ADD COLUMN IF NOT EXISTS / ADD CONSTRAINT 存在性判断一律 no-op。
-- ② 与 V0193 草案**同名同义**的三处（ck_biz_po_purchase_type / ck_biz_po_contract_pair /
--    idx_biz_po_contract）刻意照抄它的名字与判据 —— 名字一致才能互相 no-op，不产生「两套约束守同一件事」。
-- ③ 与 V0193 草案**措辞不同**一处：biz_stock_in.supplier_id 的注释。草案写「冗余拷贝，不是快照」，
--    本卡按 ARCH-2 卡面 Q3 定稿为「快照」—— 两者不是口味差异，见文档 §2.4 的论证；
--    谁后落地谁的注释生效，但**语义结论是快照**，实现卡请以本文件为准。
--
-- ── 三条不可违反的口径（实测，见证据目录）────────────────────────────────
-- 1) **单据链不建外键**：本机实测（2026-11-02，PG 18.6，cloudpivot_t06d）biz_purchase_order
--    共 9 个 UUID 列（主键 + 8 个引用）、biz_stock_in 共 10 个（主键 + 9 个引用），
--    两表的外键数都是 **0**（`select count(*) from pg_constraint where contype='f'` 原文在证据目录）。
--    同链的 biz_payable / biz_voucher 同样零外键；只有 biz_stock_transfer（V0140）与
--    biz_inventory_location 走了硬外键。本卡跟**同链**对齐（弱引用 + 应用层校验 + 巡检兜底），
--    不给 contract_id 单独加一个「全表唯一的硬约束」——那会让后来者以为 supplier_id /
--    warehouse_id 也该有外键，制造第三种口径。合同删除是软删（deleted_at），硬外键拦不住软删，
--    收益本来就只有「防手滑写错 UUID」这一点，交给应用层 400 + 巡检即可。
-- 2) **存量数据一律不回填**：历史采购单没有合同 = 就是没有（purchase_type 落 SPOT）；
--    历史单据当时的供应商名称不可知 = supplier_name_snapshot 留 NULL，显示端
--    coalesce(snapshot, 主档当前名)。回填等于编造审计事实。
-- 3) **不给 biz_stock_in.source_order_id 加 NOT NULL**：实测（2026-11-02）生产代码里
--    「采购入库 + 无来源单」有 2 条合法路径 —— 手工建单（stockin_handler.rs:595-602 只要 items
--    非空即可）与「复制为新单」（同文件 :686-692 / :778 明确清空 source_order_id，理由是原单
--    已占用该采购单的待收额度，复制单再挂会二次占用 received_qty），外加 ADR-20261031 已定稿
--    但尚未落码的第 3 条（批量生成 = 无来源入库）。加 NOT NULL 会把它们全部打死。
--    本卡改为「新建入口在应用层按路径分档强制 + 巡检统计口径」，见文档 §4.5。
--
-- ── 写法纪律（V0137/V0145 的坑，别改）────────────────────────────────────
-- · 文件内**不写** BEGIN; … COMMIT;（迁移器自己在事务里执行本文件）
-- · CHECK 一律 CASE … END 或显式布尔，不写 `A OR (B AND C)`（PG 的 CHECK 遇 NULL 视为通过）
-- · 幂等：ADD COLUMN IF NOT EXISTS / CREATE INDEX IF NOT EXISTS / ADD CONSTRAINT 走 pg_constraint 存在性判断
-- ============================================================

-- ============================================================
-- §1 biz_purchase_order —— 合同关联 + 两类快照
-- ============================================================

-- contract_id：弱引用 biz_contract.id（不建外键，见头注释口径 1）。
-- 可空策略：CONTRACT 必填 / SPOT 必须为空 —— 这条**不在列定义上**，由 §1.2 的
-- ck_biz_po_contract_pair 按 purchase_type 双向钉死（列级 NOT NULL 只能表达「全都要」或「全都不要」，
-- 表达不了「看类型」）。
ALTER TABLE biz_purchase_order ADD COLUMN IF NOT EXISTS contract_id UUID;

-- purchase_type：CONTRACT 合同采购 / SPOT 零星采购（无合同）。
-- DEFAULT 'SPOT' 让 100 万行存量一次性落 SPOT（PG 11+ 只改元数据，不重写堆表），
-- 且**语义正确**：历史单据确实没有合同，不是「忘了填」。
ALTER TABLE biz_purchase_order ADD COLUMN IF NOT EXISTS purchase_type VARCHAR(8) NOT NULL DEFAULT 'SPOT';

-- contract_no_snapshot：合同号快照。列表 / 打印 / 审批表单不必 JOIN 合同表就能显示「HT-20261102-000001」。
ALTER TABLE biz_purchase_order ADD COLUMN IF NOT EXISTS contract_no_snapshot VARCHAR(32);

-- supplier_name_snapshot：供应商名称快照。存量行为 NULL（当时名称不可知），显示端 coalesce。
ALTER TABLE biz_purchase_order ADD COLUMN IF NOT EXISTS supplier_name_snapshot VARCHAR(200);

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'ck_biz_po_purchase_type'
                      AND conrelid = 'public.biz_purchase_order'::regclass) THEN
        ALTER TABLE biz_purchase_order ADD CONSTRAINT ck_biz_po_purchase_type
            CHECK (purchase_type IN ('CONTRACT', 'SPOT'));
    END IF;

    -- 双向钉死：CONTRACT 必须「有 id + 有合同号快照」；SPOT 必须「两者皆空」。
    -- 少任何一半，purchase_type 名不副实就多出第三种状态（有合同却标 SPOT / 标 CONTRACT 没合同）。
    -- 用 CASE … ELSE FALSE END：三值逻辑下 NULL 会被 CHECK 当成通过，ELSE FALSE 才拦得住脏值。
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'ck_biz_po_contract_pair'
                      AND conrelid = 'public.biz_purchase_order'::regclass) THEN
        ALTER TABLE biz_purchase_order ADD CONSTRAINT ck_biz_po_contract_pair
            CHECK (CASE purchase_type
                     WHEN 'CONTRACT' THEN contract_id IS NOT NULL AND contract_no_snapshot IS NOT NULL
                     WHEN 'SPOT'     THEN contract_id IS NULL AND contract_no_snapshot IS NULL
                     ELSE FALSE
                   END);
    END IF;
END
$$;

-- 合同维度回查（「这张合同下有哪些采购单」/ 合同执行情况汇总）：
-- 部分索引，SPOT 行（contract_id IS NULL）不占位；order_date DESC 让列表排序在索引内完成。
CREATE INDEX IF NOT EXISTS idx_biz_po_contract
    ON biz_purchase_order (contract_id, order_date DESC) WHERE contract_id IS NOT NULL;

COMMENT ON COLUMN biz_purchase_order.contract_id IS
  '合同（**弱引用** biz_contract.id，不建外键）：CONTRACT 采购必填、SPOT 采购必须为空 ——'
  ' 由 ck_biz_po_contract_pair 与 purchase_type 双向钉死。供应商真相 = biz_contract.party_b_id';
COMMENT ON COLUMN biz_purchase_order.purchase_type IS
  '采购类型：CONTRACT 合同采购 / SPOT 零星采购（无合同）。存量单据全部落 SPOT —— 历史上确实没有合同，不编造';
COMMENT ON COLUMN biz_purchase_order.contract_no_snapshot IS
  '合同号【快照】：建单时从合同带出，之后不跟随合同改号；与 contract_id 同生同灭（ck_biz_po_contract_pair 保证）';
COMMENT ON COLUMN biz_purchase_order.supplier_id IS
  '供应商【快照·业务发生时】：CONTRACT 时在建单事务内从合同乙方（party_b_id）带出；SPOT 时用户选择。'
  ' 列 NOT NULL（V0137）保持不变。存量行不回填为「合同乙方」——它们本来就没有合同';
COMMENT ON COLUMN biz_purchase_order.supplier_name_snapshot IS
  '供应商名称【快照】：建单时名称。**存量行为 NULL**（当时名称不可知，不编造），'
  ' 显示端 coalesce(supplier_name_snapshot, 主档当前名)';

-- ============================================================
-- §2 biz_stock_in —— 只改语义 + 一条范围约束
--   本表**不加** contract_id：库里一旦出现这一列，「只绑上一环」就不再有机械保证。
--   入库单要拿合同信息，只能走 source_order_id → biz_purchase_order.contract_id（文档 §6 的写入链路）。
-- ============================================================

COMMENT ON COLUMN biz_stock_in.source_order_id IS
  '来源采购单（**弱引用** biz_purchase_order.id，不建外键）= 本单唯一的业务父指针（「只绑上一环」）。'
  '有源时：supplier_id 必须等于该采购单的 supplier_id（应用层 400 校验，见文档 §7）；'
  '无源时：仅限「复制为新单」（stockin_handler 清空后重建）与 ADR-20261031 的无来源入库路径，'
  '且 supplier_id 必须由用户显式录入。**不加 NOT NULL**：加了三类合法路径全死（见头注释口径 3）';

COMMENT ON COLUMN biz_stock_in.supplier_id IS
  '供应商【快照】：本单业务发生时的乙方。有源时从采购单带出（= 采购单.supplier_id，禁止手工修改）；'
  '无源时用户录入。**不是可重算的冗余** —— 合同改乙方后，历史入库单仍要显示当时的供应商（审计口径）；'
  '真相链 = 入库单 → 采购单 → 合同.party_b_id → 供应商主档';

DO $$
BEGIN
    -- 非采购入库（RETURN / OTHER）**不许**带供应商：带了就说明有人把它当成采购入库在用，
    -- 应付 / 凭证会把它当采购成本入账。现有 ck_biz_si_supplier 只保证「采购必有供应商」，
    -- 缺的正是这个方向。实测：表里 in_type 由 stockin_model.rs:995 硬编码 'PURCHASE'，
    -- 存量行不会违反本约束（本机 4 个库实测 0 违反）。
    IF NOT EXISTS (SELECT 1 FROM pg_constraint
                    WHERE conname = 'ck_biz_si_supplier_scope'
                      AND conrelid = 'public.biz_stock_in'::regclass) THEN
        ALTER TABLE biz_stock_in ADD CONSTRAINT ck_biz_si_supplier_scope
            CHECK (CASE WHEN in_type = 'PURCHASE' THEN TRUE ELSE supplier_id IS NULL END);
    END IF;
END
$$;

-- 无源入库口径监控（「采购入库却有没来源单」的规模，是迁移账而非缺陷）：
-- 部分索引让这条 COUNT 变成 Index Only Scan，也给「按无源筛」的运维查询提速。
CREATE INDEX IF NOT EXISTS idx_biz_si_no_source
    ON biz_stock_in (in_date DESC) WHERE source_order_id IS NULL AND deleted_at IS NULL;

-- ============================================================
-- §3 自检（空库首启必须也能过；biz_contract 不存在也过）
-- ============================================================
DO $$
DECLARE
    need_col TEXT;
    miss     TEXT := '';
    ck_list  TEXT[] := ARRAY['ck_biz_po_purchase_type','ck_biz_po_contract_pair','ck_biz_si_supplier_scope'];
    idx_list TEXT[] := ARRAY['idx_biz_po_contract','idx_biz_si_no_source'];
    need_idx TEXT;
    blocked  BOOLEAN;
    v_wh     UUID;
    v_sup    UUID;
    v_id     UUID;
BEGIN
    -- 3.1 四列齐
    FOREACH need_col IN ARRAY ARRAY['contract_id','purchase_type','contract_no_snapshot','supplier_name_snapshot'] LOOP
        IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                        WHERE table_schema = 'public' AND table_name = 'biz_purchase_order'
                          AND column_name = need_col) THEN
            miss := miss || ' biz_purchase_order.' || need_col;
        END IF;
    END LOOP;
    -- 3.2 purchase_type 必须是 NOT NULL 且默认 SPOT（存量行靠它落 SPOT，默认值掉了就全 NULL）
    IF NOT EXISTS (SELECT 1 FROM information_schema.columns
                    WHERE table_schema = 'public' AND table_name = 'biz_purchase_order'
                      AND column_name = 'purchase_type' AND is_nullable = 'NO'
                      AND column_default LIKE '''SPOT''%') THEN
        miss := miss || ' biz_purchase_order.purchase_type(NOT NULL DEFAULT SPOT)';
    END IF;
    IF miss <> '' THEN
        RAISE EXCEPTION 'V0194 自检失败：缺列 →%', miss;
    END IF;

    -- 3.3 约束 / 索引在位
    FOREACH need_col IN ARRAY ck_list LOOP
        IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = need_col) THEN
            RAISE EXCEPTION 'V0194 自检失败：缺约束 %', need_col;
        END IF;
    END LOOP;
    FOREACH need_idx IN ARRAY idx_list LOOP
        IF NOT EXISTS (SELECT 1 FROM pg_indexes WHERE schemaname = 'public' AND indexname = need_idx) THEN
            RAISE EXCEPTION 'V0194 自检失败：缺索引 %', need_idx;
        END IF;
    END LOOP;

    -- 3.4 idx_biz_po_contract 必须是部分索引（SPOT 行不占位）
    IF NOT EXISTS (SELECT 1 FROM pg_indexes
                    WHERE schemaname = 'public' AND indexname = 'idx_biz_po_contract'
                      AND indexdef LIKE '%WHERE (contract_id IS NOT NULL)%') THEN
        RAISE EXCEPTION 'V0194 自检失败：idx_biz_po_contract 不是 WHERE contract_id IS NOT NULL 的部分索引';
    END IF;

    -- 3.5 反例（每条都必须被 CHECK 拦住）
    -- ① CONTRACT 却没有 contract_id
    blocked := FALSE;
    BEGIN
        INSERT INTO biz_purchase_order (order_no, supplier_id, order_date, warehouse_id,
                                        purchase_type, total_amount, total_tax, total_with_tax)
        VALUES ('__V194_NO_CONTRACT__', gen_random_uuid(), CURRENT_DATE, gen_random_uuid(),
                'CONTRACT', 0, 0, 0);
    EXCEPTION WHEN check_violation THEN blocked := TRUE;
    END;
    IF NOT blocked THEN
        RAISE EXCEPTION 'V0194 自检失败：purchase_type=CONTRACT 却没有 contract_id 被放进库（ck_biz_po_contract_pair 失效）';
    END IF;

    -- ② SPOT 却带了 contract_id（双向钉死的另一半）
    blocked := FALSE;
    BEGIN
        INSERT INTO biz_purchase_order (order_no, supplier_id, order_date, warehouse_id,
                                        purchase_type, contract_id, total_amount, total_tax, total_with_tax)
        VALUES ('__V194_SPOT_WITH_CONTRACT__', gen_random_uuid(), CURRENT_DATE, gen_random_uuid(),
                'SPOT', gen_random_uuid(), 0, 0, 0);
    EXCEPTION WHEN check_violation THEN blocked := TRUE;
    END;
    IF NOT blocked THEN
        RAISE EXCEPTION 'V0194 自检失败：purchase_type=SPOT 却带 contract_id 被放进库（ck_biz_po_contract_pair 只钉了一半）';
    END IF;

    -- ③ purchase_type 脏值
    blocked := FALSE;
    BEGIN
        INSERT INTO biz_purchase_order (order_no, supplier_id, order_date, warehouse_id,
                                        purchase_type, total_amount, total_tax, total_with_tax)
        VALUES ('__V194_BAD_TYPE__', gen_random_uuid(), CURRENT_DATE, gen_random_uuid(),
                'CONTRCAT', 0, 0, 0);
    EXCEPTION WHEN check_violation THEN blocked := TRUE;
    END;
    IF NOT blocked THEN
        RAISE EXCEPTION 'V0194 自检失败：purchase_type 脏值被放进库（ck_biz_po_purchase_type 失效）';
    END IF;

    -- ④ 非采购入库带供应商
    blocked := FALSE;
    BEGIN
        INSERT INTO biz_stock_in (in_no, in_type, supplier_id, warehouse_id, in_date)
        VALUES ('__V194_RETURN_SUP__', 'RETURN', gen_random_uuid(), gen_random_uuid(), CURRENT_DATE);
    EXCEPTION WHEN check_violation THEN blocked := TRUE;
    END;
    IF NOT blocked THEN
        RAISE EXCEPTION 'V0194 自检失败：in_type=RETURN 却带 supplier_id 被放进库（ck_biz_si_supplier_scope 失效）';
    END IF;

    -- ⑤ 采购入库却缺供应商（既有约束 ck_biz_si_supplier 仍在，别被本卡改坏）
    blocked := FALSE;
    BEGIN
        INSERT INTO biz_stock_in (in_no, in_type, supplier_id, warehouse_id, in_date)
        VALUES ('__V194_PURCHASE_NO_SUP__', 'PURCHASE', NULL, gen_random_uuid(), CURRENT_DATE);
    EXCEPTION WHEN check_violation THEN blocked := TRUE;
    END;
    IF NOT blocked THEN
        RAISE EXCEPTION 'V0194 自检失败：PURCHASE 缺 supplier_id 被放进库（ck_biz_si_supplier 被破坏）';
    END IF;

    -- 3.6 正例（合法的两种形态都能进；合同用随机 UUID —— 弱引用不建外键，不需要 biz_contract 存在）
    INSERT INTO biz_purchase_order (order_no, supplier_id, order_date, warehouse_id,
                                    purchase_type, total_amount, total_tax, total_with_tax)
    VALUES ('__V194_SPOT_OK__', gen_random_uuid(), CURRENT_DATE, gen_random_uuid(), 'SPOT', 0, 0, 0)
    RETURNING id INTO v_id;
    DELETE FROM biz_purchase_order WHERE id = v_id;

    INSERT INTO biz_purchase_order (order_no, supplier_id, order_date, warehouse_id,
                                    purchase_type, contract_id, contract_no_snapshot,
                                    total_amount, total_tax, total_with_tax)
    VALUES ('__V194_CONTRACT_OK__', gen_random_uuid(), CURRENT_DATE, gen_random_uuid(),
            'CONTRACT', gen_random_uuid(), 'HT-20261102-000001', 0, 0, 0)
    RETURNING id INTO v_id;
    DELETE FROM biz_purchase_order WHERE id = v_id;

    RAISE NOTICE 'V0194 自检通过：4 列 / 3 约束 / 2 部分索引 / 5 条反例拦截 / 2 条正例入库';
END
$$;

-- ============================================================
-- §4 存量体检（只报数，不改数；给上线前一步跑）
--   本段刻意不抛异常：体检结论交给人看，迁移只负责把结构做对。
--   回填策略见文档 §8（结论：不回填 contract_id，不回填 supplier_name_snapshot）。
-- ============================================================
DO $$
DECLARE
    n_total      BIGINT;
    n_spot       BIGINT;
    n_contract   BIGINT;
    n_si_total   BIGINT;
    n_si_nosrc   BIGINT;
    n_si_other   BIGINT;
BEGIN
    SELECT count(*), count(*) FILTER (WHERE purchase_type = 'SPOT'),
           count(*) FILTER (WHERE purchase_type = 'CONTRACT')
      INTO n_total, n_spot, n_contract FROM biz_purchase_order;

    SELECT count(*), count(*) FILTER (WHERE source_order_id IS NULL),
           count(*) FILTER (WHERE in_type <> 'PURCHASE')
      INTO n_si_total, n_si_nosrc, n_si_other FROM biz_stock_in;

    RAISE NOTICE 'V0194 存量口径：采购单 % 张（SPOT % / CONTRACT %）；入库单 % 张（无来源 % / 非采购 %）',
        n_total, n_spot, n_contract, n_si_total, n_si_nosrc, n_si_other;
END
$$;
