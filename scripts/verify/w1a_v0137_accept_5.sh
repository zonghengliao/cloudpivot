#!/bin/bash
# W1a · V0137 验收 ⑤：20 万行模拟下仓库树 LIKE 前缀查询必须走 text_pattern_ops 索引
# 用法：bash scripts/verify/w1a_v0137_accept_5.sh（先跑 accept_1_2；跑完自动清理压测数据）
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
DB=${W1A_DB:-cloudpivot_w1a}
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
PSQL="psql -h 127.0.0.1 -p 5432 -U $NIK_DATABASE_USER -d $DB -At -v ON_ERROR_STOP=1"
ROOT='d13700ff-0000-4000-8000-000000000001'

echo "################ ⑤ 造 20 万行 ################"
$PSQL -c "delete from biz_warehouse where warehouse_code like 'BENCH-%'"
$PSQL -c "insert into biz_warehouse (id, warehouse_code, warehouse_name, level, warehouse_path, is_leaf)
          values ('$ROOT','BENCH-ROOT','压测根仓',1,'/$ROOT/',0)"
$PSQL -c "insert into biz_warehouse (warehouse_code, warehouse_name, parent_id, level, warehouse_path, is_leaf)
          select 'BENCH-C-'||g, '压测子仓'||g, '$ROOT', 2, '/$ROOT/'||gen_random_uuid()||'/', 1
            from generate_series(1,99) g"
$PSQL -c "insert into biz_warehouse (warehouse_code, warehouse_name, level, warehouse_path, is_leaf)
          select 'BENCH-'||g, '压测'||g, 1, '/'||gen_random_uuid()||'/', 1
            from generate_series(1,199900) g"
$PSQL -c "analyze biz_warehouse"
echo "总行数: $($PSQL -c "select count(*) from biz_warehouse")"
echo "目标子树行数（LIKE '/$ROOT/%'）: $($PSQL -c "select count(*) from biz_warehouse where warehouse_path like '/$ROOT/%'")"
echo "表大小: $($PSQL -c "select pg_size_pretty(pg_total_relation_size('biz_warehouse'))")"
echo "库 collation: $($PSQL -c "select datcollate from pg_database where datname=current_database()")"

Q="select count(*) from biz_warehouse where warehouse_path like '/$ROOT/%'"

echo
echo "################ ⑤-a 有 text_pattern_ops 索引 ################"
$PSQL -c "select indexname||' :: '||indexdef from pg_indexes where tablename='biz_warehouse' and indexname like 'idx_biz_warehouse_path%'"
$PSQL -c "explain (analyze, buffers, costs off) $Q" | sed 's/^/  /'

echo
echo "################ ⑤-b 去掉 pattern 索引（同一查询）################"
$PSQL -c "drop index idx_biz_warehouse_path_pattern"
$PSQL -c "analyze biz_warehouse"
$PSQL -c "explain (analyze, buffers, costs off) $Q" | sed 's/^/  /'

echo
echo "################ ⑤-c 复原索引（与迁移里的定义一字不差）################"
$PSQL -c "create index if not exists idx_biz_warehouse_path_pattern on biz_warehouse (warehouse_path text_pattern_ops)"
$PSQL -c "analyze biz_warehouse"
$PSQL -c "select indexname||' :: '||indexdef from pg_indexes where tablename='biz_warehouse' and indexname='idx_biz_warehouse_path_pattern'"
$PSQL -c "explain (analyze, buffers, costs off) $Q" | sed 's/^/  /'

echo
echo "################ ⑤-d 子树两种写法（物化路径 vs 递归 CTE）################"
$PSQL -c "explain (analyze, costs off) $Q" | grep -E "Index|Seq|Execution"
$PSQL -c "explain (analyze, costs off) with recursive sub as (
            select id from biz_warehouse where id = '$ROOT'
            union all select w.id from biz_warehouse w join sub on w.parent_id = sub.id)
          select count(*) from sub" | grep -E "Index|Seq|Execution"

echo
echo "################ 清理压测数据 ################"
$PSQL -c "delete from biz_warehouse where warehouse_code like 'BENCH-%'"
$PSQL -c "analyze biz_warehouse"
echo "剩余 biz_warehouse 行数: $($PSQL -c "select count(*) from biz_warehouse")"
echo "DONE"
