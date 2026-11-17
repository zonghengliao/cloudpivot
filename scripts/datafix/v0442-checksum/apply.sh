#!/usr/bin/env bash
# ============================================================================
# V0442 指纹修复（一次性 datafix）：把「已应用过**旧版** V0442 的库」的记录表
# checksum 更新成「修好空库首启的**新版** V0442」的指纹。
#
# ## 为什么需要它（t_08e1816a，P0）
#
# V0442 在空库首启时必然失败（它的前置护栏断言了只有种子迁移才建出的
# 「系统管理」目录行，而内核执行次序是 versioned 先、seed 后）。
# 新迁移救不了（V0443 永远轮不到执行），只能**原地改 V0442**。
# 而框架对已应用迁移是强校验：内容一变即 `DbError::MigrationChecksum` 中止启动
# （vendor/nik-db/src/migrate/history.rs，策略 `Divergent::Abort` 是默认值，
#  2026-11 实测仓库 config 未改该策略）。
#
# ⇒ 凡是在 2026-11-17 之前跑过旧版 V0442 的库，升级到本提交后**必须先跑本脚本**，
#   否则实例起不来（日志：`数据库迁移失败: … migration 442 was modified after it was applied`）。
#
# 受影响面（2026-11-17 实测）：只有 `cloudpivot_dev`
# （`2e6fbee6…`；`cloudpivot_test` / `cloudpivot_test_full` 停在 404，
#  `cloudpivot_base` 无权限读记录表；生产 144 停在 441）。
# 其它库「没应用过 442」⇒ 本脚本判定为 no-op。
#
# ## 安全阀（三层，缺一不可）
#
#   1. **只认已知的旧指纹**：库里的值必须等于 OLD_CHECKSUM（旧版 V0442 的指纹），
#      其它任何值一律中止 —— 说明是另一处未知改写，必须人工判断，不能被本脚本顺手抹平。
#   2. **只改 checksum 一列**（`WHERE kind='versioned' AND version='442'`），
#      不动 `applied_at` / `applied_seq` / `success` / `source`，因此不影响回滚顺序。
#   3. **默认 dry-run**：不带 `--apply` 只打印将要执行的 SQL 与前后值。
#
# ## 用法
#
#   export ERP_DB_URL='postgres://cloudpivot:***@127.0.0.1:5432/cloudpivot_dev'
#   bash scripts/datafix/v0442-checksum/apply.sh              # 预演
#   bash scripts/datafix/v0442-checksum/apply.sh --apply      # 真改（幂等：再跑一次是 no-op）
#
# 退出码：0 = 已修复 / 本就无需修复；1 = 需要人工介入（未知指纹、库不可达、SQL 失败）。
# ============================================================================
set -uo pipefail

# 旧版（HEAD e759628b 之前）V0442 的框架口径指纹
OLD_CHECKSUM=2e6fbee6a98c6ca081ada172f91623b922bad2ea459f431db50cce98b11cdafe
NAME=perm_gate_orphans_and_n20
VERSION=442

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$SCRIPT_DIR/../../.." && pwd)
UP_FILE="$ROOT/backend/app/migrations/business/V0442__perm_gate_orphans_and_n20.up.sql"

APPLY=0
for arg in "$@"; do
  case "$arg" in
    --apply) APPLY=1 ;;
    -h|--help) sed -n '2,45p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "未知参数：$arg（支持 --apply / --help）" >&2; exit 1 ;;
  esac
done

[ -n "${ERP_DB_URL:-}" ] || { echo "!! 需要 ERP_DB_URL（指到要修的库）" >&2; exit 1; }
command -v psql >/dev/null || { echo "!! 找不到 psql" >&2; exit 1; }
[ -f "$UP_FILE" ] || { echo "!! 找不到新版迁移文件：$UP_FILE" >&2; exit 1; }

q() { psql "$ERP_DB_URL" -Atc "$1" 2>&1; }

NEW_CHECKSUM=$(python3 "$SCRIPT_DIR/recompute_checksum.py" "$UP_FILE")
if [ -z "$NEW_CHECKSUM" ] || [ "${#NEW_CHECKSUM}" -ne 64 ]; then
  echo "!! 算不出新版指纹（recompute_checksum.py 异常）：'$NEW_CHECKSUM'" >&2
  exit 1
fi

echo "==> 库        $(q 'select current_database()')"
echo "==> 迁移文件  ${UP_FILE#"$ROOT"/}"
echo "==> 旧指纹    $OLD_CHECKSUM"
echo "==> 新指纹    $NEW_CHECKSUM"

CURRENT=$(q "select coalesce(checksum,'') from schema_migrations where kind='versioned' and version='$VERSION'")
case "$CURRENT" in
  *ERROR*|*FATAL*)
    echo "!! 读记录表失败：$CURRENT" >&2
    exit 1 ;;
esac
if [ -z "$CURRENT" ]; then
  echo "==> 该库没有 V$VERSION 的应用记录（空库首启 / 还没迁到 442）→ 无需修复，退出 0"
  exit 0
fi
echo "==> 库内现值  $CURRENT"

if [ "$CURRENT" = "$NEW_CHECKSUM" ]; then
  echo "==> 已经是新指纹（修过了）→ no-op，退出 0"
  exit 0
fi
if [ "$CURRENT" != "$OLD_CHECKSUM" ]; then
  echo "!! 库内指纹既不是旧版也不是新版 —— 这个库的 V$VERSION 被别的改写动过，" >&2
  echo "   本脚本不替你判断。请人工 diff 迁移文件与库内实际效果后再决定。" >&2
  exit 1
fi

SQL="UPDATE schema_migrations SET checksum = '$NEW_CHECKSUM'
      WHERE kind = 'versioned' AND version = '$VERSION' AND checksum = '$OLD_CHECKSUM'"

if [ "$APPLY" != "1" ]; then
  echo
  echo "==> 预演（未加 --apply，什么都没改）。将执行："
  echo "    $SQL"
  echo "    预期影响行数 1；之后再跑一次本脚本应输出 no-op。"
  exit 0
fi

echo
echo "==> 执行修复"
OUT=$(psql "$ERP_DB_URL" -Atc "$SQL" 2>&1)
echo "    $OUT"
NOW=$(q "select checksum from schema_migrations where kind='versioned' and version='$VERSION'")
if [ "$NOW" = "$NEW_CHECKSUM" ]; then
  echo "==> 已修复：库内现值 = 新指纹（回读确认）"
  echo "    下一步：重启实例，日志里不应再出现 migration $VERSION was modified / MigrationChecksum。"
  exit 0
fi
echo "!! 修复后回读不符（现值 $NOW），请人工检查" >&2
exit 1
