#!/usr/bin/env bash
# =============================================================================
# menu_invariants.sh — 菜单结构不变量（防回归闸门，卡 MENU-02）
#
# 用法：
#   bash scripts/verify/menu_invariants.sh [dbname]
#       dbname 省略时取 $NIK_DATABASE_NAME，再退回 cloudpivot_dev
#   连接参数：PGHOST / PGPORT / PGUSER / PGPASSWORD，未设时回落 NIK_DATABASE_*（psql 惯例）
#
# 退出码：0 = 8 条断言全绿 ｜ 1 = 有违规（明细已打印）｜ 2 = 连库/查询失败（基础设施问题）
#
# 8 条断言：
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
#   ⑤ 菜单总数        未删记录总数必须落在 [MENU_TOTAL_MIN, MENU_TOTAL_MAX]（默认 400-520）
#                     上限调整史：420 → 460（2026-11-08，卡 t_9d2f365c 部署同步时实测 423 报警）
#                     → 520（2026-11-11，成本核算 26 条 + 人力资源 36 条两个模块同批落地，
#                     实测 459 → 495）。
#                     原因：P2-01 询比价 / P2-02 采购申请 / P2-03 采购退货三个闭环各带「1 个页面 +
#                     3~4 个按钮」菜单，410 → 423（13 条全部 auth_code 唯一、parent 正确，②③④ 全绿）。
#                     本断言只防「批量误插 / 大面积误删」这类事故，不是功能增长闸门，故每次只按实测
#                     基数留 ~8% 余量；再破线时先确认是真实功能增长，再同步改这里和 scripts/verify/README.md。
#
#   ⑥ title 长度      menu_type 1/2（目录/页面，也就是侧栏里真正显示的那两层）的 title 长度
#                     必须 ≤ MENU_TITLE_MAX（默认 12）。title 是**侧栏显示名**，写成功能说明
#                     （「即时 / 月末汇率维护 + 按月批量录入（ARCH-7 §15.6.3 ②）」这类 38 字长句）
#                     就会换行/截断。规范见 docs/编码规范.md §1.6.1；口径与边界：
#                       · 只管 1/2：menu_type=3 是按钮权限点，不进侧栏（它的 title 长句会污染
#                         「角色-菜单」授权树，属另一件事，实测 155 条 >12 / 最长 73 字）；
#                       · 12 是**硬顶**不是目标值：规范要求 2-6 字名词短语，留出余量给少量
#                         正常偏长的业务名（实测基线最长 10 字）。
#   ⑦ 同父 sort 唯一  同一 parent_id 下 menu_type=2 的 sort_order 不得重复（重复 → 侧栏顺序
#                     依赖返回行序，刷新一次可能变一次）。**只管 menu_type=2**（照规范校验 SQL
#                     的字面口径）：跨类型同 sort（目录 vs 页面）不在本断言内，实测顶级有 1 组
#                     （通用底座 menu_type=1 / 合同管理 menu_type=2 同为 75），已登记待评估，
#                     不在这里判红 —— 判红会逼着为一条排序噪声改历史数据。
#
#   ⑧ icon 名合法     menu_type 1/2（侧栏真正渲染图标的两层）且 icon 非空的记录，icon 必须是
#                     TDesign 图标库里**真实存在**的组件名。icon 存的是图标组件名，
#                     名字不存在时 SideMenuItem.vue 的 <t-icon :name="item.icon" /> 渲染**空节点**，
#                     侧栏那一项就是空白；而 icon 非空串时组件里的 folder/app 兜底也不会触发
#                     （兜底只在 icon 为空时生效）。
#                     已经复发两次：V0170 把 purchase-order 的 shopping-cart 改成 cart，
#                     但生产(144)那条菜单的 path 是 /business/erp/purchase，没被覆盖 →
#                     老大在生产上看到「采购订单没有图标」，V0353 才补上（卡 MENU-FIX-3）。
#                     -- 图标库从哪来（按序，先到先用）：
#                        1) $MENU_ICON_LIB 显式指定：目录（tdesign-icons 的 esm/components）
#                           或文本文件（一行一个名字；也认 manifest.js 的 `stem: "x"` 格式）
#                        2) <repo>/frontend/node_modules/tdesign-icons-vue-next/esm/components
#                        3) <repo>/frontend/src/components/IconSelect/icons.ts（仓库内清单，
#                           由已安装版本枚举生成；两层目录都给不了才用它，避免清单落后于依赖）
#                     -- 找不到图标库时**跳过并明确打印原因**（⏭），既不静默绿也不报红：prod 机、
#                        CI、瘦容器上不一定装过前端依赖。要「必须校验」的环境设
#                        MENU_ICON_LIB_REQUIRED=1 → 缺库按基础设施问题 rc=2（不许假绿）。
#                     -- 现状基线（2026-11-09 实测，60 个 distinct icon 全存在）：145 dev /
#                        新库全绿；144 prod 当时只有 shopping-cart 不存在（V0353 已修）。
#                        断言落地后应当是绿的，红就是真出现了新坏名。
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
#
# ⑥⑦ 的牙（同样**只在测试库**做，做完立刻复原；生产只读，别注入）：
#   psql ... -c "update sys_menu set title='即时 / 月末汇率维护 + 按月批量录入（ARCH-7 §15.6.3 ②）'
#                where path='/business/erp/exchange-rate';"
#   bash scripts/verify/menu_invariants.sh <db>; echo rc=$?   # 期望 rc=1、⑥ 报 1（打印 38 字 title）
#   psql ... -c "insert into sys_menu (id,name,title,menu_type,path,parent_id,sort_order)
#                select gen_random_uuid(),'重复sort','重复sort',2,'/tmp/dup',
#                       parent_id, sort_order from sys_menu where path='/business/erp/exchange-rate';"
#   bash scripts/verify/menu_invariants.sh <db>; echo rc=$?   # 期望 rc=1、⑦ 报 1 组
#   复原：删掉那条 /tmp/dup、把 title 改回去（或干脆在副本库/临时库上做，见
#         docs/evidence/20261108-menu-fix2/raw/12_invariants_teeth_build.sql 的造数写法）
#
# ⑧ 的牙（同样只在测试库做，做完立刻复原）：
#   psql ... -c "update sys_menu set icon='shopping-cart' where path='/business/erp/purchase-order';"
#   bash scripts/verify/menu_invariants.sh <db>; echo rc=$?   # 期望 rc=1、⑧ 报 1（打印坏名 + 所在 path）
#   psql ... -c "update sys_menu set icon='cart' where path='/business/erp/purchase-order';"
#   bash scripts/verify/menu_invariants.sh <db>; echo rc=$?   # 期望 rc=0
#   # 缺图标库要跳过而不是假绿：mv frontend/node_modules /tmp/nm.bak → 期望 ⑧ 打印 ⏭ 跳过（rc=0），
#   # 再 mv 回来；MENU_ICON_LIB_REQUIRED=1 时同样场景期望 rc=2。
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
TOTAL_MAX="${MENU_TOTAL_MAX:-520}"
# ⑥ title 长度硬顶（menu_type 1/2）。规范目标值是 2-6 字（docs/编码规范.md §1.6.1），
#   12 是硬顶：留出余量给少量正常偏长的业务名，同时长句说明一定超。置大即放宽（不建议）。
TITLE_MAX="${MENU_TITLE_MAX:-12}"
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

# ⑧ 图标库位置（置空＝自动探测：先 frontend/node_modules 的 tdesign-icons，再仓库内
#   src/components/IconSelect/icons.ts）。目录 / 文本清单一律支持；也可指到 manifest.js。
MENU_ICON_LIB="${MENU_ICON_LIB-}"
# ⑧ 缺图标库时是否算基础设施故障：默认 0＝跳过并打印原因（prod/CI/瘦容器没装前端依赖）；
#   1＝rc=2（「必须校验」的场合用，杜绝假绿）。
MENU_ICON_LIB_REQUIRED="${MENU_ICON_LIB_REQUIRED:-0}"
# 仓库根：从脚本自身位置推（scripts/verify/ → ../../）。脚本被拷到仓库外时用 MENU_REPO_ROOT 覆盖。
MENU_REPO_ROOT="${MENU_REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"

# ⚠ 必须钉死 locale（2026-11-08 假告警的根因，别再删）：
#   本机 psql 是 Debian 的 pg_wrapper（`/usr/bin/psql -> ../share/postgresql-common/pg_wrapper`，Perl 实现）。
#   当调用方环境带着目标机没装的 locale（cron / ssh `SendEnv LC_*` 会把本机 LANG=zh_CN.UTF-8 带进远端），
#   Perl 会往 **stderr** 打十几行 "perl: warning: Setting locale failed…" 而 psql 本身**仍以 0 退出**。
#   旧版 sql() 用 2>&1 把 stderr 并进返回值：select 1 的 "1" 变成「警告串+1」，连库自检判 != "1"
#   ⇒ 打印「⛔ 连不上库」exit 2，145 测试与 144 生产同时假告警（deploy_check_v5 00:00 那条卡）。
#   LC_ALL=C 让 Perl/psql 都不抱怨；PGCLIENTENCODING=UTF8 保住中文菜单名不被 SQL_ASCII 走样。
export LC_ALL=C
export LANG=C
unset LANGUAGE 2>/dev/null || true
export PERL_BADLANG=0
export PGCLIENTENCODING=UTF8

# -X：忽略 ~/.psqlrc（谁在里面写 `\timing on` 都会污染返回值，与 locale 同一类事故）
PSQL=(psql -X -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" -d "$DB" -tA -q -v ON_ERROR_STOP=1)

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

# sql <query> —— 返回单值/多行文本（**只有 stdout**）；查询失败 → 全局记错，最终 exit 2
#   硬规矩 1：stderr 永不并入返回值。值流里混进一行警告，所有 `!= "0"` 判定就全废
#             （locale 假告警就是从这个 2>&1 漏进来的）。
#   硬规矩 2：不能用 `exit 2` 表达失败——调用点都在 `$(sql …)` 子 shell 里，exit 只杀子 shell，
#             主流程拿到空串会把「基础设施故障」误判成结构违规（rc=1）。失败写共享日志，
#             结论前统一 `-s` 判定 → rc=2。
SQL_ERR_LOG=$(mktemp "${TMPDIR:-/tmp}/menu_inv_err.XXXXXX") || { printf '⛔ mktemp 失败\n' >&2; exit 2; }
# ⑧ 用到的两个临时文件：图标名清单 / 库里出现的 (icon,path) 对。一次 SQL、一次目录列举，
#   差集用 awk 在内存里做——**不许每行 fork 一个 ls**（60 个 icon × 400+ 行菜单）。
ICON_NAMES_FILE=$(mktemp "${TMPDIR:-/tmp}/menu_icon_names.XXXXXX") || { printf '⛔ mktemp 失败\n' >&2; exit 2; }
ICON_ROWS_FILE=$(mktemp "${TMPDIR:-/tmp}/menu_icon_rows.XXXXXX") || { printf '⛔ mktemp 失败\n' >&2; exit 2; }
SKIP_NOTES=()
trap 'rm -f "$SQL_ERR_LOG" "$ICON_NAMES_FILE" "$ICON_ROWS_FILE"' EXIT

sql() {
  local out err rc
  err=$(mktemp "${TMPDIR:-/tmp}/menu_inv.XXXXXX") || { printf '⛔ mktemp 失败\n' >&2; exit 2; }
  out=$("${PSQL[@]}" -c "$1" 2>"$err"); rc=$?
  if [ "$rc" -ne 0 ]; then
    printf '⛔ 查询失败（库 %s，psql rc=%s）：\n%s\n%s\n' "$DB" "$rc" "$(cat "$err")" "$out" >&2
    printf '%s\trc=%s\t%s\n' "$(date '+%F %T')" "$rc" "$(head -2 "$err" | tr '\n' ' ')" >> "$SQL_ERR_LOG"
    rm -f "$err"; return 2
  fi
  [ -s "$err" ] && sed 's/^/    · psql 警告: /' "$err" >&2
  rm -f "$err"
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
# 自检改成「拿返回值说话」：连库失败走 sql() 的 rc=2（带 psql 报错）；能连但返回值不是 1
# 属于脚本/客户端被污染，打印回来值便于定位，别再一律报「连不上库」。
SELF_CHECK=$(sql 'select 1')
if [ "$SELF_CHECK" != "1" ]; then
  printf '⛔ 连库自检失败 %s（%s@%s:%s）：select 1 返回 [%s]\n' \
    "$DB" "$PGUSER" "$PGHOST" "$PGPORT" "$SELF_CHECK" >&2
  [ -s "$SQL_ERR_LOG" ] && sed 's/^/    · /' "$SQL_ERR_LOG" >&2
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

# ── ⑥ title 长度（menu_type 1/2，侧栏显示名 ≤ TITLE_MAX）────────────────────
# 为什么按「字数」而不是「字节」：psql 侧 length() 对 UTF8 文本按**字符**计（不是 octet_length），
# 中文菜单名一个字算一个，与卡面口径一致。
Q6="select count(*) from sys_menu
     where deleted_at is null and menu_type in (1,2) and length(title) > $TITLE_MAX"
N6=$(sql "$Q6"); N6=${N6:-x}
if [ "$N6" = "0" ]; then line '⑥ title 长度:         ' "$N6" 1 " (≤$TITLE_MAX 字)"; else
  line '⑥ title 长度:         ' "$N6" 0 " (≤$TITLE_MAX 字)"
  FAILED=$((FAILED+1)); FAIL_NAMES+=('⑥title长度')
  detail "menu_type 1/2 里 title 超过 $TITLE_MAX 字的行" \
    "select id, name, title, length(title) as len, coalesce(path,'<NULL:无 path>') as path
       from sys_menu
      where deleted_at is null and menu_type in (1,2) and length(title) > $TITLE_MAX
      order by length(title) desc, path limit 10"
fi

# ── ⑦ 同父 sort_order 唯一（menu_type=2）────────────────────────────────────
# parent_id 为 NULL 的顶级页面算一组（字面口径：NULL 也参与分组，顶级顺序同样不能撞车）。
Q7="select count(*) from (
       select parent_id, sort_order from sys_menu
        where deleted_at is null and menu_type = 2
        group by parent_id, sort_order having count(*) > 1) t"
N7=$(sql "$Q7"); N7=${N7:-x}
if [ "$N7" = "0" ]; then line '⑦ 同父 sort 唯一:     ' "$N7" 1; else
  line '⑦ 同父 sort 唯一:     ' "$N7" 0
  FAILED=$((FAILED+1)); FAIL_NAMES+=('⑦同父sort唯一')
  detail '同父节点下 sort_order 撞车的组（menu_type=2）' \
    "select coalesce(parent_id::text,'<顶级>') as parent_id, sort_order, count(*) as n,
            string_agg(name || '/' || coalesce(title,'<无标题>'), ' | ' order by name) as siblings
       from sys_menu
      where deleted_at is null and menu_type = 2
      group by parent_id, sort_order having count(*) > 1
      order by n desc, sort_order limit 10"
fi

# ── ⑧ icon 名必须在 TDesign 图标库里存在（menu_type 1/2，防「空白图标」复发）──
# 判据：库里 menu_type 1/2 且 icon 非空的记录，icon 必须能在图标库名字清单里找到同名项。
# 名字清单从哪来（icon_lib_load，按序先到先用）：
#   ① MENU_ICON_LIB 显式给（目录＝esm/components；文件＝一行一个名字 / manifest.js / icons.ts）
#   ② <repo>/frontend/node_modules/tdesign-icons-vue-next/esm/components（真实安装版本，最权威）
#   ③ <repo>/frontend/src/components/IconSelect/icons.ts（仓库内清单；只在没装依赖时才用，
#      因为升级依赖后它可能落后，那时新图标会被误报红——有 node_modules 就永远不用它）
# 一处 SQL 取 distinct (icon,path)，一次目录/文件列举得名字集，差集用 awk 在内存里做
# （不每行 fork ls：60 个 icon / 400+ 行菜单）。icon 列里的空串与 NULL 不参与（组件对空值
# 有 folder/app 兜底，见 SideMenuItem.vue 的 `props.item.icon || (hasChildren?'folder':'app')`）。
icon_lib_load() {
  local c
  local -a cands=()
  if [ -n "$MENU_ICON_LIB" ]; then
    cands=("$MENU_ICON_LIB")
  else
    cands=("$MENU_REPO_ROOT/frontend/node_modules/tdesign-icons-vue-next/esm/components"
           "$MENU_REPO_ROOT/frontend/src/components/IconSelect/icons.ts")
  fi
  for c in "${cands[@]}"; do
    if [ -d "$c" ]; then
      # 目录：取 *.js 的文件名（去扩展名）；*.js.map / *.d.ts 不算
      find "$c" -maxdepth 1 -name '*.js' -printf '%f\n' 2>/dev/null \
        | sed 's/\.js$//' | sort -u > "$ICON_NAMES_FILE"
      if [ -s "$ICON_NAMES_FILE" ]; then ICON_LIB_SRC="$c（目录枚举）"; return 0; fi
    elif [ -f "$c" ]; then
      if grep -q 'stem: "' "$c" 2>/dev/null; then
        grep -o 'stem: "[^"]*"' "$c" | sed 's/^stem: "//; s/"$//' | sort -u > "$ICON_NAMES_FILE"
      elif grep -q '^[[:space:]]*"[^"]*"' "$c" 2>/dev/null; then
        grep -o '^[[:space:]]*"[^"]*"' "$c" | sed 's/^[[:space:]]*"//; s/"$//' | sort -u > "$ICON_NAMES_FILE"
      else
        sed -e 's/[[:space:]]*$//' -e 's/^[[:space:]]*//' "$c" | grep -v '^#' | grep -v '^$' | sort -u > "$ICON_NAMES_FILE"
      fi
      if [ -s "$ICON_NAMES_FILE" ]; then ICON_LIB_SRC="$c"; return 0; fi
      ICON_LIB_ERR="$c 里没解析出图标名"
      continue
    fi
  done
  [ -n "${ICON_LIB_ERR:-}" ] || ICON_LIB_ERR="试过：$(IFS=,; echo "${cands[*]}")"
  return 1
}

ICON_LIB_SRC=''
ICON_LIB_ERR=''
if icon_lib_load; then
  ICON_LIB_N=$(grep -c . "$ICON_NAMES_FILE")
  # 先按**字符形状**筛一遍：TDesign 的 icon stem 全部匹配 ^[a-z0-9][a-z0-9-]*$（实测 2356/2356）。
  # 这一步不是洁癖：icon 里要是有换行/制表/空格，下面「一行一条」的差集会被拆成两行，而拆出来的
  # 两段**可能各自都是合法名字**（'bill\nmoney' → 'bill' 和 'money' 都在库里）⇒ 假绿。形状单独判。
  Q8_SHAPE="select count(*) from sys_menu
             where deleted_at is null and menu_type in (1,2)
               and icon is not null and btrim(icon) <> ''
               and icon !~ '^[a-z0-9][a-z0-9-]*\$'"
  N8_SHAPE=$(sql "$Q8_SHAPE"); N8_SHAPE=${N8_SHAPE:-x}
  # 形状合法的再取 distinct（icon,path），一次 SQL；名字集一次列举；差集 awk 在内存里做
  Q8="select distinct icon || E'\t' || coalesce(path,'<NULL:无 path>')
        from sys_menu
       where deleted_at is null and menu_type in (1,2)
         and icon is not null and btrim(icon) <> ''
         and icon ~ '^[a-z0-9][a-z0-9-]*\$'"
  if sql "$Q8" > "$ICON_ROWS_FILE"; then
    printf '\n' >> "$ICON_ROWS_FILE"        # sql() 不带行尾换行，补一个再数行
    ICON_ROWS_N=$(grep -c . "$ICON_ROWS_FILE")
    BAD8=$(awk -F'\t' 'NR==FNR { ok[$1]=1; next } $0 == "" { next } ($1 in ok) { next } { print }' \
             "$ICON_NAMES_FILE" "$ICON_ROWS_FILE")
    N8_LIST=$(printf '%s\n' "$BAD8" | grep -c . || true)
    # 总数 = 名字不存在 + 字符形状非法（两者都算坏；形状那条可能带换行，所以不混进同一份差集）
    case "$N8_SHAPE" in
      ''|*[!0-9]*) N8_SHAPE_BAD=1; N8_SHAPE_N=0 ;;   # 查询失败/返回值非数字：下面 infra 兜底会 rc=2
      *) if [ "$N8_SHAPE" -gt 0 ]; then N8_SHAPE_BAD=1; else N8_SHAPE_BAD=0; fi
         N8_SHAPE_N=$N8_SHAPE ;;
    esac
    N8=$(( N8_LIST + N8_SHAPE_N ))
    if [ "$N8" = "0" ]; then
      line '⑧ icon 名合法:        ' "$N8" 1
      printf '    ↳ 图标库：%s（%s 个名字）；库里 (icon,path) 去重后 %s 条，全部命中\n' \
        "$ICON_LIB_SRC" "$ICON_LIB_N" "$ICON_ROWS_N"
    else
      line '⑧ icon 名合法:        ' "$N8" 0
      FAILED=$((FAILED+1)); FAIL_NAMES+=('⑧icon名不存在')
      if [ "$N8_LIST" != "0" ]; then
        printf '    ↳ 图标库里没有这些名字（侧栏会渲染成空白，因为 icon 非空所以 folder/app 兜底不触发）：\n'
        printf '%s\n' "$BAD8" | awk -F'\t' '{ printf "       %-28s %s\n", $1, $2 }' | head -10
        [ "$N8_LIST" -gt 10 ] 2>/dev/null && printf '       …另有 %s 条未展开\n' "$((N8_LIST-10))"
      fi
      if [ "$N8_SHAPE_BAD" = "1" ]; then
        detail 'icon 里含非法字符的行（TDesign 名字只允许小写字母/数字/连字符；换行与制表符都算非法）' \
          "select id, name, coalesce(path,'<NULL:无 path>') as path,
                  replace(replace(icon, chr(10), '\\n'), chr(9), '\\t') as icon_visible,
                  length(icon) as len
             from sys_menu
            where deleted_at is null and menu_type in (1,2)
              and icon is not null and btrim(icon) <> ''
              and icon !~ '^[a-z0-9][a-z0-9-]*\$'
            order by path limit 10"
      fi
      printf '       图标库：%s\n' "$ICON_LIB_SRC"
    fi
  else
    line '⑧ icon 名合法:        ' 'x' 0
    FAILED=$((FAILED+1)); FAIL_NAMES+=('⑧icon名不存在')
  fi
else
  if [ "$MENU_ICON_LIB_REQUIRED" = "1" ]; then
    printf '⛔ ⑧ 无法校验 icon 名（找不到图标库）：%s\n' "$ICON_LIB_ERR" >&2
    printf '   （MENU_ICON_LIB_REQUIRED=1 ⇒ 缺图标库按基础设施问题 rc=2，不降级成「跳过」）\n' >&2
    exit 2
  fi
  printf '⑧ icon 名合法:         ⏭  跳过（找不到图标库 — %s）\n' "$ICON_LIB_ERR"
  printf '    ↳ 这是「没校验」，不是「校验通过」：设 MENU_ICON_LIB=<图标库目录> 或装好前端依赖后重跑\n'
  SKIP_NOTES+=('⑧icon名(无图标库)')
fi

# ── 基础设施兜底：任何一条查询 psql 失败 ⇒ rc=2（绝不降级成「结构违规」去骗迁移）──
if [ -s "$SQL_ERR_LOG" ]; then
  printf '⛔ 查询过程中 psql 失败 %s 次（基础设施问题，不是结构违规，别照着建迁移）：\n' \
    "$(wc -l < "$SQL_ERR_LOG")" >&2
  sed 's/^/    · /' "$SQL_ERR_LOG" >&2
  exit 2
fi

# ── 结论 ────────────────────────────────────────────────────────────────────
if [ "$FAILED" -eq 0 ]; then
  if [ "${#SKIP_NOTES[@]}" -gt 0 ]; then
    # 有跳过就不是「8 条全绿」，必须写出来（本仓库对「假绿」敏感：跳过 ≠ 通过）
    printf '结果: 全绿 ✅，但**有跳过**：%s —— 跳过 = 没校验，不是通过\n' \
      "$(IFS=,; echo "${SKIP_NOTES[*]}")"
  else
    printf '结果: 全绿 ✅\n'
  fi
  exit 0
fi
printf '结果: %d 条不变量不通过 ❌（%s）\n' "$FAILED" "$(IFS=,; echo "${FAIL_NAMES[*]}")"
exit 1
