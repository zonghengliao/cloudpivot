-- ============================================================================
-- V0396：薪资项种子 · 17 项（15 个计算项 + 2 个汇总项，ARCH-9-2 §4.4.3 逐行）
--
-- 卡：t_1c4a7b35（HR-35【HR】迁移 V0387）
-- 依据：docs/architecture/ARCH-9-人力资源模块设计.md
--       §4.4.1（五类薪资项语义：进不进应发 / 要不要科目）、
--       §4.4.2（公式 DSL）、§4.4.3（一期种子 17 项，本文件逐行照抄）、
--       §4.4.4（试算演算：8000/500/300 + 2 天事假 → 应发 9954.03 / 实发 8154.03）、
--       §4.6（工资凭证勾稽：借 5602.1x / 贷 2211.0x，代扣类贷负债）、
--       §7.1（迁移清单，hr_salary_items_seed）、§7.3（回滚方案）
--
-- # 号段：卡面写 V0387（设计号），实际落 V0396
--   设计 §7.1 已记录三次号段后移（成本核算 V0378~V0382 实占 → HR 全系列 +5；
--   V0385__cost_module_switch 实占 → HR-03 起再 +1；HR-26 实施时
--   V0390__cost_menu_group / V0391__flow_cost_settle_is_system / V0392__attendance_lock_guard
--   又各占一号 → hr_salary_items_seed 定为 V0396）。
--   nik 迁移加载器对版本号做**跨目录全局唯一**校验，重号 ⇒ 服务启动直接失败
--   （AGENTS.md 约定 6）。落地前实测 `ls backend/migrations/*/ | grep -oE 'V[0-9]{4}' | sort -u | tail -3`
--   = V0391 / V0392 / V0393 ⇒ V0396 空闲，按设计取 V0396（避让 V0394 hr_salary_flow /
--   V0395 hr_adjust_resign_flow / V0397 hr_selfcheck 的预留号）。
--
-- # 本迁移只做一件事：往 `biz_salary_item` 种 17 行
--   · 15 个计算项（8 收入 + 1 应发前扣减 + 4 代扣 + 2 单位承担）→ 落 `biz_salary_detail_item`
--   · 2 个汇总项（GROSS 应发合计 / NET 实发合计）→ **不落** detail_item（值已写在
--     `biz_salary_detail.gross_amount/net_amount`，库层 CHECK 保证恒等，再存一遍就是
--     「同一事实两处存」，见 §4.4.3 末段）
--   不建表、不改表、不写分录、不动科目（科目由 V0384 种）。
--
-- # ★ org_id 必须是**全零哨兵**（不是 V0386 号段用的 0000…0001）
--   HR 各表 handler 的 `INSERT` 统一写 `hr::model::SENTINEL_ORG_ID = Uuid::nil()`
--   （`00000000-0000-0000-0000-000000000000`，见 `salary_item_model::insert` 的
--   `Value::from(&SENTINEL_ORG_ID)`）。种子若用别的 org：
--     · `uk_biz_sal_item_code (org_id, item_code) WHERE deleted_at IS NULL` 判不到它们，
--       用户在「薪资项」页新建一个 `BASE` 会**成功**⇒ 同编码两行，
--       `{BASE}` 到底取哪一行由 `ORDER BY sort_order` 决定 ⇒ 工资表静默算错；
--     · `uk_biz_sal_item_sort` 同理：种子的 10/20/… 占不住号，用户新建项会撞上一半。
--   故本文件统一 `'00000000-0000-0000-0000-000000000000'::uuid`，并在自检里对拍这 17 行的 org。
--
-- # 逐列口径（设计没写死、必须定下来的三处，都在这里写清理由）
--   1. `source`（取数来源，CHECK 五值）—— 决定 HR-34 试算时**从哪取数**：
--        · `FIXED`（BASE/POST/MEAL）→ `EMPLOYEE_STANDARD`（取 `biz_employee_salary` 现行标准）
--        · `FORMULA` 且只用考勤变量（OT_*/ATT_*）→ `ATTENDANCE`（取当期 `biz_attendance_record`）
--        · `INPUT`（PERF/SI_*/HF_*/IIT/OTHER_WITHHOLD）→ `INPUT`（手工录 / 导入）
--        · `SUMMARY`（GROSS/NET）→ `SYSTEM`（系统算，不听人）
--      设计 §4.4.3 只给了「算法」列（FIXED/FORMULA/INPUT），source 是按 §4.4.1 +
--      §4.5.1（社保/公积金/个税一期手工）推出来的唯一自洽映射。
--   2. `is_taxable`（是否计入个税应税收入，二期累计预扣法用）：
--        · `EARNING` = 1（含餐补：境内以餐补形式发放的补贴并入工资薪金计税）
--        · `PRE_DEDUCT` / `WITHHOLD` / `EMPLOYER` / `SUMMARY` = 0
--      **PRE_DEDUCT 为什么是 0**：它的金额在库里是**正数**（考勤扣款 735.63），
--      符号已在应发恒等式 `Σ EARNING − Σ PRE_DEDUCT` 里体现；标成 1 会让「扣款」被当成
--      「收入」加进应税所得（反向）。二期算个税请用 gross（已减过），不要用
--      `SUM(amount) WHERE is_taxable = 1`。
--   3. `is_system`：只有 `GROSS` / `NET` = 1（`salary_item_handler` 的护栏：系统项不许删 / 不许停用 /
--      编码不可改）。其余 15 项 = 0 —— 客户可以改名称、停用、换科目、改公式，
--      这正是 §4.4.2「配置化」要的东西。
--
-- # 幂等（重复执行结果不变）
--   `INSERT … SELECT … WHERE NOT EXISTS (org_id = 哨兵 AND item_code = v.item_code AND deleted_at IS NULL)`
--   ⇒ 第二遍 0 行。唯一键也是软删唯一索引（`WHERE deleted_at IS NULL`），
--   已软删的同名行不挡路（会补一条有效的，符合「客户删了默认项要能恢复」的预期）。
--   自检是纯 `DO` 块断言，可重复执行、无副作用。
--
-- # 前置护栏（拒绝执行的三类情况，都带人能照做的处理建议）
--   ① 依赖表 `biz_salary_item` / `biz_account` 不在（迁移没跑到这里）⇒ 拒绝；
--   ② 种子的 10 个科目编码在 `biz_account` 里缺失 / 已软删 / 非末级 / 停用 ⇒ 拒绝。
--      理由：语法上 `debit_account_code` 只是 VARCHAR，写个不存在的编码也能存进去，
--      但 HR-39 过账时 `voucher_multi::ensure_leaf` 会 400 ⇒ 「薪资单算得出来、过不了账」，
--      而且错误信息指向的是过账按钮，不是配置（AGENTS.md 约定 11 的同一类坑）。
--      **先拦住比事后查账便宜**；
--   ③ `sort_order` 被**非种子项**占用（客户自己建的项占了 10/20/…）⇒ 拒绝并点名。
--      不去自动改客户的顺序（`sort_order` 决定计算顺序，静默改会让他们已有的项换序）。
--
--   不拒绝、只 `RAISE NOTICE` 的：种子编码**已存在**但形态不同（客户改过名称/公式/科目/顺序）。
--   客户对薪资项有配置权（§4.4.2），迁移不替人做主、不覆盖已有行；但是必须让人看得见
--   「这条沿用你库里已有的，没种」。表结构层面真正会炸的（缺科目 / 顺序撞车）才 EXCEPTION。
--
-- # 回滚
--   见 `V0396__hr_salary_items_seed.down.sql`：有业务数据（薪资标准引用这些编码 /
--   明细项 / 已过账薪资单）就拒绝回滚（§7.3「有业务数据就拒绝回滚」），否则按 17 个编码删。
-- ============================================================================

-- ============================================================================
-- 前置护栏
-- ============================================================================
DO $$
DECLARE
    v_org       CONSTANT uuid := '00000000-0000-0000-0000-000000000000';  -- hr::model::SENTINEL_ORG_ID
    v_missing   TEXT;
    v_acct_bad  TEXT;
    v_sort_bad  TEXT;
    v_owned     TEXT;
BEGIN
    -- ⓪ 依赖表必须都在（V0383 建 biz_salary_item；V0137/V0183 建 biz_account）
    IF to_regclass('public.biz_salary_item') IS NULL THEN
        RAISE EXCEPTION 'V0396 拒绝执行：biz_salary_item 不存在（V0383__hr_tables 未应用？）。'
            '本迁移只往已建好的表里种 17 个薪资项，不建表';
    END IF;
    IF to_regclass('public.biz_account') IS NULL THEN
        RAISE EXCEPTION 'V0396 拒绝执行：biz_account 不存在（科目表 V0137 未应用？）。'
            '薪资项的借贷科目必须指向真实存在的末级科目，没有科目表就无法校验';
    END IF;

    -- ① 10 个科目编码必须在位、未软删、末级、启用
    --    （9 个工资专用由 V0384 种；1002 不在这里用；1221 其他应收款由 V0183 种）
    SELECT string_agg(format('%s(%s)', s.code, x.why), ' / ') INTO v_acct_bad
      FROM (VALUES
             ('2211.01'), ('2211.02'), ('2211.03'), ('2221.03'),
             ('2241.03'), ('2241.04'), ('5602.10'), ('5602.11'), ('5602.12'),
             ('1221')
           ) AS s(code)
      LEFT JOIN biz_account a ON a.account_code = s.code
      LEFT JOIN LATERAL (
          SELECT CASE WHEN a.id IS NULL THEN '不存在'
                      WHEN a.deleted_at IS NOT NULL THEN '已软删'
                      WHEN a.is_leaf <> 1 THEN '非末级（记账闸会 400 拒）'
                      WHEN a.status <> 1 THEN '已停用（界面选不到，等于没种）'
                 END AS why
      ) x ON TRUE
     WHERE x.why IS NOT NULL;
    IF v_acct_bad IS NOT NULL THEN
        RAISE EXCEPTION 'V0396 拒绝执行：薪资项的科目指向不合格 —— %。'
            '17 个薪资项按设计 §4.4.3 落这些科目（借 5602.10~12 / 贷 2211.01~03、2241.03~04、2221.03、1221）。'
            '请先在「会计科目」里补齐/恢复/启用这些末级科目（或确认 V0384__hr_accounts 已应用），再重跑本迁移', v_acct_bad;
    END IF;

    -- ② 计算顺序被非种子项占用 ⇒ 拒绝（种子要占 10/20/…/170）
    SELECT string_agg(format('%s 被 %s 占用', x.sort_order, x.item_code), ' / ') INTO v_sort_bad
      FROM biz_salary_item x
     WHERE x.org_id = v_org
       AND x.deleted_at IS NULL
       AND x.sort_order IN (10,20,30,40,50,60,70,80,90,100,110,120,130,140,150,160,170)
       AND x.item_code NOT IN ('BASE','POST','MEAL','PERF','OT_WD','OT_WE','OT_HO','ATT_BONUS',
                               'ATT_DEDUCT','SI_PERSONAL','HF_PERSONAL','IIT','OTHER_WITHHOLD',
                               'SI_COMPANY','HF_COMPANY','GROSS','NET');
    IF v_sort_bad IS NOT NULL THEN
        RAISE EXCEPTION 'V0396 拒绝执行：计算顺序与客户自建薪资项撞车 —— %。'
            '`sort_order` 决定薪资项的计算顺序（公司内唯一），本迁移不去改客户已配的顺序。'
            '请先在「薪资项」页把这些项的 sort_order 调开（建议 200 之后），再重跑本迁移', v_sort_bad;
    END IF;

    -- ③ 已存在的同编码行：只提示（不覆盖客户配置）
    SELECT string_agg(x.item_code, ' / ') INTO v_owned
      FROM biz_salary_item x
     WHERE x.org_id = v_org
       AND x.deleted_at IS NULL
       AND x.item_code IN ('BASE','POST','MEAL','PERF','OT_WD','OT_WE','OT_HO','ATT_BONUS',
                           'ATT_DEDUCT','SI_PERSONAL','HF_PERSONAL','IIT','OTHER_WITHHOLD',
                           'SI_COMPANY','HF_COMPANY','GROSS','NET');
    IF v_owned IS NOT NULL THEN
        RAISE NOTICE 'V0396 提示：薪资项 % 已在库中（客户改过或本迁移跑过）—— 沿用已有行，不覆盖', v_owned;
    END IF;

    RAISE NOTICE 'V0396 前置护栏通过：依赖表齐、10 个科目编码可用（末级+启用）、计算顺序 10/20/…/170 空闲';
END
$$;

-- ============================================================================
-- ① 种 17 个薪资项（§4.4.3 表逐行；顺序 = sort_order）
--
--   列顺序刻意与设计表一致：编码 / 名称 / 类型 / 算法 / 取数来源 / 公式 / 顺序 / 借 / 贷 /
--   是否计税 / 是否系统项。名称与公式**逐字照抄设计表**（`21.75` 写在公式里而不是代码里 ——
--   客户要换成 22 或当月实际天数，改一行公式即可，见 §4.4.3 末段）。
--
--   汇总项（GROSS/NET）的公式是**校验口径**（恒等式写在 §4.4.1），它们不落 detail_item，
--   但必须留在 `biz_salary_item` 里：①前端「薪资项」页能看到两条系统项；
--   ②`check_formula` 的可用变量清单要含它们（客户自建项可以引用 `{GROSS}`）；
--   ③库层 `ck_biz_sal_item_account` 要求 SUMMARY 必须有公式。
-- ============================================================================
INSERT INTO biz_salary_item (id, org_id, item_code, item_name, item_type, calc_type, source,
                             formula, sort_order, debit_account_code, credit_account_code,
                             is_taxable, is_active, is_system, remark)
SELECT gen_random_uuid(),
       '00000000-0000-0000-0000-000000000000'::uuid,   -- SENTINEL_ORG_ID（见头注释）
       v.item_code, v.item_name, v.item_type, v.calc_type, v.source,
       v.formula, v.sort_order, v.debit_code, v.credit_code,
       v.is_taxable, 1, v.is_system,
       'ARCH-9-2 薪资项种子（V0396）'
  FROM (VALUES
        ('BASE',           '基本工资',     'EARNING',    'FIXED',   'EMPLOYEE_STANDARD', NULL, 10,  '5602.10', '2211.01', 1, 0),
        ('POST',           '岗位津贴',     'EARNING',    'FIXED',   'EMPLOYEE_STANDARD', NULL, 20,  '5602.10', '2211.01', 1, 0),
        ('MEAL',           '餐补',         'EARNING',    'FIXED',   'EMPLOYEE_STANDARD', NULL, 30,  '5602.10', '2211.01', 1, 0),
        ('PERF',           '绩效奖金',     'EARNING',    'INPUT',   'INPUT',             NULL, 40,  '5602.10', '2211.01', 1, 0),
        ('OT_WD',          '平时加班费',   'EARNING',    'FORMULA', 'ATTENDANCE', 'ROUND({BASE} / 21.75 / 8 * {ATT_OT_WD} * 1.5, 2)', 50,  '5602.10', '2211.01', 1, 0),
        ('OT_WE',          '休息日加班费', 'EARNING',    'FORMULA', 'ATTENDANCE', 'ROUND({BASE} / 21.75 / 8 * {ATT_OT_WE} * 2, 2)',   60,  '5602.10', '2211.01', 1, 0),
        ('OT_HO',          '节假日加班费', 'EARNING',    'FORMULA', 'ATTENDANCE', 'ROUND({BASE} / 21.75 / 8 * {ATT_OT_HO} * 3, 2)',   70,  '5602.10', '2211.01', 1, 0),
        ('ATT_BONUS',      '全勤奖',       'EARNING',    'FORMULA', 'ATTENDANCE', 'IF({ATT_ABSENT_DAYS} == 0 && {ATT_LEAVE_UNPAID_DAYS} == 0, 200, 0)', 80, '5602.10', '2211.01', 1, 0),
        ('ATT_DEDUCT',     '考勤扣款',     'PRE_DEDUCT', 'FORMULA', 'ATTENDANCE', 'ROUND({BASE} / 21.75 * {ATT_LEAVE_UNPAID_DAYS}, 2)', 90,  '5602.10', '2211.01', 0, 0),
        ('SI_PERSONAL',    '社保(个人)',   'WITHHOLD',   'INPUT',   'INPUT',             NULL, 100, NULL,      '2241.03', 0, 0),
        ('HF_PERSONAL',    '公积金(个人)', 'WITHHOLD',   'INPUT',   'INPUT',             NULL, 110, NULL,      '2241.04', 0, 0),
        ('IIT',            '个人所得税',   'WITHHOLD',   'INPUT',   'INPUT',             NULL, 120, NULL,      '2221.03', 0, 0),
        ('OTHER_WITHHOLD', '其他代扣',     'WITHHOLD',   'INPUT',   'INPUT',             NULL, 130, NULL,      '1221',    0, 0),
        ('SI_COMPANY',     '单位社保',     'EMPLOYER',   'INPUT',   'INPUT',             NULL, 140, '5602.11', '2211.02', 0, 0),
        ('HF_COMPANY',     '单位公积金',   'EMPLOYER',   'INPUT',   'INPUT',             NULL, 150, '5602.12', '2211.03', 0, 0),
        ('GROSS',          '应发合计',     'SUMMARY',    'FORMULA', 'SYSTEM', '{BASE}+{POST}+{MEAL}+{PERF}+{OT_WD}+{OT_WE}+{OT_HO}+{ATT_BONUS}-{ATT_DEDUCT}', 160, NULL, NULL, 0, 1),
        ('NET',            '实发合计',     'SUMMARY',    'FORMULA', 'SYSTEM', '{GROSS}-{SI_PERSONAL}-{HF_PERSONAL}-{IIT}-{OTHER_WITHHOLD}', 170, NULL, NULL, 0, 1)
       ) AS v(item_code, item_name, item_type, calc_type, source, formula, sort_order,
              debit_code, credit_code, is_taxable, is_system)
 WHERE NOT EXISTS (SELECT 1 FROM biz_salary_item s
                    WHERE s.org_id = '00000000-0000-0000-0000-000000000000'::uuid
                      AND s.item_code = v.item_code
                      AND s.deleted_at IS NULL);

-- ============================================================================
-- 自检（纯 DO 块，无副作用，可重复执行）
--   验收三段：① 17 项齐全 ② 公式在位（深解析交给 Rust 的 salary_formula 用例）
--            ③ 科目指向正确（编码真实、末级、启用；类别方向与 §4.4.3 一致）
-- ============================================================================
DO $$
DECLARE
    v_org       CONSTANT uuid := '00000000-0000-0000-0000-000000000000';
    v_marker    CONSTANT text := 'ARCH-9-2 薪资项种子（V0396）';
    v_missing   TEXT;
    v_shape_bad TEXT;
    v_acct_bad  TEXT;
    v_cat_bad   TEXT;
    v_n         BIGINT;
    v_sort_n    BIGINT;
BEGIN
    -- ① 17 个编码必须全部在位（未软删、哨兵组织）
    SELECT string_agg(x.code, ' / ') INTO v_missing
      FROM (VALUES ('BASE'),('POST'),('MEAL'),('PERF'),('OT_WD'),('OT_WE'),('OT_HO'),('ATT_BONUS'),
                   ('ATT_DEDUCT'),('SI_PERSONAL'),('HF_PERSONAL'),('IIT'),('OTHER_WITHHOLD'),
                   ('SI_COMPANY'),('HF_COMPANY'),('GROSS'),('NET')) AS x(code)
     WHERE NOT EXISTS (SELECT 1 FROM biz_salary_item s
                        WHERE s.org_id = v_org AND s.deleted_at IS NULL AND s.item_code = x.code);
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0396 自检失败：薪资项 % 没种进去（应为 17 项，§4.4.3）—— '
            '薪资单明细会少这些项，且库层恒等式（应发/实发）无法闭合', v_missing;
    END IF;

    SELECT count(*) INTO v_n
      FROM biz_salary_item
     WHERE org_id = v_org AND deleted_at IS NULL
       AND item_code IN ('BASE','POST','MEAL','PERF','OT_WD','OT_WE','OT_HO','ATT_BONUS',
                         'ATT_DEDUCT','SI_PERSONAL','HF_PERSONAL','IIT','OTHER_WITHHOLD',
                         'SI_COMPANY','HF_COMPANY','GROSS','NET');
    IF v_n <> 17 THEN
        RAISE EXCEPTION 'V0396 自检失败：17 个薪资项编码只对上 % 行（应为 17）', v_n;
    END IF;

    -- ② 本迁移种的行逐列对拍设计 §4.4.3（客户改过的行不在此列，见「不覆盖客户配置」）
    SELECT string_agg(format('%s(type=%s/calc=%s/sort=%s/借=%s/贷=%s/税=%s/系统=%s，应为 %s/%s/%s/%s/%s/%s/%s)',
                             s.item_code, s.item_type, s.calc_type, s.sort_order,
                             coalesce(s.debit_account_code, 'NULL'), coalesce(s.credit_account_code, 'NULL'),
                             s.is_taxable, s.is_system,
                             v.item_type, v.calc_type, v.sort_order,
                             coalesce(v.debit_code, 'NULL'), coalesce(v.credit_code, 'NULL'),
                             v.is_taxable, v.is_system), ' / ')
      INTO v_shape_bad
      FROM (VALUES
             ('BASE',           'EARNING',    'FIXED',   10,  '5602.10', '2211.01', 1, 0),
             ('POST',           'EARNING',    'FIXED',   20,  '5602.10', '2211.01', 1, 0),
             ('MEAL',           'EARNING',    'FIXED',   30,  '5602.10', '2211.01', 1, 0),
             ('PERF',           'EARNING',    'INPUT',   40,  '5602.10', '2211.01', 1, 0),
             ('OT_WD',          'EARNING',    'FORMULA', 50,  '5602.10', '2211.01', 1, 0),
             ('OT_WE',          'EARNING',    'FORMULA', 60,  '5602.10', '2211.01', 1, 0),
             ('OT_HO',          'EARNING',    'FORMULA', 70,  '5602.10', '2211.01', 1, 0),
             ('ATT_BONUS',      'EARNING',    'FORMULA', 80,  '5602.10', '2211.01', 1, 0),
             ('ATT_DEDUCT',     'PRE_DEDUCT', 'FORMULA', 90,  '5602.10', '2211.01', 0, 0),
             ('SI_PERSONAL',    'WITHHOLD',   'INPUT',   100, NULL,      '2241.03', 0, 0),
             ('HF_PERSONAL',    'WITHHOLD',   'INPUT',   110, NULL,      '2241.04', 0, 0),
             ('IIT',            'WITHHOLD',   'INPUT',   120, NULL,      '2221.03', 0, 0),
             ('OTHER_WITHHOLD', 'WITHHOLD',   'INPUT',   130, NULL,      '1221',    0, 0),
             ('SI_COMPANY',     'EMPLOYER',   'INPUT',   140, '5602.11', '2211.02', 0, 0),
             ('HF_COMPANY',     'EMPLOYER',   'INPUT',   150, '5602.12', '2211.03', 0, 0),
             ('GROSS',          'SUMMARY',    'FORMULA', 160, NULL,      NULL,      0, 1),
             ('NET',            'SUMMARY',    'FORMULA', 170, NULL,      NULL,      0, 1)
           ) AS v(item_code, item_type, calc_type, sort_order, debit_code, credit_code, is_taxable, is_system)
      JOIN biz_salary_item s
        ON s.item_code = v.item_code AND s.org_id = v_org AND s.deleted_at IS NULL
     WHERE s.remark = v_marker
       AND (s.item_type = v.item_type
        AND s.calc_type = v.calc_type
        AND s.sort_order = v.sort_order
        AND s.debit_account_code IS NOT DISTINCT FROM v.debit_code
        AND s.credit_account_code IS NOT DISTINCT FROM v.credit_code
        AND s.is_taxable = v.is_taxable
        AND s.is_system = v.is_system
        AND s.is_active = 1) IS NOT TRUE;
    IF v_shape_bad IS NOT NULL THEN
        RAISE EXCEPTION 'V0396 自检失败：本迁移种下的薪资项与设计 §4.4.3 不一致 —— %。'
            '金额恒等式（应发/实发）与工资凭证的借贷口径都按这张表算，一处不符就得整批回滚', v_shape_bad;
    END IF;

    -- ③ 科目指向：17 行**现值**（含客户改过的）的借/贷科目必须真实、末级、启用
    SELECT string_agg(format('%s(%s)', u.code, u.why), ' / ') INTO v_acct_bad
      FROM (
        SELECT DISTINCT k.code,
               CASE WHEN a.id IS NULL THEN '科目不存在'
                    WHEN a.deleted_at IS NOT NULL THEN '科目已软删'
                    WHEN a.is_leaf <> 1 THEN '科目非末级（记账闸会 400）'
                    WHEN a.status <> 1 THEN '科目已停用'
               END AS why
          FROM (
            SELECT s.debit_account_code AS code
              FROM biz_salary_item s
             WHERE s.org_id = v_org AND s.deleted_at IS NULL
               AND s.item_code IN ('BASE','POST','MEAL','PERF','OT_WD','OT_WE','OT_HO','ATT_BONUS',
                                   'ATT_DEDUCT','SI_PERSONAL','HF_PERSONAL','IIT','OTHER_WITHHOLD',
                                   'SI_COMPANY','HF_COMPANY','GROSS','NET')
               AND s.debit_account_code IS NOT NULL
            UNION
            SELECT s.credit_account_code
              FROM biz_salary_item s
             WHERE s.org_id = v_org AND s.deleted_at IS NULL
               AND s.item_code IN ('BASE','POST','MEAL','PERF','OT_WD','OT_WE','OT_HO','ATT_BONUS',
                                   'ATT_DEDUCT','SI_PERSONAL','HF_PERSONAL','IIT','OTHER_WITHHOLD',
                                   'SI_COMPANY','HF_COMPANY','GROSS','NET')
               AND s.credit_account_code IS NOT NULL
          ) AS k
          LEFT JOIN biz_account a ON a.account_code = k.code
      ) AS u
     WHERE u.why IS NOT NULL;
    IF v_acct_bad IS NOT NULL THEN
        RAISE EXCEPTION 'V0396 自检失败：薪资项的科目指向不合格 —— %。'
            '薪资单过账（§4.6）会按这些编码写分录，指向不存在的/非末级科目会让整张单过不了账', v_acct_bad;
    END IF;

    -- ④ 科目类别 / 余额方向：与 §4.4.3 的「借 / 贷」列语义一致
    --    5602.1x = 损益·借方（工资费用）；2211.0x / 2241.0x / 2221.03 = 负债·贷方；
    --    1221 = 资产·借方（其他代扣是「借支扣回」⇒ 贷 1221 冲减应收，方向反过来是对的）。
    --    这一条只对本迁移的 9 个 HR 专用科目硬拦（与 V0384 自检同口径）；
    --    1221 是 V0183 的通用科目，客户可能重分类过 ⇒ 只提示不拦。
    SELECT string_agg(format('%s(实际 %s/%s，应为 %s/%s)', v.code, a.account_category, a.balance_direction,
                             v.category, v.direction), ' / ')
      INTO v_cat_bad
      FROM (VALUES
             ('2211.01', 'LIABILITY',   'CREDIT'),
             ('2211.02', 'LIABILITY',   'CREDIT'),
             ('2211.03', 'LIABILITY',   'CREDIT'),
             ('2221.03', 'LIABILITY',   'CREDIT'),
             ('2241.03', 'LIABILITY',   'CREDIT'),
             ('2241.04', 'LIABILITY',   'CREDIT'),
             ('5602.10', 'PROFIT_LOSS', 'DEBIT'),
             ('5602.11', 'PROFIT_LOSS', 'DEBIT'),
             ('5602.12', 'PROFIT_LOSS', 'DEBIT')
           ) AS v(code, category, direction)
      JOIN biz_account a ON a.account_code = v.code AND a.deleted_at IS NULL
     WHERE a.account_category <> v.category OR a.balance_direction <> v.direction;
    IF v_cat_bad IS NOT NULL THEN
        RAISE EXCEPTION 'V0396 自检失败：工资科目的类别/方向与设计 §3.2 ① 不符 —— %。'
            '借贷方向决定报表取数正负，方向错了工资费用会算成冲销', v_cat_bad;
    END IF;

    IF EXISTS (SELECT 1 FROM biz_account WHERE account_code = '1221' AND deleted_at IS NULL
                AND (account_category <> 'ASSET' OR balance_direction <> 'DEBIT')) THEN
        RAISE NOTICE 'V0396 提示：科目 1221（其他应收款）已被重分类（不是 资产/借方）——'
            '「其他代扣」贷 1221 的语义是「冲减借支应收」，请确认重分类后仍符合贵司口径';
    END IF;

    -- ⑤ 计算顺序 10..170 各占一行（`uk_biz_sal_item_sort` 兜底，这里给可读报文）
    SELECT count(DISTINCT sort_order) INTO v_sort_n
      FROM biz_salary_item
     WHERE org_id = v_org AND deleted_at IS NULL
       AND item_code IN ('BASE','POST','MEAL','PERF','OT_WD','OT_WE','OT_HO','ATT_BONUS',
                         'ATT_DEDUCT','SI_PERSONAL','HF_PERSONAL','IIT','OTHER_WITHHOLD',
                         'SI_COMPANY','HF_COMPANY','GROSS','NET');
    IF v_sort_n <> 17 THEN
        RAISE EXCEPTION 'V0396 自检失败：17 个薪资项只占 % 个不同的 sort_order（应各占一个）—— '
            '顺序重复会让「先算哪个项」不确定，同一份数据算出两个结果', v_sort_n;
    END IF;

    RAISE NOTICE 'V0396 完成：17 个薪资项（8 收入 + 1 应发前扣减 + 4 代扣 + 2 单位承担 + 2 汇总）'
        '；借 5602.10~12 / 贷 2211.01~03、2241.03~04、2221.03、1221 全部指向末级启用科目；'
        '7 条公式在位（5 计算 + 2 汇总，深解析由 salary_formula 用例验证）';
END
$$;
