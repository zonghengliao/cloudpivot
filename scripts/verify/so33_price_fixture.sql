-- ======================================================
-- SO-33 取价解析（5 级）e2e 夹具 —— 库：cloudpivot_so33（cloudpivot_so32 的克隆，V0306）
--
-- 固定 uuid 前缀 f0330000-…，每条场景一一对应 price_resolve_e2e.sh 里的一次 curl。
-- 幂等：先按 id 清掉本卡夹具（价格行 → 档位行 → 物料 → 客户，顺序受 ON DELETE RESTRICT 约束），再重插。
--
-- 场景矩阵（见脚本注释）：
--   ① AGREED         C1+M1 qty=150 → 9.0000（多条命中取 min_qty=100 那条）
--   ① 取最近         C1+M1 qty=60  → 9.6000（同 min_qty=50，取 effective_from 更新的）
--   ① 有效期边界      C1+M1 qty=250 date=2026-03-31 → 8.5000（失效日当天仍有效）
--                    C1+M1 qty=250 date=2026-04-01 → 9.0000（失效日次日降级到 min_qty=100）
--   ② TIER_CUSTOMER  C1+M2 qty=50  → 6.5000（客户专属 PROMO 压过同段 TIER）
--   ② 右开边界        C1+M2 qty=100 → 8.0000（qty == max_qty 不命中档位，落通用阶梯）
--   ③ TIER_COMMON    C2+M2 qty=50  → 8.0000（C2 无专属档位）
--   ④ STANDARD       C2+M3 qty=10  → 12.5000（source_id = 物料 id）
--   ⑤ NONE           C2+M4 qty=10  → null
--   停用行           C1+M1 qty=300 → 不停 INACTIVE 的 1.0000
-- ======================================================

-- ---------- 清场（顺序不能颠倒：RESTRICT） ----------
DELETE FROM biz_customer_price
 WHERE customer_id IN ('f0330000-0000-4000-8000-000000000001'::uuid,
                       'f0330000-0000-4000-8000-000000000002'::uuid);
DELETE FROM biz_price_tier
 WHERE material_id IN ('f0330000-0000-4000-8000-000000000011'::uuid,
                       'f0330000-0000-4000-8000-000000000012'::uuid,
                       'f0330000-0000-4000-8000-000000000013'::uuid,
                       'f0330000-0000-4000-8000-000000000014'::uuid);
DELETE FROM biz_material
 WHERE id IN ('f0330000-0000-4000-8000-000000000011'::uuid,
              'f0330000-0000-4000-8000-000000000012'::uuid,
              'f0330000-0000-4000-8000-000000000013'::uuid,
              'f0330000-0000-4000-8000-000000000014'::uuid);
DELETE FROM biz_customer
 WHERE id IN ('f0330000-0000-4000-8000-000000000001'::uuid,
              'f0330000-0000-4000-8000-000000000002'::uuid);

-- ---------- 客户 ----------
INSERT INTO biz_customer (id, code, name, tax_no, credit_limit, credit_days, status, remark)
VALUES
    ('f0330000-0000-4000-8000-000000000001'::uuid, 'SO33-C1', 'SO33 客户A（有协议价）',
     NULL, 0, 0, 'ACTIVE', 'SO-33 取价 e2e 夹具'),
    ('f0330000-0000-4000-8000-000000000002'::uuid, 'SO33-C2', 'SO33 客户B（无协议价）',
     NULL, 0, 0, 'ACTIVE', 'SO-33 取价 e2e 夹具');

-- ---------- 物料（unit_id / 其它必填列沿用库里已有物料，避免造单位） ----------
INSERT INTO biz_material (id, material_code, material_name, unit_id, material_type,
                          safety_stock, costing_method, tax_rate, status, serial_required,
                          standard_price, remark)
SELECT v.id::uuid, v.code, v.name, m.unit_id, 'RAW', 0, 'MOVING_AVG', 0.1300, 1, false,
       v.standard_price, 'SO-33 取价 e2e 夹具'
  FROM (VALUES
        ('f0330000-0000-4000-8000-000000000011', 'SO33-M1', 'SO33 物料1（协议价 + 标准价）', 15.0000::numeric),
        ('f0330000-0000-4000-8000-000000000012', 'SO33-M2', 'SO33 物料2（档位价）',        20.0000::numeric),
        ('f0330000-0000-4000-8000-000000000013', 'SO33-M3', 'SO33 物料3（只有标准价）',    12.5000::numeric),
        ('f0330000-0000-4000-8000-000000000014', 'SO33-M4', 'SO33 物料4（一无所有）',      NULL::numeric)
       ) AS v(id, code, name, standard_price)
 CROSS JOIN (SELECT unit_id FROM biz_material WHERE deleted_at IS NULL ORDER BY material_code LIMIT 1) AS m;

-- ---------- ① 客户协议价（C1 × M1） ----------
INSERT INTO biz_customer_price (id, customer_id, material_id, price, min_qty,
                                effective_from, effective_to, status, remark)
VALUES
    -- 基础档：无起订量
    ('f0330000-0000-4000-8000-000000000101'::uuid,
     'f0330000-0000-4000-8000-000000000001'::uuid, 'f0330000-0000-4000-8000-000000000011'::uuid,
     10.0000, 0, '2025-01-01', NULL, 'ACTIVE', '无起订量基础协议价'),
    -- 100 起订：qty=150 时应取这条（min_qty 最大）
    ('f0330000-0000-4000-8000-000000000102'::uuid,
     'f0330000-0000-4000-8000-000000000001'::uuid, 'f0330000-0000-4000-8000-000000000011'::uuid,
     9.0000, 100, '2025-01-01', NULL, 'ACTIVE', '100 起订协议价'),
    -- 同 min_qty=50 的两条：旧价 9.8000 / 新价 9.6000 → 取新价
    ('f0330000-0000-4000-8000-000000000103'::uuid,
     'f0330000-0000-4000-8000-000000000001'::uuid, 'f0330000-0000-4000-8000-000000000011'::uuid,
     9.8000, 50, '2025-01-01', NULL, 'ACTIVE', '旧协议价（被 2026-06-01 那条取代）'),
    ('f0330000-0000-4000-8000-000000000104'::uuid,
     'f0330000-0000-4000-8000-000000000001'::uuid, 'f0330000-0000-4000-8000-000000000011'::uuid,
     9.6000, 50, '2026-06-01', NULL, 'ACTIVE', '新协议价（同起订量取最近生效）'),
    -- 有效期到 2026-03-31 的高量档：失效日当天仍有效，次日失效
    ('f0330000-0000-4000-8000-000000000105'::uuid,
     'f0330000-0000-4000-8000-000000000001'::uuid, 'f0330000-0000-4000-8000-000000000011'::uuid,
     8.5000, 200, '2025-01-01', '2026-03-31', 'ACTIVE', '限期高量协议价（失效日含）'),
    -- 停用行：价格最低但 status=INACTIVE，不参与取价
    ('f0330000-0000-4000-8000-000000000106'::uuid,
     'f0330000-0000-4000-8000-000000000001'::uuid, 'f0330000-0000-4000-8000-000000000011'::uuid,
     1.0000, 300, '2025-01-01', NULL, 'INACTIVE', '停用协议价（不得被命中）');

-- ---------- ② 客户专属阶梯（C1 × M2）：PROMO 压过同段 TIER ----------
INSERT INTO biz_price_tier (id, material_id, customer_id, tier_type, min_qty, max_qty,
                            price, effective_from, effective_to, status, remark)
VALUES
    ('f0330000-0000-4000-8000-000000000201'::uuid,
     'f0330000-0000-4000-8000-000000000012'::uuid, 'f0330000-0000-4000-8000-000000000001'::uuid,
     'TIER', 0, 100, 7.0000, '2026-01-01', NULL, 'ACTIVE', '客户专属阶梯'),
    ('f0330000-0000-4000-8000-000000000202'::uuid,
     'f0330000-0000-4000-8000-000000000012'::uuid, 'f0330000-0000-4000-8000-000000000001'::uuid,
     'PROMO', 0, 100, 6.5000, '2026-01-01', NULL, 'ACTIVE', '客户专属促销（同段优先于 TIER）'),
    -- ③ 通用阶梯（同一物料，customer_id IS NULL）
    ('f0330000-0000-4000-8000-000000000203'::uuid,
     'f0330000-0000-4000-8000-000000000012'::uuid, NULL,
     'TIER', 0, NULL, 8.0000, '2026-01-01', NULL, 'ACTIVE', '通用阶梯（无上限）');

-- ---------- e2e 前置：admin 的「首次登录必须改密」闸 ----------
-- 隔离库（cloudpivot_so33）里 admin 仍是初始口令状态，`session_guard` 会对除改密/登出外的
-- 全部接口回 403「首次登录必须先修改初始密码」。本卡验收用 admin 真实 token 直调接口，
-- 所以在**隔离库**里把该标志清掉（生产库绝不这么干；这也解释了为什么需要真实库实测）。
UPDATE sys_user SET must_change_password = FALSE WHERE username = 'admin';

-- ---------- 自检：夹具行数（任一不符 → psql 报错，脚本 stop） ----------
DO $$
DECLARE
    v_cus    integer;
    v_mat    integer;
    v_agreed integer;
    v_tier   integer;
BEGIN
    SELECT count(*) INTO v_cus FROM biz_customer
     WHERE id IN ('f0330000-0000-4000-8000-000000000001'::uuid,
                  'f0330000-0000-4000-8000-000000000002'::uuid);
    SELECT count(*) INTO v_mat FROM biz_material
     WHERE id IN ('f0330000-0000-4000-8000-000000000011'::uuid,
                  'f0330000-0000-4000-8000-000000000012'::uuid,
                  'f0330000-0000-4000-8000-000000000013'::uuid,
                  'f0330000-0000-4000-8000-000000000014'::uuid);
    SELECT count(*) INTO v_agreed FROM biz_customer_price
     WHERE customer_id = 'f0330000-0000-4000-8000-000000000001'::uuid
       AND deleted_at IS NULL;
    SELECT count(*) INTO v_tier FROM biz_price_tier
     WHERE material_id IN ('f0330000-0000-4000-8000-000000000011'::uuid,
                           'f0330000-0000-4000-8000-000000000012'::uuid)
       AND deleted_at IS NULL;
    IF v_cus <> 2 OR v_mat <> 4 OR v_agreed <> 6 OR v_tier <> 3 THEN
        RAISE EXCEPTION 'SO-33 夹具不全：客户=%（应 2）物料=%（应 4）协议价=%（应 6）档位=%（应 3）',
            v_cus, v_mat, v_agreed, v_tier;
    END IF;
    RAISE NOTICE 'SO-33 夹具就绪：客户 2 / 物料 4 / 协议价 6 / 档位 3';
END $$;
