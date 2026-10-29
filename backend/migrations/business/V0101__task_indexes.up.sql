-- ======================================================
-- V0101：工作任务管理 · 索引（批次 C+ · T1）
--
-- 依据：docs/批次C+-工作任务管理-核心设计.md §十二（索引清单）
--       §十一 页面（每一条索引都对应一个真实入口页的查询形态）
-- 回滚：V0101__task_indexes.down.sql
--
-- ── 索引 ↔ 入口页 一一对应（不做「全字段索引」） ────────────────
--   idx_biz_task_owner_status_due   → 「我的任务」/「我负责的」（owner_id 等值 + status 过滤 + due_date 排序）
--   idx_biz_task_dept_status        → 「任务板」按部门看板列
--   idx_biz_task_assigner_status    → 「我分派的」
--   idx_biz_task_parent             → 子任务列表（详情页左栏）
--   idx_biz_task_due_open（partial）→ 逾期扫描 / 临期提醒（状态未终态的才算逾期）
--   uk_biz_task_task_no             → 编号唯一（全表，软删不复用）
--
-- ── 为什么 due_open 是**部分索引** ──────────────────────────────
-- 逾期扫描的口径恒为 `status NOT IN ('DONE','CANCELLED')`：已完成/已取消的任务
-- 永远不会被扫到。把它们排除出索引后：
--   ① 索引体积 ≈ 未终态任务数（长期运行时 DONE 占比会到 80%+，体积差 5 倍）；
--   ② 扫描时不需要回表后再过滤状态，Bitmap Index Scan 命中的行就是候选集。
-- PG 只有在查询里**逐字出现**该谓词（且不包在 OR / 函数里）时才会选用它 ——
-- 所以 model::scan_overdue 的 SQL 与这里的 WHERE 必须逐字一致，改动要成对。
--
-- ── 为什么 owner 索引是 (owner_id, status, due_date) 这个列序 ────
-- 「我的任务」的形态是 `owner_id = ? AND status NOT IN (...) ORDER BY due_date`：
-- 等值列在最左 → status 做索引内过滤 → due_date 已有序（省一次 Sort 节点）。
-- 设计文档 §十二 给的就是这个列序，本文件逐字落地，不另行优化。
-- ======================================================

-- 编号唯一：全表唯一（含软删行），保证「TSK-202610-000007」永远只指向一个任务
CREATE UNIQUE INDEX uk_biz_task_task_no ON biz_task (task_no);
COMMENT ON INDEX uk_biz_task_task_no IS '任务编号全表唯一（软删不复用，审计可回溯）';

CREATE INDEX idx_biz_task_owner_status_due ON biz_task (owner_id, status, due_date);
COMMENT ON INDEX idx_biz_task_owner_status_due IS '「我的任务」：owner_id 等值 + status 过滤 + due_date 有序';

CREATE INDEX idx_biz_task_dept_status ON biz_task (dept_id, status);
COMMENT ON INDEX idx_biz_task_dept_status IS '部门看板：按 dept_id + status 列任务卡';

CREATE INDEX idx_biz_task_assigner_status ON biz_task (assigner_id, status);
COMMENT ON INDEX idx_biz_task_assigner_status IS '「我分派的」：assigner_id + status';

CREATE INDEX idx_biz_task_parent ON biz_task (parent_id);
COMMENT ON INDEX idx_biz_task_parent IS '子任务列表：parent_id 等值（详情页左栏）';

-- 逾期扫描 / 临期提醒：部分索引，谓词与 model::scan_overdue 的 WHERE 逐字一致
CREATE INDEX idx_biz_task_due_open ON biz_task (due_date)
    WHERE status NOT IN ('DONE', 'CANCELLED');
COMMENT ON INDEX idx_biz_task_due_open IS '逾期扫描（部分索引）：仅未终态任务参与，谓词必须与 scan_overdue 一致';

-- 可见性谓词第 4 分支：EXISTS (SELECT 1 FROM biz_task_member WHERE task_id = t.id AND user_id = ?)
-- UNIQUE (task_id, user_id) 已能服务「按 task_id 找成员」，但谓词是从 user_id 侧进来
-- （「我被拉进哪些任务」），所以需要 user_id 在前的这条。
CREATE INDEX idx_biz_task_member_user ON biz_task_member (user_id, task_id);
COMMENT ON INDEX idx_biz_task_member_user IS '可见性第 4 分支：按 user_id 反查我参与的任务';

-- 详情页 / 列表的协作人回显按 task_id 取（UNIQUE 已覆盖，但显式给 role 排序留一条）
CREATE INDEX idx_biz_task_comment_task ON biz_task_comment (task_id, created_at);
COMMENT ON INDEX idx_biz_task_comment_task IS '评论时间线：task_id + created_at（T3 使用）';

CREATE INDEX idx_biz_task_attachment_task ON biz_task_attachment (task_id, sort_order);
COMMENT ON INDEX idx_biz_task_attachment_task IS '附件列表：task_id + sort_order';

CREATE INDEX idx_biz_task_worklog_task ON biz_task_worklog (task_id, work_date);
COMMENT ON INDEX idx_biz_task_worklog_task IS '工时列表：task_id + work_date（P1）';

CREATE INDEX idx_biz_task_project ON biz_task (project_id) WHERE project_id IS NOT NULL;
COMMENT ON INDEX idx_biz_task_project IS '按项目聚合（P1）：只索引有项目归属的任务';

CREATE INDEX idx_biz_project_dept ON biz_project (dept_id) WHERE deleted_at IS NULL;
COMMENT ON INDEX idx_biz_project_dept IS '项目列表按部门（P1）';
