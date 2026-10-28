#!/usr/bin/env bash
# 在本地 PG 18.6 上真跑一遍 V0046 / V0047（含幂等复跑与 down 回滚），留下可核对输出。
# psql 以 postgres 用户执行，而 /root 不可读，所以先把 SQL 暂存到 /var/tmp/a7sql（644）。
set -uo pipefail
DB=a7_permcheck
STAGE=/var/tmp/a7sql
SRC=/root/work/a7
LOG=$SRC/evidence/perm_seed_check.log
mkdir -p "$SRC/evidence" "$STAGE"
chmod 755 "$STAGE"
: > "$LOG"

stage() {  # $1 = 源文件绝对路径
  cp "$1" "$STAGE/$(basename "$1")"
  chmod 644 "$STAGE/$(basename "$1")"
  echo "$STAGE/$(basename "$1")"
}

run_psql() { su postgres -c "psql -v ON_ERROR_STOP=1 -q -d $DB -f $1" >>"$LOG" 2>&1; }

echo "==== 0. 新建空库 $DB ====" | tee -a "$LOG"
su postgres -c "psql -q -c 'DROP DATABASE IF EXISTS $DB;'" >>"$LOG" 2>&1
su postgres -c "psql -q -c 'CREATE DATABASE $DB;'" >>"$LOG" 2>&1

echo "==== 1. 按版本序应用底座迁移（system → support → business） ====" | tee -a "$LOG"
for dir in system support business; do
  for f in $(ls $SRC/backend/migrations/$dir/*.up.sql 2>/dev/null | sort -V); do
    target=$(stage "$f")
    if run_psql "$target"; then
      echo "  OK   ${f#$SRC/}" | tee -a "$LOG"
    else
      echo "  FAIL ${f#$SRC/}" | tee -a "$LOG"
    fi
  done
done

echo "==== 2. 应用种子数据（角色 / 菜单） ====" | tee -a "$LOG"
for f in $(ls $SRC/backend/migrations/seed/*.sql 2>/dev/null | sort); do
  target=$(stage "$f")
  if run_psql "$target"; then echo "  OK   ${f#$SRC/}" | tee -a "$LOG"; else echo "  FAIL ${f#$SRC/}" | tee -a "$LOG"; fi
done

echo "==== 2.1 造一个非超管角色（验证「所有角色」授权分支） ====" | tee -a "$LOG"
su postgres -c "psql -v ON_ERROR_STOP=1 -q -d $DB -c \"
INSERT INTO sys_role (id, role_code, role_name, status, sort_order, remark)
VALUES ('30000000-0000-0000-0000-000000000001'::uuid, 'OPS_ONLY', '运维（仅本站权限）', 1, 99, 'A7 验收用')
ON CONFLICT (id) DO NOTHING;\"" >>"$LOG" 2>&1 && echo "  OK   非超管角色 OPS_ONLY 已建" | tee -a "$LOG"

echo "==== 3. 首次执行 V0046 / V0047 ====" | tee -a "$LOG"
for v in V0046__seed_storage_permissions V0047__seed_impexp_notice_permissions; do
  target=$(stage "$SRC/backend/migrations/business/$v.up.sql")
  if run_psql "$target"; then echo "  OK   $v.up.sql" | tee -a "$LOG"; else echo "  FAIL $v.up.sql" | tee -a "$LOG"; fi
done

echo "==== 4. 幂等复跑（第二次执行不应报错、不产生重复行） ====" | tee -a "$LOG"
for v in V0046__seed_storage_permissions V0047__seed_impexp_notice_permissions; do
  target=$(stage "$SRC/backend/migrations/business/$v.up.sql")
  if run_psql "$target"; then echo "  OK   $v.up.sql (2nd)" | tee -a "$LOG"; else echo "  FAIL $v.up.sql (2nd)" | tee -a "$LOG"; fi
done

echo "==== 5. 结果核对 ====" | tee -a "$LOG"
su postgres -c "psql -d $DB -c \"
SELECT menu_type, count(*) AS rows, count(DISTINCT auth_code) AS distinct_codes
FROM sys_menu WHERE module_code='business' GROUP BY 1 ORDER BY 1;\"" | tee -a "$LOG"
su postgres -c "psql -d $DB -c \"
SELECT auth_code, title FROM sys_menu
WHERE auth_code LIKE 'business:storage:%' OR auth_code LIKE 'business:impexp:%'
ORDER BY auth_code;\"" | tee -a "$LOG"
su postgres -c "psql -d $DB -c \"
SELECT m.path, m.title, m.is_visible FROM sys_menu m
WHERE m.module_code='business' AND m.menu_type=2 ORDER BY m.sort_order;\"" | tee -a "$LOG"
su postgres -c "psql -d $DB -c \"
SELECT r.role_code, count(*) AS granted_menus FROM sys_role_menu rm
JOIN sys_role r ON r.id = rm.role_id GROUP BY 1 ORDER BY 1;\"" | tee -a "$LOG"
su postgres -c "psql -d $DB -c \"
SELECT m.auth_code, m.title FROM sys_role_menu rm
JOIN sys_menu m ON m.id = rm.menu_id
JOIN sys_role r ON r.id = rm.role_id
WHERE r.role_code = 'OPS_ONLY' ORDER BY m.auth_code;\"" | tee -a "$LOG"

echo "==== 6. 回滚 V0047 / V0046（down） ====" | tee -a "$LOG"
for v in V0047__seed_impexp_notice_permissions V0046__seed_storage_permissions; do
  target=$(stage "$SRC/backend/migrations/business/$v.down.sql")
  if run_psql "$target"; then echo "  OK   $v.down.sql" | tee -a "$LOG"; else echo "  FAIL $v.down.sql" | tee -a "$LOG"; fi
done

echo "==== 7. 回滚后应为 0 行 ====" | tee -a "$LOG"
su postgres -c "psql -d $DB -c \"
SELECT count(*) AS leftover_batch_a_menus FROM sys_menu
WHERE id::text LIKE '21%' OR id::text LIKE '22%' OR id::text LIKE '23%';\"" | tee -a "$LOG"
su postgres -c "psql -d $DB -c \"
SELECT count(*) AS leftover_grants FROM sys_role_menu
WHERE menu_id::text LIKE '21%' OR menu_id::text LIKE '22%' OR menu_id::text LIKE '23%';\"" | tee -a "$LOG"
echo "DONE"
