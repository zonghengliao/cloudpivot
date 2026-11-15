-- ============================================================
-- V0198：采购单绑定合同（「入库单 → 采购单 → 合同 → 供应商」勾稽链的中间一环）
--
-- 依据（照做，不自行发挥）：
--   `docs/architecture/采购入库勾稽重构-设计.md`
--     · §2.1  方案 B：`supplier_id` 保留并定稿为**快照**，另加 4 列
--     · §3.2  `ck_biz_po_purchase_type` / `ck_biz_po_contract_pair` 写法（`CASE … ELSE FALSE`）
--     · §4.4  `ck_biz_si_supplier_scope`
--     · §3.3  部分索引 `idx_biz_po_contract`
--     · §8.2  S1–S6 分步步骤（本文件按 S1→S4 落 DDL，S5 = 不回填，S6 = 迁移内自检）
--   `docs/architecture/ARCH-2-合同表数据模型与校验规则.md` §5.1 V-21/V-22/V-23
--
-- 卡面与本文件的两处**取值差异**（都是为了满足设计的判定，只变宽不变窄）：
--   ① `purchase_type` 取 `varchar(16)`（设计写 8，卡面写 16；两位枚举值都装得下，取宽者）；
--   ② `ck_biz_po_contract_pair` 采用**设计 §3.2 的强化版**：`CONTRACT` 除 `contract_id` 外
--      还要求 `contract_no_snapshot` 非空（卡面 S3 只钉了 `contract_id`；设计 §3.1 表把
--      「合同号快照必填」列为 CONTRACT 的落库约束，否则列表页仍要 JOIN 合同表才能显示单号）。
--
-- # 三条写法纪律（照抄 V0137 头注释）
--   ① 文件内**不写** `BEGIN; … COMMIT;` —— 迁移器自己在事务里跑，提前提交会拆掉迁移事务；
--   ② `CHECK` 用 `CASE … ELSE FALSE END`：`CHECK` 结果为 SQL NULL 时 PG 判定**通过**，
--      拼错的枚举值（如 `'CONTRCAT'`）必须被 `ELSE FALSE` 拦下；
--   ③ 迁移**不带 CONCURRENTLY**（事务里不允许），采购单量级 <100 万，普通 `CREATE INDEX` 即可。
--
-- # 存量（S5 结论：什么都不回填）
--   · `contract_id` / `contract_no_snapshot` / `supplier_name_snapshot` → 留 NULL（历史没有就是没有，
--     回填等于编造审计事实）；
--   · `purchase_type` → 由列默认值落 `'SPOT'`（这是语义正确，不是回填）；
--   · `biz_stock_in.supplier_id` → 一行不动（S2-③ 的漂移行只报告，清理要人确认）。
--
-- # 幂等
--   加列 `IF NOT EXISTS`、约束/索引 existence-guard、播种语句 `WHERE NOT EXISTS`：
--   up 重复执行是 no-op（迁移器的 `schema_migrations` 记录是主要保障，这里是双保险）。
-- ============================================================

-- ---------- S2 存量体检（只读；有方向性异常直接中止迁移，别带病上线） ----------
DO $$
DECLARE
    bad_scope   bigint;
    bad_supplier bigint;
    drift       bigint;
BEGIN
    SELECT count(*) INTO bad_scope
      FROM biz_stock_in WHERE in_type <> 'PURCHASE' AND supplier_id IS NOT NULL;
    SELECT count(*) INTO bad_supplier
      FROM biz_stock_in WHERE in_type = 'PURCHASE' AND supplier_id IS NULL;
    SELECT count(*) INTO drift
      FROM biz_stock_in s
      JOIN biz_purchase_order o ON o.id = s.source_order_id AND o.deleted_at IS NULL
     WHERE s.deleted_at IS NULL AND s.source_order_id IS NOT NULL
       AND s.supplier_id IS DISTINCT FROM o.supplier_id;

    RAISE NOTICE 'S2 存量体检：①非采购入库带供应商=% 行；②采购入库无供应商=% 行；③入库单与采购单供应商不一致=% 行（③只报告不修改）',
                 bad_scope, bad_supplier, drift;

    IF bad_scope > 0 THEN
        RAISE EXCEPTION 'S2-① 有 % 行非采购入库带着供应商：说明存在别的写入路径，加 ck_biz_si_supplier_scope 会失败，先停下来问人', bad_scope;
    END IF;
    IF bad_supplier > 0 THEN
        RAISE EXCEPTION 'S2-② 有 % 行采购入库没有供应商：与 V0145 的 ck_biz_si_supplier 冲突，先停下来问人', bad_supplier;
    END IF;
END $$;

-- ---------- S1 加列（只改元数据，PG 11+ 不重写堆表） ----------
ALTER TABLE biz_purchase_order
    ADD COLUMN IF NOT EXISTS contract_id            uuid,
    ADD COLUMN IF NOT EXISTS purchase_type          varchar(16) NOT NULL DEFAULT 'SPOT',
    ADD COLUMN IF NOT EXISTS contract_no_snapshot   varchar(64),
    ADD COLUMN IF NOT EXISTS supplier_name_snapshot varchar(128);

COMMENT ON COLUMN biz_purchase_order.contract_id IS
    '合同（弱引用 biz_contract.id，**不建外键**：同链零外键基线，存在性由 handler 校验 + 巡检兜底）。CONTRACT 必填 / SPOT 必须为空（ck_biz_po_contract_pair）';
COMMENT ON COLUMN biz_purchase_order.purchase_type IS
    '采购类型：CONTRACT=合同采购（供应商由合同乙方派生）/ SPOT=零星采购（供应商手选）。存量与默认值一律 SPOT';
COMMENT ON COLUMN biz_purchase_order.contract_no_snapshot IS
    '合同号快照（建单时刻）。**列表/打印/审批表单只读这一列，不为显示合同号去 JOIN biz_contract**；存量行 NULL';
COMMENT ON COLUMN biz_purchase_order.supplier_name_snapshot IS
    '供应商名称快照（业务发生那一刻）。语义是**快照不是冗余**：合同换乙方后历史单仍显示当时是谁；存量行 NULL，显示端 coalesce(本列, 主档当前名)';

-- ---------- S3 加约束（存在性守卫：重复执行 no-op） ----------
DO $$
BEGIN
    -- ① 枚举（同表、可判定 → 必须落 DB）
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_po_purchase_type') THEN
        ALTER TABLE biz_purchase_order
            ADD CONSTRAINT ck_biz_po_purchase_type
            CHECK (purchase_type IN ('CONTRACT', 'SPOT'));
    END IF;

    -- ② 双向钉死：CONTRACT 必有合同 id + 合同号快照；SPOT 两者必须为空。
    --    `ELSE FALSE` 是必需项（见文件头写法纪律 ②）。
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_po_contract_pair') THEN
        ALTER TABLE biz_purchase_order
            ADD CONSTRAINT ck_biz_po_contract_pair
            CHECK (CASE purchase_type
                     WHEN 'CONTRACT' THEN contract_id IS NOT NULL AND contract_no_snapshot IS NOT NULL
                     WHEN 'SPOT'     THEN contract_id IS NULL     AND contract_no_snapshot IS NULL
                     ELSE FALSE
                   END);
    END IF;

    -- ③ 非采购入库不能带供应商（与 V0145 的 ck_biz_si_supplier 合起来才是两向约束）。
    --    本列虽属入库单，但按设计归在本迁移（它是勾稽契约的一部分）。
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_si_supplier_scope') THEN
        ALTER TABLE biz_stock_in
            ADD CONSTRAINT ck_biz_si_supplier_scope
            CHECK (CASE WHEN in_type = 'PURCHASE' THEN TRUE ELSE supplier_id IS NULL END);
    END IF;
END $$;

-- ---------- S4 建索引（合同维度回查：这张合同下有哪些采购单） ----------
CREATE INDEX IF NOT EXISTS idx_biz_po_contract
    ON biz_purchase_order (contract_id, order_date DESC)
    WHERE contract_id IS NOT NULL;

-- ---------- S5 不回填（这里只做声明，不改任何行） ----------
-- 存量单：purchase_type 由默认值落 'SPOT'，contract_id / 两个快照留 NULL。

-- ---------- S6 迁移内自检（S1/S3/S4 结构 + 双向钉死零违反 + 存量口径播报） ----------
DO $$
DECLARE
    col_count  bigint;
    con_count  bigint;
    idx_count  bigint;
    violation  bigint;
    spot_only  bigint;
BEGIN
    SELECT count(*) INTO col_count
      FROM information_schema.columns
     WHERE table_name = 'biz_purchase_order'
       AND column_name IN ('contract_id', 'purchase_type', 'contract_no_snapshot', 'supplier_name_snapshot');
    IF col_count <> 4 THEN
        RAISE EXCEPTION 'S1 自检失败：biz_purchase_order 只找到 % / 4 个合同相关列', col_count;
    END IF;

    SELECT count(*) INTO con_count
      FROM pg_constraint
     WHERE conname IN ('ck_biz_po_purchase_type', 'ck_biz_po_contract_pair', 'ck_biz_si_supplier_scope');
    IF con_count <> 3 THEN
        RAISE EXCEPTION 'S3 自检失败：只找到 % / 3 个约束', con_count;
    END IF;

    SELECT count(*) INTO idx_count
      FROM pg_indexes
     WHERE tablename = 'biz_purchase_order' AND indexname = 'idx_biz_po_contract';
    IF idx_count <> 1 THEN
        RAISE EXCEPTION 'S4 自检失败：idx_biz_po_contract 不存在';
    END IF;

    SELECT count(*) INTO violation
      FROM biz_purchase_order
     WHERE NOT (CASE purchase_type
                  WHEN 'CONTRACT' THEN contract_id IS NOT NULL AND contract_no_snapshot IS NOT NULL
                  WHEN 'SPOT'     THEN contract_id IS NULL     AND contract_no_snapshot IS NULL
                  ELSE FALSE
                END);
    IF violation > 0 THEN
        RAISE EXCEPTION 'S6 自检失败：% 行违反双向钉死 ck_biz_po_contract_pair', violation;
    END IF;

    SELECT count(*) INTO spot_only
      FROM biz_purchase_order WHERE purchase_type = 'CONTRACT';

    RAISE NOTICE 'S6 自检通过：4 列 / 3 约束 / 1 部分索引；双向钉死违反=0；存量采购单 % 张（CONTRACT=% / 其余 SPOT）',
                 (SELECT count(*) FROM biz_purchase_order), spot_only;
END $$;
