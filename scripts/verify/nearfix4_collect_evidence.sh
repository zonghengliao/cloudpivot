#!/usr/bin/env bash
# 采集 NEAR-FIX-4 证据（在 144 上跑，输出到 /root/evidence-nearfix4/）
set -uo pipefail
OUT=/root/evidence-nearfix4
mkdir -p "$OUT"
cd /opt/cloudpivot || exit 1

echo "采集：alert_check self-test"
bash scripts/ops/alert_check.sh --self-test >"$OUT/05_alert_self_test_144.txt" 2>&1
echo "  rc=$?"

echo "采集：invariant_rerun self-test"
python3 scripts/ops/invariant_rerun.py --self-test >"$OUT/04_rerun_self_test_144.txt" 2>&1
echo "  rc=$?"

echo "采集：干净库演练（四个场景）"
bash /root/drill_invariant_stale.sh >"$OUT/01_drill_stale_conclusion_144.txt" 2>&1
echo "  rc=$?"

echo "采集：/metrics 分支 + psql 不可用"
bash /root/drill_metrics_branch.sh >"$OUT/02_drill_metrics_branch_144.txt" 2>&1
echo "  rc=$?"

echo "采集：deploy.sh [7/9] 步逐字命令（演练库）"
bash /root/drill_step7_exact.sh >"$OUT/03_step7_exact_144.txt" 2>&1
echo "  rc=$?"

echo "采集：生产 --force 重跑（HTTP 401 → 脚本兜底）"
python3 scripts/ops/invariant_rerun.py --force >"$OUT/06_prod_rerun_144.txt" 2>&1
echo "  rc=$?"

echo "采集：生产现状（最后 3 行结论 + 迁移时间 + 归一对比）"
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
{
  echo "== sys_invariant_result 最近 3 行 =="
  psql -X -q -A -t -h "$NIK_DATABASE_HOST" -p "${NIK_DATABASE_PORT:-5432}" -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" \
    -c "select to_char(run_at,'YYYY-MM-DD HH24:MI:SS') || ' | ' || trigger_type || ' | ' || status || ' | total=' || total || ' passed=' || passed || ' blockers=' || blockers from sys_invariant_result order by run_at desc limit 3"
  echo "== schema_migrations.applied_at 单位分布（毫秒行是历史脏数据）=="
  psql -X -q -A -t -h "$NIK_DATABASE_HOST" -p "${NIK_DATABASE_PORT:-5432}" -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" \
    -c "select 'ms 行 ' || count(*) filter (where applied_at > 100000000000) || ' / s 行 ' || count(*) filter (where applied_at <= 100000000000) from schema_migrations" \
    -c "select 'naive max(applied_at)=' || max(applied_at) || ' → to_timestamp=' || to_timestamp(max(applied_at)) || '（毫秒行把时间顶到 58826 年）' from schema_migrations" \
    -c "select '归一后 max=' || to_char(to_timestamp(max(case when applied_at > 100000000000 then applied_at/1000 else applied_at end)),'YYYY-MM-DD HH24:MI:SS') from schema_migrations where success"
} >"$OUT/07_prod_state_144.txt" 2>&1

echo "采集：部署脚本 [7/9] 步在真实流程里的输出（--skip-migrate --skip-backend --skip-frontend）"
bash scripts/deploy/deploy.sh --skip-migrate --skip-backend --skip-frontend >"$OUT/08_deploy_wiring_144.txt" 2>&1
echo "  rc=$?"

ls -l "$OUT"
