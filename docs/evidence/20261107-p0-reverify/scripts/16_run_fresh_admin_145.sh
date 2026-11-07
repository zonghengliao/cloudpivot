#!/usr/bin/env bash
# REVERIFY 特别验证① 启动器：全新库（只有 admin，8052）+ FIX-1 引导 → 销售链
set -uo pipefail
ENVF=/etc/cloudpivot/cloudpivot-dev.env
export PGPASSWORD=$(awk -F= '/^NIK_DATABASE_PASSWORD/{print $2}' "$ENVF")
export ADMIN_PW=$(awk -F= '/^NIK_ADMIN_PASSWORD/{print $2}' "$ENVF")
export REV_BASE=${REV_BASE:-http://127.0.0.1:8052}
export REV_DB=${REV_DB:-cloudpivot_rev_admin}
export REV_RAW=${REV_RAW:-/root/reverify/raw}
exec python3 "${HARNESS:-/root/reverify/fresh_admin_chain.py}" "$@"
