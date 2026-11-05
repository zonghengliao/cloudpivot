#!/usr/bin/env bash
# 演示「run 期间（run_epoch 之后）插入的临时行」被排除：
# 测试进程先起（ensure_db 钉住 run 起点），随后 psql 每 100ms 插一条源码里不存在的码。
# 期望：n14 绿 + 「已排除…N 行」N>0。
#
# 反面（上一版实验踩到的语义）：**进程启动之前**就存在的行一律算种子（只按夹具命名过滤），
# 所以那时候插入的 live:probe:code 会被判死码 —— 这不是缺陷，正是「只排除本次 run 造的」。
set -u
export SYS_DB_URL="${SYS_DB_URL:?}"
export CARGO_TARGET_DIR=/root/menu-iso-target
DB=${PROBE_DB:?}
su - postgres -c "psql -q -d $DB -c \"DELETE FROM sys_menu WHERE auth_code='live:probe:code';\"" >/dev/null 2>&1

insert_loop() {
  sleep 1.6            # 等 cargo 起进程 + ensure_db 钉住 run_epoch
  for _ in $(seq 1 20); do
    su - postgres -c "psql -q -d $DB -c \"INSERT INTO sys_menu (id,name,menu_type,auth_code) VALUES (gen_random_uuid(),'LIVE-PROBE',3,'live:probe:code');\"" >/dev/null 2>&1
    sleep 0.1
  done
}
insert_loop &
INS=$!
cd /root/menu-iso-tree/backend || exit 1
cargo test --lib system::menu::permission_audit_tests::n14_menu_auth_codes_have_source_action_point -- --exact --nocapture > /root/iso-runs/live-fixture-proof.txt 2>&1
wait "$INS" 2>/dev/null
grep -E "^\[N14\]|test result" /root/iso-runs/live-fixture-proof.txt
echo "--- 库里留下的行数（进程结束后仍在，属历史残留，按夹具名才排除）---"
su - postgres -c "psql -t -A -d $DB -c \"select count(*) from sys_menu where auth_code='live:probe:code';\""
su - postgres -c "psql -q -d $DB -c \"DELETE FROM sys_menu WHERE auth_code='live:probe:code';\"" >/dev/null 2>&1
