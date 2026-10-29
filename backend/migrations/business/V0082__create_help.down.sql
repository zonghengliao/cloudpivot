-- ======================================================
-- V0082 回滚：删除帮助文档三表
--
-- 顺序：先版本表（引用 doc），再 doc（引用 category），最后分类表。
-- 反向依赖会因外键 RESTRICT 失败，这里显式按逆序删，不依赖 CASCADE。
-- ======================================================

DROP TABLE IF EXISTS sys_help_doc_version;
DROP TABLE IF EXISTS sys_help_doc;
DROP TABLE IF EXISTS sys_help_category;
