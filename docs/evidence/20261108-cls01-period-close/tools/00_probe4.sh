#!/usr/bin/env bash
set -uo pipefail
DB=${CLS01_DB:-cloudpivot_cls01}
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
P() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -At -d "$DB" -c "$1"; }
echo "== 附件规则列"
P "select string_agg(column_name, ', ' order by ordinal_position) from information_schema.columns where table_name='biz_attachment_rule'"
echo "== 附件规则（EXPENSE 优先）"
P "select doc_type||' | '||action_code||' | min='||min_count||' | roles='||coalesce(file_roles,'-')||' | exts='||coalesce(allow_exts,'-')||' | cond='||coalesce(condition_sql,'-') from biz_attachment_rule where enabled order by doc_type, action_code"
