#!/usr/bin/env bash
# 生产缓存失效（正确键名版）· P0-FIX-2B-C
# ----------------------------------------------------------------------------
# 踩坑（本卡实测）：应用侧缓存键带实例前缀 `cloudpivot:`，但 epoch 键在 A 卡里是用裸名
# `perm:user_scope_epoch` INCR 的 —— 应用读的是 `cloudpivot:perm:user_scope_epoch`（不存在
# → 按 0 处理），所以**裸名那个 INCR 对应用是空转**（证据：存量 scope 键全是
# `cloudpivot:perm:user_scope:0:<uid>`）。
# 本脚本两个键都推进（裸名保留历史兼容），并删掉 3 个账号的会话/权限/菜单缓存。
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
export PGPASSWORD="${NIK_DATABASE_PASSWORD}"
R() { redis-cli -n 0 "$@"; }
q() { psql -h 127.0.0.1 -U cloudpivot -d cloudpivot -X -q -At -c "$1"; }

echo "== 1) epoch（应用读的是带前缀的键）=="
echo "  cloudpivot:perm:user_scope_epoch before = $(R GET cloudpivot:perm:user_scope_epoch)"
R INCR cloudpivot:perm:user_scope_epoch
echo "  cloudpivot:perm:user_scope_epoch after  = $(R GET cloudpivot:perm:user_scope_epoch)"
echo "  perm:user_scope_epoch（裸名，历史兼容）before = $(R GET perm:user_scope_epoch)"
R INCR perm:user_scope_epoch
echo "  perm:user_scope_epoch（裸名）after  = $(R GET perm:user_scope_epoch)"

echo "== 2) 逐账号删 会话/权限/角色/菜单 缓存 =="
for u in laoliu laoliu_test henry.liao; do
    uid=$(q "SELECT id FROM sys_user WHERE username='$u' AND deleted_at IS NULL")
    for k in "cloudpivot:uv:$uid" "cloudpivot:perm:user_perms:$uid" "cloudpivot:perm:user_roles:$uid" "cloudpivot:perm:user_menus:$uid"; do
        echo "  DEL $k → $(R DEL "$k")"
    done
done

echo "== 3) 复核：残留的 scope 键（旧 epoch 的会自动失配）=="
R --scan --pattern 'cloudpivot:perm:user_scope:*' | head -5
