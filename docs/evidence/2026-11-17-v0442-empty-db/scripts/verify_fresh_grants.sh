#!/usr/bin/env bash
set -uo pipefail
export PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4
H=127.0.0.1
DB=${1:-cloudpivot_test_v0442b}
echo "===== $DB"
echo "-- V0442 授权行（确定性 id）"
psql -h "$H" -U cloudpivot -d "$DB" -P pager=off -c "
select r.role_code, coalesce(m.auth_code,'(无码目录)') as code,
       case when rm.id = md5('v0442.'||rm.role_id||':'||rm.menu_id)::uuid then 'v0442'
            when rm.id = md5('v0442c.'||rm.role_id||':'||rm.menu_id)::uuid then 'v0442c' end as seg
  from sys_role_menu rm
  join sys_role r on r.id = rm.role_id
  left join sys_menu m on m.id = rm.menu_id
 where rm.id in (md5('v0442.'||rm.role_id||':'||rm.menu_id)::uuid,
                 md5('v0442c.'||rm.role_id||':'||rm.menu_id)::uuid)
 order by r.role_code, code"
echo "-- N20 六码持有者"
psql -h "$H" -U cloudpivot -d "$DB" -P pager=off -c "
select m.auth_code, string_agg(r.role_code, ',' order by r.role_code) as roles
  from sys_role_menu rm join sys_menu m on m.id=rm.menu_id join sys_role r on r.id=rm.role_id
 where m.auth_code in ('business:erp:quotation:list','business:erp:quotation:create','business:erp:quotation:status',
                       'business:erp:rebate-policy:list','business:erp:rebate-policy:create','business:erp:rebate-settlement:list')
 group by 1 order by 1"
echo "-- 缺祖先的授权对（必须 0）"
psql -h "$H" -U cloudpivot -d "$DB" -Atc "
with recursive chain(role_id, node_id, anc_id) as (
    select rm.role_id, rm.menu_id, m.parent_id
      from sys_role_menu rm join sys_menu m on m.id = rm.menu_id and m.deleted_at is null
   union all
    select c.role_id, c.node_id, p.parent_id
      from chain c join sys_menu p on p.id = c.anc_id and p.deleted_at is null
)
select 'missing_ancestor_pairs='||count(*) from chain c
 where c.anc_id is not null
   and not exists (select 1 from sys_menu p where p.id = c.anc_id and p.deleted_at is null)"
echo "-- business:notice:mine 承载行（应 2：1 页 + 1 按钮）+ 按钮父节点"
psql -h "$H" -U cloudpivot -d "$DB" -P pager=off -c "
select m.id, m.menu_type, coalesce(p.title,'(父缺失)') as parent, p.id as parent_id
  from sys_menu m left join sys_menu p on p.id=m.parent_id
 where m.auth_code='business:notice:mine' and m.deleted_at is null order by m.menu_type"
echo "-- 菜单总数 / 系统管理是否由种子建出"
psql -h "$H" -U cloudpivot -d "$DB" -Atc "select 'menus='||count(*) from sys_menu where deleted_at is null"
psql -h "$H" -U cloudpivot -d "$DB" -Atc "select 'system_dir='||count(*) from sys_menu where id='10000000-0000-0000-0000-000000000001' and deleted_at is null"
psql -h "$H" -U cloudpivot -d "$DB" -Atc "select 'v442_checksum='||checksum from schema_migrations where kind='versioned' and version='442'"
psql -h "$H" -U cloudpivot -d "$DB" -Atc "select 'seed_rows='||count(*) from schema_migrations where kind='seed'"
