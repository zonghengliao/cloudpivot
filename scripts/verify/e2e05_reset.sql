-- E2E-05 演练数据重置（只删本卡自建数据：资产名 E2E05% / 分类码 E2E5% / 演练期间批次）
\set QUIET on
begin;
create temp table m_asset on commit drop as
  select id, asset_no from biz_fixed_asset where asset_name like 'E2E05%';
create temp table m_batch on commit drop as
  select id, coalesce(reverse_voucher_id, '00000000-0000-4000-8000-000000000000'::uuid) as rv
    from biz_depreciation_batch where period_code between '2027-12' and '2028-06';
create temp table m_notice on commit drop as
  select id from sys_notice where biz_id::text in (select id::text from m_asset);
create temp table m_inst on commit drop as
  select id from biz_flow_instance where biz_no in (select asset_no from m_asset);
create temp table m_vou on commit drop as
  select id from biz_voucher
   where source_id in (select id from m_asset)
      or source_no in (select asset_no from m_asset)
      or (source_type='DEPRECIATION' and source_id in (select id from m_batch))
      or id in (select rv from m_batch where rv <> '00000000-0000-4000-8000-000000000000'::uuid);

delete from biz_task where flow_instance_id in (select id from m_inst);
delete from biz_flow_task where instance_id in (select id from m_inst);
delete from biz_flow_cc where instance_id in (select id from m_inst);
delete from biz_flow_instance_node where instance_id in (select id from m_inst);
delete from biz_flow_instance where id in (select id from m_inst);
delete from sys_notice_receiver where notice_id in (select id from m_notice);
delete from sys_notice_delivery where notice_id in (select id from m_notice);
delete from sys_notice where id in (select id from m_notice);
delete from biz_voucher_entry where voucher_id in (select id from m_vou);
delete from biz_voucher where id in (select id from m_vou);
delete from biz_depreciation where asset_id in (select id from m_asset) or batch_id in (select id from m_batch);
delete from biz_depreciation_batch where id in (select id from m_batch);
delete from biz_fixed_asset_change where asset_id in (select id from m_asset);
delete from biz_status_log where entity_no in (select asset_no from m_asset);
delete from biz_fixed_asset where id in (select id from m_asset);
delete from biz_asset_category where category_code like 'E2E5%' or category_name like 'E2E05%';
commit;
select 'assets_left' as k, count(*)::text as v from biz_fixed_asset where asset_name like 'E2E05%'
union all select 'batches_left', count(*)::text from biz_depreciation_batch where period_code between '2027-12' and '2028-06'
union all select 'cats_left', count(*)::text from biz_asset_category where category_code like 'E2E5%'
union all select 'vouchers_left', count(*)::text from biz_voucher where source_no in (select source_no from biz_fixed_asset where asset_name like 'E2E05%');
