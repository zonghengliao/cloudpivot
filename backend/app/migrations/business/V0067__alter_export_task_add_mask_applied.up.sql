-- ======================================================
-- V0067：导出任务 · 增加 `mask_applied`（批次 B · B1）
--
-- 依据：docs/批次B-技术架构.md 附录 A.8（DDL 原文，已在 PG 18.6 实跑）；语义见 §3.7
-- 回滚：V0067__alter_export_task_add_mask_applied.down.sql（DROP COLUMN）
--
-- 为什么不是 V0065/V0066：那两条是「权限点种子」与「默认脱敏规则种子」，属 B4/B6
-- 的实施步范围，不在 B1 落地（本卡只做结构迁移）。V0067 是结构迁移，故随本批先行。
--
-- 默认 FALSE 是刻意的：历史导出产物（迁移前产生的）确实**未经脱敏**，
-- 默认 FALSE 让审计口径与事实一致，而不是给存量数据贴一个「安全」的假标签。
-- ======================================================

ALTER TABLE sys_export_task
    ADD COLUMN IF NOT EXISTS mask_applied BOOLEAN NOT NULL DEFAULT FALSE;

COMMENT ON COLUMN sys_export_task.mask_applied IS
    '产物是否已应用列级脱敏（TRUE = 安全，FALSE = 请求者持 field-mask:plain 权限导出明文）';
