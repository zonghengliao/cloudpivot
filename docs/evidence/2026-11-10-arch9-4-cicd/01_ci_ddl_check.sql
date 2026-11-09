-- ARCH-9-4 CI/CD 设计文档 §3 的 DDL 真库验证（整段在事务里跑，末尾 ROLLBACK，零残留）
-- 库：cloudpivot_arch9_ci4（99 本机 PG 18.6，已迁到 V0367）
-- 跑法：PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_arch9_ci4 -v ON_ERROR_STOP=1 -f 01_ci_ddl_check.sql
\set ON_ERROR_STOP on
\timing off
BEGIN;

\echo '=== ① 建表 sys_ci_run（一次运行一行）==='
CREATE TABLE sys_ci_run (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    run_at TIMESTAMPTZ NOT NULL,
    layer VARCHAR(8) NOT NULL,
    trigger_type VARCHAR(16) NOT NULL,
    git_head VARCHAR(40) NOT NULL,
    git_branch VARCHAR(128) NOT NULL DEFAULT '',
    worktree_dirty BOOLEAN NOT NULL DEFAULT false,
    toolchain VARCHAR(64) NOT NULL DEFAULT '',
    host VARCHAR(64) NOT NULL DEFAULT '',
    database_name VARCHAR(128) NOT NULL DEFAULT '',
    status VARCHAR(16) NOT NULL,
    total INT NOT NULL DEFAULT 0,
    passed INT NOT NULL DEFAULT 0,
    failed INT NOT NULL DEFAULT 0,
    skipped INT NOT NULL DEFAULT 0,
    baselined INT NOT NULL DEFAULT 0,
    duration_ms INT NOT NULL DEFAULT 0,
    log_dir TEXT NOT NULL DEFAULT '',
    note TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);
COMMENT ON TABLE sys_ci_run IS 'CI/回归一次运行一行（L0/L1/L2/L3；门禁 + 脚本 + e2e 的汇总）';
COMMENT ON COLUMN sys_ci_run.layer IS '层次：L0 提交前 / L1 push 前 / L2 每日 / L3 发版前';
COMMENT ON COLUMN sys_ci_run.trigger_type IS '触发方式：SCHEDULED / HOOK / MANUAL / DEPLOY';
COMMENT ON COLUMN sys_ci_run.worktree_dirty IS '运行时工作区是否有未提交改动（脏树跑的结论不可用于发版）';
COMMENT ON COLUMN sys_ci_run.baselined IS '命中「已知红台账」的失败项数（不阻断，但必须在 sys_ci_baseline 有行）';
COMMENT ON COLUMN sys_ci_run.log_dir IS '本次运行的日志目录（脚本 stdout/stderr 落盘处）';

ALTER TABLE sys_ci_run
    ADD CONSTRAINT ck_ci_run_layer CHECK (layer IN ('L0', 'L1', 'L2', 'L3')),
    ADD CONSTRAINT ck_ci_run_trigger CHECK (trigger_type IN ('SCHEDULED', 'HOOK', 'MANUAL', 'DEPLOY')),
    ADD CONSTRAINT ck_ci_run_status CHECK (status IN ('RUNNING', 'OK', 'FAIL', 'ERROR', 'ABORTED')),
    ADD CONSTRAINT ck_ci_run_counts CHECK (passed >= 0 AND failed >= 0 AND skipped >= 0 AND baselined >= 0
                                          AND passed + failed + skipped + baselined <= total);

CREATE INDEX idx_ci_run_at ON sys_ci_run (run_at DESC);
CREATE INDEX idx_ci_run_not_ok ON sys_ci_run (run_at DESC) WHERE status <> 'OK';
-- 同一台主机同一层同一个自然日（北京时间）只允许一次定时运行：两次 cron 撞车/重跑会把趋势图打歪
--   ★ host 必须在键里：L2 天然分两段跑在不同机器上（145 跑门禁与 13 域抽样、99 跑真库全量单测，
--     因为 test 二进制链接实测要 7.7 GiB RSS，145 只有 7 G 内存必然 OOM）。不带 host 的话
--     第二段会被唯一索引直接挡死 —— 这是真库跑断言时抓出来的第一个设计错误。
CREATE UNIQUE INDEX uk_ci_run_daily ON sys_ci_run (layer, host, ((run_at AT TIME ZONE 'Asia/Shanghai')::date))
    WHERE trigger_type = 'SCHEDULED' AND status <> 'ABORTED';

\echo '=== ② 建表 sys_ci_item（一项门禁/脚本/用例一行）==='
CREATE TABLE sys_ci_item (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    run_id UUID NOT NULL REFERENCES sys_ci_run (id) ON DELETE CASCADE,
    item_key VARCHAR(64) NOT NULL,
    domain VARCHAR(24) NOT NULL DEFAULT '',
    kind VARCHAR(16) NOT NULL,
    required BOOLEAN NOT NULL DEFAULT true,
    status VARCHAR(16) NOT NULL,
    baseline BOOLEAN NOT NULL DEFAULT false,
    rc INT,
    duration_ms INT NOT NULL DEFAULT 0,
    failures INT NOT NULL DEFAULT 0,
    first_line TEXT,
    log_path TEXT,
    log_sha256 CHAR(64),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);
COMMENT ON TABLE sys_ci_item IS 'CI 运行里的一项（门禁 / 演练脚本 / e2e 用例 / 真库单测分片）；append-only';
COMMENT ON COLUMN sys_ci_item.item_key IS '稳定标识：fmt / clippy / erp_invariants / menu_invariants / shop:e2e04_finance / e2e:05-approval.spec.ts';
COMMENT ON COLUMN sys_ci_item.domain IS '所属域（procurement/sales/inventory/…）或 gate；用于「13 域覆盖率」统计';
COMMENT ON COLUMN sys_ci_item.required IS '是否必过：false = 抽样轮到的可选脚本（红了只记录，不阻断 L2）';
COMMENT ON COLUMN sys_ci_item.baseline IS '是否命中已知红台账（sys_ci_baseline 有未撤销未过期的行）';
COMMENT ON COLUMN sys_ci_item.first_line IS '失败首行（裁剪到 500 字，不含 token / 口令）';

ALTER TABLE sys_ci_item
    ADD CONSTRAINT ck_ci_item_kind CHECK (kind IN ('GATE', 'SCRIPT', 'E2E', 'DBTEST')),
    ADD CONSTRAINT ck_ci_item_status CHECK (status IN ('PASS', 'FAIL', 'SKIP', 'ERROR'));

CREATE UNIQUE INDEX uk_ci_item_run_key ON sys_ci_item (run_id, item_key);
CREATE INDEX idx_ci_item_key_time ON sys_ci_item (item_key, created_at DESC);

\echo '=== ③ 建表 sys_ci_baseline（已知红台账：谁认领、到什么时候、理由）==='
CREATE TABLE sys_ci_baseline (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    item_key VARCHAR(64) NOT NULL,
    reason TEXT NOT NULL,
    owner VARCHAR(64) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ
);
COMMENT ON TABLE sys_ci_baseline IS '已知红台账：门禁长期红的显式认领（有理由 + 有到期 + 有人），过期即失效';
COMMENT ON COLUMN sys_ci_baseline.expires_at IS '到期时间；查询侧判 expires_at > now()，过期行不参与 baseline 判定';
COMMENT ON COLUMN sys_ci_baseline.revoked_at IS '主动撤销时间（修好后清账，行保留可审计）';

ALTER TABLE sys_ci_baseline
    ADD CONSTRAINT ck_ci_baseline_reason CHECK (length(btrim(reason)) >= 8),
    ADD CONSTRAINT ck_ci_baseline_window CHECK (expires_at > created_at),
    ADD CONSTRAINT ck_ci_baseline_max_window CHECK (expires_at <= created_at + interval '90 days');

CREATE UNIQUE INDEX uk_ci_baseline_key ON sys_ci_baseline (item_key) WHERE revoked_at IS NULL;
CREATE INDEX idx_ci_baseline_active ON sys_ci_baseline (expires_at) WHERE revoked_at IS NULL;

--------------------------------------------------------------------------------
\echo '=== 断言 A：三张表都在 ==='
DO $$
DECLARE n int;
BEGIN
    SELECT count(*) INTO n FROM information_schema.tables
     WHERE table_schema = 'public' AND table_name IN ('sys_ci_run', 'sys_ci_item', 'sys_ci_baseline');
    IF n <> 3 THEN RAISE EXCEPTION '断言A失败：只建出 % 张表', n; END IF;
    RAISE NOTICE '断言A通过：3 张表';
END $$;

\echo '=== 断言 B：正常插一行 run + 两行 item ==='
INSERT INTO sys_ci_run (run_at, layer, trigger_type, git_head, status, total, passed, failed, skipped, baselined, duration_ms, database_name)
VALUES ('2026-11-10 03:10:00+08', 'L2', 'SCHEDULED', 'b252db2e', 'RUNNING', 0, 0, 0, 0, 0, 0, 'cloudpivot_ci');
INSERT INTO sys_ci_run (run_at, layer, trigger_type, git_head, status, total, passed, failed, skipped, baselined, duration_ms, database_name)
VALUES ('2026-11-10 01:20:00+08', 'L1', 'HOOK', 'b252db2e', 'OK', 3, 3, 0, 0, 0, 121000, '');
INSERT INTO sys_ci_item (run_id, item_key, domain, kind, status, duration_ms)
SELECT id, 'fmt', 'gate', 'GATE', 'PASS', 3100 FROM sys_ci_run WHERE layer = 'L2'
   AND run_at = '2026-11-10 03:10:00+08';
INSERT INTO sys_ci_item (run_id, item_key, domain, kind, status, duration_ms, failures, first_line)
SELECT id, 'biz_type_domain', 'gate', 'GATE', 'FAIL', 760, 3, '域外值 PR_APPROVE × 1'
  FROM sys_ci_run WHERE layer = 'L2' AND run_at = '2026-11-10 03:10:00+08';
DO $$
DECLARE n int;
BEGIN
    SELECT count(*) INTO n FROM sys_ci_item;
    IF n <> 2 THEN RAISE EXCEPTION '断言B失败：item 行数 = %', n; END IF;
    RAISE NOTICE '断言B通过：run 2 行 / item 2 行';
END $$;

\echo '=== 断言 C：同主机同日第二次 SCHEDULED 被拒；换一台主机则允许（L2 双段）==='
DO $$
BEGIN
    BEGIN
        INSERT INTO sys_ci_run (run_at, layer, trigger_type, git_head, status)
        VALUES ('2026-11-10 03:40:00+08', 'L2', 'SCHEDULED', 'b252db2e', 'RUNNING');
        RAISE EXCEPTION '断言C失败：uk_ci_run_daily 没生效（同日同层同主机重复插入成功）';
    EXCEPTION WHEN unique_violation THEN
        RAISE NOTICE '断言C-1通过：同日同层同主机第二次定时运行被拒';
    END;
    INSERT INTO sys_ci_run (run_at, layer, trigger_type, git_head, status, host)
    VALUES ('2026-11-10 03:40:00+08', 'L2', 'SCHEDULED', 'b252db2e', 'RUNNING', '99');
    RAISE NOTICE '断言C-2通过：同一天另一台主机的 L2 段允许（145 门禁段 + 99 单测段）';
END $$;

\echo '=== 断言 D：layer / status 非法值被 CHECK 拒绝 ==='
DO $$
BEGIN
    BEGIN
        INSERT INTO sys_ci_run (run_at, layer, trigger_type, git_head, status) VALUES (now(), 'L9', 'MANUAL', 'x', 'OK');
        RAISE EXCEPTION '断言D失败：layer=L9 插进去了';
    EXCEPTION WHEN check_violation THEN RAISE NOTICE '断言D-1通过：layer 非法值被拒'; END;
    BEGIN
        INSERT INTO sys_ci_run (run_at, layer, trigger_type, git_head, status) VALUES (now(), 'L2', 'MANUAL', 'x', 'GREEN');
        RAISE EXCEPTION '断言D失败：status=GREEN 插进去了';
    EXCEPTION WHEN check_violation THEN RAISE NOTICE '断言D-2通过：status 非法值被拒'; END;
END $$;

\echo '=== 断言 E：counts 一致性 CHECK（passed+failed+skipped+baselined <= total）==='
DO $$
BEGIN
    BEGIN
        INSERT INTO sys_ci_run (run_at, layer, trigger_type, git_head, status, total, passed, failed)
        VALUES (now(), 'L1', 'MANUAL', 'x', 'OK', 2, 2, 1);
        RAISE EXCEPTION '断言E失败：counts 不一致仍被插入';
    EXCEPTION WHEN check_violation THEN RAISE NOTICE '断言E通过：计数不一致被拒'; END;
END $$;

\echo '=== 断言 F：(run_id, item_key) 唯一：同一运行同一项不能两行 ==='
DO $$
DECLARE rid uuid;
BEGIN
    SELECT id INTO rid FROM sys_ci_run WHERE layer = 'L2' AND run_at = '2026-11-10 03:10:00+08';
    BEGIN
        INSERT INTO sys_ci_item (run_id, item_key, domain, kind, status) VALUES (rid, 'fmt', 'gate', 'GATE', 'FAIL');
        RAISE EXCEPTION '断言F失败：同一 run 同一 item_key 重复插入成功';
    EXCEPTION WHEN unique_violation THEN RAISE NOTICE '断言F通过：重复 item 被拒'; END;
END $$;

\echo '=== 断言 G：删 run 级联删 item（ON DELETE CASCADE）==='
DO $$
DECLARE rid uuid; n int;
BEGIN
    SELECT id INTO rid FROM sys_ci_run WHERE layer = 'L2' AND run_at = '2026-11-10 03:10:00+08';
    DELETE FROM sys_ci_run WHERE id = rid;
    SELECT count(*) INTO n FROM sys_ci_item;
    IF n <> 0 THEN RAISE EXCEPTION '断言G失败：级联后仍剩 % 行 item', n; END IF;
    RAISE NOTICE '断言G通过：run 删除后 item 级联清空';
END $$;

\echo '=== 断言 H：baseline 的理由太短 / 窗口越界被拒 ==='
DO $$
BEGIN
    BEGIN
        INSERT INTO sys_ci_baseline (item_key, reason, owner, expires_at) VALUES ('scope_gate', '暂时', 'backend', now() + interval '7 days');
        RAISE EXCEPTION '断言H失败：理由 <8 字仍插入成功';
    EXCEPTION WHEN check_violation THEN RAISE NOTICE '断言H-1通过：理由过短被拒'; END;
    BEGIN
        INSERT INTO sys_ci_baseline (item_key, reason, owner, expires_at) VALUES ('scope_gate', '存量 5 文件待逐个裁定是否测试面', 'backend', now() + interval '100 days');
        RAISE EXCEPTION '断言H失败：窗口 >90 天仍插入成功';
    EXCEPTION WHEN check_violation THEN RAISE NOTICE '断言H-2通过：窗口超 90 天被拒'; END;
    BEGIN
        INSERT INTO sys_ci_baseline (item_key, reason, owner, expires_at) VALUES ('scope_gate', '存量 5 文件待逐个裁定是否测试面', 'backend', now() - interval '1 day');
        RAISE EXCEPTION '断言H失败：expires_at 早于 created_at 仍插入成功';
    EXCEPTION WHEN check_violation THEN RAISE NOTICE '断言H-3通过：到期早于创建被拒'; END;
END $$;

\echo '=== 断言 I：同一 item_key 只能有一条未撤销台账；撤销后可再开一条 ==='
DO $$
DECLARE first_id uuid;
BEGIN
    INSERT INTO sys_ci_baseline (item_key, reason, owner, expires_at)
    VALUES ('scope_gate', '存量 5 文件待逐个裁定是否测试面', 'backend', now() + interval '30 days')
    RETURNING id INTO first_id;
    BEGIN
        INSERT INTO sys_ci_baseline (item_key, reason, owner, expires_at)
        VALUES ('scope_gate', '第二份重复认领（测试）', 'backend', now() + interval '30 days');
        RAISE EXCEPTION '断言I失败：同一 item_key 两条活跃台账';
    EXCEPTION WHEN unique_violation THEN RAISE NOTICE '断言I-1通过：重复认领被拒'; END;
    UPDATE sys_ci_baseline SET revoked_at = now() WHERE id = first_id;
    INSERT INTO sys_ci_baseline (item_key, reason, owner, expires_at)
    VALUES ('scope_gate', '撤销后重新认领（新一轮清理）', 'frontend', now() + interval '14 days');
    RAISE NOTICE '断言I-2通过：撤销后可再开一条（历史行保留）';
END $$;

\echo '=== 断言 J：趋势查询（同一 item_key 最近 5 次）走索引 ==='
EXPLAIN (COSTS OFF) SELECT status, created_at FROM sys_ci_item WHERE item_key = 'erp_invariants' ORDER BY created_at DESC LIMIT 5;

\echo '=== 收尾：整段 ROLLBACK（零残留）==='
ROLLBACK;
\echo '=== ROLLBACK 后核对：三张表应不存在 ==='
SELECT count(*) AS should_be_0 FROM information_schema.tables
 WHERE table_schema = 'public' AND table_name IN ('sys_ci_run', 'sys_ci_item', 'sys_ci_baseline');
