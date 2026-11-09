#!/usr/bin/env bash
# 汇总本卡所有验证输出到 /tmp/fix1_evidence/（供 docs/evidence/e2e06-fix1/ 收录）
set -uo pipefail
ENV=/etc/cloudpivot/cloudpivot-dev.env
set -a; . "$ENV"; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
OUT=/tmp/fix1_evidence
mkdir -p "$OUT"
cd /opt/cloudpivot

echo "== graph 三方一致"
psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -At -f /tmp/fix1_graph_consistency.sql | tail -1 > /tmp/graph_dump.json
python3 /tmp/fix1_graph_check.py /tmp/graph_dump.json > "$OUT/graph_consistency.out" 2>&1

echo "== EMPLOYEE-only 自助链路"
bash /tmp/fix1_employee_upload_check.sh > "$OUT/employee_only.out" 2>&1

echo "== 部署后检查（坏口令 401 / 迁移记录）"
bash /tmp/fix1_post_deploy_check.sh > "$OUT/post_deploy.out" 2>&1

echo "== 菜单不变量"
(
  export PGHOST=127.0.0.1 PGUSER="$NIK_DATABASE_USER" PGDATABASE="$NIK_DATABASE_NAME"
  bash scripts/verify/menu_invariants.sh
) > "$OUT/menu_invariants.out" 2>&1

echo "== ERP 不变量 权限组"
python3 scripts/verify/erp_invariants.py --group 权限 > "$OUT/erp_invariants_authz.out" 2>&1

ls -l "$OUT"
tail -2 "$OUT/graph_consistency.out"
tail -1 "$OUT/employee_only.out"
grep -c '^PASS' /tmp/e2e06/asserts.log
