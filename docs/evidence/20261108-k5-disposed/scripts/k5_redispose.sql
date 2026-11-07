-- t_c3c2793a 边界续：作废后「重新处置」——再生成一套蓝字处置凭证 + 卡片回 DISPOSED
-- 期望：K5b 只算新蓝字（旧蓝字已被 reversed_by 标记）⇒ 不得双计
\set ON_ERROR_STOP on
BEGIN;

-- 复制红冲前的那套蓝字（voucher_no 去掉 R 后缀、去掉反向指针），得到「新一次处置」
INSERT INTO biz_voucher (voucher_word, voucher_no, period, voucher_date, voucher_type,
                         source_type, source_id, source_no, summary, total_debit, total_credit,
                         entry_count, status, is_red, reverse_of, posted_at, posted_by,
                         created_at, updated_at)
SELECT v.voucher_word, v.voucher_no || 'N', v.period, current_date, v.voucher_type,
       v.source_type, v.source_id, v.source_no, v.summary,
       v.total_debit, v.total_credit, v.entry_count, 'POSTED', 0, NULL, now(), v.posted_by,
       now(), now()
  FROM biz_voucher v
 WHERE v.source_type = 'FIXED_ASSET_DISPOSAL' AND v.is_red = 0
 ORDER BY v.voucher_no;

INSERT INTO biz_voucher_entry (voucher_id, line_no, summary, account_id, account_code, account_name,
                               debit_amount, credit_amount)
SELECT nv.id, e.line_no, e.summary, e.account_id, e.account_code, e.account_name,
       e.debit_amount, e.credit_amount
  FROM biz_voucher_entry e
  JOIN biz_voucher o ON o.id = e.voucher_id
  JOIN biz_voucher nv ON nv.voucher_no = o.voucher_no || 'N'
 WHERE o.source_type = 'FIXED_ASSET_DISPOSAL' AND o.is_red = 0;

UPDATE biz_fixed_asset a
   SET status = 'DISPOSED',
       disposal_type = 'SALE',
       disposal_date = current_date,
       disposal_voucher_id = (SELECT nv.id FROM biz_voucher nv
                               WHERE nv.voucher_no = (
                                 SELECT min(v.voucher_no) || 'N' FROM biz_voucher v
                                  WHERE v.source_type='FIXED_ASSET_DISPOSAL' AND v.is_red=0)
                                 AND nv.is_red = 0),
       updated_at = now()
 WHERE a.deleted_at IS NULL AND a.asset_no = 'FA-20261108-000001';

COMMIT;
