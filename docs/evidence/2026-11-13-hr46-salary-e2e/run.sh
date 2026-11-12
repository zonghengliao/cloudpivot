#!/usr/bin/env bash
# ============================================================================
# HR-46 证据复现脚本：一条命令重跑本目录所有证据
#
#   ① 主跑：--fresh 建演练库 → 10 步 → DROP（期望 rc=0，98 项断言）
#   ② 护栏：非演练库名被拒（期望 rc=2）
#   ③ 有牙：把 EXPECT_GROSS 改坏再跑（期望 3 条断言报红、rc=1、库仍 DROP）
#   ④ 契约测试：cargo test --lib business::hr::hr_e2e_contract_tests（期望 8 passed）
#   ⑤ 清理复核：145 / 99 上 hr46% 库计数均为 0
#
# 用法：bash docs/evidence/2026-11-13-hr46-salary-e2e/run.sh
# 前置：99 上 root@172.18.10.145 免密 ssh 可读 /etc/cloudpivot/cloudpivot-dev.env；
#       后端已 cargo build --bin cloudpivot（或给 HR46_BIN）
# ============================================================================
set -uo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
OUT=${OUT:-/root/hr46-evidence-$(date +%Y%m%d-%H%M%S)}
mkdir -p "$OUT"
cd "$ROOT" || exit 1

echo "== 产物目录：$OUT =="

echo "== ① 主跑（10 步） =="
bash scripts/verify/hr/hr_salary_e2e.sh --fresh >"$OUT/01_e2e_green.out" 2>&1
echo "   rc=$?（期望 0）"

echo "== ② 护栏：非演练库名 =="
HR46_DB=cloudpivot_prod bash scripts/verify/hr/hr_salary_e2e.sh --fresh >"$OUT/02_guard.out" 2>&1
echo "   rc=$?（期望 2）"

echo "== ③ 有牙：改坏 EXPECT_GROSS =="
cp scripts/verify/hr/hr_salary_e2e.sh scripts/verify/hr/.teeth_hr_salary_e2e.sh
sed -i 's/^EXPECT_GROSS=15834.48$/EXPECT_GROSS=99999.00/' scripts/verify/hr/.teeth_hr_salary_e2e.sh
HR46_PORT=18147 bash scripts/verify/hr/.teeth_hr_salary_e2e.sh --fresh >"$OUT/03_teeth.out" 2>&1
echo "   rc=$?（期望 1）"
rm -f scripts/verify/hr/.teeth_hr_salary_e2e.sh

echo "== ④ 契约测试 =="
( cd backend && TMPDIR=/root/tmp-cargo cargo test --lib business::hr::hr_e2e_contract_tests -- --test-threads=1 ) \
  >"$OUT/04_contract_tests.out" 2>&1
echo "   rc=$?（期望 0，8 passed）"

echo "== ⑤ 清理复核 =="
DBPW=$(ssh -o ConnectTimeout=8 root@172.18.10.145 \
  'grep "^NIK_DATABASE_PASSWORD" /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2-')
{
  echo "145 上 hr46% 库计数（期望 0）："
  PGPASSWORD="$DBPW" psql -h 172.18.10.145 -U cloudpivot -d postgres -tAc \
    "select count(*) from pg_database where datname like 'hr46%'"
  echo "99 上 hr46% 库计数（期望 0）："
  PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d postgres -tAc \
    "select count(*) from pg_database where datname like 'hr46%'"
} | tee "$OUT/05_cleanup.out"

echo "== 完成：5 个输出文件在 $OUT =="
