-- ============================================================================
-- V0445：销售返利的「其他应付款-销售返利」末级科目 —— 从被佣金占用的 2241.02 里分出来
--
-- 卡：t_627d2a4b（FIX【M3·销售】返利红字凭证贷记错科目：V0440 的 2241.02 被佣金占用）
-- 缺陷来源：t_8c53043e（M3-19F 前端入口）验收时在真库实测发现，按卡面「发现缺陷另立卡」另立。
--
-- ── 病在哪（三个条件叠加，每个库都会中，含全新空库首启）──────────────────
--   ① `V0311__commission_settle_setup`（佣金结算，SO-36）先种了
--      `2241.02 其他应付款-佣金`；
--   ② `V0440__sales_rebate_invoice` 第 173-180 行以为 `2241.02` 是空号：
--      `INSERT … WHERE NOT EXISTS (SELECT 1 FROM biz_account WHERE account_code = '2241.02')`
--      —— 在 V0311 之后必然 EXISTS，**整条种子静默跳过**（迁移不报错、不提示）；
--   ③ V0440 的自检（同文件 291-300 行）只验「存在 + 末级 + 父为 2241 + 负债/贷方」，
--      **不验名称** ⇒ 自检照样通过。
--   结果：`rebate_invoice_handler::ACCOUNT_REBATE_PAYABLE = "2241.02"` 落到佣金科目上，
--   `voucher::account_required` 只按 code 取行、名称不参与校验 ⇒ 返利红字凭证贷记的是
--   「其他应付款-佣金」（145 演练库实测：PZ-20261117-000001 第 3 行 credit 1000.00 落
--   `2241.02 其他应付款-佣金`）。金额与借贷平衡都对，错的是**科目口径**：
--   返利负债与销售佣金挤在同一个末级科目，总账 / 往来对账 / 报表附注分不开
--   「欠客户的返利」与「欠销售的佣金」。
--
-- ── 为什么是 2241.05（不是卡面举例的 2241.03）──────────────────────────
--   `2241.03 其他应付款-社保(个人)` / `2241.04 其他应付款-公积金(个人)` 早在
--   `V0384__hr_accounts` 就种下了（144 prod 与 145 dev 实测一致：
--   `select account_code, account_name from biz_account where account_code like '2241%'`
--   = 2241 / 2241.01 员工垫付 / 2241.02 佣金 / 2241.03 社保(个人) / 2241.04 公积金(个人)）。
--   卡面写「如 2241.03」只是举例，硬套会撞 V0384 的既有科目（`ON CONFLICT` 静默跳过 ⇒
--   又是同一个病）。所以取下一个空号 **2241.05**。
--
-- ── 为什么不动 2241.02 的名称 ──────────────────────────────────────────
--   `V0311` / `commission_handler`（佣金计提 借 5601 / 贷 2241.02、发放 借 2241.02 / 贷 1002）
--   在用这条科目。把 2241.02 改叫「销售返利」会让佣金结算整条链记到错的科目上 ——
--   同一个 bug 换个人受害。返利必须**另开末级**，两条业务各占一个末级科目。
--
-- ── 已有环境里的历史返利凭证：**不追溯**（口径取舍，写在这里留痕）──────────
--   本迁移**不**自动补「借 2241.02 / 贷 2241.05」的调整凭证，理由三条：
--     ① 生产（144）实测：`biz_sales_rebate_*` 表根本不存在（V0440 还没跑到 144），
--        历史上**没有**任何真实返利凭证被记到佣金科目上（见 evidence/ 的 `00_prod_precheck.txt`）；
--     ② 中招的只有 99/145 的演练库与开发库，那些凭证是夹具造的测试数据，给夹具补调整凭证
--        等于往练习环境里造假账（还会挡住后续 e2e 的「凭证张数」断言）；
--     ③ 调整凭证是**业务动作**（要选开账期间、要取凭证号、要有制单人、要能被红冲），
--        塞进 schema 迁移既不幂等也没法审计。真出现「生产上有历史错票」时按业务路径补：
--        `docs/evidence/2026-11-17-m3-19f-rebate-invoice/12_history_adjust.sql` 给了现成的
--        「借贷同额、借 2241.02 / 贷 2241.05」调整凭证 SQL（手工评审后执行，不在迁移里跑）。
--   即：**新开票走 2241.05；历史票不追溯，留在原科目上，由上面那份 SQL 在真需要时人工调账。**
--
-- ── 幂等 / 可回滚 ──────────────────────────────────────────────────
--   · 种子用 `ON CONFLICT (account_code) DO NOTHING`，重复执行不报错、不改已有行；
--   · 但**不许静默跳过**（这正是 V0440 的病）：同码异名 / 同名异码 一律 RAISE 出去，
--     让人看到「这个码被别的科目占了」而不是继续写错科目；
--   · down 只在「没有任何凭证分录用过 2241.05」时允许删，且只删本迁移种的那一行
--     （按 remark 认领），删不到就静默通过。
-- ============================================================================

-- ---------------------------------------------------------------------------
-- ① 前置断言：父科目 2241 必须在（2241 自 V0226 起是非末级汇总科目）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    v_parent uuid;
BEGIN
    SELECT id INTO v_parent
      FROM biz_account
     WHERE account_code = '2241' AND deleted_at IS NULL;

    IF v_parent IS NULL THEN
        RAISE EXCEPTION 'V0445 前置条件不满足：科目 2241（其他应付款）不存在 —— '
            '返利应付要挂在它下面，请先确认 V0226 的科目种子跑过了';
    END IF;
END $$;

-- ---------------------------------------------------------------------------
-- ② 反面教材护栏：不许再犯「静默跳过」
--
--    a) `2241.05` 已被**别的**科目占用（名称不同）⇒ 撞号了，停下来让人换码；
--    b) 已经有**同名**科目挂在别处 ⇒ 有人手工修过 / 别的迁移种过，
--       直接再种一条就会出现两个「其他应付款-销售返利」（对账时分不清哪条是真的）。
--   两种情况都 RAISE：迁移宁可红，也不要又一次「看起来成功」。
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    v_code text;
    v_name text;
BEGIN
    SELECT account_name INTO v_name
      FROM biz_account
     WHERE account_code = '2241.05';

    IF FOUND AND v_name <> '其他应付款-销售返利' THEN
        RAISE EXCEPTION 'V0445 撞号：科目 2241.05 已被「%」占用（本迁移要种的是「其他应付款-销售返利」）。'
            '请挑下一个空号并同步改 V0445 / rebate_invoice_handler::ACCOUNT_REBATE_PAYABLE', v_name;
    END IF;

    SELECT string_agg(account_code, ', ' ORDER BY account_code) INTO v_code
      FROM biz_account
     WHERE account_name = '其他应付款-销售返利'
       AND account_code <> '2241.05'
       AND deleted_at IS NULL;

    IF v_code IS NOT NULL THEN
        RAISE EXCEPTION 'V0445 断链：科目「其他应付款-销售返利」已存在但编码是 %（不是 2241.05）。'
            '同一口径挂两条会把返利负债拆成两半 —— 请人工确认保留哪一条（要么删掉手工那条，'
            '要么改本迁移与 ACCOUNT_REBATE_PAYABLE 去用它）', v_code;
    END IF;
END $$;

-- ---------------------------------------------------------------------------
-- ③ 种末级科目 `2241.05 其他应付款-销售返利`
--
--   · 级次 = 父级级次 + 1（不写死 2：客户若调过 2241 的级次也不会造出「级次错位」的树）；
--   · is_leaf = 1（可挂分录）、status = 1（启用）、负债 / 贷方；
--   · aux_dimensions = NULL（客户维度由业务路径自己决定，本仓 `docs/ERP-辅助核算口径.md` §2）；
--   · remark 是本迁移的「认领标记」，down 靠它识别哪一行是自己种的。
-- ---------------------------------------------------------------------------
INSERT INTO biz_account (account_code, account_name, parent_id, level,
                         account_category, balance_direction, is_leaf, status,
                         aux_dimensions, remark)
SELECT '2241.05', '其他应付款-销售返利', p.id, p.level + 1,
       'LIABILITY', 'CREDIT', 1, 1,
       NULL,
       'V0445：销售返利红字凭证的贷方（欠客户的返利负债）；返利付款时借记本科目'
  FROM biz_account p
 WHERE p.account_code = '2241' AND p.deleted_at IS NULL
ON CONFLICT (account_code) DO NOTHING;

-- ---------------------------------------------------------------------------
-- ④ 父科目 2241 必须保持非末级（挂着子科目的汇总科目若还是末级，就是裂树：
--    记账闸当它可记账、报表又当它汇总，同一笔钱两头算）
-- ---------------------------------------------------------------------------
UPDATE biz_account
   SET is_leaf = 0,
       updated_at = CURRENT_TIMESTAMP
 WHERE account_code = '2241'
   AND deleted_at IS NULL
   AND is_leaf <> 0;

-- ---------------------------------------------------------------------------
-- ⑤ 自检：本迁移要的东西必须**逐条**对上（含 V0440 当年漏掉的**名称**）
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    v_cnt      integer;
    v_name     text;
    v_level    integer;
    v_leaf     integer;
    v_status   integer;
    v_category text;
    v_direction text;
    v_parent   text;
    v_children integer;
    v_commission text;
BEGIN
    -- 5.1 存在且只有一条存活行
    SELECT count(*) INTO v_cnt
      FROM biz_account
     WHERE account_code = '2241.05' AND deleted_at IS NULL;
    IF v_cnt <> 1 THEN
        RAISE EXCEPTION 'V0445 自检失败：科目 2241.05 存活行应有 1 条，实得 % 条', v_cnt;
    END IF;

    -- 5.2 名称逐字（**V0440 的教训：不验名称等于没验**）
    SELECT a.account_name, a.level, a.is_leaf, a.status,
           a.account_category, a.balance_direction, p.account_code
      INTO v_name, v_level, v_leaf, v_status, v_category, v_direction, v_parent
      FROM biz_account a
      JOIN biz_account p ON p.id = a.parent_id
     WHERE a.account_code = '2241.05' AND a.deleted_at IS NULL;

    IF v_name <> '其他应付款-销售返利' THEN
        RAISE EXCEPTION 'V0445 自检失败：2241.05 的名称是「%」，应为「其他应付款-销售返利」', v_name;
    END IF;
    IF v_parent <> '2241' THEN
        RAISE EXCEPTION 'V0445 自检失败：2241.05 的父科目是 %，应为 2241', v_parent;
    END IF;
    IF v_leaf <> 1 THEN
        RAISE EXCEPTION 'V0445 自检失败：2241.05 不是末级科目（is_leaf=%），非末级不许记账', v_leaf;
    END IF;
    IF v_status <> 1 THEN
        RAISE EXCEPTION 'V0445 自检失败：2241.05 未启用（status=%）', v_status;
    END IF;
    IF v_category <> 'LIABILITY' OR v_direction <> 'CREDIT' THEN
        RAISE EXCEPTION 'V0445 自检失败：2241.05 应为负债/贷方，实得 %/%', v_category, v_direction;
    END IF;
    IF v_level <> 2 THEN
        RAISE EXCEPTION 'V0445 自检失败：2241.05 的级次是 %，应为 2（父级 1 + 1）', v_level;
    END IF;

    -- 5.3 末级位与子科目一致（没有存活子科目）
    SELECT count(*) INTO v_children
      FROM biz_account c
      JOIN biz_account a ON a.id = c.parent_id
     WHERE a.account_code = '2241.05' AND c.deleted_at IS NULL;
    IF v_children <> 0 THEN
        RAISE EXCEPTION 'V0445 自检失败：2241.05 下挂了 % 个子科目，它不是末级', v_children;
    END IF;

    -- 5.4 父科目 2241 是非末级（否则裂树）
    SELECT is_leaf INTO v_leaf
      FROM biz_account
     WHERE account_code = '2241' AND deleted_at IS NULL;
    IF v_leaf <> 0 THEN
        RAISE EXCEPTION 'V0445 自检失败：父科目 2241 仍是末级（is_leaf=%），但它已挂子科目', v_leaf;
    END IF;

    -- 5.5 **不得动到佣金科目**：2241.02 必须还是「其他应付款-佣金」
    --     （V0311 / commission_handler 在用它；它被改名就等于把 bug 挪给了佣金）
    SELECT account_name INTO v_commission
      FROM biz_account
     WHERE account_code = '2241.02' AND deleted_at IS NULL;
    IF v_commission IS NULL THEN
        RAISE EXCEPTION 'V0445 自检失败：佣金科目 2241.02 不见了（V0311 种的）';
    END IF;
    IF v_commission <> '其他应付款-佣金' THEN
        RAISE EXCEPTION 'V0445 自检失败：2241.02 的名称被改成了「%」，应为「其他应付款-佣金」'
            '（佣金结算在用它，返利另开了 2241.05）', v_commission;
    END IF;

    RAISE NOTICE 'V0445 完成：2241.05 其他应付款-销售返利 就位（末级/负债/贷方/名称逐字核对），'
        '2241.02 其他应付款-佣金 未被改动；历史返利凭证不追溯（见文件头「口径取舍」）';
END $$;

-- ---------------------------------------------------------------------------
-- ⑥ 本库历史口径提示（不阻断迁移，只把事实打到日志里）
--    有历史返利凭证仍贷记 2241.02 的库（99/145 的演练库）会在这里看到条数。
-- ---------------------------------------------------------------------------
DO $$
DECLARE
    n integer;
BEGIN
    SELECT count(*) INTO n
      FROM biz_voucher_entry e
      JOIN biz_account a ON a.id = e.account_id
     WHERE a.account_code = '2241.02' AND e.credit_amount > 0;

    IF n > 0 THEN
        RAISE NOTICE 'V0445 提示：本库 2241.02 上还有 % 条贷方分录（佣金 + 可能的返利历史错票）。'
            '历史票不追溯；若确有真实返利错票需要调账，按 docs/evidence/'
            '2026-11-17-m3-19f-rebate-invoice/12_history_adjust.sql 人工补调整凭证', n;
    END IF;
END $$;
