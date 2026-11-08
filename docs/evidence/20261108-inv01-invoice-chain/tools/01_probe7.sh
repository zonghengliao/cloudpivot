#!/usr/bin/env bash
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
DB=${INV01_DB:-cloudpivot_inv01}
q() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -At -d "$DB" -c "$1"; }
echo "== material cols"; q "select column_name from information_schema.columns where table_name='biz_material' order by ordinal_position" | tr '\n' ' '
echo; echo "== materials"; q "select material_code||' | '||material_name||' | '||(select string_agg(k||'='||v, ' ') from (select 'serial' as k, coalesce(to_jsonb(m)->>'is_serial','?') as v) s) from biz_material m order by material_code"
echo "== jsonb"; q "select to_jsonb(m)::text from biz_material m limit 5"
