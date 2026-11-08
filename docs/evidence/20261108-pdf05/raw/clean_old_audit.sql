-- PDF-05：清掉「修复前」那次跑出来的旧审计行（source 被公共件写成 PDF-03），
-- 让最终证据里的审计表只包含修复后（source=PDF-05）的行。
DELETE FROM operation_logs
 WHERE operation = 'EXPORT_FLOW_INSTANCE_PDF'
   AND COALESCE(detail->>'source', '') <> 'PDF-05';
SELECT count(*) AS remaining FROM operation_logs WHERE operation = 'EXPORT_FLOW_INSTANCE_PDF';
