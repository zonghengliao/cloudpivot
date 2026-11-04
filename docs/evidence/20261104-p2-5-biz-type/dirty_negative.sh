#!/bin/bash
# ② 巡检能抓到域外值（故意造一条脏数据 → FAIL → 清掉 → PASS）
set -u
export PGPASSWORD=alun
PS="psql -h 127.0.0.1 -U alun -d cp_p25_vocab -tAF|"
cd /opt/cloudpivot
RUN="env NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun NIK_DATABASE_NAME=cp_p25_vocab python3 scripts/verify/biz_type_domain.py"

echo "########## 造脏数据：往 biz_status_log 塞一条 entity_type='TRANSFER'（P2-5 前的旧别名）"
$PS -c "insert into biz_status_log (id, entity_type, entity_id, entity_no, from_status, to_status, reason, created_at)
        values (gen_random_uuid(), 'TRANSFER', gen_random_uuid(), 'P25-DIRTY-0001', 'DRAFT', 'IN_TRANSIT', 'P2-5 巡检负例', now())"
$PS -c "select 'DIRTY_ROWS=' || count(*) from biz_status_log where entity_type='TRANSFER'"

echo
echo "########## 巡检（期望 FAIL，退出码 1）"
$RUN
echo "EXIT_AFTER_DIRTY=$?"

echo
echo "########## 清掉脏数据"
$PS -c "delete from biz_status_log where entity_no='P25-DIRTY-0001' and entity_type='TRANSFER'"
$PS -c "select 'DIRTY_ROWS_AFTER=' || count(*) from biz_status_log where entity_type='TRANSFER'"

echo
echo "########## 巡检（期望 PASS，退出码 0）"
$RUN
echo "EXIT_AFTER_CLEAN=$?"
