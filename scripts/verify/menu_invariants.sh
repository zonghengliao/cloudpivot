#!/usr/bin/env bash
# =============================================================================
# menu_invariants.sh — 菜单结构不变量（防回归闸门，卡 MENU-02）
#
# 用法：
#   bash scripts/verify/menu_invariants.sh [dbname]
#       dbname 省略时取 $NIK_DATABASE_NAME，再退回 cloudpivot_dev
#   连接参数：PGHOST / PGPORT / PGUSER / PGPASSWORD，未设时回落 NIK_DATABASE_*（psql 惯例）
#
# 退出码：0 = 5 条断言全绿 ｜ 1 = 有违规（明细已打印）｜ 2 = 连库/查询失败（基础设施问题）
#
# 5 条断言：
#   ① 孤儿 parent     parent_id 指向不存在或已软删的记录
#   ② /system/ 归属   path ~ '/system/%' 的页面/按钮（menu_type 2/3）必须挂在 system 目录下
#   ③ /business/ 归属 path ~ '/business/%' 的节点沿 parent 链上溯到的**根**必须落在
#                     /business 命名空间：根 path 为 '/business'（通用底座）或 '/business/<模块>'
#                     （一级业务模块根）。两个显式豁免（默认值见下方变量，置空即关闭）：
#                       MENU_BIZ_EXTRA_ROOTS 根级白名单（默认空，不启用）
#                       MENU_BIZ_EXTRA_IDS   行级例外表（默认 /notice-center 根下那 4 条
#                                            /business/* 页面：公告管理 / 消息中心 / 消息通知 /
#                                            发送通知；卡 t_197ecb8e 评估结论＝不动，理由见 README）
#                     行级例外表的意义：根级白名单一放就是「这个根下任何 /business/* 都放过」，
#                     以后谁把别的 /business 页面误挂到 /notice-center 也不会报；加上 ID 例外后
#                     只有这 4 条被豁免，新错位照样报红。例外行失配（被删/被改名/已合规）会打印
#                     ⚠ 提示要求清理，但不改变退出码。
#   ④ path 唯一性     menu_type=2 的未删记录中同一 path 不得出现多次
#   ⑤ 菜单总数        未删记录总数必须落在 [MENU_TOTAL_MIN, MENU_TOTAL_MAX]（默认 400-420）
#
# 为什么 ③ 不是「必须找到 path='/business' 的祖先」：本仓库 ERP 各模块目录
# （/business/erp/{finance,sales,warehouse,purchase}-mgmt、master-data、contract）本身就是
# 一级根（parent_id IS NULL），并没有把 /business 通用底座当父级；按字面口径既有基线就有
# 63 条"违规"，脚本会永远红。因此 ③ 判的是「/business 子树不能挂到非 /business 命名空间的
# 根上」（例如误挂到 /system、/project 下面），这才是 MENU-01 那类错位的真正回归面。
#
# 手工验证脚本本身有效（注入 → 报警 → 复原，测试库可跑）：
#   psql ... -c "update sys_menu set parent_id=null where path='/system/user';"
#   bash scripts/verify/menu_invariants.sh <db>; echo rc=$?   # 期望 rc=1、② 报 1
#   psql ... -c "update sys_menu set parent_id='10000000-0000-0000-0000-000000000001'
#                where path='/system/user';"
#   bash scripts/verify/menu_invariants.sh <db>; echo rc=$?   # 期望 rc=0
# =============================================================================
set -uo pipefail

DB="${1:-${NIK_DATABASE_NAME:-cloudpivot_dev}}"
PGHOST="${PGHOST:-${NIK_DATABASE_HOST:-127.0.0.1}}"
PGPORT="${PGPORT:-${NIK_DATABASE_PORT:-5432}}"
PGUSER="${PGUSER:-${NIK_DATABASE_USER:-cloudpivot}}"
PGPASSWORD="${PGPASSWORD:-${NIK_DATABASE_PASSWORD:-}}"
export PGPASSWORD PGHOST PGPORT PGUSER

SYSTEM_DIR_ID="${MENU_SYSTEM_DIR_ID:-10000000-0000-0000-0000-000000000001}"
TOTAL_MIN="${MENU_TOTAL_MIN:-400}"
TOTAL_MAX="${MENU_TOTAL_MAX:-420}"
# ③ 豁免：根级白名单（这些根允许承载 path ~ '/business/%' 的子节点，逗号分隔）。默认空：
#   根级白名单一放就是「这个根下任何 /business/* 都放过」，会掩盖未来新错位，所以默认不启用；
#   公告中心那 4 条走下面的行级 ID 例外，只豁免已知行，新错位照样报红。
# ⚠ 用 ${VAR-default} 而不是 ${VAR:-default}：空串要能真正关掉豁免（MENU_BIZ_EXTRA_ROOTS= ），
#   否则「置空即复现基线」这个判据根本不成立（旧版就是 :-，置空会回落默认值）。
BIZ_EXTRA_ROOTS="${MENU_BIZ_EXTRA_ROOTS-}"
# ③ 豁免：行级例外表（只有这些菜单 id 允许上溯到非 /business 根；逗号分隔，置空即关闭）
#   卡 t_197ecb8e 评估结论＝不动：/notice-center 是产品级一级根（V0133，老大 2026-10-30 明确要求
#   「公告与消息」目录），4 条子菜单的 /business/* path 是 V0085/V0087/V0065 的历史既有值，
#   改名要同步动前端路由/硬编码跳转/文档，零功能收益。理由与复核证据见 scripts/verify/README.md。
BIZ_EXTRA_IDS="${MENU_BIZ_EXTRA_IDS-20000000-0000-0000-0000-000000000710,20000000-0000-0000-0000-000000000410,23000000-0000-0000-0000-000000000010,23000000-0000-0000-0000-000000000020}"

PSQL=(psql -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" -d "$DB" -tA -q -v ON_ERROR_STOP=1)

# 递归上溯 CTE：每个未删节点 → 其祖先链终点（根）的路径。深度上限 20 防环。
UP_CTE="
with recursive up as (
    select m.id, m.path, m.parent_id,
           m.id as cur_id, m.path as cur_path, m.parent_id as cur_parent, 0 as depth
      from sys_menu m
     where m.deleted_at is null
    union all
    select u.id, u.path, u.parent_id,
           p.id, p.path, p.parent_id, u.depth + 1
      from up u
      join sys_menu p on p.id = u.cur_parent and p.deleted_at is null
     where u.cur_parent is not null and u.depth < 20
), rooted as (
    select id, path, max(depth) as depth,
           (array_agg(cur_path order by depth desc))[1] as root_path
      from up
     where cur_parent is null
     group by id, path
)"

FAILED=0
FAIL_NAMES=()

# 逗号分隔 → SQL in-list 文本（a,b → a','b）；空串返回空
sqllist() { printf '%s' "$1" | tr -d ' ' | sed "s/,/','/g"; }
ROOTS_SQL=$(sqllist "$BIZ_EXTRA_ROOTS")
IDS_SQL=$(sqllist "$BIZ_EXTRA_IDS")
ROOT_NOT_IN=''
[ -n "$ROOTS_SQL" ] && ROOT_NOT_IN="and coalesce(r.root_path,'') not in ('$ROOTS_SQL')"
ID_NOT_IN=''
[ -n "$IDS_SQL" ] && ID_NOT_IN="and r.id::text not in ('$IDS_SQL')"

# sql <query> —— 返回单值/多行文本；查询失败直接退出 2
sql() {
  local out
  if ! out=$("${PSQL[@]}" -c "$1" 2>&1); then
    printf '⛔ 查询失败（库 %s）：\n%s\n' "$DB" "$out" >&2
    exit 2
  fi
  printf '%s' "$out"
}

# 输出一行断言结果：line <标签> <数值> <通过?> [额外说明]
line() {
  local label=$1 value=$2 ok=$3 extra=${4:-}
  local mark='❌'
  [ "$ok" = 1 ] && mark='✅'
  printf '%s%4s  %s%s\n' "$label" "$value" "$mark" "$extra"
}

detail() {  # detail <标题> <query>
  local out
  out=$(sql "$2")
  printf '    ↳ %s（前 10 条）：\n' "$1"
  [ -n "$out" ] && printf '%s\n' "$out" | sed 's/^/       /'
  return 0
}

# ── 连库自检 ────────────────────────────────────────────────────────────────
if [ "$(sql 'select 1')" != "1" ]; then
  printf '⛔ 连不上库 %s（%s@%s:%s）\n' "$DB" "$PGUSER" "$PGHOST" "$PGPORT" >&2
  exit 2
fi

printf '═══ 菜单结构不变量 ═══\n'
printf '库: %s\n' "$DB"

# ── ① 孤儿 parent ───────────────────────────────────────────────────────────
Q1="select count(*) from sys_menu c
     where c.deleted_at is null and c.parent_id is not null
       and not exists (select 1 from sys_menu p
                        where p.id = c.parent_id and p.deleted_at is null)"
N1=$(sql "$Q1"); N1=${N1:-x}
if [ "$N1" = "0" ]; then line '① 孤儿 parent:        ' "$N1" 1; else
  line '① 孤儿 parent:        ' "$N1" 0; FAILED=$((FAILED+1)); FAIL_NAMES+=('①孤儿parent')
  detail 'parent 不存在的子节点' "select c.id, c.name, c.path, c.parent_id
     from sys_menu c
    where c.deleted_at is null and c.parent_id is not null
      and not exists (select 1 from sys_menu p
                       where p.id = c.parent_id and p.deleted_at is null)
    order by c.path limit 10"
fi

# ── ② /system/ 归属 ─────────────────────────────────────────────────────────
Q2="select count(*) from sys_menu
     where deleted_at is null and path like '/system/%' and menu_type in (2,3)
       and parent_id is distinct from '$SYSTEM_DIR_ID'"
N2=$(sql "$Q2"); N2=${N2:-x}
if [ "$N2" = "0" ]; then line '② /system/ 归属:      ' "$N2" 1; else
  line '② /system/ 归属:      ' "$N2" 0; FAILED=$((FAILED+1)); FAIL_NAMES+=('②/system归属')
  detail "/system/ 节点未挂在 system 目录（$SYSTEM_DIR_ID）下" \
    "select id, name, path, menu_type, coalesce(parent_id::text,'NULL') as parent_id
       from sys_menu
      where deleted_at is null and path like '/system/%' and menu_type in (2,3)
        and parent_id is distinct from '$SYSTEM_DIR_ID'
      order by path limit 10"
fi

# ── ③ /business/ 归属（递归上溯到根）─────────────────────────────────────────
Q3="$UP_CTE
select count(*) from rooted r
 where r.path like '/business/%'
   and coalesce(r.root_path,'') <> '/business'
   and coalesce(r.root_path,'') not like '/business/%'
   $ROOT_NOT_IN
   $ID_NOT_IN"
N3=$(sql "$Q3"); N3=${N3:-x}
if [ "$N3" = "0" ]; then line '③ /business/ 归属:    ' "$N3" 1; else
  line '③ /business/ 归属:    ' "$N3" 0; FAILED=$((FAILED+1)); FAIL_NAMES+=('③/business归属')
  detail "/business/ 节点上溯到的根不在 /business 命名空间" \
    "$UP_CTE
     select r.id, m.name, r.path, coalesce(r.root_path,'<NULL:无根>') as root_path, r.depth
       from rooted r join sys_menu m on m.id = r.id
      where r.path like '/business/%'
        and coalesce(r.root_path,'') <> '/business'
        and coalesce(r.root_path,'') not like '/business/%'
        $ROOT_NOT_IN
        $ID_NOT_IN
      order by r.root_path, r.path limit 10"
fi

# ③ 附注：行级例外表体检（失配＝该 id 已软删/改名出 /business 或已合规 ⇒ 例外该删了）。
# 只提示不改退出码：例外表过期不等于结构坏了，但留着会掩盖未来的新错位。
if [ -n "$IDS_SQL" ]; then
  EXC_CHECK=$(sql "$UP_CTE
    select count(*) filter (where r.id is null) as missing,
           count(*) filter (where r.id is not null and (coalesce(r.root_path,'') = '/business'
                                                       or coalesce(r.root_path,'') <> '' and coalesce(r.root_path,'') like '/business/%')) as passed,
           count(*) as total
      from (select unnest(array['$IDS_SQL'])::uuid as id) e
      left join rooted r on r.id = e.id")
  EXC_MISS=${EXC_CHECK%%|*}; EXC_REST=${EXC_CHECK#*|}
  printf '    ↳ 例外表：%s 条 path 豁免（/notice-center 根），失配 %s（缺失/已合规）\n' \
    "${EXC_REST#*|}" "$(( ${EXC_MISS:-0} + ${EXC_REST%%|*} ))"
  [ "${EXC_MISS:-0}" != "0" ] && printf '       ⚠ MENU_BIZ_EXTRA_IDS 里有 id 不在未删菜单里（缺失 %s 条）——收敛/改名后请同步删例外\n' "$EXC_MISS"
fi

# ── ④ path 唯一性 ───────────────────────────────────────────────────────────
Q4="select count(*) from (
       select path from sys_menu
        where deleted_at is null and path is not null and menu_type = 2
        group by path having count(*) > 1) t"
N4=$(sql "$Q4"); N4=${N4:-x}
if [ "$N4" = "0" ]; then line '④ path 唯一性:        ' "$N4" 1; else
  line '④ path 唯一性:        ' "$N4" 0; FAILED=$((FAILED+1)); FAIL_NAMES+=('④path唯一性')
  detail '重复的 menu_type=2 path' \
    "select path, count(*) as n from sys_menu
      where deleted_at is null and path is not null and menu_type = 2
      group by path having count(*) > 1 order by n desc, path limit 10"
fi

# ── ⑤ 菜单总数 ──────────────────────────────────────────────────────────────
N5=$(sql "select count(*) from sys_menu where deleted_at is null"); N5=${N5:-x}
if [ "$N5" != "x" ] && [ "$N5" -ge "$TOTAL_MIN" ] 2>/dev/null && [ "$N5" -le "$TOTAL_MAX" ] 2>/dev/null; then
  line '⑤ 菜单总数:           ' "$N5" 1 " ($TOTAL_MIN-$TOTAL_MAX)"
else
  line '⑤ 菜单总数:           ' "$N5" 0 " (期望 $TOTAL_MIN-$TOTAL_MAX)"
  FAILED=$((FAILED+1)); FAIL_NAMES+=('⑤菜单总数')
fi

# ── 结论 ────────────────────────────────────────────────────────────────────
if [ "$FAILED" -eq 0 ]; then
  printf '结果: 全绿 ✅\n'
  exit 0
fi
printf '结果: %d 条不变量不通过 ❌（%s）\n' "$FAILED" "$(IFS=,; echo "${FAIL_NAMES[*]}")"
exit 1
