-- ============================================================================
-- V0221__erp_seed_accounting_period_year.down.sql
--
-- 回滚：删掉**本迁移种下的**当年期间行（V0221 up 的逆操作）。
--
-- # 只删「自己种的」
--
-- 新建行的 id 是确定性的：`md5('biz_accounting_period:<year>-<mm>')::uuid`。
-- down 就用这条公式**逐行重算 id 再比对**，因此：
--   · V0137 §5.4 那条字面 id（`d1370007-…-000000000001`）的 2026-10 → **不删**；
--   · 别的模块 / 运维手工 `INSERT` 的期间行 → **不删**；
--   · 已经被人改过（结账 / 备注）的种子行 → 见下一条。
--
-- # 有业务痕迹的行一律保留（回滚不是无损的，见 up 头注释）
--
-- 满足任一条件就跳过：
--   ① `status = 'CLOSED'` —— 结过账的月份删了会让「已封账」的事实消失；
--   ② 该期间有未删除的凭证 —— 删掉期间行会立刻把巡检 F2（凭证期间必有期间记录）
--      从绿打回 WARN，回滚不该制造新的账务问题。
--
-- 因此 down 是「幂等 + 保守」的：重复跑不会报错，第二次删到 0 行；
-- up → down → up 之后行数回到 12（全新库上实测，见 docs/evidence/20261104-p0-fix-5/）。
-- ============================================================================

DO $$
DECLARE
    removed int;
    kept    int;
BEGIN
    WITH candidate AS (
        SELECT p.id,
               (p.status = 'CLOSED'
                OR EXISTS (SELECT 1 FROM biz_voucher v
                            WHERE v.period = p.period_code
                              AND v.deleted_at IS NULL)) AS in_use
          FROM biz_accounting_period p
         WHERE p.id = md5('biz_accounting_period:'
                          || p.period_year::text || '-'
                          || lpad(p.period_month::text, 2, '0'))::uuid
    ), gone AS (
        DELETE FROM biz_accounting_period p
         USING candidate c
         WHERE p.id = c.id
           AND NOT c.in_use
        RETURNING p.id
    )
    SELECT (SELECT count(*) FROM gone),
           (SELECT count(*) FROM candidate WHERE in_use)
      INTO removed, kept;

    RAISE NOTICE 'V0221 down：删除种子期间 % 行，保留（已结账 / 有凭证）% 行', removed, kept;
END $$;
