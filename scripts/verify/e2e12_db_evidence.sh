#!/bin/bash
# E2E-12 DB 层证据 + 勾稽（只读）
set -u
export PGPASSWORD=$(grep -E '^NIK_DATABASE_PASSWORD=' /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2-)
P="psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_dev -At -F| -P pager=off -c"
psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_dev -At -F'|' -P pager=off <<'SQL'
\echo '=== 1. 本卡锚点采购单与附件 ==='
select 'PO='||o.order_no||' 状态='||o.status||' 行数='||(select count(*) from biz_purchase_order_item i where i.order_id=o.id)
       ||' 附件='||(select count(*) from biz_doc_attachment a where a.doc_id=o.id and a.deleted_at is null)
  from biz_purchase_order o where o.remark like 'E2E12 附件+PDF%' order by o.created_at desc limit 1;
\echo '=== 2. 附件分栏（file_role 分布）+ uploader_name 快照 ==='
select a.file_role||'|'||count(*)||'|'||count(*) filter (where a.uploader_name is null or a.uploader_name='') as empty_name
  from biz_doc_attachment a
 where a.doc_id = (select id from biz_purchase_order where remark like 'E2E12 附件+PDF%' order by created_at desc limit 1)
   and a.deleted_at is null group by a.file_role order by 1;
\echo '=== 3. 勾稽：绑定行 ↔ sys_file（悬空 / 大小一致 / 名字一致） ==='
select 'dangling_binding='||count(*) from biz_doc_attachment a left join sys_file f on f.id=a.file_id
 where a.deleted_at is null and f.id is null;
select 'size_mismatch='||count(*) from biz_doc_attachment a join sys_file f on f.id=a.file_id
 where a.deleted_at is null and f.file_size <= 0;
\echo '=== 4. 勾稽：软删附件不得出现在列表（deleted_at 与物理文件回收状态） ==='
select 'soft_deleted_binding='||count(*) filter (where a.deleted_at is not null)
       ||' 其文件仍活='||count(*) filter (where a.deleted_at is not null and f.deleted_at is null)
  from biz_doc_attachment a join sys_file f on f.id=a.file_id
 where a.doc_id = (select id from biz_purchase_order where remark like 'E2E12 附件+PDF%' order by created_at desc limit 1);
\echo '=== 5. 留痕：上传 / 下载 / 删除 三类操作码计数 ==='
select operation||'='||c from (select operation, count(*) c from operation_logs
 where operation in ('DOC_ATTACHMENT_UPLOAD','DOC_ATTACHMENT_DOWNLOAD','DOC_ATTACHMENT_DELETE')
   and created_at > now() - interval '90 minutes' group by operation) t order by 1;
\echo '=== 6. PDF 导出留痕（12 类单据/报表 → 操作码聚合） ==='
select operation||'='||c||' 目标表='||coalesce(tt,'-') from (select operation, count(*) c, max(target_type) tt
  from operation_logs where operation like 'EXPORT%' and created_at > now() - interval '90 minutes'
 group by operation) t order by 1;
\echo '=== 7. 导出审计行字段完整性（username / created_at 非空） ==='
select 'rows='||count(*)||' 有username='||count(*) filter (where username is not null and username<>'')
       ||' 有created_at='||count(*) filter (where created_at is not null)
  from operation_logs where operation like 'EXPORT%' and created_at > now() - interval '90 minutes';
\echo '=== 8. 必传闸门：自有盘点单状态与附件 ==='
select 'ST='||t.take_no||' 状态='||t.status||' 附件='||(select count(*) from biz_doc_attachment a
         where a.doc_id=t.id and a.doc_type='STOCK_TAKE' and a.deleted_at is null)
  from biz_stock_take t where t.remark like 'E2E12%' order by t.created_at desc limit 3;
\echo '=== 9. 规则表：启用状态与条件（发票两分支） ==='
select doc_type||'|'||required_at||'|'||enabled::text||'|'||coalesce(condition_sql,'-')
  from biz_attachment_rule where deleted_at is null and doc_type in ('INVOICE','STOCK_TAKE')
 order by doc_type, condition_sql;
\echo '=== 10. 本卡残留（收尾前应为非 0，收尾后为 0） ==='
select 'po='||(select count(*) from biz_purchase_order where remark like 'E2E12%')
     ||' st='||(select count(*) from biz_stock_take where remark like 'E2E12%')
     ||' att='||(select count(*) from biz_doc_attachment a join sys_file f on f.id=a.file_id where f.original_name like 'E2E12%')
     ||' file='||(select count(*) from sys_file where original_name like 'E2E12%')
     ||' role='||(select count(*) from sys_role where role_code like 'E2E12%')
     ||' user='||(select count(*) from sys_user where username like 'e2e12%')
     ||' rule='||(select count(*) from biz_attachment_rule where remark like 'E2E12%');
SQL
