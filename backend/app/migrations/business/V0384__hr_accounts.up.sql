-- ============================================================================
-- V0384：人力资源工资相关的 9 个末级科目 + 父科目翻非末级 + 2 条凭证规则
--        （ARCH-9-2 §3.2 ① / §3.2 ⑧ / §4.6，逐字口径）
--
-- 卡：t_53922091（HR-02【HR】迁移 V0379）—— 拆卡清单 §8.1 HR-02
-- 依据：docs/architecture/ARCH-9-人力资源模块设计.md
--       §3.2 ①（9 科目 SQL，真库跑通）、§3.2 ⑧（2 条 voucher_rule）、
--       §4.6（工资凭证勾稽：借 5602.1x / 贷 2211.0x，双侧带部门辅助核算）、
--       §7.1（迁移清单 V0379 → V0384）、§7.2（翻末级前查历史分录）、§7.3（回滚方案）
--
-- # 号段：卡面写 V0379，实际落 V0384
--   `V0378~V0382` 已被成本核算实占并已提交（`V0378__cost_menu_and_perm` …
--   `V0382__stock_in_production_sourceless`）。nik 迁移加载器对版本号做
--   **跨目录全局唯一**校验，重号 ⇒ 服务启动直接失败（AGENTS 约定 6）。
--   HR 全系列整体后移 5 号：`V0378→V0383`、`V0379→V0384`、…、`V0388→V0393`，
--   映射已回填设计 §7.1 / §8.1，也写在 `V0383__hr_tables.up.sql` 头注释里。
--
-- # 本迁移只做三件事（只动 `biz_account` 的科目树 + `biz_voucher_rule` 的两行口径登记）
--   ① 种 9 个末级科目：`2211.01~03` / `2221.03` / `2241.03~04` / `5602.10~12`；
--   ② 4 个父科目（`2211` / `2221` / `2241` / `5602`）**确实有未删子科目且当前仍是末级**
--      ⇒ 翻 `is_leaf = 0`；
--   ③ 种 2 条凭证规则 `PAYROLL_ACCRUAL_DEFAULT` / `PAYROLL_PAYMENT_DEFAULT`
--      （**只做口径登记与配置页可见，不驱动分录**，见 §3.2 ⑧ 末段）。
--   末尾自检：形态（9 科目逐条 / 4 父科目 / 无裂树 / 父科目零分录）
--            + 口径（2 条规则都解析到科目 / 报表项前缀仍在 / 本迁移不写分录）。
--
-- # 为什么要翻末级（不是洁癖）
--   `voucher::post_voucher` 的「记账科目口径」闸只允许**末级**科目
--   （`voucher_multi::ensure_leaf` / `voucher::non_leaf_codes`：`is_leaf = 0` 直接 400 拒绝）。
--   工资凭证要落 `2211.01~03`（欠员工 / 欠社保中心 / 欠公积金中心三种负债必须分开，
--   否则回答不了"到底欠谁"），所以父科目必须先变成汇总级。
--   翻了之后闸**更严**：`2211` / `5602` 等不能再直接记账，只能落末级子科目 —— 这正是 §4.6 要的。
--
-- # ★ 翻末级前必须查历史分录（卡面验收项之一）
--   `biz_voucher_entry` 存的是**科目编码快照**（`account_code` / `account_name`），
--   翻成非末级不会让历史分录消失，但会让这些行变成「记在汇总科目上的分录」：
--   报表按末级取数、年结按末级结转，这些金额会被漏掉，而新叶子期初为 0 ⇒ 账实不平、
--   年结试算差钱。所以父科目上只要还有分录，本迁移就 `RAISE EXCEPTION` **拒绝翻**（§7.2）。
--   口径：**只拦还需要翻的父科目**（`is_leaf = 1`）—— 已经是 0 说明本迁移（或 V0377）跑过，
--   重复执行不该因历史分录误报红。软删凭证的分录也计入（保守读法：红冲/反审核后分录仍在表里，
--   年结与报表重算会重新看到它们）。
--
-- # 与 V0377（成本核算）的 `2211.01` 幂等共存（设计 D-14）
--   V0377 的版本号更小、必然先跑：它已种 `2211.01` 并把 `2211` 翻成非末级。
--   本迁移对 9 个编码统一 `ON CONFLICT (account_code) DO NOTHING`：已存在就**复用**，
--   不覆盖（客户的改名 / 改父级属于科目维护，迁移不替人做主）。
--
--   ★ 唯一需要"补写"的一列是 `aux_dimensions`：V0377 的 INSERT 没有这一列
--     （`2211.01` 落库为 `NULL`），而 §4.6.2 硬规则 3 要求 `2211.0x` / `5602.1x`
--     **带部门辅助核算**（既能出"全公司工资"，也能出"研发部工资"）。
--     故对「这 6 个编码 + `aux_dimensions IS NULL`」的行补写 `'DEPT'`；
--     客户已配了别的维度就不动（`WHERE aux_dimensions IS NULL`）。
--     安全性：辅助核算的**必填校验只跑在手工凭证写入路径**
--     （`voucher_write::check_aux_dimensions` 仅被 `manual_in_tx` 调用），
--     成本结算的人工计提凭证走 `post_explicit_lines`、不校验 aux
--     ⇒ 给 `2211.01` 补 `DEPT` 不会让成本模块的「贷 2211.01」失效。
--
-- # 幂等（重复执行结果不变）
--   · 科目 INSERT：`ON CONFLICT (account_code) DO NOTHING`（`uk_biz_account_code` 是全表唯一索引）
--   · 翻末级 UPDATE：带 `AND p.is_leaf = 1` 与 `EXISTS (未删子科目)` ⇒ 第二遍 0 行
--   · aux 补写：带 `AND aux_dimensions IS NULL` ⇒ 第二遍 0 行
--   · 规则 INSERT：带 `WHERE NOT EXISTS (rule_code 且未删)` ⇒ 第二遍 0 行
--   · 「编码已被占用」的前置检查只拦**形态不符**（软删 / 非末级 / 父级或级次不对），
--     形态正确就直接复用 ⇒ 重复执行不报错、不重复种。
--
-- # 科目名 / 类别不参与硬断言的部分
--   编码 / 父级 / 级次 / 末级位 / 启用位是**结构**，一处不符就得拦住（否则记账闸与报表口径错）。
--   科目名是**显示**：客户可能把「管理费用-工资」改成「管理费用-薪酬」，这不该挡住升级，
--   但必须看得见 ⇒ 名称不符只 `RAISE NOTICE`，不 `RAISE EXCEPTION`。
--   类别 / 余额方向：本迁移按设计稿固定写 `LIABILITY/CREDIT`（2211/2221/2241）与
--   `PROFIT_LOSS/DEBIT`（5602），并断言落库值与设计一致；父科目若被客户改过类别，
--   只 `RAISE NOTICE` 点名（不因此挡住升级，但让人知道树形与客户改动不一致）。
--
-- # ★ 与 HR-06 的顺序依赖（本卡收不了口，已上报）
--   2 条规则的 `biz_type` 按设计是 `PAYROLL_ACCRUAL` / `PAYROLL_PAYMENT`。
--   `scripts/verify/biz_type_domain.py` 按 `doc_kind.rs` 的 `VOUCHER_RULE_BIZ_TYPE_VALUES`
--   查库：这两个值要由 **HR-06**（`doc_kind.rs` +2 `VoucherSourceType` + 交叉映射 + `COLUMN_DOMAINS`）
--   加进词表后才在域内。在 HR-06 落地前跑该巡检，会报这两条「域外值」——
--   属**已知的顺序依赖**，不是本迁移写错（把 biz_type 改成别的值反而违反 §4.6 与 HR-39 的落库口径）。
--   本卡不改 `doc_kind.rs`：那是 HR-06 的范围（卡面纪律：别的模块只报不动）。
--
-- # 回滚（down）
--   见 `.down.sql`：有分录拒绝 → 删 2 规则 → 删本迁移种的科目（`2211.01` 若归 V0377 则保留）
--   → 父科目仅在没有子科目时翻回末级 → 反向自检。
-- ============================================================================

-- ============================================================================
-- 前置护栏：表在 / 4 个父科目在且未软删 / ☆父科目历史分录为 0 / 9 个编码形态 / 规则号未被占
-- ============================================================================
DO $$
DECLARE
    v_missing   TEXT;
    v_deleted   TEXT;
    v_blocked   TEXT;
    v_conflict  TEXT;
    v_rule_bad  TEXT;
    v_entries   BIGINT;
    rec         RECORD;
BEGIN
    -- ⓪ 依赖表必须都在（否则下面的护栏会把「表不存在」报成看不懂的错）
    IF to_regclass('public.biz_account') IS NULL
       OR to_regclass('public.biz_voucher_rule') IS NULL
       OR to_regclass('public.biz_voucher_entry') IS NULL THEN
        RAISE EXCEPTION 'V0384 拒绝执行：biz_account / biz_voucher_rule / biz_voucher_entry 不齐'
            '（科目表 V0137、凭证三表 V0149）。本迁移只往既有表里种数据，不建表 —— '
            '请先确认这些迁移已执行到本版本之前';
    END IF;

    -- ① 4 个父科目 + 1002 银行存款（PAYROLL_PAYMENT 的贷方）必须存在（V0183 / V0149 种的一级科目）
    SELECT string_agg(t.code, ' / ') INTO v_missing
      FROM (VALUES ('2211'), ('2221'), ('2241'), ('5602'), ('1002')) AS t(code)
     WHERE NOT EXISTS (SELECT 1 FROM biz_account a WHERE a.account_code = t.code);
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0384 拒绝执行：科目 % 不存在。本迁移只往 V0137/V0183/V0149 已种的'
            '科目树上挂末级科目，不新建一级科目 —— 请先确认科目表初始化已完成', v_missing;
    END IF;

    -- ② 软删的父科目：翻它等于翻一个界面上看不见的科目（种出来的子科目也没人能用）
    SELECT string_agg(t.code, ' / ') INTO v_deleted
      FROM (VALUES ('2211'), ('2221'), ('2241'), ('5602')) AS t(code)
      JOIN biz_account a ON a.account_code = t.code
     WHERE a.deleted_at IS NOT NULL;
    IF v_deleted IS NOT NULL THEN
        RAISE EXCEPTION 'V0384 拒绝执行：父科目 % 已被软删除。请在「会计科目」里恢复它们'
            '（或先确认这些科目确实不用了）再执行本迁移', v_deleted;
    END IF;

    -- ③ ★ 卡面验收：父科目历史分录非 0 ⇒ 不许翻（§7.2）
    --    只看「还需要翻」的那些（is_leaf = 1）：已经是非末级说明翻过了，不必再拦
    --    （否则重复执行会因历史分录误报红）。
    v_blocked := NULL;
    FOR rec IN
        SELECT t.code AS code, a.is_leaf AS leaf_flag
          FROM (VALUES ('2211'), ('2221'), ('2241'), ('5602')) AS t(code)
          JOIN biz_account a ON a.account_code = t.code AND a.deleted_at IS NULL
         ORDER BY t.code
    LOOP
        CONTINUE WHEN rec.leaf_flag <> 1;
        SELECT count(*) INTO v_entries FROM biz_voucher_entry e WHERE e.account_code = rec.code;
        IF v_entries > 0 THEN
            v_blocked := concat_ws(' / ', v_blocked, format('%s 有 %s 条分录', rec.code, v_entries));
        END IF;
    END LOOP;
    IF v_blocked IS NOT NULL THEN
        RAISE EXCEPTION 'V0384 拒绝执行：% —— 这些汇总科目还有历史分录，翻成非末级后这些金额会'
            '记在汇总科目上（报表按末级取数、年结按末级结转，会被漏结 ⇒ 跨年试算不平）。'
            '请先把这些分录改记到末级科目（工资类先落到 2211.01~03 / 5602.10~12）'
            '或把它们红冲/作废后再执行本迁移', v_blocked;
    END IF;

    -- ④ 9 个末级编码若已存在：形态必须正确（软删 / 非末级 / 父级不对 / 级次不对都拒绝）
    SELECT string_agg(format('%s(%s)', x.code, x.why), ' / ') INTO v_conflict
      FROM (
        SELECT l.code,
               CASE WHEN a.deleted_at IS NOT NULL THEN '已软删'
                    WHEN a.is_leaf <> 1 THEN '不是末级'
                    WHEN a.level <> p.level + 1 THEN 'level=' || a.level || '（应为 ' || (p.level + 1) || '）'
                    ELSE '父级=' || coalesce(p.account_code, '<空>') || '（应为 ' || l.parent_code || '）'
               END AS why
          FROM (VALUES
                 ('2211.01', '2211'), ('2211.02', '2211'), ('2211.03', '2211'),
                 ('2221.03', '2221'),
                 ('2241.03', '2241'), ('2241.04', '2241'),
                 ('5602.10', '5602'), ('5602.11', '5602'), ('5602.12', '5602')
               ) AS l(code, parent_code)
          JOIN biz_account a ON a.account_code = l.code
          LEFT JOIN biz_account p ON p.id = a.parent_id
         WHERE a.deleted_at IS NOT NULL
            OR a.is_leaf <> 1
            OR a.level <> p.level + 1
            OR p.account_code IS DISTINCT FROM l.parent_code
      ) x;
    IF v_conflict IS NOT NULL THEN
        RAISE EXCEPTION 'V0384 拒绝执行：末级科目编码已被占用且形态不符 —— %。'
            '本迁移不改已有的科目行（改名 / 改父级 / 改末级位属于科目维护，得人来定），'
            '请人工核对后在「会计科目」里修正这些行，再重跑本迁移', v_conflict;
    END IF;

    -- ⑤ 2 条规则号若已存在：biz_type 必须与设计一致（同号不同 biz_type = 有人占了这个号）
    SELECT string_agg(format('%s(biz_type=%s，应为 %s)', r.rule_code, r.biz_type, v.biz), ' / ')
      INTO v_rule_bad
      FROM (VALUES ('PAYROLL_ACCRUAL_DEFAULT', 'PAYROLL_ACCRUAL'),
                   ('PAYROLL_PAYMENT_DEFAULT', 'PAYROLL_PAYMENT')) AS v(rule, biz)
      JOIN biz_voucher_rule r ON r.rule_code = v.rule AND r.deleted_at IS NULL
     WHERE r.biz_type <> v.biz;
    IF v_rule_bad IS NOT NULL THEN
        RAISE EXCEPTION 'V0384 拒绝执行：凭证规则号已被占用且 biz_type 不符 —— %。'
            '工资凭证的落库口径是 source_type = PAYROLL_ACCRUAL / PAYROLL_PAYMENT（§4.6.1），'
            '同号不同 biz_type 会让 HR-39 过账时的规则解析走到别的业务上，请人工核对后修正', v_rule_bad;
    END IF;

    RAISE NOTICE 'V0384 前置护栏通过：依赖表齐、4 父科目在且未软删、历史分录为 0、'
        '9 个末级编码与 2 个规则号无冲突';
END
$$;

-- ============================================================================
-- ① 种 9 个末级科目（编码 / 名称 / 类别 / 方向 / 辅助核算与设计 §3.2 ① 逐字一致）
--
--   · 父级由 `parent_id` 关联解析（不写死 UUID）；
--   · 级次 = 父级级次 + 1（父级都是 1 级 ⇒ 子科目 2 级，与设计稿字面值 2 等价，
--     但客户若调过父级级次也不会造出「级次错位」的树）；
--   · `status = 1`（启用）、`is_leaf = 1`（可记账）；
--   · `remark` 写播种来源（house style：V0149/V0158/V0160/V0377 的补种科目都带），
--     down 靠它识别「哪一行是本迁移种的」。
-- ============================================================================
INSERT INTO biz_account (account_code, account_name, parent_id, level,
                         account_category, balance_direction, is_leaf, status,
                         aux_dimensions, remark)
SELECT v.code,
       v.name,
       p.id,
       p.level + 1,
       v.category,
       v.direction,
       1,
       1,
       v.aux,
       'ARCH-9-2 工资相关科目'
  FROM (VALUES
        ('2211.01', '应付职工薪酬-工资',         '2211', 'LIABILITY',   'CREDIT', 'DEPT'),
        ('2211.02', '应付职工薪酬-社保(单位)',   '2211', 'LIABILITY',   'CREDIT', 'DEPT'),
        ('2211.03', '应付职工薪酬-公积金(单位)', '2211', 'LIABILITY',   'CREDIT', 'DEPT'),
        ('2221.03', '应交税费-应交个人所得税',   '2221', 'LIABILITY',   'CREDIT', NULL),
        ('2241.03', '其他应付款-社保(个人)',     '2241', 'LIABILITY',   'CREDIT', NULL),
        ('2241.04', '其他应付款-公积金(个人)',   '2241', 'LIABILITY',   'CREDIT', NULL),
        ('5602.10', '管理费用-工资',             '5602', 'PROFIT_LOSS', 'DEBIT',  'DEPT'),
        ('5602.11', '管理费用-社保(单位)',       '5602', 'PROFIT_LOSS', 'DEBIT',  'DEPT'),
        ('5602.12', '管理费用-公积金(单位)',     '5602', 'PROFIT_LOSS', 'DEBIT',  'DEPT')
       ) AS v(code, name, parent_code, category, direction, aux)
  JOIN biz_account p ON p.account_code = v.parent_code AND p.deleted_at IS NULL
ON CONFLICT (account_code) DO NOTHING;   -- 幂等：已存在（含 V0377 早种的 2211.01）就复用

-- ============================================================================
-- ② 父科目翻非末级（幂等：只翻「确实有未删子科目」且当前还是末级的）
--
--   §7.5 实测教训：设计第一版只翻 2211，结果老库里 2241 / 5602 挂着子科目却仍是末级
--   ⇒ 造出「末级科目挂子科目」的裂树（记账闸当它可记账、报表又当它汇总，同一笔钱两头算）。
--   所以条件里必须有 `EXISTS (未删子科目)`，而不是只看编码在不在名单里。
-- ============================================================================
UPDATE biz_account p
   SET is_leaf    = 0,
       updated_at = CURRENT_TIMESTAMP
 WHERE p.account_code IN ('2211', '2221', '2241', '5602')
   AND p.deleted_at IS NULL
   AND p.is_leaf = 1
   AND EXISTS (SELECT 1 FROM biz_account c WHERE c.parent_id = p.id AND c.deleted_at IS NULL);

-- ============================================================================
-- ③ 补写部门辅助核算（§4.6.2 硬规则 3）
--
--   只为「本迁移关心的 6 个编码 + aux_dimensions IS NULL」的行补 'DEPT'：
--   V0377 早种的 2211.01 落库时没有这一列（NULL），不补的话工资凭证的贷方分录
--   落不进部门辅助账（`GET /ledger/aux-balance?aux_type=DEPT` 永远查不到研发部工资）。
--   幂等：`aux_dimensions IS NULL` ⇒ 第二遍 0 行；客户已配别的维度就不动。
-- ============================================================================
UPDATE biz_account
   SET aux_dimensions = 'DEPT',
       updated_at     = CURRENT_TIMESTAMP
 WHERE account_code IN ('2211.01', '2211.02', '2211.03',
                        '5602.10', '5602.11', '5602.12')
   AND deleted_at IS NULL
   AND aux_dimensions IS NULL;

-- ============================================================================
-- ④ 种 2 条凭证规则（设计 §3.2 ⑧ 逐字：只做口径登记，不驱动分录）
--
--   科目 id 用 `LEFT JOIN biz_account` 解析（科目缺失会落 NULL，由自检拦下并点名）；
--   幂等：`WHERE NOT EXISTS (rule_code 且未删)`（`uk_biz_voucher_rule_code` 也是软删唯一）。
-- ============================================================================
INSERT INTO biz_voucher_rule (rule_code, biz_type, debit_account_id, credit_account_id,
                              tax_account_id, is_active, remark)
SELECT v.rule, v.biz, d.id, c.id, NULL, 1, v.remark
  FROM (VALUES
        ('PAYROLL_ACCRUAL_DEFAULT', 'PAYROLL_ACCRUAL', '5602.10', '2211.01',
         'ARCH-9-2 工资计提：借 管理费用-工资 / 贷 应付职工薪酬-工资（多借方按部门展开）'),
        ('PAYROLL_PAYMENT_DEFAULT', 'PAYROLL_PAYMENT', '2211.01', '1002',
         'ARCH-9-2 工资发放：借 应付职工薪酬-工资 / 贷 银行存款（+代扣项贷方）')
       ) AS v(rule, biz, debit, credit, remark)
  LEFT JOIN biz_account d ON d.account_code = v.debit  AND d.deleted_at IS NULL
  LEFT JOIN biz_account c ON c.account_code = v.credit AND c.deleted_at IS NULL
 WHERE NOT EXISTS (SELECT 1 FROM biz_voucher_rule r
                    WHERE r.rule_code = v.rule AND r.deleted_at IS NULL);

-- ============================================================================
-- 自检 A：形态断言（9 个末级逐条 / 4 父科目 / 无裂树 / 父科目零分录 / 名称只提示）
-- ============================================================================
DO $$
DECLARE
    v_parent_bad TEXT;
    v_leaf_bad   TEXT;
    v_name_diff  TEXT;
    v_cat_diff   TEXT;
    v_cracked    BIGINT;
    v_entries    BIGINT;
BEGIN
    -- ① 4 个父科目：必须存在、未软删、is_leaf = 0
    SELECT string_agg(format('%s(is_leaf=%s%s)', a.account_code, a.is_leaf,
                             CASE WHEN a.deleted_at IS NOT NULL THEN ',已软删' ELSE '' END), ' / ')
      INTO v_parent_bad
      FROM biz_account a
     WHERE a.account_code IN ('2211', '2221', '2241', '5602')
       AND (a.deleted_at IS NOT NULL OR a.is_leaf <> 0);
    IF v_parent_bad IS NOT NULL THEN
        RAISE EXCEPTION 'V0384 自检失败：父科目没翻成非末级 —— %（is_leaf 必须为 0，'
            '否则凭证闸仍允许直接记 2211/5602 这类汇总科目）', v_parent_bad;
    END IF;

    -- ② 9 个末级科目逐条核对**结构**：存在 / 未软删 / 末级 / 级次 = 父级+1 / 父级正确 /
    --    类别与余额方向与设计一致 / 状态启用
    SELECT string_agg(format('%s(%s)', x.code, x.why), ' / ') INTO v_leaf_bad
      FROM (
        SELECT l.code,
               CASE WHEN a.account_code IS NULL THEN '不存在'
                    WHEN a.deleted_at IS NOT NULL THEN '已软删'
                    WHEN a.is_leaf <> 1 THEN '不是末级（记账闸会拒）'
                    WHEN a.level <> p.level + 1 THEN 'level=' || a.level || '（应为 ' || (p.level + 1) || '）'
                    WHEN p.account_code IS DISTINCT FROM l.parent_code THEN
                         '父级=' || coalesce(p.account_code, '<空>') || '（应为 ' || l.parent_code || '）'
                    WHEN a.account_category <> l.category THEN
                         '类别=' || a.account_category || '（应为 ' || l.category || '）'
                    WHEN a.balance_direction <> l.direction THEN
                         '方向=' || a.balance_direction || '（应为 ' || l.direction || '）'
                    WHEN a.status <> 1 THEN '已停用（界面选不到，等于没种）'
               END AS why
          FROM (VALUES
                 ('2211.01', '2211', 'LIABILITY',   'CREDIT'),
                 ('2211.02', '2211', 'LIABILITY',   'CREDIT'),
                 ('2211.03', '2211', 'LIABILITY',   'CREDIT'),
                 ('2221.03', '2221', 'LIABILITY',   'CREDIT'),
                 ('2241.03', '2241', 'LIABILITY',   'CREDIT'),
                 ('2241.04', '2241', 'LIABILITY',   'CREDIT'),
                 ('5602.10', '5602', 'PROFIT_LOSS', 'DEBIT'),
                 ('5602.11', '5602', 'PROFIT_LOSS', 'DEBIT'),
                 ('5602.12', '5602', 'PROFIT_LOSS', 'DEBIT')
               ) AS l(code, parent_code, category, direction)
          LEFT JOIN biz_account a ON a.account_code = l.code
          LEFT JOIN biz_account p ON p.account_code = l.parent_code
      ) x
     WHERE x.why IS NOT NULL;
    IF v_leaf_bad IS NOT NULL THEN
        RAISE EXCEPTION 'V0384 自检失败：末级科目形态不对 —— %', v_leaf_bad;
    END IF;

    -- ③ 辅助核算：2211.0x / 5602.1x 必须带 DEPT（工资凭证的部门口径靠它，§4.6.2 规则 3）
    SELECT string_agg(format('%s=%s', l.code, coalesce(a.aux_dimensions, '<空>')), ' / ')
      INTO v_cat_diff
      FROM (VALUES ('2211.01'), ('2211.02'), ('2211.03'),
                   ('5602.10'), ('5602.11'), ('5602.12')) AS l(code)
      LEFT JOIN biz_account a ON a.account_code = l.code
     WHERE a.aux_dimensions IS DISTINCT FROM 'DEPT';
    IF v_cat_diff IS NOT NULL THEN
        RAISE EXCEPTION 'V0384 自检失败：以下科目的部门辅助核算没配上 —— %。'
            '工资凭证借贷双方都要按部门展开（§4.6.2 规则 3），缺了它「研发部工资」永远查不出来', v_cat_diff;
    END IF;

    -- ④ 全表不变量：**末级科目不许挂子科目**（翻漏一个就会造出裂树）
    SELECT count(*) INTO v_cracked
      FROM biz_account p
      JOIN biz_account c ON c.parent_id = p.id AND c.deleted_at IS NULL
     WHERE p.deleted_at IS NULL
       AND p.is_leaf = 1;
    IF v_cracked > 0 THEN
        RAISE EXCEPTION 'V0384 自检失败：还有 % 个「末级科目却挂着子科目」的裂树节点 —— '
            '凭证闸会把它当可记账、报表又把它当汇总，同一笔钱两头算', v_cracked;
    END IF;

    -- ⑤ 4 个父科目上不许有分录（前置护栏已拦 2211 之外的情况，这里兜底）
    SELECT count(*) INTO v_entries
      FROM biz_voucher_entry e
     WHERE e.account_code IN ('2211', '2221', '2241', '5602');
    IF v_entries > 0 THEN
        RAISE EXCEPTION 'V0384 自检失败：汇总科目上仍有 % 条分录 —— 前置护栏没拦住，'
            '这些金额会掉出末级结算', v_entries;
    END IF;

    -- ⑥ 科目名与设计不符只点名（显示层差异，不挡住升级）
    SELECT string_agg(format('%s=「%s」', l.code, a.account_name), ' / ') INTO v_name_diff
      FROM biz_account a
      JOIN (VALUES
             ('2211.01', '应付职工薪酬-工资'), ('2211.02', '应付职工薪酬-社保(单位)'),
             ('2211.03', '应付职工薪酬-公积金(单位)'), ('2221.03', '应交税费-应交个人所得税'),
             ('2241.03', '其他应付款-社保(个人)'), ('2241.04', '其他应付款-公积金(个人)'),
             ('5602.10', '管理费用-工资'), ('5602.11', '管理费用-社保(单位)'),
             ('5602.12', '管理费用-公积金(单位)')
           ) AS l(code, name) ON l.code = a.account_code
     WHERE a.account_name <> l.name
       AND a.deleted_at IS NULL;
    IF v_name_diff IS NOT NULL THEN
        RAISE NOTICE 'V0384 提示：以下末级科目的名称与设计稿（§3.2 ①）不同，可能已被客户改名？'
            '结构不受影响，仅提示：%', v_name_diff;
    END IF;

    RAISE NOTICE 'V0384 形态断言通过：4 父科目已翻非末级 + 9 个末级科目'
        '（编码/父级/级次/类别/方向/末级位/启用位/部门辅助核算 逐条核对）'
        '+ 无裂树 + 汇总科目零分录';
END
$$;

-- ============================================================================
-- 自检 B：口径断言（2 条规则能匹配到科目 / 记账闸 / 报表映射 / 本迁移不写分录）
--
-- 断言的不是表结构，而是「种完之后下游三个口径还对不对」：
--   · 规则解析不到科目 ⇒ 配置页显示的是一条空壳规则，HR-39 对账也找不到锚点；
--   · 记账闸的拒绝面/放行面 ⇒ 翻了末级却没种够，工资凭证根本写不出来；
--   · 报表项前缀 ⇒ 2211.0x 的余额必须并进「应付职工薪酬」，5602.1x 必须并进「管理费用」
--     （`report.rs::match_account` 是**前缀匹配**：`code == token || code.starts_with("token.")`，
--      所以新叶子会自动并入，前提是映射行还在）。
-- ============================================================================
DO $$
DECLARE
    v_rule_bad  TEXT;
    v_rule_n    BIGINT;
    v_gate_rej  BIGINT;
    v_gate_ok   BIGINT;
    v_map_bad   TEXT;
    v_map_gone  TEXT;
    v_entries   BIGINT;
BEGIN
    -- ① 2 条规则必须在位、启用、未删、借贷科目都解析到，且解析出来的**编码**就是设计那四个
    SELECT count(*) INTO v_rule_n
      FROM biz_voucher_rule r
     WHERE r.rule_code IN ('PAYROLL_ACCRUAL_DEFAULT', 'PAYROLL_PAYMENT_DEFAULT')
       AND r.is_active = 1
       AND r.deleted_at IS NULL;
    IF v_rule_n <> 2 THEN
        RAISE EXCEPTION 'V0384 自检失败：工资凭证规则只有 % 条在位（应为 2）—— '
            '配置页「工资凭证走哪些科目」会是空/缺行', v_rule_n;
    END IF;

    SELECT string_agg(format('%s(%s)', v.rule, x.why), ' / ') INTO v_rule_bad
      FROM (VALUES
             ('PAYROLL_ACCRUAL_DEFAULT', 'PAYROLL_ACCRUAL', '5602.10', '2211.01'),
             ('PAYROLL_PAYMENT_DEFAULT', 'PAYROLL_PAYMENT', '2211.01', '1002')
           ) AS v(rule, biz, debit_code, credit_code)
      LEFT JOIN LATERAL (
          SELECT r.biz_type, d.account_code AS debit_code, c.account_code AS credit_code,
                 CASE WHEN r.debit_account_id IS NULL THEN '借方科目解析为空'
                      WHEN r.credit_account_id IS NULL THEN '贷方科目解析为空'
                      WHEN d.account_code IS DISTINCT FROM v.debit_code THEN
                           '借方=' || coalesce(d.account_code, '<空>') || '（应为 ' || v.debit_code || '）'
                      WHEN c.account_code IS DISTINCT FROM v.credit_code THEN
                           '贷方=' || coalesce(c.account_code, '<空>') || '（应为 ' || v.credit_code || '）'
                      WHEN r.biz_type <> v.biz THEN
                           'biz_type=' || r.biz_type || '（应为 ' || v.biz || '）'
                 END AS why
            FROM biz_voucher_rule r
            LEFT JOIN biz_account d ON d.id = r.debit_account_id
            LEFT JOIN biz_account c ON c.id = r.credit_account_id
           WHERE r.rule_code = v.rule AND r.deleted_at IS NULL
      ) x ON TRUE
     WHERE x.why IS NOT NULL;
    IF v_rule_bad IS NOT NULL THEN
        RAISE EXCEPTION 'V0384 自检失败：工资凭证规则解析不对 —— %。'
            '规则只做口径登记，但空壳规则会让配置页与巡检对账失去锚点', v_rule_bad;
    END IF;

    -- ② 记账闸的**拒绝面**：4 个父科目必须全部落在 `is_leaf = 0` 那一边
    --    （`voucher::non_leaf_codes` 的谓词就是 `deleted_at IS NULL AND is_leaf = 0`）
    SELECT count(*) INTO v_gate_rej
      FROM biz_account
     WHERE deleted_at IS NULL
       AND is_leaf = 0
       AND account_code IN ('2211', '2221', '2241', '5602');
    IF v_gate_rej <> 4 THEN
        RAISE EXCEPTION 'V0384 自检失败：记账闸只会拒 % 个汇总科目（应为 4）—— '
            '有科目仍是末级，界面上会允许「借 2211」这类汇总级分录', v_gate_rej;
    END IF;

    -- ③ 记账闸的**放行面**：9 个末级科目必须全部可记账（is_leaf = 1 且启用且未软删）
    SELECT count(*) INTO v_gate_ok
      FROM biz_account
     WHERE deleted_at IS NULL
       AND is_leaf = 1
       AND status = 1
       AND account_code IN ('2211.01', '2211.02', '2211.03',
                            '2221.03',
                            '2241.03', '2241.04',
                            '5602.10', '5602.11', '5602.12');
    IF v_gate_ok <> 9 THEN
        RAISE EXCEPTION 'V0384 自检失败：可记账的末级工资科目只有 % 个（应为 9）—— '
            '翻完末级却没种够，薪资单过账（§4.6）会全部失败', v_gate_ok;
    END IF;

    -- ④ 报表口径：4 个报表项若还在，科目模式里必须**仍有**对应前缀（新叶子靠前缀自动并入）
    --    （老库 / 精简库可能整张表都没有 ⇒ 整段跳过，不把「没这张表」报成科目口径错）
    IF to_regclass('public.biz_report_item') IS NULL THEN
        RAISE NOTICE 'V0384 提示：biz_report_item 不存在（本库无报表模块）—— 跳过报表前缀校验';
        v_map_bad  := NULL;
        v_map_gone := NULL;
    ELSE
    SELECT string_agg(format('%s/%s=%s', v.report, v.item, coalesce(i.account_pattern, '<空>')), ' / ')
      INTO v_map_bad
      FROM (VALUES
             ('BALANCE_SHEET',    'PAYROLL_PAYABLE', '2211'),
             ('BALANCE_SHEET',    'TAXES_PAYABLE',   '2221'),
             ('BALANCE_SHEET',    'OTHER_PAYABLES',  '2241'),
             ('INCOME_STATEMENT', 'ADMIN_EXPENSE',   '5602')
           ) AS v(report, item, prefix)
      JOIN biz_report_item i ON i.report_code = v.report AND i.item_code = v.item
     WHERE i.account_pattern NOT LIKE '%' || v.prefix || '%';
    IF v_map_bad IS NOT NULL THEN
        RAISE EXCEPTION 'V0384 自检失败：报表项的科目模式里丢了前缀 —— %。'
            '报告取数是前缀匹配（`code == token || code.starts_with("token.")`），'
            '前缀一丢，新种的末级科目余额会从报表里静默消失', v_map_bad;
    END IF;

    -- 报表项整行缺失只提示（客户可能精简了报表项，不该挡住升级）
    SELECT string_agg(format('%s/%s', v.report, v.item), ' / ') INTO v_map_gone
      FROM (VALUES
             ('BALANCE_SHEET',    'PAYROLL_PAYABLE'),
             ('BALANCE_SHEET',    'TAXES_PAYABLE'),
             ('BALANCE_SHEET',    'OTHER_PAYABLES'),
             ('INCOME_STATEMENT', 'ADMIN_EXPENSE')
           ) AS v(report, item)
     WHERE NOT EXISTS (SELECT 1 FROM biz_report_item i
                        WHERE i.report_code = v.report AND i.item_code = v.item);
    IF v_map_gone IS NOT NULL THEN
        RAISE NOTICE 'V0384 提示：报表项 % 在本库不存在（已被精简？）——'
            '对应科目的余额不会出现在该报表项下，请人工确认报表口径', v_map_gone;
    END IF;
    END IF;   -- END IF：biz_report_item 存在性守卫

    -- ⑤ 本迁移一条分录都不许写（只动科目树与规则登记）
    SELECT count(*) INTO v_entries
      FROM biz_voucher_entry
     WHERE account_code IN ('2211.01', '2211.02', '2211.03',
                            '2221.03',
                            '2241.03', '2241.04',
                            '5602.10', '5602.11', '5602.12');
    IF v_entries > 0 THEN
        RAISE NOTICE 'V0384 提示：新末级科目上已有 % 条分录（正常应为 0 —— 本迁移只种科目；'
            '非 0 说明执行前就有人往这些编码记账，请确认是否为客户已自行上线）', v_entries;
    END IF;

    RAISE NOTICE 'V0384 口径断言通过：2 条工资凭证规则都解析到科目（5602.10/2211.01、2211.01/1002）、'
        '4 个汇总科目被记账闸拦、9 个末级科目可记账、4 个报表项前缀仍在、本迁移未写任何分录';
    RAISE NOTICE 'V0384 完成：9 个工资科目 + 4 父科目翻非末级 + 2 条凭证规则（ARCH-9-2 §3.2 ①⑧）';
END
$$;
