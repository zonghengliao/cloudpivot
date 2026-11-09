#!/usr/bin/env bash
# E2E-04 演练重置（只清本卡自己造的 2022 年度凭证 / 红字凭证 / 流程实例；期初凭证与期间行保留）
# 用途：A 段中途失败后恢复演练起点，可重复执行。
set -uo pipefail
ENV=/etc/cloudpivot/cloudpivot-dev.env
set -a; . "$ENV"; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
PSQLQ() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -At -F'|' -P pager=off -c "$1"; }

# 本卡 2022 凭证集合 = 2022 年度非 OPENING 凭证 + 它们的红字凭证
SCOPE="(select id from biz_voucher where period like '2022-%' and voucher_type<>'OPENING'
        union select id from biz_voucher where reverse_of in
              (select id from biz_voucher where period like '2022-%' and voucher_type<>'OPENING'))"

echo "== 重置前"
PSQLQ "select status||'/'||voucher_type||'/'||period,count(*) from biz_voucher
        where id in $SCOPE group by status,voucher_type,period;"

IIDS=$(PSQLQ "select id from biz_flow_instance where biz_type='VOUCHER_REVERSE' and biz_no in (select voucher_no from biz_voucher where id in $SCOPE);")
for iid in $IIDS; do
  PSQLQ "delete from biz_task where flow_instance_id='$iid';" >/dev/null
  PSQLQ "delete from biz_flow_cc where instance_id='$iid';" >/dev/null
  PSQLQ "delete from biz_flow_task where instance_id='$iid';" >/dev/null
  PSQLQ "delete from biz_flow_instance_node where instance_id='$iid';" >/dev/null
  PSQLQ "delete from biz_flow_instance where id='$iid';" >/dev/null
done
echo "== 清理红冲审批实例 $(echo $IIDS | wc -w) 个"
PSQLQ "delete from biz_status_log where entity_type='VOUCHER' and entity_id in $SCOPE;"
PSQLQ "delete from biz_voucher_entry where voucher_id in $SCOPE;"
PSQLQ "delete from biz_voucher where id in $SCOPE;"
echo "== 重置后"
PSQLQ "select status||'/'||voucher_type||'/'||period,count(*) from biz_voucher
        where period like '2022-%' or period like '2026-11' and is_red=1
        group by status,voucher_type,period;"
if [ -f /tmp/e2e04/state.sh ]; then
  grep -v -E '^(A4_SM_DONE|A4_REV_IID)=' /tmp/e2e04/state.sh > /tmp/e2e04/state.tmp || true
  mv /tmp/e2e04/state.tmp /tmp/e2e04/state.sh
  echo "== 已清 A4_SM_DONE / A4_REV_IID 标记"
fi
