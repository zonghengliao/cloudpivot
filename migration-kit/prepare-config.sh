#!/usr/bin/env bash
# =============================================================================
# prepare-config.sh —— 环境准备：把「未入库的本地配置」从模板生成出来
#
# 为什么迁移准备包里必须有这一步（真实踩到过）：
#   `backend/config/config-dev.toml` 与 `config-test.toml` 是 **.gitignore 掉的**
#   （密钥不能进 git），由每台机器从 `*.example.toml` 拷出来改。所以干净 checkout
#   里没有这两个文件 —— 后端用 `profile=dev/test` 跑不起来，而且
#   `shared/startup_guard.rs` 的集成测试 `merge_semantics_or_base_and_profile`
#   是**读真实配置文件**做断言的，缺文件会直接 FAILED（干净基线上实测 44 passed / 1 failed）。
#
#   → 这不是准备包的 bug，是「未入库配置」这个设计带来的必然步骤。
#     本脚本把它自动化，并保证幂等（已存在就不覆盖，避免冲掉本机密钥路径设置）。
#
# 用法：
#   bash prepare-config.sh [--root <repo>] [--force] [--dry-run]
#   --force  用模板覆盖已有文件（会丢本机改动，慎用）
#
# 退出码：0 成功；2 环境错误
# =============================================================================
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT=""
FORCE=0
DRY=0
while [ $# -gt 0 ]; do
  case "$1" in
    --root) ROOT="${2:?}"; shift 2 ;;
    --root=*) ROOT="${1#*=}"; shift ;;
    --force) FORCE=1; shift ;;
    --dry-run) DRY=1; shift ;;
    -h|--help) sed -n '2,22p' "$0"; exit 0 ;;
    *) echo "未知参数: $1" >&2; exit 2 ;;
  esac
done
[ -z "$ROOT" ] && ROOT="$(cd "$HERE/.." && pwd)"
CFG="$ROOT/backend/config"
[ -d "$CFG" ] || { echo "找不到 $CFG" >&2; exit 2; }

G=$'\033[32m'; Y=$'\033[33m'; R=$'\033[31m'; Z=$'\033[0m'
rc=0
echo "== 从 *.example.toml 生成未入库的本地配置（$CFG）=="
for ex in "$CFG"/config-*.example.toml; do
  [ -f "$ex" ] || continue
  dst="${ex%.example.toml}.toml"
  base="$(basename "$dst")"
  if [ -f "$dst" ] && [ "$FORCE" -eq 0 ]; then
    printf "  ${G}✓${Z} %s 已存在，保留（--force 可覆盖）\n" "$base"; continue
  fi
  if [ "$DRY" -eq 1 ]; then
    printf "  ${G}✓${Z} %s 将生成（dry-run）\n" "$base"; continue
  fi
  cp "$ex" "$dst" && printf "  ${G}✓${Z} %s 已从模板生成\n" "$base" || { printf "  ${R}✘${Z} %s 生成失败\n" "$base"; rc=2; }
done

echo "== 关键字段自检 =="
chk() { # file key expect
  local f="$1" k="$2" want="$3"
  [ -f "$f" ] || { printf "  ${Y}!${Z} %s 不存在（跳过 %s 检查）\n" "$(basename "$f")" "$k"; return; }
  if grep -qE "^[[:space:]]*${k}[[:space:]]*=[[:space:]]*${want}" "$f"; then
    printf "  ${G}✓${Z} %s: %s = %s\n" "$(basename "$f")" "$k" "$want"
  else
    printf "  ${R}✘${Z} %s: 期望 %s = %s，实际: %s\n" "$(basename "$f")" "$k" "$want" \
      "$(grep -E "^[[:space:]]*${k}" "$f" | head -1)"
    rc=2
  fi
}
# startup_guard 的集成测试断言：dev/prod/test 三个 profile 的认证都是开启的
chk "$CFG/config-dev.toml"  enabled true
chk "$CFG/config-prod.toml" enabled true
chk "$CFG/config-test.toml" enabled true
# 密钥一律不落盘
for f in "$CFG"/config*.toml; do
  case "$f" in *.example.toml) continue ;; esac
  if grep -qE "^[[:space:]]*jwt_secret[[:space:]]*=[[:space:]]*\"[^\"]+\"" "$f" 2>/dev/null; then
    printf "  ${R}✘${Z} %s 里有明文 jwt_secret —— 必须改成环境变量注入\n" "$(basename "$f")"; rc=2
  fi
done
[ "$rc" -eq 0 ] && echo "  ${G}配置自检通过${Z}"
echo "提示：真实运行还需要环境变量（见 /etc/cloudpivot/*.env，600）："
echo "      ALUN_MIDDLEWARE_AUTH_JWT_SECRET / ALUN_DATABASE_PASSWORD"
exit "$rc"
