-- ============================================================================
-- V0379.down.sql —— 回滚成本号段 + 费用项目字典（对应 V0379__cost_seq_and_dict）
--
-- 顺序：字典项 → 字典类型 → 号段
--
-- 三道护栏（宁可 down 失败，也不留"半个字典 / 删了号段还在用"）：
--   ① `biz_seq.current_no > 0` ⇒ 已经发过号。删掉规则行后重建会从 000001 重来，
--      与库里已存在的 CB/CS 单号**重号**（成本对象号是业务主键观感，重号 = 两批货同号）。
--      要回滚请先确认没有在用的成本对象/批次单号。
--   ② 字典里出现本迁移之外的项（客户自己加过费用项目）⇒ 拒绝，先人工确认再删。
--   ③ 号段 row 的 seq_type 不是 CB/CS（说明主键被别的取号键复用了）⇒ 不删。
--
-- 幂等：删不到就是 0 行，重复 down 不报错。
-- ============================================================================

DO $$
DECLARE
    n_issued INT;
    txt      TEXT;
    n_extra  INT;
    n_item   INT;
    n_type   INT;
    n_seq    INT;
BEGIN
    -- ① 号段护栏
    SELECT count(*), string_agg(seq_type || '(current_no=' || current_no || ')', ', ')
      INTO n_issued, txt
      FROM biz_seq
     WHERE deleted_at IS NULL AND seq_type IN ('COST_OBJECT', 'COST_PERIOD') AND current_no > 0;
    IF n_issued > 0 THEN
        RAISE EXCEPTION 'V0379.down 拒绝执行：号段已发过号（%）—— 删除后新号会从 000001 重来造成重号。'
                        '请先确认没有在用的成本对象/成本批次单号', txt;
    END IF;

    -- ② 字典护栏：只删本迁移的 7 项；出现别的项就停
    SELECT count(*) INTO n_extra
      FROM sys_dict_item i JOIN sys_dict_type t ON t.id = i.dict_type_id
     WHERE i.deleted_at IS NULL AND t.dict_code = 'cost_expense_item'
       AND i.item_value NOT IN ('DEPRECIATION','UTILITY','RENT','REPAIR','WORKSHOP_ADMIN','CONSUMABLE','OTHER');
    IF n_extra > 0 THEN
        RAISE EXCEPTION 'V0379.down 拒绝执行：费用项目字典里有 % 项是本迁移之外的（客户自建），请先人工确认', n_extra;
    END IF;

    DELETE FROM sys_dict_item i
     USING sys_dict_type t
     WHERE i.dict_type_id = t.id AND t.dict_code = 'cost_expense_item'
       AND i.item_value IN ('DEPRECIATION','UTILITY','RENT','REPAIR','WORKSHOP_ADMIN','CONSUMABLE','OTHER');
    GET DIAGNOSTICS n_item = ROW_COUNT;

    DELETE FROM sys_dict_type WHERE dict_code = 'cost_expense_item';
    GET DIAGNOSTICS n_type = ROW_COUNT;

    -- ③ 号段：按 (id, seq_type) 成对删，避免误删别的取号键
    DELETE FROM biz_seq
     WHERE (id = '20000000-0000-4000-8000-00000000c201'::uuid AND seq_type = 'COST_OBJECT')
        OR (id = '20000000-0000-4000-8000-00000000c202'::uuid AND seq_type = 'COST_PERIOD');
    GET DIAGNOSTICS n_seq = ROW_COUNT;

    RAISE NOTICE 'V0379.down：已删除 % 个字典项、% 个字典类型、% 条号段', n_item, n_type, n_seq;
END $$;
