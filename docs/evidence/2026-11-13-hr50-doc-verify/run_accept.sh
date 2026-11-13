#!/usr/bin/env bash
# ============================================================================
# HR-50 文档验收：照「docs/README.md + 后端二开指南 §十七」里写的命令真跑一遍
#   ① 145 建演练库（库名含 drill）→ 本机二进制经 ssh 隧道首启自动迁移
#   ② 文档里的三个探针 + 两条验收命令（hr_invariants.sh / menu_invariants.sh）
#   ③ 文档里的「实测基数」逐项对着活库复核
# 跑完：停实例 + 删 profile + DROP 库 + 关隧道
#
# 两条实测踩到的坑（写进这里，省下一位的时间）：
#   ① `NIK_DATABASE_PORT` **不在 nik-config 环境变量映射表**里，只设 env 时服务仍连
#      127.0.0.1:5432 → 「数据库连接失败」秒退。端口必须写进 profile 文件。
#   ② `NIK_PROFILE=<x>` 只叠加 `config/config-<x>.toml`，**不会**叠加 config-test.toml；
#      基础 config.toml 是 `[middleware.auth] enabled = false`，所以要看到「未登录 401」
#      必须在临时 profile 里显式打开 auth（TTL 也要在该 profile 重写，否则被默认值盖掉）。
#   ③ `schema_migrations.version` 是 VARCHAR(64)：max() 是字符串序（'403' < '91'），
#      比版本号要 `max(version::int)`，且只算 kind='versioned'。
# ============================================================================
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="$HERE"
DB=hr50_docs_drill
PROFILE=hr50
PORT=18050
TPORT=15450
BK=/opt/cloudpivot/backend
LOG="$OUT/00-server.log"
CFG="$BK/config/config-${PROFILE}.toml"

say() { printf '\n=== %s ===\n' "$*"; }
rc_of() { echo "RC=$?"; }

say "0. 读取 145 口令 + 建演练库（145 上，库名含 drill）"
DBPW="$(ssh -o ConnectTimeout=10 root@172.18.10.145 "grep '^NIK_DATABASE_PASSWORD' /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2-")"
[ -n "$DBPW" ] || { echo "FATAL: 读不到 145 的库口令"; exit 2; }
ssh root@172.18.10.145 "PGPASSWORD='$DBPW' psql -h 127.0.0.1 -U cloudpivot -d postgres -q -c \"DROP DATABASE IF EXISTS $DB\" -c \"CREATE DATABASE $DB OWNER cloudpivot\"" \
  && echo "created $DB on 145"

say "1. 本机 → 145 隧道 :$TPORT"
ssh -f -N -o ExitOnForwardFailure=yes -L ${TPORT}:127.0.0.1:5432 root@172.18.10.145 && echo "tunnel up"
TUNPID="$(pgrep -f "L ${TPORT}:127.0.0.1:5432" | head -1)"
for i in $(seq 1 20); do
  PGPASSWORD="$DBPW" psql -h 127.0.0.1 -p $TPORT -U cloudpivot -d "$DB" -tAc 'select 1' >/dev/null 2>&1 && { echo "tunnel ready after ${i} tries"; break; }
  sleep 1
done

say "2. 写临时 profile（端口 + 打开 auth）+ 首启（自动迁移）"
cat > "$CFG" <<EOF
# HR-50 文档验收临时 profile（跑完即删，不入库）
[database]
port = $TPORT

[middleware.auth]
enabled = true
access_token_expire_secs = 900
refresh_token_expire_secs = 86400
EOF
cat "$CFG"
rm -f "$LOG"
cd "$BK" || exit 2
NIK_PROFILE=$PROFILE \
NIK_SERVER_LISTEN=127.0.0.1:${PORT} \
NIK_DATABASE_HOST=127.0.0.1 \
NIK_DATABASE_USER=cloudpivot NIK_DATABASE_PASSWORD="$DBPW" NIK_DATABASE_NAME="$DB" \
NIK_MIDDLEWARE_AUTH_JWT_SECRET="hr50-docs-verify-$(openssl rand -hex 24)" \
CLOUDPIVOT_STORAGE_MASTER_KEY="$(openssl rand -hex 32)" \
X_CONFIG_SECRET_KEY="$(openssl rand -hex 16)" \
NIK_ADMIN_PASSWORD='Admin@123456' \
./target/debug/cloudpivot >"$LOG" 2>&1 &
SRV=$!

ok=0
for i in $(seq 1 150); do
  sleep 2
  code="$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:${PORT}/health" || true)"
  [ "$code" = "200" ] && { ok=1; break; }
  kill -0 "$SRV" 2>/dev/null || { echo "server exited early（见 00-server.log）"; break; }
done
echo "health ok=$ok after $((i*2))s"
grep -E "数据库迁移完成|无待应用迁移" "$LOG" | tail -3

say "3. 文档里的三个探针（auth 已打开）"
printf 'GET /health                     -> %s (期望 200)\n' "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:${PORT}/health")"
printf 'GET /api/business/hr/health     -> %s (期望 401，需登录)\n' "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:${PORT}/api/business/hr/health")"
printf 'GET /api/business/hr/nope       -> %s (期望 404，证明不是 SPA 兜底)\n' "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:${PORT}/api/business/hr/nope")"

URL="postgres://cloudpivot:${DBPW}@127.0.0.1:${TPORT}/${DB}"
cd /opt/cloudpivot || exit 2

say "4. 文档验收命令 ① hr_invariants.sh（期望 rc=0 / 12 行 viol=0）"
HR_DB_URL="$URL" bash scripts/verify/hr_invariants.sh; rc_of

say "5. 文档验收命令 ② menu_invariants.sh <库>（期望 8 条全绿）"
PGHOST=127.0.0.1 PGPORT=$TPORT PGUSER=cloudpivot PGPASSWORD="$DBPW" \
  bash scripts/verify/menu_invariants.sh "$DB"; rc_of

say "6. 文档「实测基数」对活库复核"
PGPASSWORD="$DBPW" psql -h 127.0.0.1 -p $TPORT -U cloudpivot -d "$DB" -X -q -At -F'|' -c "
select 'schema_migrations(总行数)', count(*)::text from schema_migrations
union all select '  versioned 条数', count(*)::text from schema_migrations where kind='versioned'
union all select '  seed 条数', count(*)::text from schema_migrations where kind<>'versioned'
union all select '  最大版本 V', max(version::int)::text from schema_migrations where kind='versioned'
union all select 'HR 菜单行(目录+页面+按钮)', count(*)::text from sys_menu
   where deleted_at is null and (path='/business/hr' or path like '/business/hr/%' or auth_code like 'business:hr:%')
union all select '  其中 目录(menu_type=1)', count(*)::text from sys_menu where deleted_at is null and menu_type=1 and auth_code is null and path='/business/hr'
union all select '  其中 页面(menu_type=2)', count(*)::text from sys_menu where deleted_at is null and menu_type=2 and path like '/business/hr%'
union all select '  其中 按钮(menu_type=3)', count(*)::text from sys_menu where deleted_at is null and menu_type=3 and auth_code like 'business:hr:%'
union all select 'HR 权限码(唯一 auth_code)', count(distinct auth_code)::text from sys_menu
   where deleted_at is null and auth_code like 'business:hr:%'
union all select '  其中 *:page', count(distinct auth_code)::text from sys_menu
   where deleted_at is null and auth_code like 'business:hr:%:page'
union all select '  其中 *:list', count(distinct auth_code)::text from sys_menu
   where deleted_at is null and auth_code like 'business:hr:%:list'
union all select 'HR 薪资项', count(*)::text from biz_salary_item where deleted_at is null
union all select '  其中带公式', count(*)::text from biz_salary_item where deleted_at is null and formula is not null
union all select 'HR 审批流程', count(*)::text from biz_flow_def
   where deleted_at is null and flow_code in ('hr_leave','hr_salary','hr_salary_adjust','hr_employee_resign')
union all select 'HR 表(应 10 张)', count(*)::text from pg_tables where schemaname='public'
   and tablename in ('biz_employee','biz_employee_contract','biz_employee_salary','biz_attendance_period',
      'biz_attendance_record','biz_leave_request','biz_salary_item','biz_salary_sheet',
      'biz_salary_detail','biz_salary_detail_item')
union all select 'HR_MANAGER 授权行数', count(*)::text from sys_role_menu rm join sys_role r on r.id=rm.role_id
   where r.role_code='HR_MANAGER'
" | tee "$OUT/01-db-stats.txt"

say "7. 清场：停实例 + 删 profile + DROP 库 + 关隧道"
kill "$SRV" 2>/dev/null; sleep 2; kill -9 "$SRV" 2>/dev/null
[ -n "${TUNPID:-}" ] && kill "$TUNPID" 2>/dev/null
rm -f "$CFG" && echo "removed $CFG"
ssh root@172.18.10.145 "PGPASSWORD='$DBPW' psql -h 127.0.0.1 -U cloudpivot -d postgres -q -c \"DROP DATABASE IF EXISTS $DB\"" && echo "dropped $DB"
printf '145 残留 hr50%% 库 = '
ssh root@172.18.10.145 "PGPASSWORD='$DBPW' psql -h 127.0.0.1 -U cloudpivot -d postgres -tAc \"select count(*) from pg_database where datname like 'hr50%'\""
printf '本机 :%s 监听数 = ' "$PORT"; ss -lnt 2>/dev/null | grep -c ":${PORT} " || true
echo "DONE"
