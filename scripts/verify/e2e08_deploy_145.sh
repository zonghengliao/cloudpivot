#!/usr/bin/env bash
# 145 部署（无跟踪脚本挡 ff-merge 的版本）：把未跟踪 e2e08*.sh 挪走 → ff-merge → 比对内容 → 清理备份
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
cd /opt/cloudpivot
BAK=/tmp/e2e08_scripts_bak; mkdir -p "$BAK"
echo "== 部署前 HEAD: $(git rev-parse --short HEAD)"
for f in scripts/verify/e2e08_*.sh; do
  [ -f "$f" ] && cp -f "$f" "$BAK/$(basename "$f")" && rm -f "$f"
done
git fetch -q origin
git merge --ff-only origin/main 2>&1 | tail -3
echo "== 部署后 HEAD: $(git rev-parse --short HEAD)"
echo "== 恢复/比对脚本"
for f in "$BAK"/*.sh; do
  b=$(basename "$f")
  if [ -f "scripts/verify/$b" ]; then
    if cmp -s "$f" "scripts/verify/$b"; then rm -f "$f"; else echo "  差异（保留仓库版，备份留作对照）：$b"; fi
  else
    mv "$f" "scripts/verify/$b"; echo "  还原未跟踪脚本：$b"
  fi
done
cargo build --manifest-path backend/Cargo.toml --bin cloudpivot 2>&1 | tail -5
systemctl restart cloudpivot-test
for i in $(seq 1 30); do
  sleep 2
  code=$(curl -s -m 3 -o /dev/null -w '%{http_code}' http://127.0.0.1:8033/health || true)
  [ "$code" = "200" ] && { echo "health=200 (after ${i}x2s)"; break; }
done
systemctl is-active cloudpivot-test
