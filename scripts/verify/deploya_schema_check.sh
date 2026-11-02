#!/usr/bin/env bash
# =============================================================================
# DEPLOY-A 结构验收：合同/采购/入库三批迁移的 32 条断言（PASS/FAIL，有 FAIL 即 exit 1）
#
# 用法：
#   set -a; . /etc/cloudpivot/cloudpivot.env; set +a
#   bash scripts/verify/deploya_schema_check.sh [db]
#   # 存量行数基线可用环境变量覆盖（默认取 A1–A4 上线前的生产快照 15/12）
#   EXPECT_PO=15 EXPECT_SI=12 bash scripts/verify/deploya_schema_check.sh cloudpivot
# =============================================================================
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
export PGPASSWORD="${NIK_DATABASE_PASSWORD:?}"
DB="${1:-${NIK_DATABASE_NAME:-cloudpivot}}"
PSQL_USER="${NIK_DATABASE_USER:-cloudpivot}"
PSQL_HOST="${NIK_DATABASE_HOST:-127.0.0.1}"
EXPECT_PO="${EXPECT_PO:-15}"
EXPECT_SI="${EXPECT_SI:-12}"
Q() { psql -h "$PSQL_HOST" -U "$PSQL_USER" -d "$DB" -Atq -c "$1"; }
FAILS=0; PASSES=0
chk() { # <标签> <实际> <期望>
  if [ "$2" = "$3" ]; then PASSES=$((PASSES+1)); printf '  PASS  %-46s = %s\n' "$1" "$2"
  else FAILS=$((FAILS+1)); printf '  FAIL  %-46s 实际=[%s] 期望=[%s]\n' "$1" "$2" "$3"; fi
}

echo "===== 结构验收 @ $DB ====="
echo "[V0193] 流程定义 is_system"
chk "biz_flow_def.is_system 列" "$(Q "select count(*) from information_schema.columns where table_name='biz_flow_def' and column_name='is_system'")" "1"
echo "[V0194] 流程模板库菜单"
chk "菜单 2000…07a5 存在且可见" "$(Q "select count(*) from sys_menu where id='20000000-0000-0000-0000-0000000007a5' and is_visible and deleted_at is null")" "1"

echo "[V0195] biz_contract"
chk "biz_contract 表存在" "$(Q "select count(*) from information_schema.tables where table_name='biz_contract'")" "1"
chk "biz_contract 列数" "$(Q "select count(*) from information_schema.columns where table_name='biz_contract'")" "23"
chk "biz_contract CHECK 约束数" "$(Q "select count(*) from pg_constraint where conrelid='biz_contract'::regclass and contype='c'")" "9"
chk "biz_contract 业务索引数" "$(Q "select count(*) from pg_indexes where tablename='biz_contract' and indexname in ('uk_biz_contract_no','idx_biz_contract_party_b','idx_biz_contract_status','idx_biz_contract_end')")" "4"
chk "biz_contract 索引总数(含主键)" "$(Q "select count(*) from pg_indexes where tablename='biz_contract'")" "5"
chk "biz_contract COMMENT 数(表1+列18)" "$(Q "select (case when obj_description('biz_contract'::regclass) is not null then 1 else 0 end) + (select count(*) from pg_description d join pg_class c on c.oid=d.objoid join pg_attribute a on a.attrelid=c.oid and a.attnum=d.objsubid where c.relname='biz_contract' and d.objsubid>0)")" "19"
chk "biz_seq 的 CT 取号规则" "$(Q "select count(*) from biz_seq where seq_type='CT'")" "1"

echo "[V0196] ERP 内控开关"
chk "sys_config 内控参数 4 条" "$(Q "select count(*) from sys_config where config_key like 'erp.separation_of_duties.%' and status=1 and deleted_at is null")" "4"
chk "内控参数类型全 BOOLEAN" "$(Q "select count(*) from sys_config where config_key like 'erp.separation_of_duties.%' and config_type='BOOLEAN'")" "4"
chk "默认值 enabled=false" "$(Q "select config_value from sys_config where config_key='erp.separation_of_duties.enabled'")" "false"
chk "默认值 post=false" "$(Q "select config_value from sys_config where config_key='erp.separation_of_duties.post'")" "false"
chk "默认值 approve=true" "$(Q "select config_value from sys_config where config_key='erp.separation_of_duties.approve'")" "true"
chk "默认值 pay=true" "$(Q "select config_value from sys_config where config_key='erp.separation_of_duties.pay'")" "true"
chk "字典项 erp_internal_control" "$(Q "select count(*) from sys_dict_item i join sys_dict_type t on t.id=i.dict_type_id where t.dict_code='config_category' and i.item_value='erp_internal_control'")" "1"

echo "[V0197] 合同权限码/菜单"
chk "erp:contract:* 权限码 5 个" "$(Q "select count(*) from sys_menu where auth_code like 'erp:contract:%' and deleted_at is null")" "5"
chk "合同菜单 component/path" "$(Q "select count(*) from sys_menu where id='20000000-0000-0000-0000-000000001100' and component='business/erp/contract/index' and path='/business/erp/contract'")" "1"

echo "[V0198] 采购单绑定合同"
chk "采购单 4 个新列" "$(Q "select count(*) from information_schema.columns where table_name='biz_purchase_order' and column_name in ('contract_id','purchase_type','contract_no_snapshot','supplier_name_snapshot')")" "4"
chk "3 个新约束" "$(Q "select count(*) from pg_constraint where conname in ('ck_biz_po_purchase_type','ck_biz_po_contract_pair','ck_biz_si_supplier_scope')")" "3"
chk "索引 idx_biz_po_contract" "$(Q "select count(*) from pg_indexes where indexname='idx_biz_po_contract'")" "1"
chk "存量(迁移前建单) purchase_type 全 SPOT" "$(Q "select count(distinct purchase_type)||':'||coalesce(min(purchase_type),'-') from biz_purchase_order where created_at < to_timestamp(coalesce((select applied_at from schema_migrations where kind='versioned' and version='195'), extract(epoch from now())))")" "1:SPOT"
chk "违反 ck_biz_po_contract_pair 行数" "$(Q "select count(*) from biz_purchase_order where not (case purchase_type when 'CONTRACT' then contract_id is not null and contract_no_snapshot is not null when 'SPOT' then contract_id is null and contract_no_snapshot is null else false end)")" "0"

echo "[V0199] 入库单无源标记"
chk "biz_stock_in.is_sourceless 列" "$(Q "select count(*) from information_schema.columns where table_name='biz_stock_in' and column_name='is_sourceless'")" "1"
chk "约束 ck_biz_si_sourceless_pair" "$(Q "select count(*) from pg_constraint where conname='ck_biz_si_sourceless_pair'")" "1"
chk "索引 idx_biz_si_sourceless" "$(Q "select count(*) from pg_indexes where indexname='idx_biz_si_sourceless'")" "1"
chk "无源标记与 source_order_id 一致(违反数)" "$(Q "select count(*) from biz_stock_in where (is_sourceless = (source_order_id is null)) is not true")" "0"

echo "[V0200] 合同菜单上侧栏"
chk "合同菜单 is_visible=TRUE 且根级" "$(Q "select count(*) from sys_menu where id='20000000-0000-0000-0000-000000001100' and is_visible and parent_id is null and menu_type=2")" "1"

echo "[迁移记录]"
chk "schema_migrations ≥0195 记录数" "$(Q "select count(*) from schema_migrations where kind='versioned' and version::int >= 195")" "6"
chk "schema_migrations 记录(193/194)" "$(Q "select count(*) from schema_migrations where kind='versioned' and version in ('193','194')")" "2"

echo "[存量未被动]"
chk "存量采购单行数（>= 基线）" "$(Q "select case when count(*) >= $EXPECT_PO then 'OK' else 'SHRUNK:'||count(*) end from biz_purchase_order")" "OK"
chk "存量入库单行数（>= 基线）" "$(Q "select case when count(*) >= $EXPECT_SI then 'OK' else 'SHRUNK:'||count(*) end from biz_stock_in")" "OK"

echo "===== 汇总: PASS=$PASSES FAIL=$FAILS ====="
[ "$FAILS" -eq 0 ] || exit 1
