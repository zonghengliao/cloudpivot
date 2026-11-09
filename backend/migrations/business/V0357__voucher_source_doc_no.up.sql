-- ============================================================================
-- V0357：`biz_voucher.source_doc_no` —— 凭证的「业务单据号」（E2E-FIX-5 ②，P4）
--
-- 卡：t_6e6b43a1（E2E-FIX-5【P3+P4】）
--
-- # 一、现场（E2E-02 演练发现）
--
--   退货单 SR-20261109-000001 过账后产生两张凭证：
--     · 一张 source_type = STOCK_IN、source_no = IN-…（退货入库单）
--     · 一张 source_type = INVOICE 、source_no = IV-…（红字发票）
--   于是「按退货单号查凭证」= 0 行，只能 `summary LIKE '%SR-…%'` 模糊查。
--
--   根因不是漏写，而是**语义混用**：`source_no` 表达的是「source_id 那张单的单号」
--   （凭证的真实记账依据），一张业务单据派生多张子单据（入库 / 发票 / 出库）时，
--   它必然指向子单据。业务单据号（用户手里那个号）从来没有落库。
--
-- # 二、为什么不复用 source_no，而是加一列
--
--   `source_no` 的语义已被广泛依赖（辅助核算夹具按 `source_no LIKE 'P21AUX%'` 清场、
--   `payable_bridge` 断言 `voucher.source_no = payment_no`、联动模板按 source_no 归集）。
--   把退货凭证的 source_no 改成退货单号，等于让「凭证 ↔ 记账依据单据」这条链断掉
--   （红字发票点了凭证回链就查不到了）。**加一列，两条链各自正确**：
--
--     source_type / source_id / source_no  → 记账依据（子单据），保持不变
--     source_doc_no                        → 业务单据号（人对外的单号 / 用户查的那个号）
--
--   对绝大多数单据两者相同（收款单的 source_no 就是收款单号）；只有
--   「一张业务单 → 多张子单据 → 多张凭证」的退货场景才真正分流。
--
-- # 三、取值口径
--
--   写入：`voucher::post_voucher` 落库时 `source_doc_no = COALESCE(显式传入, source_no)`。
--     显式传入的只有退货过账（销售退货 = 退货单号，采购退货 = 退货单号）；
--     其余自动凭证（收款 / 退款 / 开票 / 付款 / 入库 / 出库 / 结转）沿用 source_no。
--   读取：查询面 `GET /business/erp/voucher/page?source_doc_no=SR-…` 精确匹配；
--     关键词 `voucher_no` 也一并模糊匹配这一列（用户直接粘单号就能搜到）。
--
-- # 四、存量数据（卡面 ③）
--
--   数据量小（演练库 22 张凭证 / 生产同量级），所以**直接回填**，不留「只保证新数据」：
--     ① 历史退货凭证：从摘要里把单号捞回来（摘要格式是 `销售退货 SR-…` /
--        `采购退货 PR-…（原入库单 …）`，正则取前缀号），先填 source_doc_no；
--     ② 其余凭证：`source_doc_no = source_no`（业务单据 = 记账依据单据）；
--     ③ 摘要里的 36 位 UUID → 名称（客户 / 供应商 / 物料），锚定前缀（`客户 <uuid>`）
--        只替换真正的实体引用，长度超出 varchar(255) 的行跳过（不会因 22001 中断迁移）。
--   ①② 幂等（回填后不再命中）；③ 幂等（UUID 被替换掉后不再命中）。
--
-- # 五、幂等 / 回滚
--
--   `ADD COLUMN IF NOT EXISTS` / `CREATE INDEX IF NOT EXISTS`；重复执行不报错、不覆盖已有值。
--   文件内**不写** BEGIN/COMMIT（迁移器自己开事务）。
--   回滚：`V0357__voucher_source_doc_no.down.sql` —— 列与索引可丢，回填过的数据不可恢复
--   （摘要里的原始 UUID 一旦换成名称就回不去了；down 文件头写明）。
-- ============================================================================

-- ============================================================
-- §1 加列
-- ============================================================
ALTER TABLE biz_voucher
    ADD COLUMN IF NOT EXISTS source_doc_no VARCHAR(64);

-- ============================================================
-- §2 列注释（语义写进库，别只写在代码注释里）
-- ============================================================
COMMENT ON COLUMN biz_voucher.source_doc_no IS
    '业务单据号（用户手里那个号，E2E-FIX-5 ②）：一张业务单据派生多张凭证时的统一查询键（退货单 → 入库凭证 + 红字发票凭证）。与 source_no 的区别：source_no 是「source_id 那张单的单号」（记账依据 / 子单据），本列是「人对外的业务单号」；大多数单据两者相同，退货场景才分流。写入口径见 V0357 头注释';
-- ============================================================
-- §3 索引（按业务单号查凭证 = 本列存在的全部理由）
-- ============================================================
CREATE INDEX IF NOT EXISTS idx_biz_voucher_source_doc_no
    ON biz_voucher (source_doc_no) WHERE deleted_at IS NULL;

-- ============================================================
-- §4 存量回填 ①：历史退货凭证的业务单号（从摘要里捞）
-- ============================================================
-- 摘要格式（sales_return_posting::cost_voucher_summary / revenue_voucher_summary）：
--   `销售退货 SR-20261109-000001 成本冲减` / `… 收入冲减`
-- 摘要格式（purchase_return_posting::voucher_summary）：
--   `采购退货 PR-20261109-000001（原入库单 IN-…）冲减应付与存货`
UPDATE biz_voucher
   SET source_doc_no = substring(summary FROM '(SR-[0-9]{8}-[0-9]{6})')
 WHERE source_doc_no IS NULL
   AND summary LIKE '%SR-%';

UPDATE biz_voucher
   SET source_doc_no = substring(summary FROM '(PR-[0-9]{8}-[0-9]{6})')
 WHERE source_doc_no IS NULL
   AND summary LIKE '%PR-%';

-- ============================================================
-- §5 存量回填 ②：其余凭证 = 记账依据单据号
-- ============================================================
UPDATE biz_voucher
   SET source_doc_no = source_no
 WHERE source_doc_no IS NULL
   AND source_no IS NOT NULL
   AND btrim(source_no) <> '';

-- ============================================================
-- §6 存量回填 ③：摘要里的 UUID → 名称（AGENTS.md 约定 13）
-- ============================================================
-- 只替换**锚定实体前缀**的 UUID（`客户 <uuid>` / `供应商 <uuid>` / `物料 <uuid>`）：
-- 裸替换 36 位 hex 会误伤 remark / 其他字段里的 id 语义，而且看不出替换的是哪类实体。
-- 长度守卫：替换后超过 varchar(255) 的行跳过（PG 会抛 22001 把整个迁移打断）。
UPDATE biz_voucher v
   SET summary = replace(v.summary, '客户 ' || c.id::text, '客户 ' || c.name)
  FROM biz_customer c
 WHERE v.summary LIKE '%客户 ' || c.id::text || '%'
   AND length(v.summary) - 36 + length(c.name) <= 255;

UPDATE biz_voucher v
   SET summary = replace(v.summary, '供应商 ' || s.id::text, '供应商 ' || s.supplier_name)
  FROM biz_supplier s
 WHERE v.summary LIKE '%供应商 ' || s.id::text || '%'
   AND length(v.summary) - 36 + length(s.supplier_name) <= 255;

UPDATE biz_voucher v
   SET summary = replace(v.summary, '物料 ' || m.id::text, '物料 ' || m.material_name)
  FROM biz_material m
 WHERE v.summary LIKE '%物料 ' || m.id::text || '%'
   AND length(v.summary) - 36 + length(m.material_name) <= 255;

-- 分录摘要同样口径（分录摘要 = 单头摘要 + 后缀，UUID 一样在里面）
UPDATE biz_voucher_entry e
   SET summary = replace(e.summary, '客户 ' || c.id::text, '客户 ' || c.name)
  FROM biz_customer c
 WHERE e.summary LIKE '%客户 ' || c.id::text || '%'
   AND length(e.summary) - 36 + length(c.name) <= 255;

UPDATE biz_voucher_entry e
   SET summary = replace(e.summary, '供应商 ' || s.id::text, '供应商 ' || s.supplier_name)
  FROM biz_supplier s
 WHERE e.summary LIKE '%供应商 ' || s.id::text || '%'
   AND length(e.summary) - 36 + length(s.supplier_name) <= 255;

UPDATE biz_voucher_entry e
   SET summary = replace(e.summary, '物料 ' || m.id::text, '物料 ' || m.material_name)
  FROM biz_material m
 WHERE e.summary LIKE '%物料 ' || m.id::text || '%'
   AND length(e.summary) - 36 + length(m.material_name) <= 255;

-- ============================================================
-- §7 自检（up 后必须全为真）
-- ============================================================
DO $$
DECLARE
    n_col    integer;
    n_type   integer;
    n_idx    integer;
    n_left   integer;
BEGIN
    -- ① 列在
    SELECT count(*) INTO n_col
      FROM information_schema.columns
     WHERE table_schema = 'public'
       AND table_name = 'biz_voucher'
       AND column_name = 'source_doc_no';
    IF n_col <> 1 THEN
        RAISE EXCEPTION 'V0357 自检失败：预期 1 列 biz_voucher.source_doc_no，实际 %', n_col;
    END IF;

    -- ② 类型 = varchar(64)（长度写死，防止有人手改成 text）
    SELECT count(*) INTO n_type
      FROM information_schema.columns
     WHERE table_schema = 'public'
       AND table_name = 'biz_voucher'
       AND column_name = 'source_doc_no'
       AND data_type = 'character varying'
       AND character_maximum_length = 64;
    IF n_type <> 1 THEN
        RAISE EXCEPTION 'V0357 自检失败：source_doc_no 必须是 varchar(64)，实际匹配 % 列', n_type;
    END IF;

    -- ③ 索引在
    SELECT count(*) INTO n_idx
      FROM pg_indexes
     WHERE schemaname = 'public'
       AND indexname = 'idx_biz_voucher_source_doc_no';
    IF n_idx <> 1 THEN
        RAISE EXCEPTION 'V0357 自检失败：缺索引 idx_biz_voucher_source_doc_no';
    END IF;

    -- ④ 回填后不该再有「锚定实体前缀 + 36 位 UUID」的摘要残留
    SELECT count(*) INTO n_left
      FROM biz_voucher
     WHERE deleted_at IS NULL
       AND summary ~ '(客户|供应商|物料) [0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}';
    IF n_left > 0 THEN
        RAISE NOTICE 'V0357 提示：仍有 % 张凭证摘要含实体 UUID（主数据已被物理删除 / 名称超长跳过），属已知残留', n_left;
    END IF;

    RAISE NOTICE 'V0357 完成：biz_voucher.source_doc_no varchar(64) + 索引就绪，存量 source_doc_no 与摘要名称已回填';
END $$;
