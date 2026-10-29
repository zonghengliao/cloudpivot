#!/bin/bash
set -a; . /etc/cloudpivot/cloudpivot-test.env; set +a
export PGPASSWORD="$ALUN_DATABASE_PASSWORD"
DB=cloudpivot_l1fix
psql -h 127.0.0.1 -U cloudpivot -d $DB -c \
"SELECT id, status, exec_mode, hermes_task_id, hermes_status, progress,
        length(ai_result::text) AS ai_result_len, left(ai_result::text, 80) AS ai_result80
   FROM biz_task ORDER BY created_at DESC LIMIT 3"
echo "=== 附件表结构 ==="
psql -h 127.0.0.1 -U cloudpivot -d $DB -Atc \
"SELECT string_agg(column_name, ', ' ORDER BY ordinal_position) FROM information_schema.columns WHERE table_name='biz_task_attachment'"
echo "=== 附件行 ==="
psql -h 127.0.0.1 -U cloudpivot -d $DB -c "SELECT * FROM biz_task_attachment ORDER BY created_at DESC LIMIT 5"
echo "=== kanban card ==="
env HERMES_HOME=/root/.hermes /root/.hermes/hermes-agent/hermes kanban --board dev show t_c319bc13 2>&1 | head -30
