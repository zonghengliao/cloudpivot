-- ======================================================
-- V0083 回滚：删除帮助文档索引
--
-- 只 DROP 本迁移建的 5 个索引，**不动表与数据**（表由 V0082 的回滚负责）。
-- trgm 升级预案默认是注释态，从未执行 → 回滚侧无需 DROP EXTENSION；
-- 若运维手工放开过预案，需自行 DROP 那两个 gin 索引与本扩展（见文件末尾）。
-- ======================================================

DROP INDEX IF EXISTS idx_sys_help_doc_published;
DROP INDEX IF EXISTS idx_sys_help_doc_category;
DROP INDEX IF EXISTS uk_sys_help_doc_slug;
DROP INDEX IF EXISTS idx_sys_help_category_parent;
DROP INDEX IF EXISTS uk_sys_help_category_code;

-- 若曾手工放开 trgm 升级预案，回滚时同步清理（默认未执行）：
-- DROP INDEX IF EXISTS idx_sys_help_doc_content_trgm;
-- DROP INDEX IF EXISTS idx_sys_help_doc_title_trgm;
