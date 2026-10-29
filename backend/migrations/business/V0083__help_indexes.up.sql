-- ======================================================
-- V0083：帮助文档索引（批次 C · C4）
--
-- 依据：docs/批次C-技术架构.md §3.1 / §3.3.2（检索方案结论）/ §3.7 / 附录 A.4
-- 回滚：V0083__help_indexes.down.sql
--
-- ── 检索方案：本批**不建 trgm 索引、不上 tsvector** ────────────────
-- 架构 §3.3 的结论（POC-6/7 实测，不是偏好）：
--   ① `tsvector` + 默认分词器把「系统公告管理」当一个词 → 搜「公告」0 命中；
--      本机 PG 18.6 没有 zhparser / pg_jieba，方案直接不可用；
--   ② `ILIKE '%kw%'`：2000 篇实测 4.2–6.1 ms，远低于 200 ms 预算 → **本批选它**；
--   ③ `pg_trgm` GIN：5 万篇时 3 字稀词 0.238 ms，但 2 字中文查询反而退化到
--      153 ms（比全表扫描还慢），且索引体积 ≈ 表的 50% → **条件启用**。
--
-- 所以本迁移只建 5 个"结构性索引"（唯一性 / 分类列 / 发布态 / 版本序号），
-- trgm 升级预案以**注释态**保留在文件末尾（架构 §3.3.2 的触发条件：
-- 行数 > 5000 或检索 P95 > 50 ms 时由运维手工放开，**不改代码**）。
--
-- ── 为什么唯一索引是"部分索引" ────────────────────────────
-- `WHERE deleted_at IS NULL`：软删后同一 slug / code 可再次使用。
-- 代价是应用层不能用 `ON CONFLICT (slug)` 依赖部分索引做 upsert（PG 要求
-- 冲突目标与索引谓词完全一致），所以写路径用「先查后插」+ 唯一冲突兜底，
-- 见 help/model.rs 的 save_doc / create_category。
-- ======================================================

-- ---------- 分类树 ----------
CREATE UNIQUE INDEX uk_sys_help_category_code
    ON sys_help_category (code) WHERE deleted_at IS NULL;
CREATE INDEX idx_sys_help_category_parent
    ON sys_help_category (parent_id, sort_order);

-- ---------- 文档 ----------
CREATE UNIQUE INDEX uk_sys_help_doc_slug
    ON sys_help_doc (slug) WHERE deleted_at IS NULL;
CREATE INDEX idx_sys_help_doc_category
    ON sys_help_doc (category_id, sort_order) WHERE deleted_at IS NULL;
-- 用户侧列表 / 检索的排序主索引（status + published_at DESC，部分索引吃满选择性）
CREATE INDEX idx_sys_help_doc_published
    ON sys_help_doc (status, published_at DESC) WHERE deleted_at IS NULL;

-- ---------- 版本快照 ----------
-- 版本列表按 (doc_id, version DESC) 取最近 20 版；uk 已是 (doc_id, version) 升序，
-- 反向扫描同样走这条唯一索引，**不额外建索引**（多一个索引就是多一份写入放大）。
-- 清理任务按 doc_id 聚合 max(version)，同样命中 uk 索引。

-- ---------- 升级预案（注释态，默认不执行）----------
-- 触发条件：sys_help_doc 行数 > 5000 或检索 P95 > 50 ms（架构 §3.3.2）
-- CREATE EXTENSION IF NOT EXISTS pg_trgm;
-- CREATE INDEX idx_sys_help_doc_title_trgm   ON sys_help_doc USING gin (title gin_trgm_ops);
-- CREATE INDEX idx_sys_help_doc_content_trgm ON sys_help_doc USING gin (content_md gin_trgm_ops);
-- 注意：2 字中文查询仍会退化为全表（实测 153 ms），别指望 trgm 解决所有中文子串检索。

-- 迁移文件非空保底（框架按版本序合并迁移；占位便于将来就地放开上面的预案）
SELECT 1;
