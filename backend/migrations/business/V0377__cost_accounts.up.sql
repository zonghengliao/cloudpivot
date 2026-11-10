-- ============================================================================
-- V0377：成本科目翻末级 + 种 9 个末级科目（ARCH-9 §3.3 ③ / §1 D-14）+「历史分录为 0」自检
--
-- 卡：t_486e652d（COST-B1-6【成本】迁移 V0375）—— 拆卡清单 §8.1 的 B1-6
-- 依据：docs/architecture/ARCH-9-成本核算升级设计.md
--       §3.3 ③（科目 DDL，逐字）、§1 D-14（科目补种表：4001.01~03 / 4101.01~04,09 / 2211.01）、
--       §7.2（存量处理：翻非末级前先查历史分录）、§4.7（三张结转凭证的借贷科目）
--
-- # 号段：卡面写 V0375，实际落 V0377
--   V0375 = B1-5 出库加列、V0376 = B1-5 入库 PRODUCTION；V0372/0373 = B1-3、V0374 = B1-4。
--   nik 迁移加载器对版本号做**跨目录全局唯一**校验，重号 ⇒ 服务启动直接失败（AGENTS 约定 6）。
--
-- # 本迁移只做三件事（只动 biz_account 的**科目树形状**，不建表、不碰单据）
--   ① 4001 生产成本 / 4101 制造费用 / 2211 应付职工薪酬 翻成**非末级**（is_leaf = 0）；
--   ② 种 9 个末级科目（父子关系 / 级次 / 类别 / 余额方向 / 末级位）：
--        4001.01 生产成本-直接材料      4001.02 生产成本-直接人工    4001.03 生产成本-制造费用
--        4101.01 制造费用-折旧费        4101.02 制造费用-水电费      4101.03 制造费用-修理费
--        4101.04 制造费用-车间管理费    4101.09 制造费用-其他        2211.01 应付职工薪酬-工资
--   ③ 自检：翻之前断言三科目**历史分录为 0**（§7.2），翻之后断言树形 / 记账口径 / 报表口径。
--
-- # 为什么必须翻末级（这不是洁癖）
--   `voucher.rs::post_voucher` 的「记账科目口径」闸只允许**末级**科目（non_leaf_codes：
--   `is_leaf = 0` 的科目直接 400 拒绝）。4001/4101/2211 现在都是 is_leaf=1 且无子科目
--   ⇒ 直接记 4001 反而能过闸；而资产负债表 / 年结都按末级科目结算，记在汇总科目上的
--   余额会被漏结（跨年试算不平）。本卡把三个科目翻成汇总级、把可记账的叶子补出来，
--   闸从此**更严**而不是更松 —— 记账只能落 4001.01~03 / 4101.01~04,09 / 2211.01。
--
-- # ★ 为什么翻之前必须查历史分录（卡面验收项）
--   biz_voucher_entry 存的是**科目编码快照**（account_code / account_name），翻 non-leaf
--   不会让历史分录消失，但会让这些行变成「记在汇总科目上的分录」：报表取数按末级结算、
--   年结按末级结转，这些金额会被漏掉，而新叶子期初余额为 0 ⇒ 账实不平、年结试算差钱。
--   所以三科目任意一个还有分录，本迁移就 RAISE EXCEPTION **拒绝翻**（§7.2「0 行才翻」）。
--   设计期实测三科目 0 分录（backend/src 里 4001/4101 只出现在 cash_flow.rs 的科目归类
--   清单与测试里，没有一处 post_voucher 记它们；2211 要等人力资源卡上线才有），
--   但客户环境可能被手工改过账 ⇒ 闸必须留在迁移里，而不是留在文档里。
--   口径：**软删凭证的分录也计入**（保守读法 —— 红冲/反审核后分录仍在表里，
--   年结与报表重算会重新看到它们）。
--
-- # 幂等（重复执行结果不变）
--   · UPDATE 带 `AND is_leaf = 1`（第二遍 0 行）；
--   · INSERT 走 `ON CONFLICT (account_code) DO NOTHING`
--     （uk_biz_account_code 是**全表**唯一索引，有意不带 WHERE deleted_at IS NULL，见 V0137 §0.2）；
--   · 「编码已被占用」的前置检查只拦**形态不符**（软删 / 非末级 / 父级或级次不对），
--     形态正确就直接复用 ⇒ 重复执行不报错、不重复种。
--
-- # 与人力资源卡（ARCH9-2）的 2211.01 撞键
--   设计 D-14 明说「与 ARCH9-2 的 2211/5602 种子幂等共存」：两张卡都会种 2211.01。
--   谁先跑谁种，后跑的一方 ON CONFLICT DO NOTHING 跳过；本迁移的自检只断言**结果形态**
--   （2211.01 存在、挂在 2211 下、level = 父级+1、末级、类别/方向继承父级），
--   不断言「是本卡刚种的那一行」，否则并行上线时后跑的那张卡会误报红。
--
-- # 科目名不参与硬断言（只在日志里点名）
--   编码 / 父级 / 级次 / 类别 / 方向 / 末级位是**结构**，一处不符就得拦住（否则报表口径错）。
--   科目名是**显示**：客户可能把「生产成本-直接材料」改成「生产成本-原材料」，这不该挡住
--   升级，但必须看得见 ⇒ 名称不符只 RAISE NOTICE，不 RAISE EXCEPTION。
--
-- # 回滚（down）
--   ① 9 个末级科目有任何历史分录 ⇒ 拒绝（删科目会让分录指向不存在的科目）；
--   ② 三个上级科目下有本迁移之外的子科目 ⇒ 拒绝（把父翻回末级会造出「末级挂子」的裂树）；
--   ③ 删 9 个末级科目 → 三个上级翻回 is_leaf = 1 → 反向自检（见 .down.sql）。
-- ============================================================================

-- ============================================================================
-- 前置护栏：三科目存在 / 未软删 / ☆历史分录为 0 / 9 个编码未被错误占用
-- ============================================================================
DO $$
DECLARE
    v_missing  TEXT;
    v_deleted  TEXT;
    v_blocked  TEXT;
    v_conflict TEXT;
    v_entries  BIGINT;
    rec        RECORD;
BEGIN
    -- ⓪ 分录表本身必须在（否则下面的护栏把「表不存在」报成看不懂的错）
    IF to_regclass('public.biz_voucher_entry') IS NULL THEN
        RAISE EXCEPTION 'V0377 拒绝执行：biz_voucher_entry 不存在 —— 无法校验三科目的历史分录。'
            '请先确认凭证模块的迁移（V0149）已执行到本版本之前';
    END IF;

    -- ① 三个上级科目必须在（V0137/V0183 种的一级科目；本迁移不负责初始化科目表）
    SELECT string_agg(t.code, ' / ') INTO v_missing
      FROM (VALUES ('4001'), ('4101'), ('2211')) AS t(code)
     WHERE NOT EXISTS (SELECT 1 FROM biz_account a WHERE a.account_code = t.code);
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0377 拒绝执行：上级科目 % 不存在。本迁移只翻 V0137/V0183 已种的一级科目，'
            '不新建上级 —— 请先确认科目表初始化已完成', v_missing;
    END IF;

    -- ② 软删的上级科目：翻它等于翻一个界面上看不见的科目（种出来的子科目也没人能用）
    SELECT string_agg(t.code, ' / ') INTO v_deleted
      FROM (VALUES ('4001'), ('4101'), ('2211')) AS t(code)
      JOIN biz_account a ON a.account_code = t.code
     WHERE a.deleted_at IS NOT NULL;
    IF v_deleted IS NOT NULL THEN
        RAISE EXCEPTION 'V0377 拒绝执行：上级科目 % 已被软删除。请在「会计科目」里恢复它们'
            '（或先确认这些科目确实不用了）再执行本迁移', v_deleted;
    END IF;

    -- ③ ★ 卡面验收：三科目历史分录非 0 ⇒ 不许翻（§7.2）
    --    只看「还需要翻」的那些（is_leaf = 1）：已经是非末级说明本迁移跑过，
    --    不必为它再拦一次（否则重复执行会因历史分录误报红）。
    v_blocked := NULL;
    FOR rec IN
        SELECT t.code AS code, a.is_leaf AS leaf_flag
          FROM (VALUES ('4001'), ('4101'), ('2211')) AS t(code)
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
        RAISE EXCEPTION 'V0377 拒绝执行：% —— 这些汇总科目还有历史分录，翻成非末级后这些金额会'
            '记在汇总科目上（报表按末级取数、年结按末级结转，会被漏结 ⇒ 跨年试算不平）。'
            '请先把这些分录改记到新的末级科目（4001.01~03 / 4101.01~04,09 / 2211.01）'
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
                 ('4001.01', '4001'), ('4001.02', '4001'), ('4001.03', '4001'),
                 ('4101.01', '4101'), ('4101.02', '4101'), ('4101.03', '4101'),
                 ('4101.04', '4101'), ('4101.09', '4101'),
                 ('2211.01', '2211')
               ) AS l(code, parent_code)
          JOIN biz_account a ON a.account_code = l.code
          LEFT JOIN biz_account p ON p.id = a.parent_id
         WHERE a.deleted_at IS NOT NULL
            OR a.is_leaf <> 1
            OR a.level <> p.level + 1
            OR p.account_code IS DISTINCT FROM l.parent_code
      ) x;
    IF v_conflict IS NOT NULL THEN
        RAISE EXCEPTION 'V0377 拒绝执行：末级科目编码已被占用且形态不符 —— %。'
            '本迁移不改已有的科目行（改名/改父级属于科目维护，得人来定），'
            '请人工核对后在「会计科目」里修正这些行，再重跑本迁移', v_conflict;
    END IF;

    RAISE NOTICE 'V0377 前置护栏通过：三科目存在且未软删、历史分录为 0、9 个末级编码无冲突';
END
$$;

-- ============================================================================
-- ① 三科目翻非末级（★ 只改 is_leaf / updated_at，不动名称、不动类别、不动 remark）
-- ============================================================================
UPDATE biz_account
   SET is_leaf    = 0,
       updated_at = CURRENT_TIMESTAMP
 WHERE account_code IN ('4001', '4101', '2211')
   AND deleted_at IS NULL
   AND is_leaf = 1;                     -- 幂等：翻过就是 0，第二遍 0 行

-- ============================================================================
-- ② 种 9 个末级科目（编码 / 名称 / 父级与设计 §3.3 ③、§1 D-14 逐字一致）
--
--   · 级次 = 父级级次 + 1（4001/4101/2211 都是 1 级 ⇒ 子科目 2 级）；
--   · 类别 / 余额方向一律**继承父级**（不从设计稿硬编码：父级若被客户改过类别，
--     硬编码会让「制造费用挂在资产下」这种烂账进报表）；
--   · status = 1（启用）、is_leaf = 1（可记账）、aux_dimensions 留空；
--   · remark 写播种来源（house style：V0149/V0158/V0160 的补种科目都带）。
-- ============================================================================
INSERT INTO biz_account (account_code, account_name, parent_id, level,
                         account_category, balance_direction, is_leaf, status, remark)
SELECT v.code,
       v.name,
       p.id,
       p.level + 1,
       p.account_category,
       p.balance_direction,
       1,
       1,
       'ARCH-9 成本核算升级 D-14 播种：成本末级科目（V0377）'
  FROM (VALUES
         ('4001.01', '生产成本-直接材料',   '4001'),
         ('4001.02', '生产成本-直接人工',   '4001'),
         ('4001.03', '生产成本-制造费用',   '4001'),
         ('4101.01', '制造费用-折旧费',     '4101'),
         ('4101.02', '制造费用-水电费',     '4101'),
         ('4101.03', '制造费用-修理费',     '4101'),
         ('4101.04', '制造费用-车间管理费', '4101'),
         ('4101.09', '制造费用-其他',       '4101'),
         ('2211.01', '应付职工薪酬-工资',   '2211')
       ) AS v(code, name, parent_code)
  JOIN biz_account p ON p.account_code = v.parent_code AND p.deleted_at IS NULL
ON CONFLICT (account_code) DO NOTHING;   -- 幂等：已存在（含 ARCH9-2 先种的 2211.01）就复用

-- ============================================================================
-- 自检 A：形态断言（科目树 / 9 个末级逐条 / 无裂树 / 无残留分录）
-- ============================================================================
DO $$
DECLARE
    v_parent_bad TEXT;
    v_leaf_bad   TEXT;
    v_name_diff  TEXT;
    v_children   BIGINT;
    v_cracked    BIGINT;
    v_entries    BIGINT;
BEGIN
    -- ① 三个上级科目：必须存在、未软删、is_leaf = 0
    SELECT string_agg(format('%s(is_leaf=%s%s)', a.account_code, a.is_leaf,
                             CASE WHEN a.deleted_at IS NOT NULL THEN ',已软删' ELSE '' END), ' / ')
      INTO v_parent_bad
      FROM biz_account a
     WHERE a.account_code IN ('4001', '4101', '2211')
       AND (a.deleted_at IS NOT NULL OR a.is_leaf <> 0);
    IF v_parent_bad IS NOT NULL THEN
        RAISE EXCEPTION 'V0377 自检失败：上级科目没翻成非末级 —— %（is_leaf 必须为 0，'
            '否则凭证闸仍允许直接记 4001/4101/2211）', v_parent_bad;
    END IF;

    -- ② 9 个末级科目逐条核对**结构**：存在 / 未软删 / 末级 / 级次 = 父级+1 / 父级编码正确 /
    --    类别与余额方向继承父级 / 状态启用
    SELECT string_agg(format('%s(%s)', x.code, x.why), ' / ') INTO v_leaf_bad
      FROM (
        SELECT l.code,
               CASE WHEN a.account_code IS NULL THEN '不存在'
                    WHEN a.deleted_at IS NOT NULL THEN '已软删'
                    WHEN a.is_leaf <> 1 THEN '不是末级（记账闸会拒）'
                    WHEN a.level <> p.level + 1 THEN 'level=' || a.level || '（应为 ' || (p.level + 1) || '）'
                    WHEN p.account_code IS DISTINCT FROM l.parent_code THEN
                         '父级=' || coalesce(p.account_code, '<空>') || '（应为 ' || l.parent_code || '）'
                    WHEN a.account_category <> p.account_category THEN
                         '类别=' || a.account_category || '（应继承父级 ' || p.account_category || '）'
                    WHEN a.balance_direction <> p.balance_direction THEN
                         '方向=' || a.balance_direction || '（应继承父级 ' || p.balance_direction || '）'
                    WHEN a.status <> 1 THEN '已停用（界面选不到，等于没种）'
               END AS why
          FROM (VALUES
                 ('4001.01', '4001'), ('4001.02', '4001'), ('4001.03', '4001'),
                 ('4101.01', '4101'), ('4101.02', '4101'), ('4101.03', '4101'),
                 ('4101.04', '4101'), ('4101.09', '4101'),
                 ('2211.01', '2211')
               ) AS l(code, parent_code)
          LEFT JOIN biz_account a ON a.account_code = l.code
          LEFT JOIN biz_account p ON p.account_code = l.parent_code
      ) x
     WHERE x.why IS NOT NULL;
    IF v_leaf_bad IS NOT NULL THEN
        RAISE EXCEPTION 'V0377 自检失败：末级科目形态不对 —— %', v_leaf_bad;
    END IF;

    -- ③ 三个上级科目下**恰好** 9 个末级子科目（多一个少一个都不行）
    SELECT count(*) INTO v_children
      FROM biz_account c
      JOIN biz_account p ON p.id = c.parent_id
     WHERE p.account_code IN ('4001', '4101', '2211')
       AND p.deleted_at IS NULL
       AND c.deleted_at IS NULL
       AND c.is_leaf = 1;
    IF v_children <> 9 THEN
        RAISE EXCEPTION 'V0377 自检失败：4001/4101/2211 下应恰有 9 个末级科目，实际 % 个'
            '（少了 = 种漏，多了 = 有人在这三个汇总科目下加了别的子科目，报表口径需人工确认）',
            v_children;
    END IF;

    -- ④ 全表不变量：**末级科目不许挂子科目**（翻非末级漏一个，就会造出裂树）
    SELECT count(*) INTO v_cracked
      FROM biz_account p
      JOIN biz_account c ON c.parent_id = p.id AND c.deleted_at IS NULL
     WHERE p.deleted_at IS NULL
       AND p.is_leaf = 1;
    IF v_cracked > 0 THEN
        RAISE EXCEPTION 'V0377 自检失败：还有 % 个「末级科目却挂着子科目」的裂树节点 —— '
            '凭证闸会把它当可记账、报表又把它当汇总，同一笔钱两头算', v_cracked;
    END IF;

    -- ⑤ 三个汇总科目上不许有分录（翻完再出现就说明有人绕过闸改了库）
    SELECT count(*) INTO v_entries
      FROM biz_voucher_entry e
     WHERE e.account_code IN ('4001', '4101', '2211');
    IF v_entries > 0 THEN
        RAISE EXCEPTION 'V0377 自检失败：汇总科目上仍有 % 条分录 —— 前置护栏没拦住，'
            '这些金额会掉出末级结算', v_entries;
    END IF;

    -- ⑥ 科目名与设计不符只在日志里点名（显示层差异，不挡住升级）
    SELECT string_agg(format('%s=「%s」', l.code, a.account_name), ' / ') INTO v_name_diff
      FROM biz_account a
      JOIN (VALUES
             ('4001.01', '生产成本-直接材料'), ('4001.02', '生产成本-直接人工'),
             ('4001.03', '生产成本-制造费用'), ('4101.01', '制造费用-折旧费'),
             ('4101.02', '制造费用-水电费'),   ('4101.03', '制造费用-修理费'),
             ('4101.04', '制造费用-车间管理费'), ('4101.09', '制造费用-其他'),
             ('2211.01', '应付职工薪酬-工资')
           ) AS l(code, name) ON l.code = a.account_code
     WHERE a.account_name <> l.name
       AND a.deleted_at IS NULL;
    IF v_name_diff IS NOT NULL THEN
        RAISE NOTICE 'V0377 提示：以下末级科目的名称与设计稿（§1 D-14）不同，已被客户改名？'
            '结构不受影响，仅提示：%', v_name_diff;
    END IF;

    RAISE NOTICE 'V0377 形态断言通过：4001/4101/2211 已翻非末级 + 9 个末级科目'
        '（编码/父级/级次/类别/方向/末级位/启用位 逐条核对）+ 无裂树 + 汇总科目零分录';
END
$$;

-- ============================================================================
-- 自检 B：口径断言（记账闸 / 报表映射 / 存量分录零改动）
--
-- 这一块断言的不是表结构，而是「翻完之后下游两个口径还对不对」——
-- 翻末级最容易犯的错是「翻了但没种够」，这时记账闸会拒到没人能记账（4001 非末级、
-- 4001.01 不存在），而报表还照样出 0，谁也发现不了。
-- ============================================================================
DO $$
DECLARE
    v_gate_reject BIGINT;
    v_gate_allow  BIGINT;
    v_map         BIGINT;
    v_entries     BIGINT;
BEGIN
    -- ① 记账闸的**拒绝面**：三个汇总科目必须全部落在 `is_leaf = 0` 那一边
    --    （voucher.rs::non_leaf_codes 的谓词就是 `deleted_at IS NULL AND is_leaf = 0`）
    SELECT count(*) INTO v_gate_reject
      FROM biz_account
     WHERE deleted_at IS NULL
       AND is_leaf = 0
       AND account_code IN ('4001', '4101', '2211');
    IF v_gate_reject <> 3 THEN
        RAISE EXCEPTION 'V0377 自检失败：记账闸只会拒 % 个汇总科目（应为 3）—— '
            '有科目仍是末级，界面上会允许「借 4001」这类汇总级分录', v_gate_reject;
    END IF;

    -- ② 记账闸的**放行面**：9 个末级科目必须全部可记账（is_leaf = 1 且启用且未软删）
    SELECT count(*) INTO v_gate_allow
      FROM biz_account
     WHERE deleted_at IS NULL
       AND is_leaf = 1
       AND status = 1
       AND account_code IN ('4001.01', '4001.02', '4001.03',
                            '4101.01', '4101.02', '4101.03', '4101.04', '4101.09',
                            '2211.01');
    IF v_gate_allow <> 9 THEN
        RAISE EXCEPTION 'V0377 自检失败：可记账的末级成本科目只有 % 个（应为 9）—— '
            '翻完末级却没种够，三张结转凭证（§4.7）会全部失败', v_gate_allow;
    END IF;

    -- ③ 报表口径：资产负债表「存货」项（V0183 INVENTORY）的科目模式里必须有 4001/4101。
    --    report.rs::match_account 用的是**前缀匹配**（`code == token || code.starts_with("token.")`），
    --    所以 4001.01 的余额会自动并进「存货」项，映射一行都不用改 ——
    --    但前提是模式里真的还有这两个前缀（有人把它删了就会静默掉出报表）。
    SELECT count(*) INTO v_map
      FROM biz_report_item
     WHERE report_code = 'BALANCE_SHEET'
       AND item_code = 'INVENTORY'
       AND account_pattern LIKE '%4001%'
       AND account_pattern LIKE '%4101%';
    IF v_map <> 1 THEN
        RAISE EXCEPTION 'V0377 自检失败：资产负债表「存货」项的科目模式不再同时包含 4001 与 4101 —— '
            '生产成本/制造费用余额会掉出「存货」（V0183:192 的 INVENTORY 映射被改动了？）';
    END IF;

    -- ④ 存量分录零改动：本迁移一条凭证分录都不许写（只动科目树）
    SELECT count(*) INTO v_entries
      FROM biz_voucher_entry
     WHERE account_code LIKE '4001.%'
        OR account_code LIKE '4101.%'
        OR account_code LIKE '2211.%';
    IF v_entries > 0 THEN
        RAISE NOTICE 'V0377 提示：新末级科目上已有 % 条分录（正常情况应为 0 —— 本迁移只种科目；'
            '非 0 说明执行前就有人记账到这些编码，请确认是否为客户已自行上线）', v_entries;
    END IF;

    RAISE NOTICE 'V0377 口径断言通过：汇总科目 3 个被记账闸拦、9 个末级科目可记账、'
        '存货报表项含 4001/4101（新叶子按前缀自动并入）、本迁移未写任何分录';
END
$$;
