#!/usr/bin/env bash
# 门禁变异反证：把 V0442 换回「硬护栏」旧版（不含其它改动），门禁必须重新判红。
set -uo pipefail
WS=/root/.hermes/kanban/boards/dev/workspaces/t_08e1816a
REPO=/opt/cloudpivot
PROBE=$WS/gate_probe
rm -rf "$PROBE"
mkdir -p "$PROBE/backend/app" "$PROBE/scripts/lint"
cp -a "$REPO/backend/app/migrations" "$PROBE/backend/app/migrations"
cp "$REPO/scripts/lint/seed_dependency_allow.txt" "$PROBE/scripts/lint/"

echo "===== ① 修后（当前树）"
python3 "$REPO/scripts/lint/seed_dependency_gate.py" --root "$REPO" | tail -3
echo "    rc=$?"

echo
echo "===== ② 变异：把 V0442 换回旧版硬护栏"
git -C "$REPO" show HEAD:backend/app/migrations/business/V0442__perm_gate_orphans_and_n20.up.sql \
  > "$PROBE/backend/app/migrations/business/V0442__perm_gate_orphans_and_n20.up.sql"
grep -n "先应用系统菜单迁移'" "$PROBE/backend/app/migrations/business/V0442__perm_gate_orphans_and_n20.up.sql" | head -2
python3 "$REPO/scripts/lint/seed_dependency_gate.py" --root "$PROBE"
rc=${PIPESTATUS[0]}
echo "    （上面是变异树的输出；期望 rc=1）"
