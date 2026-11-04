#!/usr/bin/env bash
# 收口D 验收：144 生产「只读」事实核对（PGOPTIONS 强制 default_transaction_read_only=on）
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
export PGOPTIONS='-c default_transaction_read_only=on'
cd /opt/cloudpivot || exit 9
OUT=/tmp/d4-out; mkdir -p "$OUT"
PSQL="psql -X -q -h 127.0.0.1 -U $NIK_DATABASE_USER -d $NIK_DATABASE_NAME"

echo "########## A. 生产环境事实（只读）##########"
hostname; date '+%F %T %Z'; git log --oneline -1
echo "-- 当前事务只读校验 --"
$PSQL -tAc "show default_transaction_read_only;"
echo "-- c_di* 视图数量 / 全部 11 列 --"
$PSQL -tAc "select 'c_di 视图数='||count(*) from pg_views where schemaname='di' and viewname like 'c_di%';"
$PSQL -tAc "select '非 11 列的视图: '||coalesce(string_agg(v.viewname||'('||v.n||')', ', '), '（无）')
 from (select c.relname::text as viewname, count(a.attname) as n
         from pg_class c join pg_namespace ns on ns.oid=c.relnamespace
         join pg_attribute a on a.attrelid=c.oid and a.attnum>0 and not a.attisdropped
        where ns.nspname='di' and c.relkind='v' and c.relname like 'c_di%'
        group by 1) v where v.n <> 11;"
echo "-- DI-18 豁免台账（唯一 1 行？）--"
$PSQL -c "select check_id, src_table, src_id, checked_on, expires_on, (expires_on > current_date) as active from di.v_poly_ref_waiver;"
echo "-- 线上视图定义是否 = 仓库 HEAD 口径 --"
$PSQL -tAc "select 'c_di18 含[已核查豁免#]: '||(position('已核查豁免#' in pg_get_viewdef('di.c_di18'::regclass))>0)::text
        ||' ; c_di18 含[current_date]: '||(position('current_date' in pg_get_viewdef('di.c_di18'::regclass))>0)::text
        ||' ; c_di19 含[PENDING]: '||(position('PENDING' in pg_get_viewdef('di.c_di19'::regclass))>0)::text;"
echo "-- 生产 SHOW row_security / 是否只读连接 --"
$PSQL -tAc "select 'txid_current_if_assigned='||coalesce(txid_current_if_assigned()::text,'(只读事务未分配 xid)');"

echo; echo "########## B. DI-18 全行（2 条，含豁免前缀）##########"
$PSQL -c "select entity_no, severity, snapshot_value, truth_value from di.c_di18 order by severity desc, entity_no;" | tee "$OUT/b-di18.tsv"
echo; echo "########## C. DI-19 全行（47 条）按实例状态分布 ##########"
$PSQL -tAc "select 'DI-19 行数='||count(*)||'  去重 entity_id='||count(distinct entity_id) from di.c_di19;"
$PSQL -c "select i.status, c.severity, count(*) as rows
            from di.c_di19 c left join biz_flow_instance i on i.id = c.entity_id
           group by 1,2 order by 3 desc;" | tee "$OUT/c-di19-by-status.tsv"
$PSQL -c "select entity_no, i.status, c.severity from di.c_di19 c left join biz_flow_instance i on i.id=c.entity_id order by entity_no;" > "$OUT/c-di19-all-rows.tsv"
echo "（全部 47 行明细见 c-di19-all-rows.tsv，行数：$(wc -l < "$OUT/c-di19-all-rows.tsv")）"
echo; echo "########## D. DI-20 行数 ##########"
$PSQL -tAc "select 'DI-20 行数='||count(*)||'  severity='||string_agg(distinct severity, ',') from di.c_di20;"
echo; echo "########## E. 与巡检 JSON 结果文件对账 ##########"
ls -l /var/log/cloudpivot/snapshot-audit.latest.json
python3 -c "import json;d=json.load(open('/var/log/cloudpivot/snapshot-audit.latest.json'));print('summary=',d['summary']);print('generated_at=',d['generated_at'])"
