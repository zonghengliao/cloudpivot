#!/usr/bin/env bash
# E2E-06 一键：清理本卡旧数据 → A（费用类型/预算编制/映射）→ B（报销/拦截/执行）→ 勾稽
# 用法：bash e2e06_run_all.sh   （在 145 上跑；DB=cloudpivot_dev，后端 127.0.0.1:8033）
set -uo pipefail
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
export PATH=/usr/bin:/bin:/usr/local/bin:$PATH
LOG=/tmp/e2e06/asserts.log
mkdir -p /tmp/e2e06/raw
: > "$LOG"; : > /tmp/e2e06/state.sh

set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"

echo "### 0) 清理本卡旧数据（只删 E2E06-BDG 部门 / e2e06_clerk 的单据）"
psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -v ON_ERROR_STOP=1 -f "$DIR/e2e06_reset.sql" | tail -2

echo "### 1) 阶段 A"
bash "$DIR/e2e06_budget_a.sh"
echo "### 2) 阶段 B"
bash "$DIR/e2e06_budget_b.sh"
echo "### 2b) E2E06-FIX-2 执行报表口径（月度 + effective 时点）"
bash "$DIR/e2e06_fix2_report_probe.sh"

echo "### 3) 勾稽"
REC=/tmp/e2e06/reconcile.log
psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -f "$DIR/e2e06_reconcile.sql" > "$REC" 2>&1
RC_FAIL=0
while IFS='|' read -r name actual expected; do
  case "$name" in check) continue ;; ---*) break ;; esac
  if [ "$expected" = "-" ] || [ -z "$expected" ]; then continue; fi
  if [ "$actual" = "$expected" ]; then
    printf 'PASS\t勾稽 %s = %s\n' "$name" "$actual" >> "$LOG"
    echo "  ✅ $name = $actual"
  else
    printf 'FAIL\t勾稽 %s = %s（期望 %s）\n' "$name" "$actual" "$expected" >> "$LOG"
    echo "  ❌ $name = $actual（期望 $expected）"; RC_FAIL=$((RC_FAIL+1))
  fi
done < "$REC"

P=$(grep -c '^PASS' "$LOG"); F=$(grep -c '^FAIL' "$LOG")
echo
echo "================ 汇总：PASS=$P FAIL=$F（其中勾稽失败 $RC_FAIL）================"
grep '^FAIL' "$LOG" || true
