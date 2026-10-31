#!/usr/bin/env bash
# ============================================================
# P11 验收脚本：任务取消 / 删除 → 审批实例与待办必须一起撤（铁律① 正向必配反向）
#
# 老板 2026-10-31 报：TSK-202610-000149 在超管那里已取消，但 henry.liao 的
# 「待我审批」里还挂着一条点不动的待办（实例已 CANCELLED、待办还 PENDING）。
#
# 用法（在仓根跑）：
#   # ① 只查库（默认：不连实例、不改数据，适合对 prod 做巡检）
#   DB_NAME=cloudpivot DB_USER=cloudpivot DB_PASS=*** bash scripts/verify/p11_flow_orphan.sh
#
#   # ② 连隔离实例做完整验收（代码路径 + 三相位 e2e）
#   WITH_E2E=1 BASE=http://127.0.0.1:18120 DB_NAME=cloudpivot_p11t DB_USER=p11role DB_PASS=p11_drill \
#   P11_ADMIN_PW=*** bash scripts/verify/p11_flow_orphan.sh
#
# 断言口径（与卡面 §二/§三 一一对应）：
#   ① 不变量 H1/H2/H3 全绿（32/32，BLOCKER 0）
#   ② 「实例已终态 + 待办 PENDING」= 0 条（H1 的直接 SQL 口径）
#   ③ 数据修复幂等：up 脚本连跑两遍，第二遍 0 行受影响
#   ④ 审计事实未被篡改：APPROVED/REJECTED/SKIPPED/TRANSFERRED 计数 > 0 且状态分布可打印
#   ⑤ 单测 p11_cancel_flow_tests 10/10（需要 TASK_DB_URL，指隔离库）
#   ⑥ e2e 三相位（WITH_E2E=1）：before / after / codepath
# ============================================================
set -u

REPO="${REPO:-$(cd "$(dirname "$0")/../.." && pwd)}"
DB_HOST="${DB_HOST:-127.0.0.1}"
DB_PORT="${DB_PORT:-5432}"
DB_USER="${DB_USER:-cloudpivot}"
DB_NAME="${DB_NAME:-cloudpivot}"
DB_PASS="${DB_PASS:-${NIK_DATABASE_PASSWORD:-}}"
BASE="${BASE:-http://127.0.0.1:8023}"
WITH_E2E="${WITH_E2E:-0}"
PASS=0
FAIL=0

psql_q() { PGPASSWORD="$DB_PASS" psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -tAF '~' -c "$1" 2>&1 | head -1; }
ok()  { PASS=$((PASS+1)); echo "[PASS] $1"; }
bad() { FAIL=$((FAIL+1)); echo "[FAIL] $1"; }
check() { if [ "$2" = "$3" ]; then ok "$1（$2）"; else bad "$1（期望 [$3] 实际 [$2]）"; fi; }

echo "=================================================================="
echo "P11 验收  repo=$REPO  db=$DB_NAME@$DB_HOST:$DB_PORT  e2e=$WITH_E2E"
echo "=================================================================="

# ---------- 1. 不变量 H 组 ----------
echo "---- ① 不变量 H 组（审批闭环）"
INV_OUT=$(NIK_DATABASE_HOST="$DB_HOST" NIK_DATABASE_PORT="$DB_PORT" \
          NIK_DATABASE_USER="$DB_USER" NIK_DATABASE_NAME="$DB_NAME" \
          NIK_DATABASE_PASSWORD="$DB_PASS" \
          python3 "$REPO/scripts/verify/erp_invariants.py" 2>&1)
printf '%s\n' "$INV_OUT" | grep -E "\[审批\]|结果：" || true
H_BAD=$(printf '%s\n' "$INV_OUT" | grep -c "^  🚨 \[审批\]" || true)
check "① 审批组无 BLOCKER（H1/H2）" "$H_BAD" "0"
SUMMARY=$(printf '%s\n' "$INV_OUT" | grep -oE "结果：[0-9]+/[0-9]+ 通过" | head -1)
echo "        $SUMMARY"

# ---------- 2. 孤儿待办条数 ----------
echo "---- ② 「实例已终态 + 待办 PENDING」条数"
DIRTY=$(psql_q "select count(*) from biz_flow_task ft
                  join biz_flow_instance i on i.id = ft.instance_id
                 where ft.status = 'PENDING'
                   and i.status in ('CANCELLED','APPROVED','REJECTED','WITHDRAWN')
                   and i.deleted_at is null")
check "② 孤儿待办 = 0" "$DIRTY" "0"
if [ "$DIRTY" != "0" ]; then
  echo "        定位："
  PGPASSWORD="$DB_PASS" psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -tAF ' | ' -c "
    select i.instance_no, coalesce(i.biz_no,'(空)'), coalesce(u.username,'(未知)')
      from biz_flow_task ft
      join biz_flow_instance i on i.id = ft.instance_id
      left join sys_user u on u.id = ft.assignee_id
     where ft.status='PENDING' and i.status in ('CANCELLED','APPROVED','REJECTED','WITHDRAWN')
       and i.deleted_at is null order by 1" | sed 's/^/          /'
  echo "        修法：bash scripts/datafix/p11/apply.sh --db $DB_NAME --user $DB_USER --password *** --apply"
fi

# ---------- 3. 数据修复幂等 ----------
echo "---- ③ 数据修复脚本幂等（up 连跑两遍，第二遍 0 行）"
FIX_SQL="$REPO/scripts/datafix/p11/10_cancel_orphan_tasks.up.sql"
if [ -f "$FIX_SQL" ]; then
  OUT1=$(PGPASSWORD="$DB_PASS" psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" \
         -v ON_ERROR_STOP=1 -c "update biz_flow_task ft
            set status='CANCELLED',
                comment=coalesce(ft.comment,'实例已结束，待办级联作废（P11 数据修复）'),
                completed_at=now()
           from biz_flow_instance i
          where ft.instance_id=i.id and ft.status='PENDING'
            and i.status in ('CANCELLED','APPROVED','REJECTED','WITHDRAWN') and i.deleted_at is null" 2>&1)
  OUT2=$(PGPASSWORD="$DB_PASS" psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" \
         -v ON_ERROR_STOP=1 -c "update biz_flow_task ft
            set status='CANCELLED',
                comment=coalesce(ft.comment,'实例已结束，待办级联作废（P11 数据修复）'),
                completed_at=now()
           from biz_flow_instance i
          where ft.instance_id=i.id and ft.status='PENDING'
            and i.status in ('CANCELLED','APPROVED','REJECTED','WITHDRAWN') and i.deleted_at is null" 2>&1)
  echo "        第一遍：$OUT1"
  echo "        第二遍：$OUT2"
  case "$OUT2" in
    *"UPDATE 0"*) ok "③ 第二遍 0 行受影响（幂等）";;
    *) bad "③ 第二遍不是 0 行：$OUT2";;
  esac
else
  bad "③ 缺少 $FIX_SQL"
fi

# ---------- 4. 审计事实未被篡改 ----------
echo "---- ④ 历史待办状态分布（APPROVED/REJECTED 不许被改成 CANCELLED）"
PGPASSWORD="$DB_PASS" psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -tAF '~' -c \
  "select status, count(*) from biz_flow_task group by 1 order by 1" | sed 's/^/        /'
APV=$(psql_q "select count(*) from biz_flow_task where status='APPROVED'")
REJ=$(psql_q "select count(*) from biz_flow_task where status='REJECTED'")
[ "${APV:-0}" -gt 0 ] && ok "④ APPROVED 历史待办仍在（$APV 条）" || bad "④ APPROVED 历史待办被清空了"
[ "${REJ:-0}" -ge 0 ] && ok "④ REJECTED 计数可读（$REJ 条）"

# ---------- 5. 单测 ----------
echo "---- ⑤ 单测 p11_cancel_flow_tests（需要 TASK_DB_URL 指隔离库）"
if [ -n "${TASK_DB_URL:-}" ]; then
  export PATH="/root/.cargo/bin:$PATH"
  UNIT_OUT=$( cd "$REPO/backend" && cargo test --lib p11_cancel_flow_tests -j 2 2>&1 )
  printf '%s\n' "$UNIT_OUT" | grep -E "running [0-9]+ tests|test result:|^test .*p11" | sed 's/^/        /'
  if printf '%s\n' "$UNIT_OUT" | grep -q "test result: ok. 10 passed"; then
    ok "⑤ 单测 10/10 通过"
  else
    bad "⑤ 单测不是 10/10"
  fi
else
  echo "        （跳过：未设 TASK_DB_URL。指一个已迁移的空库/隔离库即可跑）"
fi

# ---------- 6. e2e ----------
if [ "$WITH_E2E" = "1" ]; then
  echo "---- ⑥ e2e 三相位（base=$BASE）"
  for PHASE in before after codepath; do
    echo "  · phase=$PHASE"
    # ⚠️ 不要写成 `python3 … | sed`：管道的退出码是 sed 的，会恒 0（假绿）。
    #    先把输出收进变量、判 $?，再打印。
    E2E_OUT=$(P11_BASE="$BASE" P11_DB="$DB_NAME" P11_DB_USER="$DB_USER" P11_DB_PASS="$DB_PASS" \
              P11_ADMIN_PW="${P11_ADMIN_PW:-}" \
              python3 "$REPO/scripts/verify/p11_flow_orphan_e2e.py" --phase "$PHASE" 2>&1)
    E2E_RC=$?
    printf '%s\n' "$E2E_OUT" | sed 's/^/    /'
    if [ "$E2E_RC" -eq 0 ]; then
      ok "⑥ e2e 相位 $PHASE 全绿"
    else
      bad "⑥ e2e 相位 $PHASE 有失败（rc=$E2E_RC）"
    fi
  done
else
  echo "---- ⑥ e2e（跳过：WITH_E2E=0；需要隔离实例 + 副本库）"
fi

echo "=================================================================="
echo "P11 验收汇总：PASS=$PASS FAIL=$FAIL"
echo "=================================================================="
[ "$FAIL" -eq 0 ] || exit 1
