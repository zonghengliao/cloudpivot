-- ======================================================
-- V0226：ARCH-6 第 0 期数据前置（预算卡 FIN-1 落地）
--
-- 内容（沿用 ARCH-6 §6.6 / §6.9 的口径，号段因 V0220/V0221 已被 P-FLOW/P2-8 占用而顺延）：
--   ① 科目种子 9 条：`2241.01 其他应付款-员工垫付` + `5602.01…5602.09` 管理费用二级科目
--   ② `2241` / `5602` 从「末级」翻成「非末级（仅汇总）」
--   ③ 费用类科目 `aux_dimensions = 'DEPT'`（DC-11 口径：这些科目按部门核算）
--   ④ `sys_config` 三条：预算控制策略默认 WARN / 采购提交是否强制 / 报销付款提醒天数
--
-- ── 为什么必须先翻 is_leaf 再插子科目 ─────────────────────────
-- 会计引擎的 `voucher::account_required` 只允许**末级科目**记账（ARCH-6 §1.3 实测），
-- 而 `5602` / `2241` 现在都是 `is_leaf = 1` 的一级末级科目。直接挂二级会出现
-- 「父科目还是末级」的脏结构：`5602` 本级与 `5602.02` 各自可记账 ⇒ 预算按科目累加会重复计算
-- （ARCH-6 本章红线 3）。所以先翻转，再插入。
--
-- ── 为什么 5602 而不是 6602 ─────────────────────────────────
-- 卡面与 PRD 附录 A.3 写的是企业会计准则编号 `6602 管理费用`，但**库里没有 66xx**
-- （`select count(*) from biz_account where account_code like '66%'` = 0），
-- 实际落库的是小企业会计制度编号 `5602`（`V0183:119`），且利润表只映射 5602
-- （`income_statement.rs` 的 `ADMIN_EXPENSE`）。新加 6602 会让管理费用在利润表里消失。
-- 详见 ARCH-6 §8.1 ①。
--
-- ── 幂等 ───────────────────────────────────────────────
-- 全部 `WHERE NOT EXISTS` / `AND is_leaf = 1` 守卫，重复执行零变更；
-- 末尾 `DO $$` 自检种子与约束，任一不符 → 迁移失败（宁可起不来，也不要种出半成品）。
--
-- ── 回滚 ───────────────────────────────────────────────
-- `V0226__erp_budget_subjects_and_config.down.sql`：删新增子科目、把父科目翻回末级
-- （只在父科目已无子科目时）、清掉 aux_dimensions 与三条 sys_config。
-- ======================================================

-- ---------- ① 科目种子 ----------
-- 其他应付款-员工垫付（报销入账贷方 / 报销付款借方；FIN-3 用它，本卡先备好口径）
INSERT INTO biz_account (account_code, account_name, parent_id, level, account_category,
                         balance_direction, is_leaf, status, aux_dimensions, remark)
SELECT '2241.01', '其他应付款-员工垫付',
       (SELECT id FROM biz_account WHERE account_code = '2241' AND deleted_at IS NULL),
       2, 'LIABILITY', 'CREDIT', 1, 1, 'DEPT',
       'ARCH-6：员工费用报销挂账（报销入账贷方 / 付款借方）'
 WHERE NOT EXISTS (SELECT 1 FROM biz_account WHERE account_code = '2241.01')
   AND EXISTS (SELECT 1 FROM biz_account WHERE account_code = '2241' AND deleted_at IS NULL);

-- 管理费用二级科目（8 条，含折旧费与其他）
INSERT INTO biz_account (account_code, account_name, parent_id, level, account_category,
                         balance_direction, is_leaf, status, aux_dimensions, remark)
SELECT v.code, v.name,
       (SELECT id FROM biz_account WHERE account_code = '5602' AND deleted_at IS NULL),
       2, 'PROFIT_LOSS', 'DEBIT', 1, 1, 'DEPT', v.remark
  FROM (VALUES
        ('5602.01', '管理费用-折旧费',     'ARCH-6：固定资产月度折旧'),
        ('5602.02', '管理费用-差旅费',     'ARCH-6：费用报销-差旅'),
        ('5602.03', '管理费用-办公费',     'ARCH-6：费用报销-办公'),
        ('5602.04', '管理费用-业务招待费', 'ARCH-6：费用报销-招待（进项不可抵扣）'),
        ('5602.05', '管理费用-通讯费',     'ARCH-6：费用报销-通讯'),
        ('5602.06', '管理费用-培训费',     'ARCH-6：费用报销-培训'),
        ('5602.07', '管理费用-职工福利费', 'ARCH-6：费用报销-福利（进项不可抵扣）'),
        ('5602.09', '管理费用-其他',       'ARCH-6：费用报销兜底科目')
       ) AS v(code, name, remark)
 WHERE NOT EXISTS (SELECT 1 FROM biz_account b WHERE b.account_code = v.code)
   AND EXISTS (SELECT 1 FROM biz_account WHERE account_code = '5602' AND deleted_at IS NULL);

-- ---------- ② 父科目翻成非末级（先翻转，理由见文件头） ----------
UPDATE biz_account
   SET is_leaf = 0, updated_at = now()
 WHERE account_code IN ('2241', '5602')
   AND is_leaf = 1
   AND deleted_at IS NULL
   AND EXISTS (
       SELECT 1 FROM biz_account child
        WHERE child.parent_id = biz_account.id AND child.deleted_at IS NULL
   );

-- ---------- ③ 费用类科目按部门辅助核算（DC-11 口径） ----------
UPDATE biz_account
   SET aux_dimensions = 'DEPT', updated_at = now()
 WHERE account_code IN ('5601', '5602', '5603', '5711')
   AND aux_dimensions IS NULL
   AND deleted_at IS NULL;

-- ---------- ④ sys_config 三条 ----------
INSERT INTO sys_config (config_key, config_value, config_type, description, category,
                        sort_order, config_level, is_modifiable)
VALUES
    ('erp.budget.control_mode', 'WARN', 'STRING',
     '预算控制策略：OFF 不校验 / WARN 标记超预算并强制审批（默认）/ BLOCK 直接 409 阻断',
     'erp_budget', 10, 2, TRUE),
    ('erp.budget.enforce_on_po', 'true', 'BOOLEAN',
     '采购单提交时是否做预算校验（控制点②）：true 校验，false 只算采购侧不占用预算',
     'erp_budget', 20, 2, TRUE),
    ('erp.expense.pay_remind_days', '3', 'NUMBER',
     '费用报销待付款提醒：审批通过后第几天提醒出纳付款（FIN-3 使用）',
     'erp_budget', 30, 2, TRUE)
ON CONFLICT (config_key) WHERE deleted_at IS NULL DO NOTHING;

-- ---------- ⑤ 自检（失败即迁移失败 → 启动失败） ----------
DO $$
DECLARE
    parents int;
    leaves  int;
    bad_parent int;
    cfg int;
BEGIN
    -- 5.1 5602 家族结构：1 个父 + 8 个子，且子科目 parent 全部指向 5602
    SELECT count(*) FILTER (WHERE is_leaf = 0),
           count(*) FILTER (WHERE is_leaf = 1),
           count(*) FILTER (WHERE is_leaf = 1
                              AND parent_id IS DISTINCT FROM (SELECT id FROM biz_account WHERE account_code = '5602'))
      INTO parents, leaves, bad_parent
      FROM biz_account WHERE account_code LIKE '5602%' AND deleted_at IS NULL;
    IF parents <> 1 OR leaves <> 8 OR bad_parent <> 0 THEN
        RAISE EXCEPTION 'V0226 自检失败：5602 家族结构不对（父 % / 子 % / 父指向错 %，期望 1/8/0）',
            parents, leaves, bad_parent;
    END IF;

    -- 5.2 2241 家族：父非末级 + 一个子
    IF (SELECT is_leaf FROM biz_account WHERE account_code = '2241') <> 0 THEN
        RAISE EXCEPTION 'V0226 自检失败：2241 仍是末级科目';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM biz_account WHERE account_code = '2241.01' AND is_leaf = 1) THEN
        RAISE EXCEPTION 'V0226 自检失败：2241.01 未种或不是末级';
    END IF;

    -- 5.3 aux_dimensions
    IF EXISTS (SELECT 1 FROM biz_account
                WHERE account_code IN ('5601','5602','5603','5711')
                  AND deleted_at IS NULL AND aux_dimensions IS DISTINCT FROM 'DEPT') THEN
        RAISE EXCEPTION 'V0226 自检失败：费用类科目 aux_dimensions 未标成 DEPT';
    END IF;

    -- 5.4 sys_config
    SELECT count(*) INTO cfg FROM sys_config
     WHERE config_key IN ('erp.budget.control_mode', 'erp.budget.enforce_on_po',
                          'erp.expense.pay_remind_days')
       AND deleted_at IS NULL;
    IF cfg <> 3 THEN
        RAISE EXCEPTION 'V0226 自检失败：预算 sys_config 只种了 % 条（期望 3）', cfg;
    END IF;
    IF (SELECT config_value FROM sys_config WHERE config_key = 'erp.budget.control_mode') <> 'WARN' THEN
        RAISE EXCEPTION 'V0226 自检失败：erp.budget.control_mode 出厂值必须是 WARN（ARCH-6 D3）';
    END IF;
END $$;
