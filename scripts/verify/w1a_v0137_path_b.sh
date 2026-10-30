#!/bin/bash
# W1a · V0137 附加验收（路径 B）：批次1 基线表「先落地」时 V0137 仍能跑
#   验证 CREATE TABLE IF NOT EXISTS / ADD COLUMN IF NOT EXISTS / warehouse_path 回填
# 用法：REPO=/opt/cloudpivot bash scripts/verify/w1a_v0137_path_b.sh
set -uo pipefail
REPO=${REPO:-/opt/cloudpivot}
DIR=$(cd "$(dirname "$0")" && pwd)
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
DB=${W1A_PATHB_DB:-w1a_pathb}
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
PSQL="psql -h 127.0.0.1 -p 5432 -U $NIK_DATABASE_USER -d $DB -v ON_ERROR_STOP=1"

su - postgres -c "psql -Atc \"select 1 from pg_database where datname='$DB'\"" | grep -q 1 && \
  su - postgres -c "psql -Atc \"drop database $DB\"" >/dev/null
su - postgres -c "createdb -O $NIK_DATABASE_USER $DB"

echo "################ 先按批次1 原样建 5 张基线表（无层级列 + 一行存量数据）################"
$PSQL -q -f "$DIR/w1a_v0137_pathb_baseline.sql"
echo "基表已就绪；warehouse_path 列存在数（应为 0）：$($PSQL -Atc "select count(*) from information_schema.columns where table_name='biz_warehouse' and column_name='warehouse_path'")"

echo
echo "################ 路径 B：在「表已存在」的库上跑 V0137 up ################"
$PSQL -f "$REPO/backend/migrations/business/V0137__erp_master_data_and_warehouse.up.sql" 2>&1 \
  | grep -E "ERROR|自检通过|种子核对" | sed 's/^/  /'

echo
echo "结构核对（层级四列 + NOT NULL）："
$PSQL -Atc "select column_name||' '||data_type||' null='||is_nullable from information_schema.columns where table_name='biz_warehouse' and column_name in ('parent_id','level','warehouse_path','is_leaf') order by 1" | sed 's/^/  /'
echo "存量行回填结果："
$PSQL -Atc "select warehouse_code||' path='||coalesce(warehouse_path,'NULL')||' level='||level||' leaf='||is_leaf from biz_warehouse where warehouse_code='LEGACY-01'" | sed 's/^/  /'
echo "public 表数（5 基表 + 6 主数据 + 2 新表 = 13）：$($PSQL -Atc "select count(*) from pg_class c join pg_namespace n on n.oid=c.relnamespace where n.nspname='public' and c.relkind='r'")"
echo "路径索引 opclass："
$PSQL -Atc "select ic.relname||' → '||oc.opcname from pg_index i join pg_class ic on ic.oid=i.indexrelid join pg_opclass oc on oc.oid=i.indclass[0] where ic.relname='idx_biz_warehouse_path_pattern'" | sed 's/^/  /'

su - postgres -c "psql -Atc \"drop database $DB\"" >/dev/null
echo "路径 B 完成，scratch 库 $DB 已删除"
