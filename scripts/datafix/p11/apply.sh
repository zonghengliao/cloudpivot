#!/bin/bash
# P11 数据修复执行器：备份 → 演练 → 应用 → 幂等复核 → 留痕
#
#   bash scripts/datafix/p11/apply.sh --db cloudpivot_p11 --user p11role --password xxx --backup-tag drill
#   bash scripts/datafix/p11/apply.sh --env /etc/cloudpivot/cloudpivot.env --backup-tag prod-20261031
#
# 默认只做 dry-run（打印将被改的行），加 --apply 才真的 update。
# 备份目录：/root/datafix-backup-p11-<tag>/（biz_flow_task + biz_flow_instance 全量 CSV + MANIFEST）
set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
HOST=127.0.0.1
PORT=5432
DB=""
USER=""
PASS=""
ENVFILE=""
TAG="$(date +%Y%m%d-%H%M%S)"
APPLY=0
SKIP_DOWN=0

while [ $# -gt 0 ]; do
  case "$1" in
    --db) DB="$2"; shift 2 ;;
    --host) HOST="$2"; shift 2 ;;
    --port) PORT="$2"; shift 2 ;;
    --user) USER="$2"; shift 2 ;;
    --password) PASS="$2"; shift 2 ;;
    --env) ENVFILE="$2"; shift 2 ;;
    --backup-tag) TAG="$2"; shift 2 ;;
    --apply) APPLY=1; shift ;;
    --skip-down) SKIP_DOWN=1; shift ;;
    *) echo "未知参数：$1"; exit 2 ;;
  esac
done

if [ -n "$ENVFILE" ]; then
  set -a; . "$ENVFILE"; set +a
  DB="${DB:-$NIK_DATABASE_NAME}"
  USER="${USER:-$NIK_DATABASE_USER}"
  PASS="${PASS:-$NIK_DATABASE_PASSWORD}"
  HOST="${NIK_DATABASE_HOST:-$HOST}"
  PORT="${NIK_DATABASE_PORT:-$PORT}"
fi
[ -n "$DB" ] || { echo "缺少 --db（或用 --env 指定 env 文件）"; exit 2; }
export PGPASSWORD="$PASS"

P() { psql -h "$HOST" -p "$PORT" -U "$USER" -d "$DB" -tAF'|' -c "$1"; }
SQL() { psql -h "$HOST" -p "$PORT" -U "$USER" -d "$DB" -v ON_ERROR_STOP=1 -q -f "$1"; }

DIRTY_SQL="select count(*) from biz_flow_task ft join biz_flow_instance i on i.id = ft.instance_id
            where ft.status='PENDING' and i.status in ('CANCELLED','APPROVED','REJECTED','WITHDRAWN')
              and i.deleted_at is null"

echo "=================================================================="
echo "P11 数据修复  db=$DB host=$HOST  tag=$TAG  apply=$APPLY"
echo "=================================================================="

BEFORE=$(P "$DIRTY_SQL")
echo "① 修复前「实例已终态 + 待办 PENDING」：$BEFORE 条"
P "select i.instance_no, coalesce(i.biz_no,'(空)'), u.username
     from biz_flow_task ft
     join biz_flow_instance i on i.id = ft.instance_id
     left join sys_user u on u.id = ft.assignee_id
    where ft.status='PENDING' and i.status in ('CANCELLED','APPROVED','REJECTED','WITHDRAWN')
      and i.deleted_at is null
    order by 1,3"

if [ "$APPLY" -eq 0 ]; then
  echo "② dry-run：不改任何数据。要真改加 --apply"
  exit 0
fi

BK="/root/datafix-backup-p11-$TAG"
mkdir -p "$BK"
echo "② 备份 → $BK"
psql -h "$HOST" -p "$PORT" -U "$USER" -d "$DB" -c "\copy (select * from biz_flow_task) to '$BK/biz_flow_task.csv' with csv header"
psql -h "$HOST" -p "$PORT" -U "$USER" -d "$DB" -c "\copy (select * from biz_flow_instance) to '$BK/biz_flow_instance.csv' with csv header"
{
  echo "database  : $DB on $HOST:$PORT"
  echo "tag       : $TAG"
  echo "created_at: $(date -Is)"
  echo "before    : $BEFORE dirty rows"
  echo "files     : biz_flow_task.csv biz_flow_instance.csv"
  echo "restore   : 见 docs/datafix-20261031-p11.md 的「回滚」一节"
} > "$BK/MANIFEST.txt"
wc -l "$BK"/biz_flow_task.csv "$BK"/biz_flow_instance.csv | tail -3

echo "③ 应用 up"
SQL "$HERE/10_cancel_orphan_tasks.up.sql"

AFTER=$(P "$DIRTY_SQL")
echo "④ 修复后：$AFTER 条（期望 0）"

echo "⑤ 幂等复核：再跑一遍 up，期望 0 行受影响"
psql -h "$HOST" -p "$PORT" -U "$USER" -d "$DB" -v ON_ERROR_STOP=1 -q \
  -c "update biz_flow_task ft set status='CANCELLED', comment=coalesce(ft.comment,'实例已结束，待办级联作废（P11 数据修复）'), completed_at=now()
        from biz_flow_instance i
       where ft.instance_id = i.id and ft.status='PENDING'
         and i.status in ('CANCELLED','APPROVED','REJECTED','WITHDRAWN') and i.deleted_at is null" \
  | sed 's/^/    /'

echo "⑥ 审计事实未被篡改：APPROVED/REJECTED/SKIPPED/TRANSFERRED 计数"
P "select status, count(*) from biz_flow_task where status <> 'CANCELLED' group by 1 order by 1"

if [ "$SKIP_DOWN" -eq 0 ] && [ "${DRILL:-0}" = "1" ]; then
  echo "⑦ 演练库可逆性：down → 脏行重现 → up → 再清零"
  SQL "$HERE/10_cancel_orphan_tasks.down.sql"
  echo "    down 后：$(P "$DIRTY_SQL") 条（期望 = 修复前的 $BEFORE）"
  SQL "$HERE/10_cancel_orphan_tasks.up.sql"
  echo "    up  后：$(P "$DIRTY_SQL") 条（期望 0）"
fi

echo "=================================================================="
echo "P11 数据修复完成：$BEFORE → $AFTER      备份：$BK"
echo "=================================================================="
[ "$AFTER" = "0" ] || exit 1
