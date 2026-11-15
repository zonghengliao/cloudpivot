-- ======================================================
-- V0232：固定资产的 3 条凭证规则 + 附件类型放宽（ARCH-6 §6.4 / §6.5 / §5.3.3）
--
-- ── ① 凭证规则（3 条）────────────────────────────────────
--   FIXED_ASSET_ACQ_DEFAULT  FIXED_ASSET_ACQ   借 1601 / 贷 2202 / 税 2221.01  （赊购）
--   FIXED_ASSET_ACQ_CASH     FIXED_ASSET_ACQ   借 1601 / 贷 1002 / 税 2221.01  （现付）
--   FIXED_ASSET_ACQ_STOCK    FIXED_ASSET_ACQ   借 1601 / 贷 1405 / 无税        （由收货单转入）
--   DEPRECIATION_DEFAULT     DEPRECIATION      借 5602.01（按部门拆） / 贷 1602
--
--   三条购入规则**共用同一个 `biz_type`**（`FIXED_ASSET_ACQ`），靠 `rule_code`
--   区分 —— 这正是 ARCH-6 §6.4 边界 ② 说的「`create_by_rule` 用 `template_code`
--   显式指定」的场景。设计 §6.4 的表格把第三条写了 `FIXED_ASSET_ACQ_CASH` 作为
--   biz_type，但 §6.4 边界 ② 的原话是「同一 biz_type 的两条规则」，且巡检
--   `biz_type_domain.py` 要求规则的 `biz_type` ∈ `VoucherSourceType` 取值域
--   （`FIXED_ASSET_ACQ_CASH` 不是合法来源类型）⇒ 统一成 `FIXED_ASSET_ACQ`，
--   差异写进证据 README。
--
--   `FIXED_ASSET_ACQ_STOCK` 是设计 §5.3.3 的 `FROM_STOCK` 路径（借 1601 / 贷 1405）
--   所需的规则行 —— §6.4 的 5 条清单里漏了它，但 §5.3.3 明确要求这条分录，
--   不种它这条路径就只能写死科目（失去可配置性）。
--
-- ── ② 附件类型放宽（只放宽，不改存量）──────────────────────
--   `ck_biz_doc_attachment_type`：5 值 → 6 值（+ `FIXED_ASSET`）。
--   与 `doc_kind.rs::ATTACHMENT_DOC_KINDS` **逐字一致**（那边同步加了 FixedAsset）。
--   `EXPENSE_REPORT` 不在本迁移（属 FIN-3，与它的写入口一起加）。
--
-- ── 幂等 ────────────────────────────────────────────────
-- 规则 `WHERE NOT EXISTS`；CHECK 先 DROP IF EXISTS 再 ADD（可重放）。
-- ======================================================

-- ---------- ① 凭证规则 ----------
INSERT INTO biz_voucher_rule (
    rule_code, biz_type, debit_account_id, credit_account_id, tax_account_id, is_active, remark)
SELECT v.rule_code, v.biz_type,
       (SELECT id FROM biz_account WHERE account_code = v.debit_code AND deleted_at IS NULL),
       (SELECT id FROM biz_account WHERE account_code = v.credit_code AND deleted_at IS NULL),
       CASE WHEN v.tax_code IS NULL THEN NULL
            ELSE (SELECT id FROM biz_account WHERE account_code = v.tax_code AND deleted_at IS NULL)
       END,
       1, v.remark
  FROM (VALUES
        ('FIXED_ASSET_ACQ_DEFAULT', 'FIXED_ASSET_ACQ', '1601', '2202', '2221.01',
         '资产购入（赊购）：借 固定资产 / 借 进项税 / 贷 应付账款'),
        ('FIXED_ASSET_ACQ_CASH',    'FIXED_ASSET_ACQ', '1601', '1002', '2221.01',
         '资产购入（现付）：借 固定资产 / 借 进项税 / 贷 银行存款'),
        ('FIXED_ASSET_ACQ_STOCK',   'FIXED_ASSET_ACQ', '1601', '1405', NULL,
         '资产由收货单转入（FROM_STOCK）：借 固定资产 / 贷 库存商品（冲回已入存货的资产）'),
        ('DEPRECIATION_DEFAULT',    'DEPRECIATION',    '5602.01', '1602', NULL,
         '折旧计提：借 管理费用-折旧费（按部门拆行）/ 贷 累计折旧')
       ) AS v(rule_code, biz_type, debit_code, credit_code, tax_code, remark)
 WHERE NOT EXISTS (SELECT 1 FROM biz_voucher_rule r
                    WHERE r.rule_code = v.rule_code AND r.deleted_at IS NULL);

-- ---------- ② 附件类型 5 → 6（+ FIXED_ASSET） ----------
ALTER TABLE biz_doc_attachment DROP CONSTRAINT IF EXISTS ck_biz_doc_attachment_type;
ALTER TABLE biz_doc_attachment
    ADD CONSTRAINT ck_biz_doc_attachment_type
    CHECK (doc_type IN ('CONTRACT','PURCHASE_ORDER','STOCK_IN','PAYABLE','PAYMENT','FIXED_ASSET'));

-- ---------- ③ 自检 ----------
DO $$
DECLARE
    n int;
    v_missing text;
BEGIN
    -- 3.1 四条规则齐全（两条购入 + 一条转固 + 一条折旧）
    SELECT count(*) INTO n FROM biz_voucher_rule
     WHERE rule_code IN ('FIXED_ASSET_ACQ_DEFAULT','FIXED_ASSET_ACQ_CASH',
                         'FIXED_ASSET_ACQ_STOCK','DEPRECIATION_DEFAULT')
       AND is_active = 1 AND deleted_at IS NULL;
    IF n <> 4 THEN
        RAISE EXCEPTION 'V0232 自检失败：固定资产凭证规则只种了 % 条（期望 4）', n;
    END IF;

    -- 3.2 四条规则的三个科目都必须解析到（科目缺失 → 规则是空壳）
    SELECT string_agg(v.rule_code, ',') INTO v_missing
      FROM (VALUES ('FIXED_ASSET_ACQ_DEFAULT'),('FIXED_ASSET_ACQ_CASH'),
                   ('FIXED_ASSET_ACQ_STOCK'),('DEPRECIATION_DEFAULT')) AS v(rule_code)
     WHERE EXISTS (SELECT 1 FROM biz_voucher_rule r
                    WHERE r.rule_code = v.rule_code
                      AND (r.debit_account_id IS NULL OR r.credit_account_id IS NULL));
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0232 自检失败：规则 % 的借/贷科目解析为空（科目种子缺失）', v_missing;
    END IF;

    -- 3.3 附件 CHECK 必须已放宽到 6 值
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_biz_doc_attachment_type'
           AND pg_get_constraintdef(oid) LIKE '%FIXED_ASSET%'
    ) THEN
        RAISE EXCEPTION 'V0232 自检失败：ck_biz_doc_attachment_type 未放宽到含 FIXED_ASSET';
    END IF;
END $$;
