#!/usr/bin/env bash
# =============================================================================
# P0-FIX-1：在生产实例上把 4 个 ERP 提醒任务启用（走项目自己的接口，不直接改库）
#
#   ① 登录取 token（admin / 首次口令见 AGENTS.md）
#   ② 逐条 PATCH /api/support/job/{id}/status {status:1}   ← 启停接口（会触发调度器 reload）
#   ③ 逐条 POST  /api/support/job/{id}/trigger             ← 「立即执行」，落 sys_job_log
#   ④ 打印改前 / 改后 + sys_job_log（用 psql 直查做交叉验证）
#
# 只读体检请用 p0fix1_prod_probe.sh。本脚本会写库（status 与执行日志）。
# 用法：bash p0fix1_prod_enable.sh [--dry-run]
# =============================================================================
set -uo pipefail

BASE="http://127.0.0.1:8023"
ADMIN_USER="${P0FIX1_ADMIN_USER:-admin}"
DRY=0
[ "${1:-}" = "--dry-run" ] && DRY=1

set -a; . /etc/cloudpivot/cloudpivot.env 2>/dev/null; set +a
# 口令依次取 P0FIX1_ADMIN_PASS / 部署 env 里的 NIK_ADMIN_PASSWORD（都不回显）
ADMIN_PASS="${P0FIX1_ADMIN_PASS:-${NIK_ADMIN_PASSWORD:-Admin@123456}}"
export PGPASSWORD="${NIK_DATABASE_PASSWORD:-}"
Q() { psql -h 127.0.0.1 -U cloudpivot -d cloudpivot -X -q -tAF' | ' -c "$1"; }

echo "== ① 登录（$ADMIN_USER）"
LOGIN=$(curl -s -X POST "$BASE/api/system/auth/login" \
        -H 'Content-Type: application/json' \
        -d "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_PASS\"}")
TOKEN=$(echo "$LOGIN" | jq -r '.data.access_token // empty')
if [ -z "$TOKEN" ]; then
  echo "❌ 登录失败：$(echo "$LOGIN" | head -c 300)"
  exit 1
fi
echo "   ✅ token 长度 ${#TOKEN}"

echo
echo "== ② 启用前 4 行状态"
Q "select job_name, status::text from sys_job where deleted_at is null and job_name like 'erp\_%\_scan' order by job_name"
BEFORE=$(Q "select count(*) from sys_job_log")

if [ "$DRY" = "1" ]; then
  echo "（--dry-run：不写库，到此为止）"; exit 0
fi

echo
echo "== ③ PATCH status=1（走 /api/support/job/{id}/status）"
while IFS='|' read -r name jid; do
  [ -z "$name" ] && continue
  code=$(curl -s -o /tmp/p0fix1_patch.json -w '%{http_code}' -X PATCH \
         "$BASE/api/support/job/$jid/status" \
         -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
         -d '{"status":1}')
  echo "   $name -> HTTP $code $(head -c 160 /tmp/p0fix1_patch.json)"
done < <(Q "select job_name || '|' || id from sys_job where deleted_at is null and job_name like 'erp\_%\_scan' order by job_name")

echo
echo "== ④ 启用后 4 行状态（psql 交叉验证）"
Q "select job_name, status::text, coalesce(to_char(next_run_at at time zone 'Asia/Shanghai','MM-DD HH24:MI'),'-')
     from sys_job where deleted_at is null and job_name like 'erp\_%\_scan' order by job_name"

echo
echo "== ⑤ POST trigger（立即执行一次）"
while IFS='|' read -r name jid; do
  [ -z "$name" ] && continue
  code=$(curl -s -o /tmp/p0fix1_trig.json -w '%{http_code}' -X POST \
         "$BASE/api/support/job/$jid/trigger" \
         -H "Authorization: Bearer $TOKEN")
  echo "   $name -> HTTP $code $(head -c 200 /tmp/p0fix1_trig.json)"
done < <(Q "select job_name || '|' || id from sys_job where deleted_at is null and job_name like 'erp\_%\_scan' order by job_name")

AFTER=$(Q "select count(*) from sys_job_log")
echo
echo "== ⑥ sys_job_log：$BEFORE -> $AFTER 条"
Q "select job_name, status::text, to_char(started_at at time zone 'Asia/Shanghai','MM-DD HH24:MI:SS'),
          duration_ms::text, coalesce(message,'') from sys_job_log order by started_at desc limit 8"
echo
echo "== ⑦ 4 行 last_run_at / next_run_at"
Q "select job_name, coalesce(to_char(last_run_at at time zone 'Asia/Shanghai','MM-DD HH24:MI:SS'),'-'),
          coalesce(to_char(next_run_at at time zone 'Asia/Shanghai','MM-DD HH24:MI:SS'),'-')
     from sys_job where deleted_at is null and job_name like 'erp\_%\_scan' order by job_name"
echo
echo "== ⑧ 通知（本库当前无到期单据，预期 0 条）"
Q "select count(*) from sys_notice where biz_type in ('payable_due','po_overdue','contract_expire','low_stock')"
