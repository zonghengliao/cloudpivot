-- t_c3c2793a 边界：作废处置（红冲 + 卡片回 IN_USE）后，K5/K5b 不许假红
-- 用 SQL 复刻 asset_bridge::reverse_disposal_vouchers 的效果：
--   ① 每张蓝字处置凭证生成一张红字（is_red=1, reverse_of=原 id, 分录借贷对调, source_type/source_id 继承）
--   ② 原蓝字置 reversed_by
--   ③ 卡片 DISPOSED → IN_USE，disposal_* 清空
\set ON_ERROR_STOP on
BEGIN;

INSERT INTO biz_voucher (voucher_word, voucher_no, period, voucher_date, voucher_type,
                         source_type, source_id, source_no, summary, total_debit, total_credit,
                         entry_count, status, is_red, reverse_of, posted_at, posted_by,
                         created_at, updated_at)
SELECT v.voucher_word, v.voucher_no || 'R', v.period, current_date, v.voucher_type,
       v.source_type, v.source_id, v.source_no, '红冲 ' || v.voucher_no,
       v.total_credit, v.total_debit, v.entry_count, 'POSTED', 1, v.id, v.posted_at, v.posted_by,
       now(), now()
  FROM biz_voucher v
 WHERE v.source_type = 'FIXED_ASSET_DISPOSAL' AND v.is_red = 0;

INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name,
                               debit_amount, credit_amount)
SELECT nr.id, e.line_no, e.summary, e.account_id, e.account_code, e.account_name,
       e.credit_amount, e.debit_amount
  FROM biz_voucher_entry e
  JOIN biz_voucher o ON o.id = e.voucher_id
  JOIN biz_voucher nr ON nr.reverse_of = o.id
 WHERE o.source_type = 'FIXED_ASSET_DISPOSAL' AND o.is_red = 0;

UPDATE biz_voucher o
   SET reversed_by = nr.id, updated_at = now()
  FROM biz_voucher nr
 WHERE nr.reverse_of = o.id;

UPDATE biz_fixed_asset
   SET status = 'IN_USE', disposal_type = NULL, disposal_date = NULL, disposal_voucher_id = NULL,
       updated_at = now()
 WHERE deleted_at IS NULL AND status = 'DISPOSED';

COMMIT;
