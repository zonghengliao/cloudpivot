#!/usr/bin/env bash
# =============================================================================
# menu_structure_dump.sh — 菜单结构归一化导出（卡 MENU-03，给跨环境结构比对用）
#
# 为什么只导 path / parent_path / menu_type：
#   不同环境的 name / title / sort_order / icon 可能因为业务调整或历史手工改动而不同，
#   但它们**不影响导航树骨架**；真正会造成「菜单乱了」的是层级（parent）与类型。
#   所以结构基线的口径 = 每个未删节点一行 `path|parent_path|menu_type`，按 path 排序。
#
# 三种特殊标注（都不能省，否则结构缺陷会被静默抹平）：
#   -ROOT-      节点自身是顶级（parent_id IS NULL）
#   -MISSING-   父节点不存在或已软删（孤儿，属于结构缺陷，必须能被 diff 看见）
#   ~NULLPATH~  父节点存在但自身 path IS NULL（例如 prod 里 /receipt-refund 这类
#               只种了页面行、漏了 path 的菜单，它的子按钮父路径为空）
#   节点自身 path 为空时导出为 ~NULL~（同样必须可见 —— 不会因为 coalesce 被吞掉）
#
# 用法：
#   bash scripts/verify/menu_structure_dump.sh <db> [输出文件]
#     连接参数：PGHOST / PGPORT / PGUSER / PGPASSWORD；未设时从环境变量文件里取
#     ENV_FILE 可显式指定（默认依次尝试 /etc/cloudpivot/cloudpivot.env、
#     cloudpivot-dev.env、cloudpivot-test.env）
#   输出文件省略时打印到 stdout。
#
# 退出码：0 = 导出成功 ｜ 2 = 连库/参数失败
#
# 典型用法（跨机器比对，生产库只在生产机上本机连）：
#   # 生产机（144）本机导出（用本仓脚本，避免各机脚本版本不一致）
#   bash /root/menu_structure_dump.sh cloudpivot > /tmp/prod_menu_struct.txt
#   # 任意机器上比对（新环境库与已提交的生产基线）
#   diff -u scripts/verify/baseline/prod_menu_structure.txt \
#           <(bash scripts/verify/menu_structure_dump.sh <新库>)
# =============================================================================
set -uo pipefail

DB="${1:?用法: menu_structure_dump.sh <db> [输出文件]}"
OUT="${2:-}"

# ── 连接参数：显式环境变量优先，其次环境变量文件 ────────────────────────────────
if [ -z "${PGPASSWORD:-}" ]; then
  for f in "${ENV_FILE:-}" /etc/cloudpivot/cloudpivot.env \
           /etc/cloudpivot/cloudpivot-dev.env /etc/cloudpivot/cloudpivot-test.env; do
    [ -n "$f" ] && [ -f "$f" ] && { set -a; . "$f"; set +a; break; }
  done
fi
PGHOST="${PGHOST:-${NIK_DATABASE_HOST:-127.0.0.1}}"
PGPORT="${PGPORT:-${NIK_DATABASE_PORT:-5432}}"
PGUSER="${PGUSER:-${NIK_DATABASE_USER:-cloudpivot}}"
PGPASSWORD="${PGPASSWORD:-${NIK_DATABASE_PASSWORD:-}}"
export PGPASSWORD PGHOST PGPORT PGUSER

# 每行：path | 直接父节点的 path（-ROOT- / -MISSING- / ~NULLPATH- / 真实路径） | menu_type
# left join 而非 join：父已软删的孤儿节点也要导出来（比对时能被看见，而不是静默消失）
SQL="
select coalesce(m.path, '~NULL~') || '|' ||
       case when m.parent_id is null then '-ROOT-'
            when p.id is null then '-MISSING-'
            else coalesce(p.path, '~NULLPATH~') end || '|' ||
       m.menu_type
  from sys_menu m
  left join sys_menu p on p.id = m.parent_id and p.deleted_at is null
 where m.deleted_at is null
 order by 1"

if ! RES=$(psql -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" -d "$DB" -At -q \
             -v ON_ERROR_STOP=1 -c "$SQL" 2>&1); then
  printf '⛔ 结构导出失败（库 %s）：\n%s\n' "$DB" "$RES" >&2
  exit 2
fi

if [ -n "$OUT" ]; then
  printf '%s\n' "$RES" > "$OUT"
  printf '结构已导出：%s（%s 行，库 %s）\n' "$OUT" "$(printf '%s\n' "$RES" | wc -l)" "$DB"
else
  printf '%s\n' "$RES"
fi
