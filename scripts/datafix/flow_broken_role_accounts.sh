#!/usr/bin/env bash
# =============================================================================
# flow_broken_role_accounts.sh —— 把**不可用的账号**从角色里摘掉（会签死锁的根因清理）
#
# 卡：t_8733a7a0 / E2E07-FIX-1 ⑤
#
# 为什么需要它
#   全员会签（approval_mode=ALL）节点要求「所有候选人都签过」。角色里只要挂着一个
#   **永远进不来**的账号（停用 / 软删 / 口令是占位符 'x' —— 登录直接 500），
#   这个节点就永远凑不齐票：单据停在 PENDING_APPROVAL，没有待办、没有告警、界面上
#   也没有按钮。E2E-06 是 `p3a_0002`（预算 DEPT_LEADER 会签），E2E-07 是 `p3a_0003`
#   （合同 PURCHASE_MANAGER 会签），两次都得靠改库才推得动。
#
#   停用 / 软删的账号挂在角色上**本来就是无意义的**（`flow::assignee` 的
#   filter_candidates 会先把它们过滤掉），留着只会：
#     ① 让「这个角色有几个人能签」难以一眼看清；
#     ② 一旦哪天节点被配成 ALL，就变成死锁源。
#   口令是占位符的账号更直接：它在系统里**根本登不进来**，占着角色等于占着会签的一票。
#
# 用法
#   bash scripts/datafix/flow_broken_role_accounts.sh              # 只体检（默认，dry-run）
#   bash scripts/datafix/flow_broken_role_accounts.sh --apply      # 执行摘除（先留档再删）
#   库 / 连接参数同 scripts/verify/*.sh：$1=库名（省略取 NIK_DATABASE_NAME→cloudpivot_dev），
#   PGHOST/PGPORT/PGUSER/PGPASSWORD 未设时回落 NIK_DATABASE_*
#
# 判据（**只动角色授权行，不碰用户本身**）
#   命中任一条即视为「不可用账号」：
#     · `sys_user.deleted_at` 非空（已软删）
#     · `sys_user.status <> 1`（停用）
#     · `password` 是占位符（'x' / '' / 'CHANGE_ME'）或长度 < 20（bcrypt=60 / argon2=97）
#   两条硬边界：
#     · **不动 SUPER_ADMIN**：把坏号超管从超管角色摘掉 = 把自己锁在门外，属于事故；
#       超管口令有问题请走「重置密码」接口。
#     · **不删用户**：只删 `sys_user_role` 这一行授权，用户与他的历史单据全留着。
#       要恢复授权重新勾角色即可（留档里有原行 JSONB）。
#
# 留档
#   `--apply` 会先把每一行 `sys_user_role` 的整行 JSONB 快照写进通用留档表
#   `biz_data_fix_backup`（fix_code = 'E2E07-FIX-1'，唯一键 (fix_code, table_name, row_id)），
#   再删授权 —— 出事可一行 SQL 还原：
#     insert into sys_user_role select * from jsonb_populate_record(null::sys_user_role, row_data)
#      from biz_data_fix_backup where fix_code='E2E07-FIX-1' and table_name='sys_user_role';
#
# 退出码：0 = 体检通过（或 apply 成功）｜ 1 = apply 后仍有残留（不该发生）｜ 2 = 连库/查询失败
# =============================================================================
set -uo pipefail

DB="${1:-${NIK_DATABASE_NAME:-cloudpivot_dev}}"
[ "${DB:0:2}" = "--" ] && DB="${NIK_DATABASE_NAME:-cloudpivot_dev}"
APPLY=0
for arg in "$@"; do
  case "$arg" in
    --apply) APPLY=1 ;;
    --dry-run) APPLY=0 ;;
    --*) printf '⛔ 未知参数：%s（只支持 --apply / --dry-run）\n' "$arg" >&2; exit 2 ;;
  esac
done

PGHOST="${PGHOST:-${NIK_DATABASE_HOST:-127.0.0.1}}"
PGPORT="${PGPORT:-${NIK_DATABASE_PORT:-5432}}"
PGUSER="${PGUSER:-${NIK_DATABASE_USER:-cloudpivot}}"
PGPASSWORD="${PGPASSWORD:-${NIK_DATABASE_PASSWORD:-}}"
export PGPASSWORD PGHOST PGPORT PGUSER
export LC_ALL=C LANG=C
unset LANGUAGE 2>/dev/null || true
export PERL_BADLANG=0 PGCLIENTENCODING=UTF8

PSQL=(psql -X -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" -d "$DB" -tA -q -v ON_ERROR_STOP=1)

# 「不可用账号的角色授权」清单（本文件里**唯一**一份判据，body / 计数 / 留档 / 删除都用它）
BROKEN_CTE="
with broken as (
  select ur.id            as grant_id,
         ur.user_id       as user_id,
         r.role_code      as role_code,
         u.username       as username,
         u.status         as status,
         length(u.password) as pwd_len,
         concat_ws('+',
           case when u.deleted_at is not null then '已软删' end,
           case when u.status <> 1 then '停用(' || u.status || ')' end,
           case when u.password in ('x','','CHANGE_ME') or length(u.password) < 20
                then '口令占位符(len=' || length(u.password) || ')' end
         ) as reason
    from sys_user_role ur
    join sys_user u on u.id = ur.user_id
    join sys_role r on r.id = ur.role_id and r.deleted_at is null
   where r.role_code <> 'SUPER_ADMIN'
     and (u.deleted_at is not null
          or u.status <> 1
          or u.password in ('x','','CHANGE_ME') or length(u.password) < 20)
)"

q() { "${PSQL[@]}" -c "$1"; }

probe=$(q "select 1")
if [ "$probe" != "1" ]; then
  printf '⛔ 连不上库（库 %s @ %s:%s user=%s）\n' "$DB" "$PGHOST" "$PGPORT" "$PGUSER" >&2
  exit 2
fi

CNT=$(q "$BROKEN_CTE select count(*) from broken")
[ -z "$CNT" ] && { printf '⛔ 查询失败\n' >&2; exit 2; }

printf '===== 不可用账号的角色授权（库 %s）=====\n' "$DB"
if [ "$CNT" = "0" ]; then
  printf '  （无：角色里没有停用 / 软删 / 口令占位符的账号）\n'
  printf '结果: 体检通过 ✅（0 行；无需清理）\n'
  exit 0
fi

q "$BROKEN_CTE
   select '  · ' || role_code || ' / ' || username || ' / ' || reason
     from broken order by role_code, username" | sed 's/^ *//'
printf '命中 %s 行角色授权\n' "$CNT"

if [ "$APPLY" != "1" ]; then
  printf '\n（dry-run：什么都没改。确认无误后加 --apply 执行摘除；摘除前会先写 biz_data_fix_backup）\n'
  exit 0
fi

printf '\n===== 执行：先留档，再摘除 =====\n'
q "$BROKEN_CTE
   insert into biz_data_fix_backup (fix_code, table_name, row_id, row_data)
   select 'E2E07-FIX-1', 'sys_user_role', b.grant_id, to_jsonb(ur)
     from broken b join sys_user_role ur on ur.id = b.grant_id
   on conflict (fix_code, table_name, row_id) do nothing"
BAK=$(q "select count(*) from biz_data_fix_backup where fix_code='E2E07-FIX-1' and table_name='sys_user_role'")

q "$BROKEN_CTE
   delete from sys_user_role ur
    using broken b
    where ur.id = b.grant_id"
LEFT=$(q "$BROKEN_CTE select count(*) from broken")

printf '  · 留档行数（累计）：%s\n' "$BAK"
printf '  · 删除后残留：%s\n' "$LEFT"
if [ "$LEFT" != "0" ]; then
  printf '⛔ 摘除后仍有 %s 行残留（不该发生）\n' "$LEFT" >&2
  exit 1
fi
printf '结果: 已摘除 %s 行不可用账号的角色授权（留档 fix_code=E2E07-FIX-1，可 JSONB 还原）✅\n' "$CNT"
