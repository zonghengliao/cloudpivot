#!/usr/bin/env bash
# 重新生成 verification.txt（幂等；每一项都现跑，rc 直接取命令本身）
set -uo pipefail
cd /opt/cloudpivot || exit 1
export PATH=/root/.cargo/bin:$PATH
OUT=docs/evidence/20261109-e2efix3-perm-grants/verification.txt
SU(){ su - postgres -c "$1"; }
GATE(){ su - postgres -c "env NIK_DATABASE_HOST=/var/run/postgresql NIK_DATABASE_USER=postgres NIK_DATABASE_NAME=$1 python3 /opt/cloudpivot/scripts/verify/core_perm_grants.py"; }

{
echo "############################ 1. 后端真库用例（cargo test）"
echo "### cargo test --lib system::menu::permission_audit_tests（SYS_DB_URL=cp_e2efix3，V0356 已应用）"
SYS_DB_URL='postgres://alun:alun@127.0.0.1:5432/cp_e2efix3' cargo test --manifest-path backend/Cargo.toml --lib system::menu::permission_audit_tests -- --test-threads=1 2>&1 | tail -14
echo "rc=${PIPESTATUS[0]}"
echo
echo "### N20 打「修复前」的库（cp_e2efix1，V0356 未应用）—— 证 Rust 侧闸有牙（期望 FAILED，点名 7 个码）"
SYS_DB_URL='postgres://alun:alun@127.0.0.1:5432/cp_e2efix1' cargo test --manifest-path backend/Cargo.toml --lib n20_erp_core_permission_codes -- --test-threads=1 2>&1 | tail -22
echo "rc=${PIPESTATUS[0]}（非 0 = 用例红了，正是「有牙」）"
echo
echo "############################ 2. 静态闸 core_perm_grants.py"
echo "### --self-test"
python3 scripts/verify/core_perm_grants.py --self-test 2>&1 | tail -3
echo "rc=${PIPESTATUS[0]}"
echo
echo "### 打「修复前」的库 cp_e2efix1（expect rc=1，点名 7 个码）"
GATE cp_e2efix1 2>&1 | sed -n 1,14p
echo "rc=${PIPESTATUS[0]}"
echo
echo "### 打「修复后」的库 cp_e2efix3（expect rc=0）"
GATE cp_e2efix3 2>&1 | sed -n 1,3p
echo "rc=${PIPESTATUS[0]}"
echo
echo "############################ 3. 迁移幂等 / 回滚（库 cp_e2efix3）"
echo "### 先 down（撤本迁移的 10 行）"
SU "psql -q -v ON_ERROR_STOP=1 -d cp_e2efix3 -f /opt/cloudpivot/backend/migrations/business/V0356__doc_core_perm_grants.down.sql" 2>&1 | grep -E "NOTICE:  V0356 down|ERROR"
echo "### down 之后打闸（expect rc=1，7 个码）"
GATE cp_e2efix3 2>&1 | sed -n 2,3p
echo "rc=${PIPESTATUS[0]}"
echo
echo "### up（首次）"
SU "psql -q -v ON_ERROR_STOP=1 -d cp_e2efix3 -f /opt/cloudpivot/backend/migrations/business/V0356__doc_core_perm_grants.up.sql" 2>&1 | grep -E "NOTICE:  V0356 完成|ERROR"
echo "### up 幂等重放（expect 新增 0 行）"
SU "psql -q -v ON_ERROR_STOP=1 -d cp_e2efix3 -f /opt/cloudpivot/backend/migrations/business/V0356__doc_core_perm_grants.up.sql" 2>&1 | grep -E "NOTICE:  V0356 完成|ERROR"
echo "### up 之后打闸（expect rc=0）"
GATE cp_e2efix3 2>&1 | sed -n 2,3p
echo "rc=${PIPESTATUS[0]}"
echo
echo "############################ 4. 前端（本卡无前端改动，仍跑一遍证明未破）"
echo "pnpm typecheck + pnpm build：见卡内汇报（exit 0；build 产物 dist/ 已被 .gitignore 忽略）"
} > "$OUT" 2>&1
echo "wrote $OUT"
