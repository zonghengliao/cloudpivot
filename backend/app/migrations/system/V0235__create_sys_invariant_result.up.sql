-- 不变量巡检结果表：每次 ERP 账务不变量审计（scripts/verify/erp_invariants.py）的结果快照
--
-- 为什么要有这张表（P2-7）：
--   巡检脚本原先只在终端打印「本次通过多少 / 哪几条失败」，跑完即散 —— 既看不出
--   「账务健康度是变好还是变坏」（没有历史），也没法在新窗口里回答「现在有哪几条红着」。
--   落库后：趋势（近 30 天）+ 当前失败项 + 每次运行的完整明细都只有一个真相来源。
--
-- 粒度：**一次运行一行**（不是一条检查一行）。
--   检查条数随脚本演进会变（多一条 / 少一条都正常），如果把检查项拆成行，
--   「今天 40 条检查、下周 42 条」会让跨天对比永远对不齐；而一次运行一行的口径下，
--   `total / passed / failed` 与 `detail`（全量明细 JSONB）永远是同一次运行的同一份快照。
CREATE TABLE sys_invariant_result (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(), -- 主键
    run_at TIMESTAMPTZ NOT NULL, -- 本次巡检开始时间（趋势按此列切自然日）
    trigger_type VARCHAR(16) NOT NULL, -- 触发方式：SCHEDULED（内置 cron）/ MANUAL（看板或定时任务页手动）
    status VARCHAR(16) NOT NULL, -- 运行结论：OK（全过）/ FAIL（有失败项）/ ERROR（脚本没跑成，非账务结论）
    total INT NOT NULL DEFAULT 0, -- 检查总数
    passed INT NOT NULL DEFAULT 0, -- 通过数
    failed INT NOT NULL DEFAULT 0, -- 失败数（BLOCKER + WARN + INFO 中 ok=false 的合计）
    blockers INT NOT NULL DEFAULT 0, -- 失败项里 BLOCKER 级别数（>0 表示账已错）
    warns INT NOT NULL DEFAULT 0, -- 失败项里 WARN 级别数
    duration_ms INT NOT NULL DEFAULT 0, -- 本次运行耗时（毫秒，含脚本启动 + 全部 SQL）
    database_name VARCHAR(128) NOT NULL DEFAULT '', -- 巡检目标库名（快照，避免"这行到底扫的哪个库"说不清）
    script_path TEXT NOT NULL DEFAULT '', -- 实际执行的脚本路径（快照）
    detail JSONB, -- 全量检查明细：[{name, group, level, ok, detail}]
    failures JSONB, -- 失败项子集（看板「当前失败项」直出，不必在 JSON 里过滤）
    error TEXT, -- status = ERROR 时的原因（脚本缺失 / 超时 / 输出解析失败 / psql 连不上）
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP -- 落库时间
);

COMMENT ON TABLE sys_invariant_result IS 'ERP 账务不变量巡检结果（一次运行一行；趋势 + 当前失败项的真相来源）';
COMMENT ON COLUMN sys_invariant_result.id IS '主键';
COMMENT ON COLUMN sys_invariant_result.run_at IS '本次巡检开始时间（趋势按此列切自然日）';
COMMENT ON COLUMN sys_invariant_result.trigger_type IS '触发方式：SCHEDULED / MANUAL';
COMMENT ON COLUMN sys_invariant_result.status IS '运行结论：OK / FAIL / ERROR（ERROR 表示脚本没跑成，不是账务结论）';
COMMENT ON COLUMN sys_invariant_result.total IS '检查总数';
COMMENT ON COLUMN sys_invariant_result.passed IS '通过数';
COMMENT ON COLUMN sys_invariant_result.failed IS '失败数（ok=false 的检查项合计）';
COMMENT ON COLUMN sys_invariant_result.blockers IS '失败项里 BLOCKER 级别数（>0 = 账已错）';
COMMENT ON COLUMN sys_invariant_result.warns IS '失败项里 WARN 级别数';
COMMENT ON COLUMN sys_invariant_result.duration_ms IS '本次运行耗时（毫秒）';
COMMENT ON COLUMN sys_invariant_result.database_name IS '巡检目标库名（快照）';
COMMENT ON COLUMN sys_invariant_result.script_path IS '实际执行的脚本路径（快照）';
COMMENT ON COLUMN sys_invariant_result.detail IS '全量检查明细 JSONB：[{name, group, level, ok, detail}]';
COMMENT ON COLUMN sys_invariant_result.failures IS '失败项子集 JSONB（看板直出）';
COMMENT ON COLUMN sys_invariant_result.error IS 'status=ERROR 时的原因（脚本缺失 / 超时 / 解析失败 / 连不上库）';
COMMENT ON COLUMN sys_invariant_result.created_at IS '落库时间';

-- 运行结论只允许三种取值：写错的字符串不该悄悄进库，否则看板「当前失败项」会漏判
ALTER TABLE sys_invariant_result
    ADD CONSTRAINT ck_invariant_status CHECK (status IN ('OK', 'FAIL', 'ERROR')),
    ADD CONSTRAINT ck_invariant_trigger CHECK (trigger_type IN ('SCHEDULED', 'MANUAL'));

-- 趋势 / 最新一条都按 run_at 倒序取
CREATE INDEX idx_invariant_run_at ON sys_invariant_result (run_at DESC);
-- 看板「最近一次失败」与排障时的「只看有问题的运行」：部分索引只收非 OK 行
CREATE INDEX idx_invariant_not_ok ON sys_invariant_result (run_at DESC)
    WHERE status <> 'OK';

-- 巡检任务在「定时任务」页的可见行。
-- status = 0（暂停）是**故意的**：真正的每日调度由编译期内置任务
-- `invariant_audit` 承担（cron 见 backend/src/support/invariant/runner.rs），
-- 这行只是给运维一个「查看配置 + 立即执行一次」的入口；若置 1 会与内置调度重复触发。
INSERT INTO sys_job (job_name, job_group, cron_expr, invoke_target, params, status, remark)
SELECT 'invariant_audit', 'SYSTEM', '0 20 2 * * *', 'invariant_audit', NULL, 0,
       'ERP 账务不变量巡检（内置任务已注册，本行仅供查看/手动执行）：每天 02:20 跑 scripts/verify/erp_invariants.py，结果落 sys_invariant_result'
 WHERE NOT EXISTS (
           SELECT 1 FROM sys_job j WHERE j.job_name = 'invariant_audit' AND j.deleted_at IS NULL
       );
