-- ============================================================================
-- V0358：历史占用行回填 ——「压根没编预算」不再被标成「超支」（E2E06-FIX-3 ①，P4）
--
-- 卡：t_1cf59295（E2E06-FIX-3【P4×2】）
--
-- # 一、现场（E2E-06 演练发现）
--
--   FIX-5（V0325）之前 `BudgetVerdict::evaluate` 里 `missing_budget` 与 `over` 同时为真：
--   该槽压根没有本期生效的 ACTIVE 预算时，占用行落成
--
--       over_budget = 1 / over_amount = 全额
--
--   于是「没编预算」的历史单在执行报表 / 详情页的「超支」列里显示成超支（虚高超支槽）。
--   145 dev 库实测 5 行（EXP-20261107-000002/3/4/13/14），over_amount 全是全额。
--
-- # 二、修法：一次性回填，只动「确实没编预算」的行
--
--   判据（与 FIX-5 的口径一一对应）：
--     · `biz_budget_usage.budget_id IS NULL` —— 写入时没有命中任何预算版本 = 压根没编；
--     · `over_budget = 1` —— 只有旧判定会在这种行上置 1（FIX-5 之后 `over` 要求
--       `budget_id.is_some()`），所以这是「历史脏标记」的精确指纹。
--
--   **有 `budget_id` 且 `over_budget=1` 的行是真超支，一行不动**（B 组对照）。
--
--   顺带把单据头 `biz_expense_report` 的同一个脏标记也回填（否则报销列表页还是一排
--   「超预算」红标）：判据更严 —— 该单**所有**有效占用行都没绑预算，才认定「压根没编」，
--   置 `missing_budget = 1`（FIX-5 的互斥语义：没编预算不是超支）。有任何一行绑了预算就
--   跳过，宁可不修也不误改真超支。
--
-- # 三、备份（验收 ③：回填前有原值）
--
--   新建通用数据修复留档表 `biz_data_fix_backup`，把**将被改动的整行**以 `to_jsonb(row)`
--   存快照（`fix_code = 'E2E06-FIX-3'`）。这样回填是可逆的（down 直接按快照还原），
--   而且不依赖「3 行」这个会过期的数字 —— 现场实际是 5 行，换库可能更多或 0 行。
--
-- # 四、幂等
--
--   · 备份表 `CREATE TABLE IF NOT EXISTS`；快照 `ON CONFLICT DO NOTHING`；
--   · UPDATE 的谓词就是「脏标记」本身 —— 回填后不再命中，重复执行 0 行；
--   · 末尾自检：两类脏行都必须为 0，否则抛异常让迁移失败（不静默半修）。
--
-- # 五、回滚
--
--   down 按快照把 `over_budget / over_amount / missing_budget` 还原，再删本 fix_code 的快照。
-- ============================================================================

-- ---------- ① 通用数据修复留档表 ----------
CREATE TABLE IF NOT EXISTS biz_data_fix_backup (
    id           UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    fix_code     VARCHAR(32)  NOT NULL,
    table_name   VARCHAR(64)  NOT NULL,
    row_id       UUID         NOT NULL,
    row_data     JSONB        NOT NULL,
    backed_up_at TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,

    CONSTRAINT uk_biz_data_fix_backup UNIQUE (fix_code, table_name, row_id)
);

COMMENT ON TABLE biz_data_fix_backup IS
    '一次性数据修复的行级快照（fix_code 标识哪次修复；用于回滚与审计「改前长什么样」）';
COMMENT ON COLUMN biz_data_fix_backup.row_data IS
    'to_jsonb(整行) 快照；列结构变化后仍可读（JSONB 不跟随 DDL）';

-- ---------- ② 回填（快照 → 占用行 → 单据头 → 自检） ----------
DO $$
DECLARE
    usage_snap int;
    usage_upd  int;
    report_snap int;
    report_upd int;
    usage_left int;
    report_left int;
BEGIN
    -- 2.1 占用行：先存快照（同一谓词，同一事务，不会漏也不会多）
    INSERT INTO biz_data_fix_backup (fix_code, table_name, row_id, row_data)
    SELECT 'E2E06-FIX-3', 'biz_budget_usage', u.id, to_jsonb(u)
      FROM biz_budget_usage u
     WHERE u.over_budget = 1
       AND u.budget_id IS NULL
       AND u.deleted_at IS NULL
    ON CONFLICT (fix_code, table_name, row_id) DO NOTHING;
    GET DIAGNOSTICS usage_snap = ROW_COUNT;

    -- 2.2 占用行：没编预算 → 不超支（over_amount 超支才有值，所以是 NULL）
    UPDATE biz_budget_usage u
       SET over_budget = 0,
           over_amount = NULL,
           updated_at  = now()
     WHERE u.over_budget = 1
       AND u.budget_id IS NULL
       AND u.deleted_at IS NULL;
    GET DIAGNOSTICS usage_upd = ROW_COUNT;

    -- 2.3 单据头快照：该单有占用行，且**一行都没绑预算**（= 压根没编）
    INSERT INTO biz_data_fix_backup (fix_code, table_name, row_id, row_data)
    SELECT 'E2E06-FIX-3', 'biz_expense_report', r.id, to_jsonb(r)
      FROM biz_expense_report r
     WHERE r.over_budget = 1
       AND r.deleted_at IS NULL
       AND EXISTS (SELECT 1 FROM biz_budget_usage u
                    WHERE u.source_type = 'EXPENSE_REPORT' AND u.source_id = r.id
                      AND u.deleted_at IS NULL)
       AND NOT EXISTS (SELECT 1 FROM biz_budget_usage u
                        WHERE u.source_type = 'EXPENSE_REPORT' AND u.source_id = r.id
                          AND u.deleted_at IS NULL
                          AND u.budget_id IS NOT NULL)
    ON CONFLICT (fix_code, table_name, row_id) DO NOTHING;
    GET DIAGNOSTICS report_snap = ROW_COUNT;

    -- 2.4 单据头：改标「未编制预算」，超支额清空
    UPDATE biz_expense_report r
       SET over_budget    = 0,
           over_amount    = NULL,
           missing_budget = 1,
           updated_at     = now()
     WHERE r.over_budget = 1
       AND r.deleted_at IS NULL
       AND EXISTS (SELECT 1 FROM biz_budget_usage u
                    WHERE u.source_type = 'EXPENSE_REPORT' AND u.source_id = r.id
                      AND u.deleted_at IS NULL)
       AND NOT EXISTS (SELECT 1 FROM biz_budget_usage u
                        WHERE u.source_type = 'EXPENSE_REPORT' AND u.source_id = r.id
                          AND u.deleted_at IS NULL
                          AND u.budget_id IS NOT NULL);
    GET DIAGNOSTICS report_upd = ROW_COUNT;

    -- 2.5 自检：脏标记必须清零（真超支行不受影响，见下面的对照断言）
    SELECT count(*) INTO usage_left
      FROM biz_budget_usage
     WHERE over_budget = 1 AND budget_id IS NULL AND deleted_at IS NULL;
    IF usage_left <> 0 THEN
        RAISE EXCEPTION 'V0358 自检失败：仍有 % 行「未编预算却标超支」的占用行', usage_left;
    END IF;

    SELECT count(*) INTO report_left
      FROM biz_expense_report r
     WHERE r.over_budget = 1
       AND r.deleted_at IS NULL
       AND EXISTS (SELECT 1 FROM biz_budget_usage u
                    WHERE u.source_type = 'EXPENSE_REPORT' AND u.source_id = r.id
                      AND u.deleted_at IS NULL)
       AND NOT EXISTS (SELECT 1 FROM biz_budget_usage u
                        WHERE u.source_type = 'EXPENSE_REPORT' AND u.source_id = r.id
                          AND u.deleted_at IS NULL
                          AND u.budget_id IS NOT NULL);
    IF report_left <> 0 THEN
        RAISE EXCEPTION 'V0358 自检失败：仍有 % 张报销单「未编预算却标超支」', report_left;
    END IF;

    RAISE NOTICE 'V0358 回填完成：占用行 快照 % / 回填 %；报销单 快照 % / 回填 %',
        usage_snap, usage_upd, report_snap, report_upd;
END
$$;
