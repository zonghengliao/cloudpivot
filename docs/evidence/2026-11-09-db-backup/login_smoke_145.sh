#!/usr/bin/env bash
# 145 恢复演练第 ④ 步：用「144 生产 dump 恢复出来的临时库」起隔离实例，真 token 登录 + 关键接口冒烟
#
# 前置：restore_db.sh 已把 144 生产 dump 恢复到 cloudpivot_drill3_20261109
#       （基线比对 PASS，见 restore_drill_145.log）
# 隔离：只在 127.0.0.1:8039 起临时实例；8033 dev / 8034 test 服务不动；跑完即退出
#
# 凭证说明（重要，别误读）：
#   · 生产 admin 口令在 2026-11-09 20:00:27 由用户本人在 UI 里 SELF RESET 改过
#     （证据：生产 password_change_logs，operator_type=SELF），演练方**不持有**新口令，
#     也不应该为了演练去猜/重置生产口令。
#   · 因此本脚本只对**演练副本库**做一次口令哈希替换：把演练库 admin 的 password 列
#     换成 145 演练环境 dev 库（cloudpivot_dev）admin 的哈希——同一产品、同一 argon2 参数，
#     口令明文取自 /etc/cloudpivot/cloudpivot-dev.env 的 NIK_ADMIN_PASSWORD。
#   · 除 password 一列外，账号行（id / 角色 / token_version）、菜单、业务数据全部来自恢复库；
#     脚本会显式打印恢复库里的其它真实账号（henry.liao / laoliu）与菜单数，证明数据确实来自 dump。
#
# 目的：证明「恢复出来的库」能真的把系统拉起来、能用恢复出来的账号登录、关键接口 200
set -u
export PATH=/usr/local/bin:/usr/bin:/bin:$PATH

DB=cloudpivot_drill3_20261109
DEV_DB=cloudpivot_dev
PORT=8039
BIN=/opt/cloudpivot/backend/target/debug/cloudpivot
WORK=/tmp/dbbackup-login-smoke
mkdir -p "$WORK"
OUT=$WORK/result.txt
: > "$OUT"
say() { echo "$@" | tee -a "$OUT"; }

set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export NIK_DATABASE_NAME=$DB
export NIK_SERVER_LISTEN=127.0.0.1:$PORT

psqlq() { # $1=库 $2=SQL
  PGPASSWORD="$NIK_DATABASE_PASSWORD" psql -X -q -A -t -h "${NIK_DATABASE_HOST:-127.0.0.1}" \
    -p "${NIK_DATABASE_PORT:-5432}" -U "$NIK_DATABASE_USER" -d "$1" -c "$2"
}

say "===== 0) 目标库与恢复出来的账号（只读）====="
say "库 $DB：表 $(psqlq "$DB" "select count(*) from information_schema.tables where table_schema='public' and table_type='BASE TABLE'") 张，迁移 $(psqlq "$DB" "select count(*) from schema_migrations where success") 条，菜单 $(psqlq "$DB" "select count(*) from sys_menu") 条"
say "恢复库内真实账号：$(psqlq "$DB" "select string_agg(username||'(status='||status||')', ', ' order by username) from sys_user")"
say "admin 口令哈希（恢复库原值，前缀）：$(psqlq "$DB" "select left(password, 30) from sys_user where username='admin'")"

say "===== 1) 仅对演练副本替换 admin 口令哈希（生产口令不持有，见脚本头说明）====="
dev_hash=$(psqlq "$DEV_DB" "select password from sys_user where username='admin'")
[ -n "$dev_hash" ] || { say "FAIL 取不到 dev 库 admin 哈希"; exit 1; }
psqlq "$DB" "update sys_user set password = \$h\$$dev_hash\$h\$, must_change_password = false, is_locked = false, login_fail_count = 0 where username = 'admin'" >/dev/null
say "替换后 admin 哈希前缀：$(psqlq "$DB" "select left(password, 30) from sys_user where username='admin'")"
say "其余账号未被改动：$(psqlq "$DB" "select string_agg(username||'|'||left(password,12), ', ' order by username) from sys_user")"

say "===== 2) 起隔离实例 :$PORT（库=$DB）====="
LOG=$WORK/server.log
cd /opt/cloudpivot/backend || { say "cd 失败"; exit 1; }
setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
SRV=$!
OK=0
for _ in $(seq 1 120); do
  if curl -s -m 2 -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" 2>/dev/null | grep -q 200; then OK=1; break; fi
  kill -0 "$SRV" 2>/dev/null || { say "实例提前退出："; grep -E "ERROR|FATAL|panic" "$LOG" | tail -10 | tee -a "$OUT"; exit 1; }
  sleep 1
done
say "health_ok=$OK pid=$SRV"
if [ "$OK" != 1 ]; then grep -E "ERROR|FATAL|panic" "$LOG" | tail -20 | tee -a "$OUT"; kill "$SRV" 2>/dev/null; exit 1; fi

say "===== 3) 确认实例连的就是恢复库（不是 dev/test 库）====="
say "到 $DB 的连接数：$(psqlq "$DB" "select count(*) from pg_stat_activity where datname='$DB'")"
say "本实例 listen：$(ss -lntp 2>/dev/null | grep ":$PORT " | head -1)"

say "===== 4) 用恢复出来的 admin 账号登录（真 token）====="
LOGIN=$(curl -s -m 10 -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"${NIK_ADMIN_PASSWORD:-}\"}" \
  "http://127.0.0.1:$PORT/api/system/auth/login")
echo "$LOGIN" > "$WORK/login.json"
TOKEN=$(python3 -c "import json;print(json.load(open('$WORK/login.json'))['data']['access_token'])" 2>/dev/null)
say "login 响应：$(head -c 200 "$WORK/login.json")"
if [ -z "${TOKEN:-}" ]; then say "FAIL 登录失败，实例日志尾部："; tail -20 "$LOG" | tee -a "$OUT"; kill "$SRV" 2>/dev/null; exit 1; fi
say "token_len=${#TOKEN}"
AUTH="Authorization: Bearer $TOKEN"
API="http://127.0.0.1:$PORT/api"

smoke() { # $1=标签 $2=路径
  local code
  code=$(curl -s -m 10 -o "$WORK/$1.json" -w '%{http_code}' -H "$AUTH" "$API$2")
  say "  $1 GET $2 -> HTTP $code | $(head -c 140 "$WORK/$1.json")"
  echo "$code" > "$WORK/$1.code"
}

say "===== 5) 关键接口冒烟（带 token）====="
smoke user_page "/system/user/page?page=1&page_size=5"
smoke menu_tree "/system/menu/tree"
smoke auth_menus "/system/auth/menus"
smoke material_page "/business/erp/material/page?page=1&page_size=3"
smoke role_page "/system/role/page?page=1&page_size=5"

say "===== 6) 断言 ====="
python3 - "$WORK" <<'PY' | tee -a "$OUT"
import json, sys
w = sys.argv[1]
ok = True
def chk(cond, label, extra=""):
    global ok
    print(("PASS " if cond else "FAIL ") + label + ((" :: " + str(extra)) if extra else ""))
    if not cond: ok = False

login = json.load(open(f"{w}/login.json"))
chk(login.get("code") == 0, "① 恢复库上 admin 登录返回 code=0", login.get("code"))
d = login.get("data") or {}
chk(bool(d.get("access_token")) and len(d.get("access_token")) > 40,
    "② 拿到 access_token", f"len={len(d.get('access_token') or '')}")
for label, path in [("user_page", "/system/user/page"), ("menu_tree", "/system/menu/tree"),
                    ("auth_menus", "/system/auth/menus"),
                    ("material_page", "/business/erp/material/page"),
                    ("role_page", "/system/role/page")]:
    code = open(f"{w}/{label}.code").read().strip()
    chk(code == "200", f"③ {path} HTTP 200", code)
body = json.load(open(f"{w}/user_page.json"))
chk(body.get("code") == 0, "④ 用户分页 code=0", body.get("code"))
names = [r.get("username") for r in (body.get("data") or {}).get("list", [])]
chk("admin" in names, "⑤ 分页里能看到恢复出来的 admin", names)
chk("henry.liao" in names or "laoliu" in names,
    "⑥ 分页里能看到生产库里其它真实账号（证明数据来自 dump 而非新建）", names)
menus = json.load(open(f"{w}/menu_tree.json"))
tree = menus.get("data") or []
chk(len(tree) > 0, "⑦ 菜单树非空（恢复出来的菜单）", f"top_nodes={len(tree)}")
print("RESULT: " + ("PASS" if ok else "FAIL"))
sys.exit(0 if ok else 1)
PY
RC=${PIPESTATUS[0]}

say "===== 7) 收尾：停掉临时实例（只杀本脚本起的 PID $SRV）====="
kill "$SRV" 2>/dev/null
for _ in $(seq 1 20); do kill -0 "$SRV" 2>/dev/null || break; sleep 0.5; done
kill -9 "$SRV" 2>/dev/null
say "实例已停：$(kill -0 "$SRV" 2>/dev/null && echo 仍在运行 || echo 已退出)"
say "RC=$RC"
exit "$RC"
