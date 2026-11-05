#!/usr/bin/env bash
# ============================================================================
# BIZPERM · 缓存失效（改授权后必须跑）
# ----------------------------------------------------------------------------
# 三件事（缺一件就会出现「库里补了码、接口还 403」或「刚登录就 401」）：
#   1. 推进 perm scope epoch（**必须带 cloudpivot: 前缀**：应用读
#      `cloudpivot:perm:user_scope_epoch`，裸名 `perm:user_scope_epoch` 是空转 ——
#      A 卡实测，见 scripts/datafix/superconv/README.md §缓存失效）
#   2. 删每个持有者的 `cloudpivot:uv:<uid>`（不删 → 推进 token_version 后 60s 内
#      连重新登录都 401/20008）
#   3. 删 `cloudpivot:perm:user_perms|user_roles|user_menus:<uid>` 与
#      `cloudpivot:perm:user_scope:*:<uid>`
#
# 只对「持有 5 个业务角色」的账号做（不动 admin 与其他账号的会话）。
# 用法：bash cache_invalidate.sh [--dry-run]
# ============================================================================
set -uo pipefail

ENV_FILE="${CLOUDPIVOT_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}"
set -a; . "$ENV_FILE"; set +a
export PGPASSWORD="${NIK_DATABASE_PASSWORD}"
DB_HOST="${NIK_DATABASE_HOST:-127.0.0.1}"
DB_PORT="${NIK_DATABASE_PORT:-5432}"
DB_USER="${NIK_DATABASE_USER:-cloudpivot}"
DB_NAME="${BIZPERM_DB:-${NIK_DATABASE_NAME:-cloudpivot}}"

DRY=0
[ "${1:-}" = "--dry-run" ] && DRY=1

R() { redis-cli -n 0 "$@"; }
q() { psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -X -q -At -c "$1"; }

echo "== 0) 目标账号（持有 5 个业务角色者）=="
USERS=$(q "SELECT DISTINCT u.username FROM sys_user u
             JOIN sys_user_role ur ON ur.user_id = u.id
             JOIN sys_role r ON r.id = ur.role_id
            WHERE r.role_code IN ('DEPT_LEADER','FINANCE_MANAGER','WAREHOUSE_MANAGER','PURCHASE_MANAGER','GENERAL_MANAGER')
              AND u.deleted_at IS NULL ORDER BY 1")
echo "$USERS" | sed 's/^/  /'

echo "== 1) scope epoch =="
for k in cloudpivot:perm:user_scope_epoch perm:user_scope_epoch; do
    before=$(R GET "$k")
    if [ "$DRY" = "1" ]; then
        echo "  [dry-run] INCR $k（当前 $before）"
    else
        R INCR "$k" >/dev/null
        echo "  $k: $before → $(R GET "$k")"
    fi
done

echo "== 2/3) 逐账号删会话 + 权限 + 角色 + 菜单 + scope 缓存 =="
for u in $USERS; do
    uid=$(q "SELECT id FROM sys_user WHERE username='$u' AND deleted_at IS NULL")
    keys=("cloudpivot:uv:$uid" "cloudpivot:perm:user_perms:$uid" "cloudpivot:perm:user_roles:$uid" "cloudpivot:perm:user_menus:$uid")
    while IFS= read -r sk; do [ -n "$sk" ] && keys+=("$sk"); done < <(R --scan --pattern "cloudpivot:perm:user_scope:*:$uid" 2>/dev/null)
    for k in "${keys[@]}"; do
        if [ "$DRY" = "1" ]; then
            echo "  [dry-run] DEL $k（exists=$(R EXISTS "$k")）"
        else
            echo "  DEL $k → $(R DEL "$k")"
        fi
    done
done

echo "== 4) 复核 =="
echo "  cloudpivot:perm:user_scope_epoch = $(R GET cloudpivot:perm:user_scope_epoch)"
echo "  残留 scope 键（旧 epoch 的会自然失配）:"
R --scan --pattern 'cloudpivot:perm:user_scope:*' 2>/dev/null | head -10 | sed 's/^/    /'
