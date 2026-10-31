-- ============================================================
-- V0158：应付付款（biz_payment）+ 4 个写权限码（批次 2 · P7）
--
-- 依据：
--   backend/src/business/erp/payable_payment.rs   （付款登记 / 应付作废）
--   backend/src/business/erp/voucher_write.rs     （凭证红冲 / 手工凭证）
-- 迁移号：V0157 已被 P5（stock_transfer_flow_seed）占用，本卡顺延到 V0158（卡面硬红线 3）。
--
-- # 为什么建 biz_payment 而不是复用别的表
--
--   开工 `\dt biz_*` 实测：库里**没有**任何付款 / 收款的记录表（只有 biz_payable 台账）。
--   而一张应付单要支持**多次部分付款**（卡面 §一 ②），付款次数、每次金额、日期、方式、
--   对应凭证号都必须留痕 —— 只靠 biz_payable.paid_amount 一个汇总值说不出「什么时候付了多少」。
--
-- # 为什么顺手补种科目 1002 银行存款
--
--   开工 `select account_code from biz_account` 实测只有 1405 / 2202 / 2221 / 2221.01 四个科目，
--   **没有 1002 银行存款**。付款凭证的口径是「借 2202 应付账款 / 贷 1002 银行存款」，
--   缺科目时付款接口会直接报「科目映射失败：科目编码 1002 不存在」—— 功能等于不可用。
--   所以本迁移幂等补种 1002（与 V0149 补种 2221 / 2221.01 同口径），不自创科目编码。
--
--   ⚠️ 与卡面的差异：卡面说「科目不存在要报明确错误」，本迁移**同时**保证默认科目存在，
--   让「标准口径」开箱可用；科目被停用 / 被改编码时接口仍然按卡面报明确错误。
--
-- # 权限码（sys_menu.auth_code 是唯一来源，本仓没有 sys_permission 表）
--
--   business:erp:payable:pay        POST /business/erp/payable/{id}/pay
--   business:erp:payable:cancel     POST /business/erp/payable/{id}/cancel
--   business:erp:voucher:reverse    POST /business/erp/voucher/{id}/reverse
--   business:erp:voucher:create     POST /business/erp/voucher
--
--   ID 段：开工 `select right(id::text,4) from sys_menu order by id` 全量扫过，
--   1009 / 1010（V0151 应付）之后 **1011 起为空段**，本迁移用 1011–1014。
--
-- # 幂等
--
--   全部 `CREATE TABLE IF NOT EXISTS` / `NOT EXISTS` 判重 / `ON CONFLICT (id) DO NOTHING`：
--   重复执行不覆盖运维改过的标题，也不会重复授权。
-- ============================================================

-- ---------- ① 付款记录表 ----------
CREATE TABLE IF NOT EXISTS biz_payment (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    payment_no  varchar(32)  NOT NULL,
    payable_id  uuid         NOT NULL REFERENCES biz_payable (id),
    amount      numeric(18,4) NOT NULL,
    pay_date    date         NOT NULL,
    pay_method  varchar(32),
    voucher_id  uuid,
    remark      text,
    created_at  timestamptz  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at  timestamptz  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by   uuid,
    update_by   uuid,
    deleted_at  timestamptz,
    CONSTRAINT ck_biz_payment_amount CHECK (amount > 0)
);

COMMENT ON TABLE biz_payment IS '应付付款记录（一张应付单可多次部分付款，P7/V0158）';
COMMENT ON COLUMN biz_payment.payment_no IS '付款单号 PM-yyyyMMdd-000001（biz_seq.seq_type=PM）';
COMMENT ON COLUMN biz_payment.voucher_id IS '本次付款生成的付款凭证（借 2202 应付账款 / 贷 1002 银行存款）';

CREATE UNIQUE INDEX IF NOT EXISTS uk_biz_payment_no
    ON biz_payment (payment_no)
    WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_biz_payment_payable
    ON biz_payment (payable_id, pay_date);

CREATE INDEX IF NOT EXISTS idx_biz_payment_voucher
    ON biz_payment (voucher_id)
    WHERE voucher_id IS NOT NULL;

-- ---------- ② 补种科目 1002 银行存款（付款凭证的贷方） ----------
INSERT INTO biz_account (account_code, account_name, parent_id, level, account_category,
                         balance_direction, is_leaf, status, remark)
SELECT '1002', '银行存款', NULL, 1, 'ASSET', 'DEBIT', 1, 1,
       'P7 付款凭证贷方科目（V0158 幂等补种，缺失时付款接口会报科目不存在）'
 WHERE NOT EXISTS (SELECT 1 FROM biz_account WHERE account_code = '1002');

-- ---------- ③ 4 个写权限码 ----------
-- 应付「付款」按钮（menu_type 3 = 按钮）
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001011'::uuid, 'business:erp:payable:pay', 3,
     'business:erp:payable:pay', NULL,
     '20000000-0000-0000-0000-000000001009'::uuid, NULL,
     2, 1, NULL, '应付付款登记', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- 应付「作废」按钮
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001012'::uuid, 'business:erp:payable:cancel', 3,
     'business:erp:payable:cancel', NULL,
     '20000000-0000-0000-0000-000000001009'::uuid, NULL,
     3, 1, NULL, '应付单作废', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- 凭证「红冲」按钮
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001013'::uuid, 'business:erp:voucher:reverse', 3,
     'business:erp:voucher:reverse', NULL,
     '20000000-0000-0000-0000-000000001004'::uuid, NULL,
     2, 1, NULL, '凭证红冲', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- 凭证「新增凭证」按钮
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000001014'::uuid, 'business:erp:voucher:create', 3,
     'business:erp:voucher:create', NULL,
     '20000000-0000-0000-0000-000000001004'::uuid, NULL,
     3, 1, NULL, '手工新增凭证', FALSE, 'business')
ON CONFLICT (id) DO NOTHING;

-- ---------- ④ 授权给已存在的角色（空库首启时是空操作） ----------
DO $$
DECLARE
    n_menus  integer;
    n_roles  integer;
    n_grants integer;
BEGIN
    SELECT count(*) INTO n_menus
      FROM sys_menu
     WHERE id IN ('20000000-0000-0000-0000-000000001011'::uuid,
                  '20000000-0000-0000-0000-000000001012'::uuid,
                  '20000000-0000-0000-0000-000000001013'::uuid,
                  '20000000-0000-0000-0000-000000001014'::uuid);
    IF n_menus <> 4 THEN
        RAISE EXCEPTION 'V0158 自检失败：预期 4 个权限节点，实际 %', n_menus;
    END IF;

    SELECT count(*) INTO n_roles FROM sys_role WHERE deleted_at IS NULL;
    IF n_roles = 0 THEN
        RAISE NOTICE 'V0158：sys_role 为空（空库首启，迁移先于 bootstrap），跳过授权；bootstrap 后重放本迁移即可补齐';
        RETURN;
    END IF;

    INSERT INTO sys_role_menu (id, role_id, menu_id)
    SELECT gen_random_uuid(), r.id, m.id
      FROM sys_role r
      CROSS JOIN (VALUES
            ('20000000-0000-0000-0000-000000001011'::uuid),
            ('20000000-0000-0000-0000-000000001012'::uuid),
            ('20000000-0000-0000-0000-000000001013'::uuid),
            ('20000000-0000-0000-0000-000000001014'::uuid)) AS m(id)
     WHERE r.deleted_at IS NULL
       AND r.role_code IN ('SUPER_ADMIN', 'ADMIN', 'TEST_UADMIN', 'FINANCE', 'ACCOUNTANT')
       AND NOT EXISTS (
            SELECT 1 FROM sys_role_menu rm WHERE rm.role_id = r.id AND rm.menu_id = m.id
       );
    GET DIAGNOSTICS n_grants = ROW_COUNT;
    RAISE NOTICE 'V0158：应付付款 / 凭证红冲 4 个权限码已种，授权新增 % 行（角色数 %）', n_grants, n_roles;
END $$;
