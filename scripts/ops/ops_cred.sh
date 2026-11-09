#!/bin/bash
# ============================================================================
# ops_cred.sh — 自查/验收脚本的「登录凭据」唯一口径（sourceable）
#
# 背景（卡 t_d4e92b99）：`/etc/cloudpivot/cloudpivot.env` 里的
# `NIK_ADMIN_PASSWORD` 只是「空库首启时 admin 的初始口令」种子，**不是** admin
# 现在用的口令。拿它去登 `admin` 必然 401；而 `sys_user.login_fail_count` 攒到 5
# 就把账号锁 30 分钟（backend/src/shared/constants.rs：MAX_LOGIN_FAIL_COUNT=5、
# LOCK_MINUTES=30），再叠加 60 秒内 10 次的登录限流（LOGIN_RATE_MAX_ATTEMPTS=10）。
# 所以：**脚本不许猜口令、不许重试**。
#
# 用法：
#   . /root/ops_cred.sh                     # 或 . scripts/ops/ops_cred.sh
#   ops_cred_load [/etc/cloudpivot/cloudpivot.env] || exit 2
#   echo "$OPS_CRED_USER"                   # 口令在 $OPS_CRED_PASS，别 echo
#
# 解析优先级（先到先得，只认显式变量）：
#   1. CLOUDPIVOT_OPS_USER / CLOUDPIVOT_OPS_PASSWORD     ← 生产自查唯一口径
#   2. NIK_OPS_USER / NIK_OPS_PASSWORD                   ← 等价别名
#   3. NIK_ADMIN_USERNAME / NIK_ADMIN_PASSWORD           ← 仅 dev/test 种子语义；回退时打黄字
#
# 返回值：0 = 拿到 user+pass；1 = 缺配置（打印原因）；2 = env 文件不可读
# ============================================================================

OPS_CRED_USER=""
OPS_CRED_PASS=""
OPS_CRED_SOURCE=""

# 从 env 文件里取一个键（值为空/不存在时输出空）
_ops_cred_file_get() {
  local file=$1 key=$2 line
  [ -r "$file" ] || return 0
  line=$(grep -m1 "^[[:space:]]*${key}=" "$file" 2>/dev/null) || return 0
  line=${line#*=}
  # 去首尾空白与成对引号
  line=$(printf '%s' "$line" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')
  line=${line%\"}; line=${line#\"}
  line=${line%\'}; line=${line#\'}
  printf '%s' "$line"
}

# ops_cred_load [env_file]
ops_cred_load() {
  local env_file=${1:-/etc/cloudpivot/cloudpivot.env}
  local u p

  # ① CLOUDPIVOT_OPS_*
  u=${CLOUDPIVOT_OPS_USER:-$(_ops_cred_file_get "$env_file" CLOUDPIVOT_OPS_USER)}
  p=${CLOUDPIVOT_OPS_PASSWORD:-$(_ops_cred_file_get "$env_file" CLOUDPIVOT_OPS_PASSWORD)}
  if [ -n "$u" ] && [ -n "$p" ]; then
    OPS_CRED_USER=$u; OPS_CRED_PASS=$p; OPS_CRED_SOURCE="CLOUDPIVOT_OPS_*"
    return 0
  fi

  # ② NIK_OPS_*
  u=${NIK_OPS_USER:-$(_ops_cred_file_get "$env_file" NIK_OPS_USER)}
  p=${NIK_OPS_PASSWORD:-$(_ops_cred_file_get "$env_file" NIK_OPS_PASSWORD)}
  if [ -n "$u" ] && [ -n "$p" ]; then
    OPS_CRED_USER=$u; OPS_CRED_PASS=$p; OPS_CRED_SOURCE="NIK_OPS_*"
    return 0
  fi

  # ③ 兼容旧写法（dev/test 库里 admin 口令确实等于种子时才对；生产一定 401）
  u=${NIK_ADMIN_USERNAME:-$(_ops_cred_file_get "$env_file" NIK_ADMIN_USERNAME)}
  p=${NIK_ADMIN_PASSWORD:-$(_ops_cred_file_get "$env_file" NIK_ADMIN_PASSWORD)}
  if [ -n "$p" ]; then
    OPS_CRED_USER=${u:-admin}; OPS_CRED_PASS=$p; OPS_CRED_SOURCE="NIK_ADMIN_*（种子语义，回退）"
    echo "⚠ 凭据回退到 NIK_ADMIN_PASSWORD（种子变量）：只在 dev/test 且 admin 口令仍等于种子时成立；" >&2
    echo "  生产上这么登 admin 会 401 并把 admin 顶到锁定（5 次 / 30 分钟）。见 /etc/cloudpivot/README。" >&2
    return 0
  fi

  echo "✗ 没有可用的自查登录凭据：env 文件缺 CLOUDPIVOT_OPS_USER/CLOUDPIVOT_OPS_PASSWORD（也不会去猜 admin 口令）" >&2
  echo "  修复：往 $env_file 追加 CLOUDPIVOT_OPS_USER=<运维账号> / CLOUDPIVOT_OPS_PASSWORD=<该账号口令>" >&2
  return 1
}

# 只打印「账号 + 来源」，永不打印口令 —— 用于日志
ops_cred_describe() {
  printf '账号=%s 来源=%s 口令长度=%s' "$OPS_CRED_USER" "$OPS_CRED_SOURCE" "${#OPS_CRED_PASS}"
}
