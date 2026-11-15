-- ============================================================================
-- V0379：成本核算升级 · 号段（biz_seq）+ 费用项目字典（sys_dict_type / sys_dict_item）
--
-- 卡：t_191cf205（COST-B1-7【成本】迁移 V0376/0377/0379）—— B1-7 第二件
-- 依据：docs/architecture/ARCH-9-成本核算升级设计.md
--       §7.1（设计稿把这块叫 V0377__cost_seq_and_dict：`biz_seq` 两条 CB/CS + 字典 1 类 7 项）、
--       §3.2（`biz_cost_overhead.expense_item` 走字典 `cost_expense_item`）、
--       实证脚本 docs/evidence/2026-11-10-arch9-3-cost-ddl/01_cost_ddl_check.sql §三 ⑤ ⑥。
--
-- # 号段为什么是 V0379
--   V0378 = B1-7 第一件（菜单权限），V0380 = B1-7 第三件（批号回填）。
--   设计稿编号已整体顺延 3 位（V0375/0376/0377 被 B1-5/B1-6 占用），见 V0378 文件头。
--
-- # 为什么只种两条号段
--   一期只有两种需要号码的单据/主数据：成本对象（CB）与成本核算批次（CS）。
--   凭证号（PZ）、入库（IN）、出库（OUT）等号段早已存在，成本模块复用，不重种。
--
-- # 为什么字典只种一个（cost_expense_item）
--   费用项目随行业扩展（客户会加「模具摊销」「烤版费」），落字典能免改代码；
--   而「成本要素」「分摊标准」是**算法分支**（代码必须认识才能算），落字典只会制造
--   「配了但算不了」的坑 —— 与 V0201「选了不会算错」的教训同源（设计 §7.1 原文）。
--   所以 `expense_item` 落字典，`cost_element` / `alloc_basis` 走 Rust 枚举 + 前端常量。
--
-- # 幂等
--   `biz_seq` 用 `ON CONFLICT (seq_type) WHERE deleted_at IS NULL DO NOTHING`
--   （**不是**设计稿写的 `ON CONFLICT (id)`）：uk_biz_seq_type 才是真正的唯一键，
--   客户环境里若已有一行 COST_OBJECT（不同 id，比如先跑过应用、ensure_seq_row 自动补过），
--   按 id 冲突会直接撞唯一索引报错；按 seq_type 冲突才是「已有就不动」。
--   —— 与 backend/src/business/erp/voucher.rs::ensure_seq_row、transfer_base.rs::ensure_seq_row
--      同一口径，重放/并发启动都安全。
--   字典：sys_dict_type 按 `ON CONFLICT (dict_code) WHERE deleted_at IS NULL`，
--         sys_dict_item 按 `ON CONFLICT (dict_type_id, item_value) WHERE deleted_at IS NULL`
--         （两处 uk_* 都是带软删条件的局部唯一索引）。
--
-- # 已知偏差（写在这里，免得下游卡再踩）
--   `biz_seq.date_pattern` 目前**只是给人看的列**：取号逻辑 `voucher::next_doc_no` 一律用
--   `period_key_of(now)` = 北京时间 yyyyMMdd 作周期键（serial_model.rs:432），不读 date_pattern。
--   ⇒ 本迁移按设计把 CS 的 date_pattern 种成 `yyyyMM`（语义：一个期间一张单，月滚），
--     但真正要「按月滚动」的实现得由 B3 的 CS 取号自带月度周期键（本卡不碰 voucher.rs）。
--     COST_OBJECT 的 CB 是天滚（yyyyMMdd），与现有实现完全一致，无此问题。
--
-- # 回滚
--   V0379.down.sql：删 2 条号段（带护栏：`current_no > 0` 说明已经发过号，删了会重号 → 拒绝）
--   + 删 7 个字典项 + 删字典类型（带护栏：字典里出现本迁移之外的项 → 拒绝，只删自己的）。
-- ============================================================================

-- ----------------------------------------------------------------------------
-- §0 前置护栏：两个固定 id 不许被别的 seq_type 占着（否则主键冲突会以看不懂的方式炸）
-- ----------------------------------------------------------------------------
DO $$
DECLARE
    n INT;
    txt TEXT;
BEGIN
    SELECT count(*), string_agg(seq_type, ', ') INTO n, txt
      FROM biz_seq
     WHERE id IN ('20000000-0000-4000-8000-00000000c201'::uuid,
                  '20000000-0000-4000-8000-00000000c202'::uuid)
       AND seq_type NOT IN ('COST_OBJECT', 'COST_PERIOD');
    IF n > 0 THEN
        RAISE EXCEPTION 'V0379 拒绝执行：号段主键已被其它取号键占用（%）。请先人工处理', txt;
    END IF;
END $$;

-- ----------------------------------------------------------------------------
-- §1 号段：COST_OBJECT（CB-yyyyMMdd-000001，天滚）/ COST_PERIOD（CS-yyyyMM-0001，月滚）
--     current_no 从 0 起（第一条号是 000001）；period_key 留空 = 「还没发过号」，
--     首次取号时 bump_seq 走「新周期 → 清零重来」分支，行为正确。
-- ----------------------------------------------------------------------------
INSERT INTO biz_seq (id, seq_type, seq_name, prefix, date_pattern, period_key, current_no,
                     step, separator, status, remark)
VALUES
  ('20000000-0000-4000-8000-00000000c201'::uuid, 'COST_OBJECT', '成本对象号', 'CB', 'yyyyMMdd', '', 0, 1, '-', 1,
   '成本对象号：CB-yyyyMMdd-000001，按天（CST）滚动（ARCH-9 成本核算）'),
  ('20000000-0000-4000-8000-00000000c202'::uuid, 'COST_PERIOD', '成本核算批次号', 'CS', 'yyyyMM', '', 0, 1, '-', 1,
   '成本核算批次号：CS-yyyyMM-0001，按月滚动（一个期间一张单；ARCH-9 成本核算）')
ON CONFLICT (seq_type) WHERE deleted_at IS NULL DO NOTHING;

-- ----------------------------------------------------------------------------
-- §2 字典：制造费用项目（cost_expense_item，7 项，默认「折旧费」）
--     与 biz_account 的 4101.01~4101.04 / 4101.09 末级科目对应（V0377 已种）：
--       折旧费 ↔ 4101.01 / 水电费 ↔ 4101.02 / 修理费 ↔ 4101.03 / 车间管理费 ↔ 4101.04
--       其余（租金 / 机物料消耗 / 其他）由客户按需映射到 4101.09 或自建末级科目。
-- ----------------------------------------------------------------------------
INSERT INTO sys_dict_type (dict_code, dict_name, description, status, sort_order)
VALUES ('cost_expense_item', '制造费用项目',
        'ARCH-9 预置：折旧 / 水电 / 租金 / 修理 / 车间管理 / 机物料 / 其他（成本费用台账的费用项目下拉）', 1, 110)
ON CONFLICT (dict_code) WHERE deleted_at IS NULL DO NOTHING;

INSERT INTO sys_dict_item (dict_type_id, item_value, item_label, description, sort_order, is_default, status)
SELECT t.id, v.item_value, v.item_label, v.description, v.sort_order, v.is_default, 1
  FROM sys_dict_type t
 CROSS JOIN (VALUES
        ('DEPRECIATION',   '折旧费',     '对应科目 4101.01 制造费用-折旧费',   10, TRUE),
        ('UTILITY',        '水电费',     '对应科目 4101.02 制造费用-水电费',   20, FALSE),
        ('RENT',           '租金',       '厂房/设备租金（按需映射 4101.09）',  30, FALSE),
        ('REPAIR',         '修理费',     '对应科目 4101.03 制造费用-修理费',   40, FALSE),
        ('WORKSHOP_ADMIN', '车间管理费', '对应科目 4101.04 制造费用-车间管理费', 50, FALSE),
        ('CONSUMABLE',     '机物料消耗', '低值易耗 / 机物料（按需映射 4101.09）', 60, FALSE),
        ('OTHER',          '其他',       '未归类的车间费用（映射 4101.09）',    70, FALSE)
       ) AS v(item_value, item_label, description, sort_order, is_default)
 WHERE t.dict_code = 'cost_expense_item' AND t.deleted_at IS NULL
ON CONFLICT (dict_type_id, item_value) WHERE deleted_at IS NULL DO NOTHING;

-- ----------------------------------------------------------------------------
-- §3 自检（任一不符 → 迁移失败）
-- ----------------------------------------------------------------------------
DO $$
DECLARE
    n_seq      INT;
    n_bad_seq  INT;
    n_type     INT;
    n_item     INT;
    n_bad_item INT;
    n_default  INT;
    txt        TEXT;
BEGIN
    -- 3.1 两条号段齐、形态对（前缀 / 日期段 / 状态 / 步长 / 计数从 0 起）
    SELECT count(*) INTO n_seq FROM biz_seq
     WHERE deleted_at IS NULL AND seq_type IN ('COST_OBJECT', 'COST_PERIOD');
    IF n_seq <> 2 THEN
        RAISE EXCEPTION 'V0379 自检失败：成本号段应有 2 条（COST_OBJECT / COST_PERIOD），实际 %', n_seq;
    END IF;

    SELECT count(*) INTO n_bad_seq FROM biz_seq
     WHERE deleted_at IS NULL
       AND (   (seq_type = 'COST_OBJECT' AND (prefix <> 'CB' OR date_pattern <> 'yyyyMMdd'))
            OR (seq_type = 'COST_PERIOD' AND (prefix <> 'CS' OR date_pattern <> 'yyyyMM'))
            OR status <> 1 OR step <> 1 OR separator <> '-' OR current_no < 0 OR seq_name = '')
       AND seq_type IN ('COST_OBJECT', 'COST_PERIOD');
    IF n_bad_seq > 0 THEN
        RAISE EXCEPTION 'V0379 自检失败：% 条成本号段的形态不对（前缀/日期段/状态/步长/分隔符）', n_bad_seq;
    END IF;

    -- 3.2 字典类型在且启用
    SELECT count(*) INTO n_type FROM sys_dict_type
     WHERE deleted_at IS NULL AND dict_code = 'cost_expense_item' AND status = 1;
    IF n_type <> 1 THEN
        RAISE EXCEPTION 'V0379 自检失败：字典类型 cost_expense_item 应有且只有 1 条启用的，实际 %', n_type;
    END IF;

    -- 3.3 7 个字典项，值/标签/顺序逐条对齐设计（客户自己加项不影响本断言）
    SELECT count(*) INTO n_item
      FROM sys_dict_item i JOIN sys_dict_type t ON t.id = i.dict_type_id
     WHERE i.deleted_at IS NULL AND t.deleted_at IS NULL AND t.dict_code = 'cost_expense_item';
    IF n_item <> 7 THEN
        RAISE EXCEPTION 'V0379 自检失败：费用项目字典应有 7 项，实际 %', n_item;
    END IF;

    SELECT count(*), string_agg(v.item_value || '/' || coalesce(i.item_label, '<缺>'), ', ')
      INTO n_bad_item, txt
      FROM (VALUES ('DEPRECIATION','折旧费',10),('UTILITY','水电费',20),('RENT','租金',30),
                   ('REPAIR','修理费',40),('WORKSHOP_ADMIN','车间管理费',50),
                   ('CONSUMABLE','机物料消耗',60),('OTHER','其他',70))
           AS v(item_value, item_label, sort_order)
      LEFT JOIN sys_dict_type t ON t.dict_code = 'cost_expense_item' AND t.deleted_at IS NULL
      LEFT JOIN sys_dict_item i ON i.dict_type_id = t.id AND i.item_value = v.item_value
                               AND i.deleted_at IS NULL
     WHERE i.id IS NULL OR i.item_label <> v.item_label OR i.sort_order <> v.sort_order OR i.status <> 1;
    IF n_bad_item > 0 THEN
        RAISE EXCEPTION 'V0379 自检失败：% 个费用项目字典项与设计不一致（标签/顺序/状态）：%', n_bad_item, txt;
    END IF;

    -- 3.4 默认项唯一且是「折旧费」（前端 DictSelect 会把它作为新建台账的初值）
    SELECT count(*) INTO n_default
      FROM sys_dict_item i JOIN sys_dict_type t ON t.id = i.dict_type_id
     WHERE i.deleted_at IS NULL AND t.dict_code = 'cost_expense_item' AND i.is_default;
    IF n_default <> 1 THEN
        RAISE EXCEPTION 'V0379 自检失败：费用项目字典的默认项应恰好 1 个，实际 %', n_default;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM sys_dict_item i JOIN sys_dict_type t ON t.id = i.dict_type_id
                    WHERE i.deleted_at IS NULL AND t.dict_code = 'cost_expense_item'
                      AND i.is_default AND i.item_value = 'DEPRECIATION') THEN
        RAISE EXCEPTION 'V0379 自检失败：费用项目字典的默认项不是 DEPRECIATION（折旧费）';
    END IF;

    RAISE NOTICE 'V0379：号段 CB/CS + 费用项目字典 7 项 已就绪';
END $$;
