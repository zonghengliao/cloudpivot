#!/usr/bin/env bash
# REVERIFY：把 P0-03..06 的原始演练脚本复制到本目录并改三处环境（BASE / 库名 / $EV），
# 原目录一个字节不改（原始证据保持可追溯）。
set -euo pipefail
ROOT=/opt/cloudpivot/docs/evidence
DST=$ROOT/20261107-p0-reverify/chains
DB=$(ssh root@172.18.10.145 'cat /root/backup/.rev_db')
PORT=${REV_PORT:-8050}
mkdir -p "$DST"
for src in 20261107-p003-inventory-chain 20261107-p004-fixed-asset-chain 20261107-p005-expense-chain 20261107-p006-budget-chain; do
  short=$(echo "$src" | sed 's/20261107-\(p00[0-9]\).*/\1/')
  rm -rf "$DST/$short"
  cp -a "$ROOT/$src" "$DST/$short"
  rm -rf "$DST/$short/raw"; mkdir -p "$DST/$short/raw"
  # ① BASE：172.18.10.145:80 → 本机 SSH 隧道 → 145 隔离实例
  sed -i "s#export BASE=\"http://172.18.10.145\"#export BASE=\"\${REV_BASE:-http://127.0.0.1:$PORT}\"#" "$DST/$short/00_env.sh"
  # ② 库名：cloudpivot_dev → 拷贝库
  sed -i "s/-d cloudpivot_dev /-d $DB /g; s/-d cloudpivot_dev\"/-d $DB\"/g" "$DST/$short/00_env.sh"
  # ③ EV：原始证据目录 → 本目录（不覆盖旧 raw）
  sed -i "s#/opt/cloudpivot/docs/evidence/$src#/opt/cloudpivot/docs/evidence/20261107-p0-reverify/chains/$short#g" "$DST/$short/00_env.sh"
  # ④ 令牌文件目录分卡隔离
  sed -i "s#/root/.p00\([0-9]\)-tokens#/root/.rev-p00\1-tokens#g" "$DST/$short/00_env.sh"
  echo "== $short patched:"
  grep -E "export BASE|psql -h|export EV|TKDIR=" "$DST/$short/00_env.sh" | sed 's/^/   /'
done
echo "$DB" > /root/reverify/.rev_db
echo "rev_db=$DB"
