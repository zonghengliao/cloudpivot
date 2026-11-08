-- =============================================================================
-- V0337 回滚：删掉六条主数据取号规则 + 位数列
-- =============================================================================
--
-- 只删**本迁移种的那六条**（按 seq_type + date_pattern='none' + period_key='GLOBAL'
-- 三重限定），不碰单据号段；`pad_width` 是本次新增列，一并撤掉。
-- 已经在库里发过号（current_no > 0）也照删：主数据编码本身存在业务表里，
-- 序列只是发号器；真要保留发号进度，请先导出 biz_seq 再回滚。

DELETE FROM biz_seq
 WHERE seq_type IN ('SUP','CUS','MAT','WH','AC','ET')
   AND date_pattern = 'none'
   AND period_key = 'GLOBAL';

ALTER TABLE biz_seq DROP CONSTRAINT IF EXISTS ck_biz_seq_pad_width;
ALTER TABLE biz_seq DROP COLUMN IF EXISTS pad_width;
