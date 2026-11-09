#!/usr/bin/env bash
# E2E-08 主数据域一键演练：清场 → A（自动编码矩阵+单位）→ B（五档案生命周期+序列号）→ C（价格+编码开关+AUD-01）→ DB 证据
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
bash "$HERE/e2e08_reset.sh" || exit 1
say "=================================================================="
bash "$HERE/e2e08_master_a.sh" || say "A 脚本异常退出 rc=$?"
say "=================================================================="
bash "$HERE/e2e08_master_b.sh" || say "B 脚本异常退出 rc=$?"
say "=================================================================="
bash "$HERE/e2e08_master_c.sh" || say "C 脚本异常退出 rc=$?"
say "=================================================================="
bash "$HERE/e2e08_db_evidence.sh"
echo
echo "---- 断言汇总（累计） ----"
printf 'PASS=%s FAIL=%s\n' "$(grep -c '^PASS' /tmp/e2e08/asserts.log)" "$(grep -c '^FAIL' /tmp/e2e08/asserts.log)"
grep '^FAIL' /tmp/e2e08/asserts.log || echo "（无 FAIL）"
