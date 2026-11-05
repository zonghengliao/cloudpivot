#!/usr/bin/env bash
# 隔离树构建：shared 主仓正被别的卡改到编译不过（business/flow 拆分重构中），
# 取 HEAD + 本卡 2 个文件到独立树，用私有 target 编译/跑测试（skill §8）。
set -eu
TREE=/root/menu-iso-tree
TARGET=/root/menu-iso-target
mkdir -p "$TREE"
cd /opt/cloudpivot
git archive HEAD | tar -x -C "$TREE"
# 本卡改动（只有这两个文件）
install -D backend/src/system/menu/permission_audit_tests.rs "$TREE/backend/src/system/menu/permission_audit_tests.rs"
install -D backend/src/system/tests_support.rs "$TREE/backend/src/system/tests_support.rs"
# skill §8.1：config-*.toml 是 gitignore 的本地配置，git archive 里没有（shared::startup_guard 用例会读）
cp backend/config/config-dev.toml "$TREE/backend/config/config-dev.toml"
cp backend/config/config-test.toml "$TREE/backend/config/config-test.toml"
cp backend/config/config.toml "$TREE/backend/config/config.toml" 2>/dev/null || true
echo "tree ready: $TREE"
diff -r --brief backend/src/system/menu/permission_audit_tests.rs "$TREE/backend/src/system/menu/permission_audit_tests.rs" && echo "audit file synced"
