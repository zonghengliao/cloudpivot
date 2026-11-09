#!/usr/bin/env bash
# R4 生产部署（144）：拉代码 → 编 release → 编前端 dist → 重启 → 只读冒烟
# 用法（在 144 上）：bash scripts/verify/r4_deploy_144.sh
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH
TS=$(date +%Y%m%d%H%M%S)
cd /opt/cloudpivot

echo "== 1) 拉代码（main 跟踪 bare 远端，必须 ff-only）=="
git fetch origin
git merge --ff-only origin/main || { echo "MERGE_FAILED"; exit 1; }
git log --oneline -1

echo "== 2) 备份旧二进制 =="
cp -a backend/target/release/cloudpivot /root/cloudpivot.bak.$TS && echo "backup=/root/cloudpivot.bak.$TS"

echo "== 3) 编 release（只编增量）=="
cd backend
cargo build --release 2>&1 | tail -3
BIN_MD5=$(md5sum target/release/cloudpivot | awk '{print $1}')
ls -l target/release/cloudpivot
echo "binary_md5=$BIN_MD5"

echo "== 4) 编前端 dist =="
cd ../frontend
OLD_ENTRY=$(grep -oh "BasicLayout-[A-Za-z0-9_-]*\.js" dist/index.html dist/assets/index-*.js 2>/dev/null | head -1)
pnpm build 2>&1 | tail -6
NEW_ENTRY=$(grep -oh "BasicLayout-[A-Za-z0-9_-]*\.js" dist/index.html dist/assets/index-*.js 2>/dev/null | head -1)
echo "entry_chunk: ${OLD_ENTRY:-<none>} → ${NEW_ENTRY:-<none>}"
echo "-- 新包特征（每个应命中 ≥1 个文件）--"
for token in btn-yearend-close btn-yearend-reopen btn-yearend-refresh yearend-status-tag \
             yearend-check-PERIOD_NOT_CLOSED yearend-voucher yearend-opening \
             yearend-close-dialog yearend-reopen-dialog yearend-net-profit; do
  n=$(grep -rl "$token" dist/assets/*.js 2>/dev/null | wc -l)
  echo "  $token → $n"
done

echo "== 5) 重启服务 =="
systemctl restart cloudpivot
for i in $(seq 1 60); do
  code=$(curl -s -o /dev/null -w '%{http_code}' -m 2 http://127.0.0.1:8023/health || true)
  [ "$code" = "200" ] && break
  sleep 2
done
echo "health=$code"

echo "== 6) 进程 == 磁盘 核对 =="
MAINPID=$(systemctl show -p MainPID --value cloudpivot)
PROC_MD5=$(md5sum /proc/$MAINPID/exe | awk '{print $1}')
echo "main_pid=$MAINPID proc_md5=$PROC_MD5 disk_md5=$BIN_MD5"
[ "$PROC_MD5" = "$BIN_MD5" ] && echo "PROC_EQ_DISK=OK" || echo "PROC_EQ_DISK=FAIL"

echo "== 7) 迁移与启动公告 =="
journalctl -u cloudpivot --since "-6 min" --no-pager | grep -E "数据库迁移完成|V0190|自检通过" | tail -8

echo "== 8) 结构与权限节点核对 =="
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
PSQL="psql -h 127.0.0.1 -U $NIK_DATABASE_USER -d $NIK_DATABASE_NAME -tAc"
$PSQL "select max(version::int) from schema_migrations"
$PSQL "select count(*) from sys_menu where auth_code like 'business:erp:yearend:%' and deleted_at is null"
$PSQL "select pg_get_constraintdef(oid) from pg_constraint where conrelid='public.biz_voucher'::regclass and conname='ck_biz_voucher_type'"
$PSQL "select indexname from pg_indexes where tablename='biz_voucher' and indexname='uk_biz_voucher_year_end_period'"
$PSQL "select column_name||'/'||is_nullable from information_schema.columns where table_name='biz_opening_balance' and column_name='source'"
echo "-- 三码授权行数（生产角色可能不带 ADMIN/FINANCE，SUPER_ADMIN 短路）--"
$PSQL "select count(*) from sys_role_menu rm join sys_menu m on m.id=rm.menu_id where m.auth_code like 'business:erp:yearend:%'"

echo "== 9) 只读冒烟（生产不做年结动作）=="
# 登录口径：运维账号（CLOUDPIVOT_OPS_*），单次尝试、失败即停 —— 见 docs/运维规程-凭据与自查账号.md
for C in "${OPS_CRED_LIB:-}" /root/ops_cred.sh "$(dirname "$0")/../ops/ops_cred.sh"; do
  [ -n "$C" ] && [ -r "$C" ] && { . "$C"; break; }
done
if ! command -v ops_cred_load >/dev/null; then echo "缺 ops_cred.sh（scripts/ops/ops_cred.sh 或 /root/ops_cred.sh）"; exit 2; fi
ops_cred_load "${OPS_ENV_FILE:-/etc/cloudpivot/cloudpivot.env}" || exit 2
export OPS_USER_FOR_SMOKE="$OPS_CRED_USER"
export OPS_PASS_FOR_SMOKE="$OPS_CRED_PASS"
python3 - <<'PY'
import json, os, urllib.request, urllib.error

def api(method, path, body=None, token=None):
    url = f"http://127.0.0.1:8023{path}"
    data = json.dumps(body).encode() if body is not None else None
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=30) as res:
            return res.status, json.loads(res.read().decode() or "{}")
    except urllib.error.HTTPError as err:
        raw = err.read().decode()
        try:
            return err.code, json.loads(raw)
        except Exception:
            return err.code, {"raw": raw}

username = os.environ.get("OPS_USER_FOR_SMOKE") or os.environ.get("CLOUDPIVOT_OPS_USER") or "admin"
password = os.environ.get("OPS_PASS_FOR_SMOKE") or os.environ.get("CLOUDPIVOT_OPS_PASSWORD") or os.environ.get("NIK_ADMIN_PASSWORD") or ""
if not password:
    print("缺登录凭据（CLOUDPIVOT_OPS_PASSWORD）——单次尝试、不猜 admin 口令")
    raise SystemExit(2)
status, payload = api("POST", "/api/system/auth/login", {"username": username, "password": password})
data = payload.get("data") or {}
token = data.get("access_token")
print(f"login: HTTP {status} token={'yes' if token else 'no'} must_change={data.get('must_change_password')}")
if not token:
    raise SystemExit(1)

status, payload = api("GET", "/api/business/erp/year-end/check?year=2026", token=token)
body = payload.get("data") or {}
blockers = [item.get("type") for item in (body.get("blockers") or [])]
print(f"check 2026: HTTP {status} can_close={body.get('can_close')} blockers={blockers}")
state = body.get("state") or {}
print(f"state: status={state.get('status')} periods_closed={state.get('periods_closed')} "
      f"next_year_ready={state.get('next_year_periods_ready')} closed={state.get('closed')}")
assert status == 200, status
assert body.get("can_close") is False, "生产 2026 还没走完，不该能年结"
assert "PERIOD_NOT_CLOSED" in blockers, blockers
print("PROD_SMOKE=OK")
PY
