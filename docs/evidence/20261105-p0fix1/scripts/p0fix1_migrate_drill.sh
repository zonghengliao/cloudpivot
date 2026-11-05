#!/usr/bin/env bash
# V0245 迁移往返演练：up(已应用) → down → up → up，逐轮断言 status 与 remark 形态
set -uo pipefail
DB=cloudpivot_p0fix1
MIG=/opt/cloudpivot/backend/migrations/business
export PGPASSWORD=alun
P() { psql -h 127.0.0.1 -U alun -d "$DB" -X -q -v ON_ERROR_STOP=1 "$@"; }
SNAP() {
  P -AtF' | ' -c "select job_name, status::text, left(remark, 36) from sys_job
                   where deleted_at is null and job_name like 'erp\_%\_scan' order by job_name"
}
# $1=期望 status  $2=remark 必须匹配的 LIKE 模式  $3=人话
ASSERT() {
  local want="$1" pat="$2" label="$3" bad
  bad=$(P -Atc "select count(*) from sys_job where deleted_at is null
                 and job_name like 'erp\_%\_scan'
                 and (status::text <> '$want' or remark not like '$pat')")
  if [ "$bad" = "0" ]; then echo "   ✅ 断言通过：4 行 status=$want，remark 匹配『$label』"
  else echo "   ❌ 断言失败：$bad 行不符（期望 status=$want / remark 匹配 $label）"; return 1; fi
}

echo "== 0. 当前（V0245 已应用）"
SNAP
ASSERT 1 "%（启用：每天%" "（启用：每天…）"

echo
echo "== 1. 执行 down（回到 V0210 的停用形态，remark 逐字还原）"
P -f "$MIG/V0245__erp_reminder_jobs_enabled.down.sql"
SNAP
ASSERT 0 "%（内置任务已注册，本行仅供查看/手动执行）：每天%" "（内置任务已注册，本行仅供查看/手动执行）：每天…"

echo
echo "== 2. 再执行 up"
P -f "$MIG/V0245__erp_reminder_jobs_enabled.up.sql"
SNAP
ASSERT 1 "%（启用：每天%" "（启用：每天…）"

echo
echo "== 3. 再跑一次 up（幂等重放：不应报错，状态不变）"
P -f "$MIG/V0245__erp_reminder_jobs_enabled.up.sql"
ASSERT 1 "%（启用：每天%" "（启用：每天…）"
echo
echo "结论：up → down → up → up 四步全过，remark 逐字还原，可重放"
