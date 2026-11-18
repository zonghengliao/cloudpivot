-- ============================================================================
-- V0463.down：标准成本版本护栏回滚（M5-08）
--
-- 只拆本卡加的东西，并**把 V0374 的 uk_biz_cost_std 建回来**（本卡把它换成了带
-- `status = 1` 谓词的 uk_biz_cost_std_active_start，见 up 头注释 ②）：
--   ① ex_biz_cost_std_active_window（启用版本区间排除约束）
--   ② uk_biz_cost_std_active_start（仅启用行同日唯一）+ uk_biz_cost_std_ver + idx_biz_cost_std_window
--   ③ 恢复 uk_biz_cost_std（同物料同生效日唯一，连失效行一起锁）
--
-- 为什么恢复 ③ 可能失败、失败就报错：旧索引连**失效**行一起锁，而 up 之后数据里
-- 完全可能存在「同物料同生效日、一条失效一条启用」（这正是本卡放开的能力）。
-- 这种数据在 down 出来之前必须人工清理 —— 宁可让 down 明确失败，也不静默丢掉那些行。
-- 报文给出定位查询。
--
-- 为什么不删 btree_gist：V0336 / V0407 同口径 —— 扩展是库级共享对象，
-- 别的表（biz_exchange_rate、flow_delegate…）可能也在用，拆一个下游不该拆上游。
--
-- 为什么不拦数据：这些对象都是**护栏**不是数据；拆掉后 V0374 的表结构（除索引名外）
-- 与行一行不变，重新 up 会原样重建（探针自检也会再跑一遍）。
-- ============================================================================

-- ① 区间排除约束（btree_gist 缺失时本来就没建，IF EXISTS 保证重放安全）
ALTER TABLE biz_cost_standard
    DROP CONSTRAINT IF EXISTS ex_biz_cost_std_active_window;

-- ② 本卡加的索引
DROP INDEX IF EXISTS uk_biz_cost_std_active_start;
DROP INDEX IF EXISTS uk_biz_cost_std_ver;
DROP INDEX IF EXISTS idx_biz_cost_std_window;

-- ③ 把 V0374 的 uk_biz_cost_std 建回来（失败 = 数据里有「同日一条启用一条失效」，需人工处理）
DO $$
BEGIN
    IF to_regclass('public.uk_biz_cost_std') IS NULL THEN
        BEGIN
            CREATE UNIQUE INDEX uk_biz_cost_std
                ON biz_cost_standard (material_id, effective_from) WHERE deleted_at IS NULL;
        EXCEPTION WHEN unique_violation THEN
            RAISE EXCEPTION 'V0463 down 拒绝回滚：数据里有「同物料同生效日」的多行（含失效行），'
                '旧索引 uk_biz_cost_std 建不回来。先人工确认这些行（下面查询），删掉或改生效日后再回滚：'
                'SELECT material_id, effective_from, count(*) FROM biz_cost_standard '
                'WHERE deleted_at IS NULL GROUP BY 1, 2 HAVING count(*) > 1;';
        END;
    END IF;
END $$;

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_constraint
                WHERE conrelid = 'public.biz_cost_standard'::regclass
                  AND conname = 'ex_biz_cost_std_active_window')
       OR to_regclass('public.uk_biz_cost_std_active_start') IS NOT NULL
       OR to_regclass('public.uk_biz_cost_std_ver') IS NOT NULL
       OR to_regclass('public.idx_biz_cost_std_window') IS NOT NULL
       OR to_regclass('public.uk_biz_cost_std') IS NULL THEN
        RAISE EXCEPTION 'V0463 down 失败：护栏对象状态不对（本卡索引未拆干净，或 uk_biz_cost_std 未恢复）';
    END IF;
    RAISE NOTICE 'V0463 down 完成：本卡护栏已拆，V0374 的 uk_biz_cost_std 已恢复，表与数据未动';
END $$;
