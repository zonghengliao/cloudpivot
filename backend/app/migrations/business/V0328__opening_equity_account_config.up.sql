-- ======================================================
-- V0328：库存期初「对方科目」可配（FIX-6）
--
-- 现状（卡面 §二）：`opening.rs` 把库存期初的贷方科目写死 3103（本年利润），
-- 期初库存 18000 一律记进「未分配利润期初」。财务口径按账套不同（有人贷 3101 盈余公积、
-- 有人贷 3001 实收资本），写死等于每换一个账套就得改代码。
--
-- 本迁移只做一件事：种一条系统参数 `inventory_opening_equity_account`（默认 3103）。
-- 代码侧（`opening::inventory_equity_account_code`）读它决定：
--   · 生成期初凭证时「借 1405 / 贷 <该科目>」；
--   · 方式 A（科目期初）与方式 B（库存期初）的互斥校验比对的就是这个科目；
--   · 期初总览返回体的 `inventory_credit_account` 与提示文案。
--
-- 幂等：`ON CONFLICT (config_key) WHERE deleted_at IS NULL DO NOTHING` ——
-- 运维手工改过值/描述一律保留，本迁移不覆盖。
-- 回滚：down 删掉这个键（代码侧读不到就回退常量 3103，行为回到迁移前）。
-- ======================================================

INSERT INTO sys_config (config_key, config_value, config_type, description, category,
                        config_level, is_modifiable, is_encrypted, is_restart, status, sort_order)
VALUES (
    'inventory_opening_equity_account',
    '3103',
    'STRING',
    '库存期初的对方科目（贷方）：生成期初凭证时「借 1405 库存商品 / 贷 本科目」，'
    || '缺省 3103 本年利润；账套按「未分配利润」建账的填 3103，按「盈余公积」填 3101，'
    || '按「实收资本」填 3001。填的科目必须已在「会计科目」里启用且是末级科目',
    'erp',
    2,
    TRUE,
    FALSE,
    FALSE,
    1,
    60
)
ON CONFLICT (config_key) WHERE deleted_at IS NULL DO NOTHING;

-- ---------- 自检（失败即迁移失败 → 启动失败） ----------
DO $$
DECLARE
    value     TEXT;
    leaf      BOOLEAN;
BEGIN
    SELECT config_value INTO value
      FROM sys_config
     WHERE config_key = 'inventory_opening_equity_account' AND deleted_at IS NULL;
    IF value IS NULL OR btrim(value) = '' THEN
        RAISE EXCEPTION 'V0328 自检失败：inventory_opening_equity_account 没种上或为空（% ）',
            coalesce(value, '<null>');
    END IF;

    -- 缺省值必须指向一个真实存在的科目，否则「什么都不配」的新库第一次录库存期初就 400
    SELECT is_leaf INTO leaf FROM biz_account WHERE account_code = btrim(value);
    IF leaf IS NULL THEN
        RAISE NOTICE 'V0328 提示：缺省对方科目 %（本年利润）不在 biz_account 里，'
                     '录库存期初前请先在「会计科目」页启用它，或把本参数改成已存在的权益科目', value;
    ELSIF NOT leaf THEN
        RAISE NOTICE 'V0328 提示：缺省对方科目 % 不是末级科目，录库存期初时会被拒——请改成末级科目', value;
    END IF;
END
$$;
