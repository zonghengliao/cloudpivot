-- ============================================================================
-- V0357 回滚：撤掉 `biz_voucher.source_doc_no`（E2E-FIX-5 ②）
--
-- ⚠ 数据不可恢复（两件事）：
--   ① 回填进去的「业务单据号」丢了 —— 尤其历史退货凭证，退货单号**只**存在这一列
--      （来源本来就是摘要里的文本，回滚后只能再用 `summary LIKE '%SR-…%'` 模糊查）；
--   ② 摘要里被换成名称的 UUID 回不去（原始 UUID 已被 UPDATE 覆盖）。
--   真要回滚先存档：
--     \copy (SELECT id, voucher_no, source_doc_no, summary FROM biz_voucher) TO ...
--     \copy (SELECT voucher_id, line_no, summary FROM biz_voucher_entry) TO ...
--
-- 回滚后的行为：代码与 schema 必须同时回滚 —— `voucher::post_voucher` 的 INSERT 列清单
-- 与 `voucher_query::HEADER_COLUMNS` 都引用了这一列，只回滚 schema 会让「新建凭证 / 查凭证」
-- 直接 500。本文件只负责 schema 部分。
--
-- 幂等：DROP COLUMN / DROP INDEX IF EXISTS。
-- ============================================================================

DROP INDEX IF EXISTS idx_biz_voucher_source_doc_no;

ALTER TABLE biz_voucher
    DROP COLUMN IF EXISTS source_doc_no;
