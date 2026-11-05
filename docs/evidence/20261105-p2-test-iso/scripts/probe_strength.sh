#!/usr/bin/env bash
# 判据强度自证：在探针库里种两条「过去的」行
#   ① PROBE-死码（普通名字）→ 必须被判死码（红）
#   ② N99-xxx（夹具前缀命名）→ 必须被排除（历史夹具残留，不算种子）
# 说明 run_epoch 过滤没有放宽判据，只排除了「本次 run 造的 + 夹具命名」的行。
set -u
export SYS_DB_URL="${SYS_DB_URL:?}"
export CARGO_TARGET_DIR=/root/menu-iso-target
su - postgres -c "dropdb --if-exists ${PROBE_DB:?}; createdb -O "${PROBE_OWNER:?}" -T ${PROBE_DB:?} ${PROBE_DB:?}"
su - postgres -c "psql -q -d ${PROBE_DB:?} -c \"INSERT INTO sys_menu (id,name,menu_type,auth_code,created_at) VALUES (gen_random_uuid(),'PROBE-死码',3,'probe:dead:code', now() - interval '1 day');\""
cd /root/menu-iso-tree/backend || exit 1
echo "=== ① 只有 PROBE-死码（普通名字、过去的行）→ 期望 n14 红 ==="
cargo test --lib system::menu::permission_audit_tests::n14_menu_auth_codes_have_source_action_point -- --exact --nocapture > /root/iso-runs/probe-deadcode.txt 2>&1
grep -E "probe:dead:code|\[N14\]|test result" /root/iso-runs/probe-deadcode.txt | head -6

su - postgres -c "psql -q -d ${PROBE_DB:?} -c \"DELETE FROM sys_menu WHERE auth_code='probe:dead:code'; INSERT INTO sys_menu (id,name,menu_type,auth_code,created_at) VALUES (gen_random_uuid(),'N99-T-stale:btn',3,'N99-T-stale:btn', now() - interval '1 day');\""
echo "=== ② 只有 N99- 前缀（历史夹具残留、过去的行）→ 期望 n14 绿 ==="
cargo test --lib system::menu::permission_audit_tests::n14_menu_auth_codes_have_source_action_point -- --exact --nocapture > /root/iso-runs/probe-fixtureprefix.txt 2>&1
grep -E "\[N14\]|test result" /root/iso-runs/probe-fixtureprefix.txt | head -4
su - postgres -c "psql -q -d ${PROBE_DB:?} -c \"DELETE FROM sys_menu WHERE name LIKE 'N99-%'\""
