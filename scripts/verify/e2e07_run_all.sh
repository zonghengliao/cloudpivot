#!/usr/bin/env bash
# E2E-07 一键：A（四类建档/附件/变更/提交）→ B（审批/履约/提醒/归档）→ 勾稽
# 用法：bash e2e07_run_all.sh   （在 145 上跑；DB=cloudpivot_dev，后端 127.0.0.1:8033）
set -uo pipefail
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
export PATH=/usr/bin:/bin:/usr/local/bin:$PATH
LOG=/tmp/e2e07/asserts.log
mkdir -p /tmp/e2e07/raw
: > "$LOG"; : > /tmp/e2e07/state.sh

set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"

echo "### 0) 清理本卡旧演练数据（只删 E2E07-% 合同及其派生单据）"
psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -v ON_ERROR_STOP=1 -f "$DIR/e2e07_reset.sql" | tail -3

echo "### 1) 阶段 A"
bash "$DIR/e2e07_contract_a.sh"
echo "### 2) 阶段 B"
bash "$DIR/e2e07_contract_b.sh"

echo "### 3) 勾稽"
REC=/tmp/e2e07/reconcile.log
psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -f "$DIR/e2e07_reconcile.sql" > "$REC" 2>&1
RC_FAIL=0
while IFS='|' read -r name actual expected; do
  case "$name" in check) ;; *) continue ;; esac
  if [ -z "$expected" ]; then continue; fi
  if [ "$actual" = "$expected" ]; then
    printf 'PASS\t勾稽 %s = %s\n' "$actual" "$actual" >> "$LOG"
    echo "  ✅ 勾稽项 = $actual（期望 $expected）"
  else
    printf 'FAIL\t勾稽项 = %s（期望 %s）\n' "$actual" "$expected" >> "$LOG"
    echo "  ❌ 勾稽项 = $actual（期望 $expected）"; RC_FAIL=$((RC_FAIL+1))
  fi
done < "$REC"

P=$(grep -c '^PASS' "$LOG"); F=$(grep -c '^FAIL' "$LOG")
echo
echo "================ 汇总：PASS=$P FAIL=$F（其中勾稽失败 $RC_FAIL）================"
grep '^FAIL' "$LOG" || true
