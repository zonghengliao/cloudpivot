-- ======================================================
-- V0084：变更日志 · 新建 `sys_change_log`（批次 C · C5）
--
-- 依据：docs/批次C-技术架构.md 附录 A.5（DDL 原文，PG 18.6 实跑通过）+ §4.3 / §4.9
-- 回滚：V0084__create_change_log.down.sql（DROP TABLE，分区随父表一起删除）
--
-- 五个设计点（每条都有实测依据，见架构 §4.3.1 / §4.9 与 POC-9/10/12）：
--
-- 1. **主键必须是 (id, created_at)**：分区表的主键必须包含分区键，否则
--    `ERROR: unique constraint on partitioned table must include all partitioning columns`。
--    连带影响（写进代码注释）：`sys_change_log` 不派生单主键 `Model::find`，
--    详情接口必须带 `created_at` 才能分区裁剪。
-- 2. **按月 RANGE 分区，且刻意不建 DEFAULT 分区**：POC-10 实测——有 default 分区时，
--    一旦它堆了历史行，新建正式分区会直接失败（`updated partition constraint ... would be violated`），
--    运维只能手工搬数据。没有 default 时越界写入立刻报
--    `no partition of relation "sys_change_log" found for row`，故障可见、可恢复。
-- 3. **预建 3 个月分区用 DO 块动态生成**（当前月 + 后 2 个月）：
--    架构 A.5 写的是硬编码 `2026_10 / 2026_11 / 2026_12`，但迁移执行时刻不确定，
--    硬编码会在「别的月份部署」时立刻越界写失败。动态生成的**分区名格式与架构一致**
--    （`sys_change_log_YYYY_MM`），语义完全相同：永远有当月与后两个月。
--    后续月份由 C6 的 `changelog_partition_maintain` 任务在每月 1 日 00:05 预建。
-- 4. **5 个索引对应 4 类真实查询 + 请求关联**（§4.8）：对象历史 / 操作人 / 模块+动作 /
--    `changed_fields` GIN（`&&` 反查「哪些变更动过 X 字段」）/ `request_id`（与 operation_logs 对齐）。
-- 5. **CHECK 封闭五个枚举**（来源 / 动作 / 模式 / 结果 / 资源名）：脏值进不来。
--    `result` 允许 FAILED 但 v1 只写 SUCCESS（架构 §4.3.3，失败尝试不进本表）。
--
-- 不做什么（C5 边界）：
-- - 不建 `sys_change_log` 的触发器/自动写入：写入必须由业务代码在**同一事务**内显式调用
--   `business::changelog::record(&tx, ..)`（D11）。触发器拿不到事务外上下文，也过不了脱敏（D13）。
-- - 不改 `operation_logs` / `audit_logs`（它们的职责调整在 C6 的定位说明里）。
-- ======================================================

CREATE TABLE sys_change_log (
    id               UUID NOT NULL DEFAULT gen_random_uuid(),  -- 业务侧用 UUIDv7 显式赋值
    request_id       VARCHAR(36),                              -- 与 operation_logs.request_id 可关联
    trace_source     VARCHAR(16) NOT NULL DEFAULT 'API',       -- API/JOB/IMPORT/SYSTEM
    operator_id      UUID,                                     -- 操作人（冗余 operator_name/dept 供改名/调部门后仍可读）
    operator_name    VARCHAR(50),
    operator_dept_id UUID,
    client_ip        INET,
    module           VARCHAR(32) NOT NULL,                     -- system / business / support
    resource         VARCHAR(64) NOT NULL,                     -- 表名（白名单键，如 sys_user）
    resource_id      VARCHAR(64) NOT NULL,                     -- 业务主键（UUID 或复合键字符串）
    action           VARCHAR(32) NOT NULL,                     -- CREATE/UPDATE/DELETE/STATUS/ASSIGN/PUBLISH/OFFLINE/UPLOAD/DOWNLOAD/CLEAN/LOGIN
    action_label     VARCHAR(100) NOT NULL,                    -- 中文可读（「修改用户」）
    diff             JSONB NOT NULL DEFAULT '{}'::jsonb,       -- {v,mode,fields,big,meta}
    diff_mode        VARCHAR(16) NOT NULL DEFAULT 'WHITELIST',
    changed_fields   TEXT[],                                   -- 冗余字段名数组，供 GIN && 反查
    result           VARCHAR(16) NOT NULL DEFAULT 'SUCCESS',
    error_code       INT,
    duration_ms      INT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (id, created_at),          -- 分区表主键必须含分区键（POC-9 实测）
    CONSTRAINT ck_sys_change_log_source   CHECK (trace_source IN ('API','JOB','IMPORT','SYSTEM')),
    CONSTRAINT ck_sys_change_log_action   CHECK (action IN
        ('CREATE','UPDATE','DELETE','STATUS','ASSIGN','PUBLISH','OFFLINE','UPLOAD','DOWNLOAD','CLEAN','LOGIN')),
    CONSTRAINT ck_sys_change_log_mode     CHECK (diff_mode IN ('WHITELIST','FULL','REDACTED')),
    CONSTRAINT ck_sys_change_log_result   CHECK (result IN ('SUCCESS','FAILED')),
    CONSTRAINT ck_sys_change_log_resource CHECK (resource ~ '^[a-z][a-z0-9_]{1,63}$')
) PARTITION BY RANGE (created_at);

COMMENT ON TABLE sys_change_log IS
    '变更日志：业务事务内显式写入的字段级 diff（成功路径），月分区，DROP TABLE 分区清理';
COMMENT ON COLUMN sys_change_log.diff IS
    'JSON schema v1：{v,mode,fields:{col:{b,a,masked?}},big:{col:{b:{len,sha256,preview},a:{...},truncated}},meta}';
COMMENT ON COLUMN sys_change_log.changed_fields IS
    '冗余字段名数组，供 GIN && 反查「哪些变更动过 X 字段」';
COMMENT ON COLUMN sys_change_log.result IS
    '本批只写 SUCCESS（失败尝试已在 operation_logs / audit_logs 留痕，架构 §4.3.3）';
COMMENT ON COLUMN sys_change_log.diff_mode IS
    'WHITELIST=只记登记白名单字段的差异（v1 唯一取值）；FULL/REDACTED 预留给后续批次';
COMMENT ON COLUMN sys_change_log.trace_source IS
    '变更来源：API 手工操作 / JOB 定时任务 / IMPORT 导入 / SYSTEM 系统自维护';

-- 初始 3 个分区：当前月 + 后 2 个月（动态生成，见文件头第 3 点）
DO $$
DECLARE
    base  DATE := date_trunc('month', CURRENT_DATE)::DATE;
    lo    DATE;
    hi    DATE;
    pname TEXT;
    i     INT;
BEGIN
    FOR i IN 0..2 LOOP
        lo := (base + (i || ' month')::INTERVAL)::DATE;
        hi := (lo + INTERVAL '1 month')::DATE;
        pname := 'sys_change_log_' || to_char(lo, 'YYYY_MM');
        EXECUTE format(
            'CREATE TABLE IF NOT EXISTS %I PARTITION OF sys_change_log FOR VALUES FROM (%L) TO (%L)',
            pname, lo, hi
        );
    END LOOP;
END $$;

-- ⚠️ 刻意不建 DEFAULT 分区：越界写入必须立刻报错（POC-9 / POC-10）

-- 5 个索引（建在分区表上，PG 会自动在每个分区建同构索引）
CREATE INDEX idx_sys_change_log_object   ON sys_change_log (resource, resource_id, created_at DESC);
CREATE INDEX idx_sys_change_log_operator ON sys_change_log (operator_id, created_at DESC);
CREATE INDEX idx_sys_change_log_module   ON sys_change_log (module, action, created_at DESC);
CREATE INDEX idx_sys_change_log_fields   ON sys_change_log USING gin (changed_fields);
CREATE INDEX idx_sys_change_log_request  ON sys_change_log (request_id);
