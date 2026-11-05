-- ============================================================================
-- 40：把「已经用 psql 执行过的迁移」正式记账到 schema_migrations
--
-- 背景：生产 144 的检出停在 7d96692（迁移文件最大 V0246），V0247（P0-FIX-2）与
-- V0249（本卡）都是**用同一份仓库迁移文件 psql 直放**的，引擎没写过账。不记账的后果：
--   · 下次启动会认为它们「待应用」（V0249 幂等无副作用，V0247 亦幂等，但日志会撒谎）；
--   · 版本号「246 → 249」出现乱序补迁 WARN，排查时误导人。
--
-- 记账的合法形态（nik-db 的枚举硬约束，写错只在启动时炸）：
--   kind ∈ versioned|repeatable|seed|baseline   → versioned
--   applied_via ∈ executed|baseline|imported    → imported（内容确被执行过，但不是本引擎执行的）
-- checksum = up 脚本 sha256（与引擎 content_checksum 同口径：LF 归一化后 sha256sum），
-- 已用引擎在克隆库上实跑得到的值校验过（cloudpivot_e2e_iso：247/249 两个值逐字节相等）。
--
-- 幂等：ON CONFLICT (kind, name) DO NOTHING —— 已是引擎记录的库再跑不会覆盖。
-- ============================================================================
\pset pager off

INSERT INTO schema_migrations
       (kind, name, version, checksum, applied_via, applied_at, applied_seq, execution_ms, success, source)
SELECT 'versioned',
       'data_scope_minimal_grants',
       '247',
       'dda0287e0892c74810b5fb7b9665111a584a50e581084d0a1812437e64a021dc',
       'imported',
       extract(epoch FROM clock_timestamp())::bigint,
       (SELECT coalesce(max(applied_seq), 0) + 1 FROM schema_migrations),
       NULL, TRUE,
       'migrations/business/V0247__data_scope_minimal_grants.up.sql'
ON CONFLICT (kind, name) DO NOTHING;

INSERT INTO schema_migrations
       (kind, name, version, checksum, applied_via, applied_at, applied_seq, execution_ms, success, source)
SELECT 'versioned',
       'qa_fixture_role_scope_self_only',
       '249',
       '5962afa3bb37e312fe01e714736e1f3b1533a1cd1a3cbc5cdcd10c82f3efab61',
       'imported',
       extract(epoch FROM clock_timestamp())::bigint,
       (SELECT coalesce(max(applied_seq), 0) + 1 FROM schema_migrations),
       NULL, TRUE,
       'migrations/business/V0249__qa_fixture_role_scope_self_only.up.sql'
ON CONFLICT (kind, name) DO NOTHING;

DO $$
DECLARE
    v_n integer;
BEGIN
    SELECT count(*) INTO v_n
      FROM schema_migrations
     WHERE name IN ('data_scope_minimal_grants', 'qa_fixture_role_scope_self_only')
       AND kind = 'versioned' AND success;
    IF v_n <> 2 THEN
        RAISE EXCEPTION '记账后应有 2 行（V0247/V0249），实际 %', v_n;
    END IF;
    RAISE NOTICE '记账完成：V0247 / V0249 均已在 schema_migrations（applied_via=imported）';
END $$;

SELECT version, name, applied_via, checksum, applied_seq
  FROM schema_migrations
 WHERE name IN ('data_scope_minimal_grants', 'qa_fixture_role_scope_self_only')
 ORDER BY version;
