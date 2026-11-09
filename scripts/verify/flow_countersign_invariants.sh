#!/usr/bin/env bash
# =============================================================================
# flow_countersign_invariants.sh —— 全员会签（approval_mode=ALL）节点的「永久卡死」闸门
#
# 卡：t_8733a7a0 / E2E07-FIX-1（P2 系统性）
#
# 为什么需要这条闸门（两次实测的同一个病）
#   `ALL` 的推进条件是「所有候选人都签过」。候选人里只要有一个**永远签不了**
#   （离职未摘角色 / 停用 / 口令是占位符 / 授权过期），节点就永远凑不齐票：
#   没有待办可点、没有报错、界面上也没有按钮 —— 单据停在 PENDING_APPROVAL，
#   最后只能改库。E2E-06（预算 n_dept）与 E2E-07（合同 n_pm / n_pm_hi）各实测一次。
#
# 用法
#   bash scripts/verify/flow_countersign_invariants.sh [dbname]
#       dbname 省略时取 $NIK_DATABASE_NAME，再退回 cloudpivot_dev
#   连接参数：PGHOST / PGPORT / PGUSER / PGPASSWORD，未设时回落 NIK_DATABASE_*
#   （与 menu_invariants.sh 同一套，psql 惯例）
#
# 退出码：0 = 4 条断言全绿 ｜ 1 = 有违规（明细已打印）｜ 2 = 连库/查询失败（基础设施问题）
#
# 4 条断言
#   ① ALL 节点必须配兜底        `approval_mode='ALL'` 且（timeout_hours 为空或 ≤0）
#                               且既无 fallback_user_id 也无 fallback_role_id ⇒ 违规。
#                               与发布校验 R28（`graph::all_countersign_issues`，65048）**同口径**：
#                               发布期挡新图，本脚本管**存量库**。注意「只填了 timeout_action、
#                               没填 timeout_hours」也算违规 —— `flow::timeout` 的扫描谓词是
#                               `timeout_hours IS NOT NULL`，没有小时数永远不会被扫到。
#   ② 角色型审批不得写成全员会签  `node_type='APPROVAL'`（语义=一人签字即可）+ `assignee_type='ROLE'`
#                               + `approval_mode='ALL'` ⇒ 违规。这是 V0339 按「候选人数 ≥2 ⇒ 冻结
#                               成 ALL」留下的错配（采购经理 / 部门主管被冻成「委员会」），
#                               迁移 V0359 已全系统修正；本断言防复发。
#                               跨角色会签用 `COUNTERSIGN`（本断言不管，那是正确配置）。
#   ③ ALL 节点的角色里不得有坏号 角色成员里出现：已软删 / 状态停用 / 授权已过期 /
#                               `password` 是占位符（'x' / '' / 'CHANGE_ME'）或长度 < 20
#                               ⇒ 违规。长度判据来自卡面：bcrypt=60 / argon2=97，占位符是 1 字节。
#   ④ ALL 节点的可用候选人不得为 0 全员会签节点解析出的候选人为 0 时只剩 `empty_assignee` 兜底：
#                               配 FALLBACK_ADMIN 就退化成「全部超管会签」（同一条死锁换个角色重演，
#                               多一个超管夹具账号就中招），配 SKIP 则**审批被静默跳过**。
#                               两种情况都不该悄悄发生在生产上，所以按违规报。
#
# 标准输出 = 证据（可直接贴进卡里）：先是 ALL 节点清单，再是 4 条断言，最后一行结论。
# =============================================================================
set -uo pipefail

DB="${1:-${NIK_DATABASE_NAME:-cloudpivot_dev}}"
PGHOST="${PGHOST:-${NIK_DATABASE_HOST:-127.0.0.1}}"
PGPORT="${PGPORT:-${NIK_DATABASE_PORT:-5432}}"
PGUSER="${PGUSER:-${NIK_DATABASE_USER:-cloudpivot}}"
PGPASSWORD="${PGPASSWORD:-${NIK_DATABASE_PASSWORD:-}}"
export PGPASSWORD PGHOST PGPORT PGUSER

# locale 钉死（与 menu_invariants.sh 同因：Debian 的 psql 是 Perl pg_wrapper，
# 目标机没装调用方的 locale 时会往 stderr 打十几行警告而 psql 仍以 0 退出）
export LC_ALL=C
export LANG=C
unset LANGUAGE 2>/dev/null || true
export PERL_BADLANG=0
export PGCLIENTENCODING=UTF8

PSQL=(psql -X -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" -d "$DB" -tA -q -v ON_ERROR_STOP=1)

FAILED=0
FAIL_NAMES=()

SQL_ERR_LOG=$(mktemp "${TMPDIR:-/tmp}/flow_cs_err.XXXXXX") || { printf '⛔ mktemp 失败\n' >&2; exit 2; }
trap 'rm -f "$SQL_ERR_LOG"' EXIT

# sql <query> —— 只返回 stdout；失败记日志，结论前统一判 rc=2（不许把基础设施故障判成违规）
sql() {
  local out err rc
  err=$(mktemp "${TMPDIR:-/tmp}/flow_cs.XXXXXX") || { printf '⛔ mktemp 失败\n' >&2; exit 2; }
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

line() { # line <标签> <数值> <通过?> [额外说明]
  local label=$1 value=$2 ok=$3 extra=${4:-}
  local mark='❌'
  [ "$ok" = 1 ] && mark='✅'
  printf '%s%4s  %s%s\n' "$label" "$value" "$mark" "$extra"
}

detail() { # detail <标题> <query>
  local out
  out=$(sql "$2")
  printf '    ↳ %s（前 10 条）：\n' "$1"
  if [ -n "$out" ]; then printf '%s\n' "$out" | head -10 | sed 's/^/       /'; else printf '       （无）\n'; fi
  return 0
}

# ── 连库自检（连不上就 rc=2，不打印「全绿」）────────────────────────────────
probe=$(sql "select 1")
if [ "$probe" != "1" ]; then
  printf '⛔ 连不上库 / 无法查询（库 %s @ %s:%s user=%s）\n' "$DB" "$PGHOST" "$PGPORT" "$PGUSER" >&2
  exit 2
fi

# 「当前版本」口径：每个流程只取 is_current 的版本（历史版本留着是审计用的，不参与运行）
CUR="
  select d.id as def_id, d.flow_code, v.id as version_id, v.version_no
    from biz_flow_def d
    join biz_flow_def_version v on v.id = d.current_version_id
   where d.deleted_at is null and d.status = 'PUBLISHED'"

# 「可用成员」口径（与 flow::assignee::load_role + filter_candidates 同源）：
#   授权未过期 + 用户未删未停 + 口令不是占位符（占位符账号登不进来 = 永远签不了）
USABLE="u.deleted_at is null and u.status = 1
        and (ur.expire_time is null or ur.expire_time > now())
        and u.password not in ('x', '', 'CHANGE_ME') and length(u.password) >= 20"

printf '===== ALL 会签节点清单（PUBLISHED 当前版本）=====\n'
sql "
  select c.flow_code || '  ' || n.node_key || '  ' || n.node_name
         || '  [' || n.node_type || '/' || coalesce(n.assignee_type,'-') || ']'
         || '  timeout=' || coalesce(n.timeout_hours::text,'-') || 'h/' || n.timeout_action
         || '  fallback=' || case when n.fallback_user_id is not null or n.fallback_role_id is not null then 'Y' else '-' end
    from ($CUR) c
    join biz_flow_node n on n.version_id = c.version_id
   where n.approval_mode = 'ALL'
   order by c.flow_code, n.node_key" | sed 's/^/  · /'
[ -z "$(sql "select 1 from ($CUR) c join biz_flow_node n on n.version_id = c.version_id where n.approval_mode = 'ALL' limit 1")" ] \
  && printf '  （当前库里没有 ALL 会签节点）\n'
printf '\n'

printf '===== 4 条不变量 =====\n'

# ── ① ALL 节点必须配兜底（超时小时 >0 或 兜底人）────────────────────────────
N1_ANY=$(sql "select 1 from ($CUR) c join biz_flow_node n on n.version_id = c.version_id where n.approval_mode = 'ALL' limit 1")
N1=$(sql "
  select count(*)
    from ($CUR) c
    join biz_flow_node n on n.version_id = c.version_id
   where n.approval_mode = 'ALL'
     and (n.timeout_hours is null or n.timeout_hours <= 0)
     and n.fallback_user_id is null and n.fallback_role_id is null")
if [ -z "$N1" ]; then
  printf '⛔ ① 查询失败\n' >&2
  exit 2
fi
if [ "$N1" = "0" ]; then
  if [ -z "$N1_ANY" ]; then
    line '① ALL 节点无兜底:      ' 0 1 '  （库里暂无 ALL 节点，本断言空转）'
  else
    line '① ALL 节点无兜底:      ' 0 1 '  （每个 ALL 节点都有超时小时或兜底人）'
  fi
else
  line '① ALL 节点无兜底:      ' "$N1" 0
  detail '报红节点（流程 / 节点 / 超时 / 兜底）' "
    select c.flow_code || ' / ' || n.node_key || ' / timeout=' || coalesce(n.timeout_hours::text,'NULL')
           || ' ' || n.timeout_action || ' / fallback=' ||
           case when n.fallback_user_id is not null or n.fallback_role_id is not null then 'Y' else 'NULL' end
      from ($CUR) c
      join biz_flow_node n on n.version_id = c.version_id
     where n.approval_mode = 'ALL'
       and (n.timeout_hours is null or n.timeout_hours <= 0)
       and n.fallback_user_id is null and n.fallback_role_id is null
     order by c.flow_code, n.node_key"
  FAILED=$((FAILED+1)); FAIL_NAMES+=('①ALL无兜底')
fi

# ── ② 角色型审批（APPROVAL + ROLE）不得写成 ALL ─────────────────────────────
N2=$(sql "
  select count(*)
    from ($CUR) c
    join biz_flow_node n on n.version_id = c.version_id
   where n.node_type = 'APPROVAL' and n.assignee_type = 'ROLE' and n.approval_mode = 'ALL'")
if [ -z "$N2" ]; then printf '⛔ ② 查询失败\n' >&2; exit 2; fi
if [ "$N2" = "0" ]; then
  line '② 角色型审批误配 ALL: ' 0 1 '  （APPROVAL+ROLE+ALL = 0；跨角色会签请用 COUNTERSIGN）'
else
  line '② 角色型审批误配 ALL: ' "$N2" 0 '  （APPROVAL 的语义是「岗位任一人签字即可」）'
  detail '报红节点' "
    select c.flow_code || ' / ' || n.node_key || '（' || n.node_name || '）'
      from ($CUR) c
      join biz_flow_node n on n.version_id = c.version_id
     where n.node_type = 'APPROVAL' and n.assignee_type = 'ROLE' and n.approval_mode = 'ALL'
     order by c.flow_code, n.node_key"
  FAILED=$((FAILED+1)); FAIL_NAMES+=('②角色型审批误配ALL')
fi

# ── ③ ALL 节点的角色里不得有坏号 ────────────────────────────────────────────
N3=$(sql "
  select count(*)
    from ($CUR) c
    join biz_flow_node n on n.version_id = c.version_id
    join biz_flow_node_assignee a on a.node_id = n.id and a.assignee_type = 'ROLE'
    join sys_user_role ur on ur.role_id = a.ref_id
    join sys_user u on u.id = ur.user_id
   where n.approval_mode = 'ALL'
     and not ($USABLE)")
if [ -z "$N3" ]; then printf '⛔ ③ 查询失败\n' >&2; exit 2; fi
if [ "$N3" = "0" ]; then
  line '③ ALL 节点角色坏号:    ' 0 1 '  （无软删 / 停用 / 授权过期 / 占位符口令）'
else
  line '③ ALL 节点角色坏号:    ' "$N3" 0 '  （这些账号永远签不了 ⇒ 会签永远凑不齐）'
  detail '坏号明细（流程 / 节点 / 角色 / 账号 / 原因）' "
    select c.flow_code || ' / ' || n.node_key || ' / ' || r.role_code || ' / ' || u.username || ' / ' ||
           concat_ws('+',
             case when u.deleted_at is not null then '已软删' end,
             case when u.status <> 1 then '停用(' || u.status || ')' end,
             case when ur.expire_time is not null and ur.expire_time <= now() then '授权过期' end,
             case when u.password in ('x','','CHANGE_ME') or length(u.password) < 20
                  then '口令占位符(len=' || length(u.password) || ')' end)
      from ($CUR) c
      join biz_flow_node n on n.version_id = c.version_id
      join biz_flow_node_assignee a on a.node_id = n.id and a.assignee_type = 'ROLE'
      join sys_role r on r.id = a.ref_id
      join sys_user_role ur on ur.role_id = a.ref_id
      join sys_user u on u.id = ur.user_id
     where n.approval_mode = 'ALL'
       and not ($USABLE)
     order by c.flow_code, n.node_key, r.role_code, u.username"
  FAILED=$((FAILED+1)); FAIL_NAMES+=('③ALL角色坏号')
fi

# ── ④ ALL 节点的可用候选人不得为 0 ──────────────────────────────────────────
N4=$(sql "
  select count(*)
    from (
      select c.flow_code, n.node_key, n.empty_assignee,
             (select count(distinct u.id)
                from biz_flow_node_assignee a
                join sys_user_role ur on ur.role_id = a.ref_id
                join sys_user u on u.id = ur.user_id
               where a.node_id = n.id and a.assignee_type = 'ROLE' and ($USABLE)) as usable
        from ($CUR) c
        join biz_flow_node n on n.version_id = c.version_id
       where n.approval_mode = 'ALL'
    ) t
   where t.usable = 0")
if [ -z "$N4" ]; then printf '⛔ ④ 查询失败\n' >&2; exit 2; fi
if [ "$N4" = "0" ]; then
  line '④ ALL 节点候选人为 0:  ' 0 1 '  （每个 ALL 节点都至少有一个能签的人）'
else
  line '④ ALL 节点候选人为 0:  ' "$N4" 0 '  （只能靠 empty_assignee 兜：FALLBACK_ADMIN=超管会签、SKIP=静默跳过）'
  detail '报红节点' "
    select t.flow_code || ' / ' || t.node_key || ' / empty_assignee=' || t.empty_assignee
      from (
        select c.flow_code, n.node_key, n.empty_assignee,
               (select count(distinct u.id)
                  from biz_flow_node_assignee a
                  join sys_user_role ur on ur.role_id = a.ref_id
                  join sys_user u on u.id = ur.user_id
                 where a.node_id = n.id and a.assignee_type = 'ROLE' and ($USABLE)) as usable
          from ($CUR) c
          join biz_flow_node n on n.version_id = c.version_id
         where n.approval_mode = 'ALL'
      ) t
     where t.usable = 0
     order by t.flow_code, t.node_key"
  FAILED=$((FAILED+1)); FAIL_NAMES+=('④ALL候选人为0')
fi

# ── 基础设施兜底 ────────────────────────────────────────────────────────────
if [ -s "$SQL_ERR_LOG" ]; then
  printf '⛔ 查询过程中 psql 失败 %s 次（基础设施问题，不是不变量违规）：\n' "$(wc -l < "$SQL_ERR_LOG")" >&2
  sed 's/^/    · /' "$SQL_ERR_LOG" >&2
  exit 2
fi

if [ "$FAILED" -eq 0 ]; then
  printf '结果: 4 条不变量全绿 ✅（库 %s）\n' "$DB"
  exit 0
fi
printf '结果: %d 条不变量不通过 ❌（%s）（库 %s）\n' "$FAILED" "$(IFS=,; echo "${FAIL_NAMES[*]}")" "$DB"
exit 1
