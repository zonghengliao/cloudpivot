-- ============================================================
-- V0199：入库单改绑采购单（A4）—— 「无源单据」显式标记
--
-- 依据（照做，不自行发挥）：
--   `docs/architecture/采购入库勾稽重构-设计.md`
--     · §4.3  「禁止手工修改供应商」三层收口（DTO / handler / DB）
--     · §4.4  `ck_biz_si_supplier_scope`（V0198 已落；本文件只复核它在）
--     · §4.5  `source_order_id` 必填化评估：**不做 NOT NULL**，改为「按入口分档强制 + 口径巡检」
--     · §6.4  明令不落库的两样东西（`contract_id` / 合同乙方）—— 本文件**不加**这两列
--   `docs/architecture/通用勾稽规则与业务链映射表.md`
--     · AP-4  入库单不许让用户手填供应商（正确做法：只读带出；手工场景走「无源单据」流程）
--     · AP-5  `PURCHASE` 型允许 `source_order_id = NULL` 成为常态是反模式
--             → 只允许 E4 情形（期初 / 复制 / 其他），且**必须打无源标记 + 进巡检**
--     · E4 / I-P4（「复制单没采购单怎么办」的现成裁决：允许，但必须打标记 + 进巡检）
--
-- # 为什么加一列，而不是用 `source_order_id IS NULL` 隐式表达
--   AP-5 的判据是「可枚举、能进巡检」，而不是「读的人自己知道空就是无源」：
--   ① 无源是**建单时的显式意图**（用户点「无源单据」/ 系统复制），不是一个可以被顺手漏填出来的状态；
--      handler 层因此要求「没绑采购单 ⇒ 必须显式声明 is_sourceless=true」，否则 400；
--   ② 标记是 NOT NULL 的布尔列 + 部分索引，巡检 / 报表 / 列表筛选直接 `WHERE is_sourceless` 命中，
--      不必在每条 SQL 里重复解释「空值含义」；
--   ③ 语义由 `ck_biz_si_sourceless_pair` 钉死（`is_sourceless = (source_order_id IS NULL)`），
--      所以它**不会**变成第二个真相：任何绕过接口直接改库的写法都会被 DB 当场拒掉。
--
-- # 存量（E4 情形）
--   现网的「无源入库」只可能来自两条路径：手工建单、复制为新单（`copy_stock_in` 会清空来源单）。
--   本文件把它们**如实标记**为无源（`source_order_id IS NULL ⇒ is_sourceless = true`），
--   不改任何业务列，也不改任何单据状态。
--
-- # 三条写法纪律（照抄 V0137 / V0198 头注释）
--   ① 文件内**不写** `BEGIN; … COMMIT;` —— 迁移器自己在事务里跑；
--   ② `CHECK` 用 `CASE … ELSE FALSE END` 兜「拼错的枚举值」；本文件的 CHECK 是布尔恒等式，
--      已用 `IS NOT TRUE` 写法兜 NULL（`NULL = …` 会判定通过）；
--   ③ 迁移**不带 CONCURRENTLY**（事务里不允许）；入库单量级 <100 万，普通 `CREATE INDEX` 即可。
--
-- # 幂等
--   加列 `IF NOT EXISTS`、约束 existence-guard、索引 `IF NOT EXISTS`、回填语句本身幂等：
--   up 重复执行是 no-op（迁移器的 `schema_migrations` 记录是主要保障，这里是双保险）。
-- ============================================================

-- ---------- S2 存量体检（只读；方向性异常直接中止，别带病上线） ----------
DO $$
DECLARE
    sourceless_total bigint;
    comment_missing  bigint;
    bad_purchase     bigint;
    drift            bigint;
BEGIN
    SELECT count(*) INTO sourceless_total
      FROM biz_stock_in WHERE deleted_at IS NULL AND source_order_id IS NULL;
    SELECT count(*) INTO comment_missing
      FROM biz_stock_in
     WHERE deleted_at IS NULL AND source_order_id IS NULL
       AND coalesce(btrim(remark), '') = '';
    SELECT count(*) INTO bad_purchase
      FROM biz_stock_in WHERE in_type = 'PURCHASE' AND supplier_id IS NULL;
    SELECT count(*) INTO drift
      FROM biz_stock_in s
      JOIN biz_purchase_order o ON o.id = s.source_order_id AND o.deleted_at IS NULL
     WHERE s.deleted_at IS NULL AND s.source_order_id IS NOT NULL
       AND s.supplier_id IS DISTINCT FROM o.supplier_id;

    RAISE NOTICE 'S2 存量体检：无源入库单=% 张（其中无备注=% 张，只报告）；采购入库缺供应商=% 张；入库单与采购单供应商不一致=% 张（只报告不修改）',
                 sourceless_total, comment_missing, bad_purchase, drift;

    IF bad_purchase > 0 THEN
        RAISE EXCEPTION 'S2 有 % 张采购入库没有供应商：与 V0145 的 ck_biz_si_supplier 冲突，先停下来问人', bad_purchase;
    END IF;
END $$;

-- ---------- S1 加列（只改元数据，PG 11+ 不重写堆表） ----------
ALTER TABLE biz_stock_in
    ADD COLUMN IF NOT EXISTS is_sourceless boolean NOT NULL DEFAULT false;

-- ---------- S2' 回填：无源入库如实标记（E4：复制 / 手工 / 期初） ----------
-- 只有 NULL → true 这一个方向；有源单（`source_order_id` 非空）保持 false。
UPDATE biz_stock_in
   SET is_sourceless = true
 WHERE source_order_id IS NULL
   AND is_sourceless IS DISTINCT FROM true;

-- ---------- S3 加约束（把「无源」与「没有采购单」钉成一件事） ----------
-- `IS NOT TRUE`：NULL 也判失败（`CHECK` 结果为 NULL 时 PG 判定**通过**，这里不能给它机会）。
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_si_sourceless_pair') THEN
        ALTER TABLE biz_stock_in
            ADD CONSTRAINT ck_biz_si_sourceless_pair
            CHECK ((is_sourceless = (source_order_id IS NULL)) IS TRUE);
    END IF;
END $$;

-- ---------- S3' 复核 A3 落的供应商方向约束还在（只读断言，不重建） ----------
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'ck_biz_si_supplier_scope') THEN
        RAISE EXCEPTION 'S3 复核失败：ck_biz_si_supplier_scope 不存在（A3 的 V0198 未落或已被回滚），先停下来问人';
    END IF;
END $$;

-- ---------- S4 建索引（无源单据枚举 / 巡检：`WHERE is_sourceless` 走部分索引） ----------
CREATE INDEX IF NOT EXISTS idx_biz_si_sourceless
    ON biz_stock_in (in_date DESC, in_no DESC)
    WHERE is_sourceless;

-- ---------- S5 注释：把「快照」语义写进库（显示口径要冻结） ----------
COMMENT ON COLUMN biz_stock_in.is_sourceless IS
    '无源单据标记（A4）：true = 不绑采购单（期初/复制/其他，E4 情形），由建单时显式声明；与 source_order_id 由 ck_biz_si_sourceless_pair 钉死';
COMMENT ON COLUMN biz_stock_in.supplier_id IS
    '供应商快照（A4 定稿写入来源）：PURCHASE+有源时由 biz_purchase_order.supplier_id 派生，客户端传入不等于采购单即 400；无源单据由用户显式填写。仅用于显示/审计/应付取数，**不得**用于跨环判定';
COMMENT ON COLUMN biz_stock_in.source_order_id IS
    '来源采购单（唯一主关联，R2：只绑上一环）。PURCHASE 有源单必填、供应商从该单派生；无源单为 NULL 且 is_sourceless=true';

-- ---------- S6 迁移内自检（结构 + 钉死零违反 + 存量口径播报） ----------
DO $$
DECLARE
    col_count   bigint;
    con_count   bigint;
    idx_count   bigint;
    violation   bigint;
    src_true    bigint;
    src_null    bigint;
BEGIN
    SELECT count(*) INTO col_count
      FROM information_schema.columns
     WHERE table_name = 'biz_stock_in' AND column_name = 'is_sourceless';
    IF col_count <> 1 THEN
        RAISE EXCEPTION 'S1 自检失败：biz_stock_in.is_sourceless 不存在';
    END IF;

    SELECT count(*) INTO con_count
      FROM pg_constraint
     WHERE conname IN ('ck_biz_si_sourceless_pair', 'ck_biz_si_supplier_scope');
    IF con_count <> 2 THEN
        RAISE EXCEPTION 'S3 自检失败：只找到 % / 2 个约束', con_count;
    END IF;

    SELECT count(*) INTO idx_count
      FROM pg_indexes
     WHERE tablename = 'biz_stock_in' AND indexname = 'idx_biz_si_sourceless';
    IF idx_count <> 1 THEN
        RAISE EXCEPTION 'S4 自检失败：idx_biz_si_sourceless 不存在';
    END IF;

    SELECT count(*) INTO violation
      FROM biz_stock_in
     WHERE (is_sourceless = (source_order_id IS NULL)) IS NOT TRUE;
    IF violation > 0 THEN
        RAISE EXCEPTION 'S6 自检失败：% 行违反 ck_biz_si_sourceless_pair', violation;
    END IF;

    SELECT count(*) INTO src_true
      FROM biz_stock_in WHERE deleted_at IS NULL AND source_order_id IS NOT NULL;
    SELECT count(*) INTO src_null
      FROM biz_stock_in WHERE deleted_at IS NULL AND is_sourceless;

    RAISE NOTICE 'S6 自检通过：1 列 / 2 约束 / 1 部分索引；钉死违反=0；存量入库单 % 张（有源=% / 无源标记=%）',
                 src_true + src_null, src_true, src_null;
END $$;
