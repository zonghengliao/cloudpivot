#!/usr/bin/env bash
# P2-8 项目模块现状普查（可复跑，两轮输出必须逐字一致：除首行时间戳外 diff 为空）
#
# 目的：用「库里的真实行数 + 库里的真实 DDL」回答评审的三个疑问——
#   ① 真有 3 个项目吗？② 成员为什么是 0？③ 里程碑 / 预算 / 工时归集到底有没有？
#
# 用法：bash docs/evidence/20261104-p2-8/survey.sh > docs/evidence/20261104-p2-8/run1.txt
#
# 口径说明：
#   · 遍历本机**全部**业务库（不只 cloudpivot_* 前缀：dev 后端实际跑在 cp_* 库上，
#     只扫前缀会漏掉评审看到的那套数据），跳过没有 biz_project 表的库；只读、不写任何数据；
#   · 「空壳项目」= deleted_at IS NULL 且 owner_id IS NULL 且 0 条未删任务
#     （这正是 V0132 在空库首启时的落库形态：迁移早于用户种子 → owner 解析为 NULL）；
#   · 「工时归集」只算**挂在项目任务身上**的工时（项目统计接口的口径就是
#     sum(biz_task.actual_hours) WHERE project_id 非空），不把散任务的工时算进来。
set -uo pipefail

PSQL="psql -tA -F'|'"          # 无表头、竖线分隔（竖线必须带引号，否则被外层 shell 当管道）
run() { su postgres -c "$1" 2>/dev/null; }

ALL_DBS=$(run "psql -tA -d postgres -c \"SELECT datname FROM pg_database
              WHERE datname NOT IN ('postgres','template0','template1') ORDER BY 1\"")

printf '### A. 逐库普查（%s）\n' "$(date '+%F %T %Z')"
printf 'db|projects|owner_null|shell_projects|tasks|worklogs|members\n'

total_db=0; total_proj=0; total_shell=0; total_members=0; db_with_members=0
total_tasks=0; total_wl=0; db_with_wl=0
for db in $ALL_DBS; do
  has=$(run "$PSQL -d $db -c \"SELECT to_regclass('public.biz_project') IS NOT NULL\"")
  [ "$has" = "t" ] || continue
  row=$(run "$PSQL -d $db -c \"
    SELECT
      (SELECT count(*) FROM biz_project WHERE deleted_at IS NULL) || '|' ||
      (SELECT count(*) FROM biz_project WHERE deleted_at IS NULL AND owner_id IS NULL) || '|' ||
      (SELECT count(*) FROM biz_project p WHERE p.deleted_at IS NULL AND p.owner_id IS NULL
          AND NOT EXISTS (SELECT 1 FROM biz_task t WHERE t.project_id = p.id AND t.deleted_at IS NULL)) || '|' ||
      (SELECT count(*) FROM biz_task WHERE deleted_at IS NULL) || '|' ||
      (SELECT count(*) FROM biz_task_worklog) || '|' ||
      (SELECT count(*) FROM biz_project_member)\"")
  [ -n "$row" ] || continue
  echo "$db|$row"
  total_db=$((total_db+1))
  IFS='|' read -r p on sh ta wl mb <<<"$row"
  total_proj=$((total_proj+p)); total_shell=$((total_shell+sh)); total_members=$((total_members+mb))
  total_tasks=$((total_tasks+ta)); total_wl=$((total_wl+wl))
  [ "$mb" -gt 0 ] && db_with_members=$((db_with_members+1))
  [ "$wl" -gt 0 ] && db_with_wl=$((db_with_wl+1))
done

printf '\n### B. 汇总\n'
printf 'databases_with_biz_project=%d\nprojects=%d\nshell_projects(owner_null_and_no_task)=%d\nmember_rows=%d\ndatabases_with_member_rows=%d\ntask_rows=%d\nworklog_rows=%d\ndatabases_with_worklog=%d\n' \
  "$total_db" "$total_proj" "$total_shell" "$total_members" "$db_with_members" \
  "$total_tasks" "$total_wl" "$db_with_wl"

# 参照库：优先用最新迁移最全的库（cp_p23_prod），退化到 cloudpivot_audit，再退化到任意有 biz_project 的库
ref=$(run "psql -tA -d postgres -c \"SELECT datname FROM pg_database WHERE datname='cp_p23_prod'\"")
[ -n "$ref" ] || ref=$(run "psql -tA -d postgres -c \"SELECT datname FROM pg_database WHERE datname='cloudpivot_audit'\"")
[ -n "$ref" ] || ref=$(for db in $ALL_DBS; do
  [ "$(run "$PSQL -d $db -c \"SELECT to_regclass('public.biz_project') IS NOT NULL\"")" = "t" ] && { echo "$db"; break; }
done)

printf '\n### C. 里程碑 / 预算 / 工时三件套的能力存量（只读定义，不看数据）\n'
printf 'ref_db=%s\n' "$ref"
printf 'table:biz_project_member=%s\n'      "$(run "$PSQL -d $ref -c \"SELECT to_regclass('public.biz_project_member') IS NOT NULL\"")"
printf 'table:biz_task_worklog=%s\n'        "$(run "$PSQL -d $ref -c \"SELECT to_regclass('public.biz_task_worklog') IS NOT NULL\"")"
printf 'table:any_milestone=%s\n'           "$(run "$PSQL -d $ref -c \"SELECT count(*) > 0 FROM information_schema.tables WHERE table_schema='public' AND table_name LIKE '%milestone%'\"")"
printf 'table:biz_budget=%s\n'              "$(run "$PSQL -d $ref -c \"SELECT to_regclass('public.biz_budget') IS NOT NULL\"")"
printf 'col:biz_project.budget_any=%s\n'    "$(run "$PSQL -d $ref -c \"SELECT count(*) > 0 FROM information_schema.columns WHERE table_name='biz_project' AND column_name LIKE '%budget%'\"")"
printf 'col:biz_task.is_milestone=%s\n'     "$(run "$PSQL -d $ref -c \"SELECT count(*) > 0 FROM information_schema.columns WHERE table_name='biz_task' AND column_name='is_milestone'\"")"
printf 'perm:codes=%s\n'                    "$(run "$PSQL -d $ref -c \"SELECT string_agg(DISTINCT auth_code, ',' ORDER BY auth_code) FROM sys_menu WHERE auth_code LIKE 'business:project:%'\"")"
printf 'menu:names=%s\n'                    "$(run "$PSQL -d $ref -c \"SELECT string_agg(DISTINCT name, ',' ORDER BY name) FROM sys_menu WHERE path = '/project'\"")"

printf '\n### D. 工时归集是否被真正跑通过（跨库：挂在项目下的任务身上的工时记录）\n'
printf 'db|worklog_on_project_tasks|projects_with_worklog\n'
hit_db=0; hit_rows=0
for db in $ALL_DBS; do
  has=$(run "$PSQL -d $db -c \"SELECT to_regclass('public.biz_task_worklog') IS NOT NULL\"")
  [ "$has" = "t" ] || continue
  row=$(run "$PSQL -d $db -c \"
    SELECT count(*) || '|' || count(DISTINCT t.project_id)
      FROM biz_task_worklog w JOIN biz_task t ON t.id = w.task_id
     WHERE t.project_id IS NOT NULL AND t.deleted_at IS NULL\"")
  [ -n "$row" ] || continue
  n=${row%%|*}
  [ "${n:-0}" -gt 0 ] || continue
  echo "$db|$row"
  hit_db=$((hit_db+1)); hit_rows=$((hit_rows+n))
done
printf 'databases_with_project_worklog=%d\nworklog_rows_on_project_tasks=%d\n' "$hit_db" "$hit_rows"
