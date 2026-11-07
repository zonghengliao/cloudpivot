#!/usr/bin/env bash
# 佐证：全量套件里唯一那条 FAILED（p16_graph_snapshot_selfcheck_is_clean）不是本卡造成
# 结论：漂移的流程是 prt_approve（V0332 采购退货，**未跟踪文件**，属他人进行中的卡）；
#       在「不含 V0332 的干净树」建的库上，同一条测试 1 passed
set -uo pipefail
cd /opt/cloudpivot

echo "===== ① 全量套件里失败的那条 ====="
grep -E "^test .*p16_graph_snapshot_selfcheck_is_clean" /root/seedemp_cargo_test.log || true

echo
echo "===== ② 漂移事实（审计行，selfcheck 自己写的）====="
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_test_seedemp -At -F '|' -c \
  "select created_at, operation, target_id, detail::text from audit_logs
    where operation = 'FLOW_GRAPH_HASH_DRIFT' order by created_at desc limit 3"

echo
echo "===== ③ prt_approve 出自哪个文件 / 是否被本卡提交 ====="
grep -rln "prt_approve" backend/migrations/ || echo "(当前工作区无)"
echo "--- git 是否跟踪该文件 ---"
git status --short backend/migrations/business/V0332__purchase_return.up.sql || true
echo "--- 本卡提交 3022a2f 的迁移目录里有没有 prt_approve ---"
git grep -l "prt_approve" 3022a2f -- backend/migrations/ || echo "本卡提交内 0 处（V0332 未被任何提交跟踪）"

echo
echo "===== ④ 干净树（不含 V0332）建库后，同一条测试复跑 ====="
grep -E "就绪|迁移最新版本|角色|DEPT_DEMO" /root/make_clean_db.log | tail -8
cd /opt/cloudpivot/backend && U='postgres://alun:alun@127.0.0.1:5432/cloudpivot_test_clean'
ERP_DB_URL="$U" SYS_DB_URL="$U" TASK_DB_URL="$U" P0FIX_DB_URL="$U" \
  ./target/debug/deps/cloudpivot-12f8f1e6c8ad4f29 --test-threads=1 p16_graph_snapshot 2>&1 | tail -6
