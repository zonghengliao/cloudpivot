#!/usr/bin/env bash
# E2E-09 一键演练：清场 → A（用户/角色/菜单/部门+权限闸）→ B（数据权限/脱敏/互斥/变更日志）
#                → C（审批降级链）→ DB 证据与勾稽
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
bash "$HERE/e2e09_cleanup.sh" | tail -3
: > /tmp/e2e09/state.sh
: > /tmp/e2e09/asserts.log
say() { printf '%s\n' "$*"; }
say "=================================================================="
bash "$HERE/e2e09_sys_a.sh" || say "A 脚本异常退出 rc=$?"
say "=================================================================="
bash "$HERE/e2e09_sys_b.sh" || say "B 脚本异常退出 rc=$?"
say "=================================================================="
bash "$HERE/e2e09_escalation.sh" || say "C 脚本异常退出 rc=$?"
say "=================================================================="
bash "$HERE/e2e09_db_evidence.sh"
echo
echo "---- 断言汇总（累计） ----"
printf 'PASS=%s FAIL=%s\n' "$(grep -c '^PASS' /tmp/e2e09/asserts.log)" "$(grep -c '^FAIL' /tmp/e2e09/asserts.log)"
grep '^FAIL' /tmp/e2e09/asserts.log || echo "（无 FAIL）"
