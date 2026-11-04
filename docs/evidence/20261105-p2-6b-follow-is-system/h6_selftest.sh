#!/usr/bin/env bash
# H6 静态检查的双向自测（正向夹具 / 负向夹具）
#
# 为什么要有它：H6 是「扫 migrations 的 INSERT 列清单」，它自己是检查器，
# 必须证明「真有漏列时会红、写全了会绿」，否则它可能只是永远绿的空壳。
#
# 跑法（在仓库根目录）：
#   bash docs/evidence/20261105-p2-6b-follow-is-system/h6_selftest.sh
#
# 三个夹具目录：
#   real  —— 直接扫仓库真迁移（期望：绿；并列出 4 处历史漏列已被 V0220 / V0233 补偿）
#   bad   —— 一条 V0240 种子漏写 is_system，且无人补偿（期望：BLOCKER / ok=false）
#   ok    —— 同样的种子但列里写了 is_system，外加一条 <V0193 的老迁移漏写（期望：绿 —— 老迁移不算错）
set -uo pipefail

REPO="$(cd "$(dirname "$0")/../../.." && pwd)"
CHECK="$REPO/scripts/verify/erp_invariants.py"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

cat > "$TMP/driver.py" <<'PY'
import importlib.util, json, os, sys
spec = importlib.util.spec_from_file_location("erp_inv", sys.argv[1])
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
m._check_flow_seed_is_system()
r = m.RESULT[-1]
print("ok=%s level=%s\n%s" % (r["ok"], r["level"], r["detail"]))
PY

mkdir -p "$TMP/bad/business" "$TMP/ok/business" "$TMP/implicit/business"
cat > "$TMP/bad/business/V0240__bad_seed.up.sql" <<'SQL'
-- 负向夹具：种子 INSERT 漏写 is_system，且没有任何后续迁移补偿
INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve, owner_dept_id)
VALUES ('11111111-1111-1111-1111-111111111111', 'fake_seed_flow', '假种子流程',
        'FAKE', '测试', null, 'PUBLISHED', null, null, false, false, null);
SQL
cat > "$TMP/ok/business/V0240__bad_seed.up.sql" <<'SQL'
-- 正向夹具：同一条种子，列清单里显式写了 is_system
INSERT INTO biz_flow_def (
    id, flow_code, flow_name, biz_type, category, description, status,
    current_version_id, current_version_no, is_default, allow_auto_approve, owner_dept_id, is_system)
VALUES ('11111111-1111-1111-1111-111111111111', 'fake_seed_flow', '假种子流程',
        'FAKE', '测试', null, 'PUBLISHED', null, null, false, false, null, TRUE);
SQL
cat > "$TMP/ok/business/V0100__old_seed.up.sql" <<'SQL'
-- 旧版本（<V0193，is_system 列尚不存在）：漏写不算错
INSERT INTO biz_flow_def (id, flow_code, flow_name)
VALUES ('22222222-2222-2222-2222-222222222222', 'very_old_flow', '上古流程');
SQL
cat > "$TMP/implicit/business/V0241__implicit_seed.up.sql" <<'SQL'
-- 负向夹具：隐式列清单（没有括号列名）—— 静态无法确认写了 is_system
INSERT INTO biz_flow_def
SELECT gen_random_uuid(), 'implicit_flow', '隐式列流程', 'FAKE', '测试', null, 'PUBLISHED';
SQL

for case in real bad ok implicit; do
  case "$case" in
    real) root="" ;;
    *)    root="$TMP/$case" ;;
  esac
  if [ -z "$root" ]; then
    printf '\n########## 夹具 %s（仓库真迁移 %s）##########\n' "$case" "$REPO/backend/migrations"
    out="$(python3 "$TMP/driver.py" "$CHECK")"
  else
    printf '\n########## 夹具 %s（%s）##########\n' "$case" "$root"
    out="$(ERP_MIGRATIONS_ROOT="$root" python3 "$TMP/driver.py" "$CHECK")"
  fi
  printf '%s\n' "$out"
done
