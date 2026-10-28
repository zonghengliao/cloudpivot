-- V0050：导出任务的运行期字段（批次 A · A4 异步任务与 worker）
--
-- 依据：docs/批次A-技术架构.md §3.5（异步导出执行器 / SKIP LOCKED 认领 / 进度节流）
--       + 本卡「僵尸回收 30 min 无心跳 / 失败重试最大次数 + 退避 / RUNNING 期间可取消」三项硬要求。
--
-- 为什么必须加列（不改 V0041 原文件，避免已上线迁移的 checksum 变化）：
--   1. `heartbeat_at`：「RUNNING 超 30 分钟无心跳」必须有一个**独立于业务进度**的心跳时刻。
--      不能复用 `updated_at`（任何一次状态回写都会改它，取消标记也会），否则一次「取消请求」
--      就能把僵尸任务的判定时间推后，回收逻辑 30 分钟内失效。
--   2. `attempt_count` + `max_attempts`：失败重试的**最大次数**必须是任务行上的快照，
--      不能只写常量——否则运维调整阈值会改变「已经在跑的任务」的重试历史。
--   3. `next_run_at`：退避（backoff）要能被认领 SQL 读到（`next_run_at <= now()` 才可认领），
--      把「等 30 秒再试」表达成数据而不是进程内 sleep（进程重启不丢）。
--   4. `cancel_requested`：`RUNNING` 任务不能强杀（可能正在写文件），只能打标记，
--      由 worker 在**批次边界**检查后自行中止（本卡「取消」要求）。
--   5. `locked_by`：多 worker 时能一眼看出「这条任务现在归谁」，并发验收要贴这个证据。
--   6. `file_path` / `file_sha256`：产物落盘路径与内容摘要。V0041 只有 `file_id`
--      （等 A2 的 storage 登记接入后回填），而本卡的下载/过期清理都按**文件**操作，
--      不能依赖尚未接入的 storage 登记——与 `sys_import_task.file_path` 保持同一取舍。
--
-- 回滚：`.down.sql` 删列删索引，任务数据行保留（只丢运行期元数据）。

-- ---------- 1. 导出任务补充运行期列 ----------
ALTER TABLE sys_export_task
    ADD COLUMN IF NOT EXISTS file_path VARCHAR(500), -- 产物落盘路径
    ADD COLUMN IF NOT EXISTS file_sha256 VARCHAR(64), -- 产物 SHA-256（下载校验 / 完整性）
    ADD COLUMN IF NOT EXISTS attempt_count SMALLINT NOT NULL DEFAULT 0, -- 已尝试次数
    ADD COLUMN IF NOT EXISTS max_attempts SMALLINT NOT NULL DEFAULT 3, -- 最大尝试次数快照
    ADD COLUMN IF NOT EXISTS next_run_at TIMESTAMPTZ, -- 最早可认领时间（退避）
    ADD COLUMN IF NOT EXISTS cancel_requested BOOLEAN NOT NULL DEFAULT FALSE, -- 取消请求标记
    ADD COLUMN IF NOT EXISTS locked_by VARCHAR(100), -- 认领者（worker 标识）
    ADD COLUMN IF NOT EXISTS heartbeat_at TIMESTAMPTZ; -- 最近心跳

COMMENT ON COLUMN sys_export_task.file_path IS '产物落盘路径（下载与过期清理按文件操作，不依赖 storage 登记）';
COMMENT ON COLUMN sys_export_task.file_sha256 IS '产物 SHA-256，供下载校验与完整性核对';
COMMENT ON COLUMN sys_export_task.attempt_count IS '已开始的执行次数（含僵尸回收后重新排队的那次）';
COMMENT ON COLUMN sys_export_task.max_attempts IS '最大尝试次数快照（建任务时写入，运行期改配置不影响在跑任务）';
COMMENT ON COLUMN sys_export_task.next_run_at IS '最早可被认领的时间；NULL 表示立即可认领（失败重试的指数退避）';
COMMENT ON COLUMN sys_export_task.cancel_requested IS '取消请求标记：PENDING 直接取消；RUNNING 由 worker 在批次边界检查后中止';
COMMENT ON COLUMN sys_export_task.locked_by IS '当前认领该任务的 worker 标识（多实例排查用）';
COMMENT ON COLUMN sys_export_task.heartbeat_at IS '最近心跳时刻；RUNNING 且超过阈值无心跳 => 僵尸任务，由清理任务回收';

-- 认领走 idx_sys_export_task_claim（status = 'PENDING' 的部分索引，V0041 已建），
-- 但带上 next_run_at 过滤后需要它参与排序，故补一条覆盖「到期时间」的索引。
CREATE INDEX IF NOT EXISTS idx_sys_export_task_due ON sys_export_task (next_run_at, created_at)
    WHERE status = 'PENDING' AND deleted_at IS NULL;

-- 僵尸回收：只扫 RUNNING 行，部分索引足够小
CREATE INDEX IF NOT EXISTS idx_sys_export_task_heartbeat ON sys_export_task (heartbeat_at)
    WHERE status = 'RUNNING';

-- 过期产物清理：按 expire_at 扫 SUCCESS 行（V0041 已有同名索引，此处 IF NOT EXISTS 幂等）
CREATE INDEX IF NOT EXISTS idx_sys_export_task_expire ON sys_export_task (expire_at)
    WHERE status = 'SUCCESS';

-- ---------- 2. 导入任务补充「取消 / 重试」所需的最小列 ----------
-- 导入在 A3 是「同步预检 + 确认入库」，没有 RUNNING 长任务，因此**不加**心跳/退避列；
-- 只补 `cancel_requested`（给确认入库过程中的批次边界检查留位）与 `attempt_count`
-- （供「重试」链路的审计可见性）。
ALTER TABLE sys_import_task
    ADD COLUMN IF NOT EXISTS attempt_count SMALLINT NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS cancel_requested BOOLEAN NOT NULL DEFAULT FALSE;

COMMENT ON COLUMN sys_import_task.attempt_count IS '已执行次数（重试后新建任务的来源可追溯）';
COMMENT ON COLUMN sys_import_task.cancel_requested IS '取消请求标记：确认入库的批次边界检查后中止';
