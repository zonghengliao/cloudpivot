#!/usr/bin/env bash
# A/B 归因：把隔离树里本卡的两个文件回滚成 HEAD 原版 → 同一实验（run 期间插临时行）
# 应复现红线；再装回修复版 → 应转绿。同路径只增量编译。
set -eu
TREE=/root/menu-iso-tree
mkdir -p /root/iso-runs/fixed
cp "$TREE/backend/src/system/menu/permission_audit_tests.rs" /root/iso-runs/fixed/
cp "$TREE/backend/src/system/tests_support.rs" /root/iso-runs/fixed/
git -C /opt/cloudpivot show HEAD:backend/src/system/menu/permission_audit_tests.rs > "$TREE/backend/src/system/menu/permission_audit_tests.rs"
git -C /opt/cloudpivot show HEAD:backend/src/system/tests_support.rs > "$TREE/backend/src/system/tests_support.rs"
echo "== 已回滚到 HEAD 版本，重建（增量）=="
cd "$TREE/backend"
CARGO_TARGET_DIR=/root/menu-iso-target CARGO_BUILD_JOBS=3 cargo test --lib --no-run 2>&1 | tail -2
echo "== 跑同一实验（A：HEAD 原版，期望红）=="
bash /root/iso-runs/probe_live_fixture.sh 2>&1 | tail -6
echo "== 装回修复版并重建 =="
cp /root/iso-runs/fixed/permission_audit_tests.rs "$TREE/backend/src/system/menu/permission_audit_tests.rs"
cp /root/iso-runs/fixed/tests_support.rs "$TREE/backend/src/system/tests_support.rs"
CARGO_TARGET_DIR=/root/menu-iso-target CARGO_BUILD_JOBS=3 cargo test --lib --no-run 2>&1 | tail -2
echo "== 跑同一实验（B：修复版，期望绿）=="
bash /root/iso-runs/probe_live_fixture.sh 2>&1 | tail -6
