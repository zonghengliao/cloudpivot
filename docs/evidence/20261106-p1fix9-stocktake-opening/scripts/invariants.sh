#!/usr/bin/env bash
# 独立三本账审计（不信任业务代码）：erp_invariants.py 指向隔离库
set -u
label=${1:-after}
NIK_DATABASE_HOST=127.0.0.1 \
NIK_DATABASE_PORT=5432 \
NIK_DATABASE_USER=alun \
NIK_DATABASE_PASSWORD=alun \
NIK_DATABASE_NAME=cloudpivot_st9 \
python3 /opt/cloudpivot/scripts/verify/erp_invariants.py > "/root/st9-run/invariants_${label}.txt" 2>&1
echo "exit=$? -> /root/st9-run/invariants_${label}.txt"
tail -12 "/root/st9-run/invariants_${label}.txt"
