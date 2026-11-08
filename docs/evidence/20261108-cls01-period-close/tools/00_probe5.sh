#!/usr/bin/env bash
set -uo pipefail
DB=${CLS01_DB:-cloudpivot_cls01}
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
P() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -At -F ' | ' -d "$DB" -c "$1" 2>&1; }
echo "--- rules (raw)"
P "select doc_type, coalesce(file_role,'-'), required_at, min_count, coalesce(allow_exts,'-'), enabled, coalesce(condition_sql,'-') from biz_attachment_rule order by doc_type, required_at"
echo "--- 附件相关表"
P "select table_name from information_schema.tables where table_schema='public' and (table_name like '%attach%' or table_name like '%file%') order by 1"
