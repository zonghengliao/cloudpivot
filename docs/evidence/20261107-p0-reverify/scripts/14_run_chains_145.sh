#!/usr/bin/env bash
# REVERIFY（t_2f92ee87）六条链 harness 启动器（145 本机）
# 口令从 /etc/cloudpivot/cloudpivot-dev.env 就地读取，**不打印、不落盘**
set -uo pipefail
ENVF=/etc/cloudpivot/cloudpivot-dev.env
export PGPASSWORD=$(awk -F= '/^NIK_DATABASE_PASSWORD/{print $2}' "$ENVF")
export ADMIN_PW=$(awk -F= '/^NIK_ADMIN_PASSWORD/{print $2}' "$ENVF")
export DEMO_PW=${DEMO_PW:-ChangeMe@2026}
export REV_DB=${REV_DB:-cloudpivot_rev_demo}
export REV_BASE=${REV_BASE:-http://127.0.0.1:8051}
export REV_RAW=${REV_RAW:-/root/reverify/raw}
exec python3 "${HARNESS:-/root/reverify/reverify_chains.py}" "$@"
