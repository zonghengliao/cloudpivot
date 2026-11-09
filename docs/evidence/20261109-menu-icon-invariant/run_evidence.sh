#!/usr/bin/env bash
# 复跑「菜单不变量第 ⑧ 条」的全部证据（本脚本生成同目录 raw/ 下的原始输出）。
# 只读：对 145/144 只跑 SELECT；写操作只发生在一次性副本库（teeth）。
#
# 用法：bash docs/evidence/20261109-menu-icon-invariant/run_evidence.sh <teeth_db>
#   前置：<teeth_db> 是一次性副本库（sys_menu 表已 dump），本机 PG 口令/用户见 PGPASSWORD/PGUSER
set -uo pipefail

TEETH_DB="${1:-}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../../.." && pwd)"
RAW="$HERE/raw"
mkdir -p "$RAW"
cd "$REPO" || exit 1
export LC_ALL=C LANG=C

run_local() {  # run_local <标签> <库>
  local tag=$1 db=$2
  bash scripts/verify/menu_invariants.sh "$db" > "$RAW/$tag.txt" 2>&1
  echo "rc=$?" >> "$RAW/$tag.txt"
}
run_remote() {  # run_remote <标签> <host> <env文件>
  local tag=$1 host=$2 envf=$3
  scp -q scripts/verify/menu_invariants.sh "root@$host:/tmp/menu_invariants_8.sh"
  ssh -o ConnectTimeout=8 "root@$host" \
    "set -a; . /etc/cloudpivot/$envf; set +a; export PGOPTIONS='-c default_transaction_read_only=on';
     MENU_REPO_ROOT=/opt/cloudpivot bash /tmp/menu_invariants_8.sh; echo rc=\$?" \
    > "$RAW/$tag.txt" 2>&1
}

run_local 01_local_head_db cloudpivot_code09
run_local 04_prod_shape_copy cloudpivot_r2
run_local 05_old_db_invoice cloudpivot_test_iso2
run_remote 02_145_dev 172.18.10.145 cloudpivot-dev.env
run_remote 03_144_prod 172.18.10.144 cloudpivot.env

if [ -n "$TEETH_DB" ]; then
  DB="$TEETH_DB" OUT="$RAW/06_teeth" bash scripts/verify/icon_invariant_teeth.sh > "$RAW/06_teeth_summary.txt" 2>&1
  echo "rc=$?" >> "$RAW/06_teeth_summary.txt"
fi

grep -H '^rc=' "$RAW"/*.txt | sed 's|.*/raw/||'
