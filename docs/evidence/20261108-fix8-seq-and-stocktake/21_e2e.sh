#!/usr/bin/env bash
# ============================================================================
# FIX-8 145 取证 · 步骤 3：问题 2（盘点两步）+ 问题 1（取号器并发）真机 E2E
#
# 段 A（问题 2）：复用 scripts/verify/p13_stock_take_e2e.py —— 它就是
#   建单 → 填实盘 → 提交审批 → （未审批过账 409）→ 审批通过 → 过账 → 作废 的完整链，
#   每步都读三本账 + 凭证分录。本卡不改它，只把它指向隔离实例 + 拷贝库。
# 段 B（问题 1）：20_seq_e2e.py —— 10 个并发 HTTP 建单不重号 + 按类型独立递增 + 重号扫描。
#
# 跑法：ssh root@172.18.10.145 'bash /opt/cloudpivot/docs/evidence/20261108-fix8-seq-and-stocktake/21_e2e.sh'
# ============================================================================
set -uo pipefail
EV="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "$EV/00_env.sh"

if [ -z "${DBNAME:-}" ]; then
  echo "没有 /root/backup/.fix8_db：先跑 10_prepare_145.sh" >&2
  exit 1
fi
echo "实例=$BASE 库=$DBNAME"

RAW="$EV/raw"
mkdir -p "$RAW"

# ---------- 段 0：不变量基线（照抄 cloudpivot_dev 的既有状态，用来区分「本卡弄红的」） ----------
echo
echo "===== 段 0：不变量基线（拷贝库现状，任何改动之前） ====="
NIK_DATABASE_USER=cloudpivot NIK_DATABASE_PASSWORD="$NIK_DATABASE_PASSWORD" \
NIK_DATABASE_NAME="$DBNAME" NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432 \
  python3 /opt/cloudpivot/scripts/verify/erp_invariants.py > "$RAW/00_invariants_baseline.txt" 2>&1
echo "baseline exit=$? （$(grep -c '❌' "$RAW/00_invariants_baseline.txt" 2>/dev/null || echo 0) 处 ❌）"
grep -E '^结果|FAIL|❌' "$RAW/00_invariants_baseline.txt" | tail -5

# ---------- 段 A：问题 2 盘点两步 ----------
echo
echo "===== 段 A：问题 2 盘点「审批 → 待过账 → 过账 → 库存变动」 ====="
# 前置：审批节点是 ROLE（WAREHOUSE_MANAGER）——**角色里每个成员都会各拿一个待办**，
# 拷贝库里除了本卡造的 p13_keeper 还留着演练账号（演练审批人），它不点就永远 PENDING，
# 过账必然 409「审批尚未通过」。这是拷贝库的既有数据，不是代码问题：
# 正式环境一个角色多人 = 会签，本来就该所有人都批。这里把成员收敛成只有 p13_keeper。
psqlc -q -c "DELETE FROM sys_user_role
              WHERE role_id = '20000000-0000-4000-8000-0000000f0004'::uuid
                AND user_id NOT IN (SELECT id FROM sys_user WHERE username = 'p13_keeper')" \
  > /dev/null 2>&1 || true
echo "    角色 WAREHOUSE_MANAGER 现有成员：$(sq "SELECT count(*) FROM sys_user_role WHERE role_id = '20000000-0000-4000-8000-0000000f0004'::uuid")"
P13_BASE="$BASE" \
P13_DB_HOST=127.0.0.1 P13_DB_PORT=5432 P13_DB_USER=cloudpivot \
P13_DB_PASSWORD="$NIK_DATABASE_PASSWORD" P13_DB_NAME="$DBNAME" \
P13_ADMIN_PASS="$ADMIN_PASS" \
  python3 /opt/cloudpivot/scripts/verify/p13_stock_take_e2e.py > "$RAW/10_stocktake_two_step.txt" 2>&1
A_RC=$?
tail -12 "$RAW/10_stocktake_two_step.txt"
echo "段 A exit=$A_RC"

# ---------- 段 B：问题 1 取号器并发 ----------
echo
echo "===== 段 B：问题 1 取号器 10 并发不重号 + 按单据类型独立 ====="
F8_BASE="$BASE" \
F8_DB_HOST=127.0.0.1 F8_DB_PORT=5432 F8_DB_USER=cloudpivot \
F8_DB_PASSWORD="$NIK_DATABASE_PASSWORD" F8_DB_NAME="$DBNAME" \
F8_ADMIN_PASS="$ADMIN_PASS" \
  python3 "$EV/20_seq_e2e.py" > "$RAW/20_seq_concurrency.txt" 2>&1
B_RC=$?
tail -20 "$RAW/20_seq_concurrency.txt"
echo "段 B exit=$B_RC"

# ---------- 收尾：8033 服役实例必须还是健康的 ----------
echo
echo "===== 收尾：服役实例 8033 ====="
echo "8033 health=$(curl -s -m 3 -o /dev/null -w '%{http_code}' http://127.0.0.1:8033/health)"
echo "本实例 8048 health=$(curl -s -m 3 -o /dev/null -w '%{http_code}' http://127.0.0.1:8048/health)"

echo
echo "段 A exit=$A_RC / 段 B exit=$B_RC"
[ "$A_RC" = "0" ] && [ "$B_RC" = "0" ] && echo "RESULT: ALL PASS" || echo "RESULT: FAILED"
