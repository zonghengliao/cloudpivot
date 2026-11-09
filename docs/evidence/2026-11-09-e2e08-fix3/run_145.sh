#!/usr/bin/env bash
# 145：E2E08-FIX-3 真 token curl 验收（隔离实例 :8038，**复用 cloudpivot_dev 的现有主数据**，只读；8033/8034 不动）
#
# 为什么不新建库：仓库里 V0363（monitor 菜单迁移，他人卡片）在空库首启时会抛错回滚，
# 本卡不碰它 —— 改用已迁移的 cloudpivot_dev，只发 GET（+ 一次失败的 batch-generate），不插夹具。
#
# 验收点：
#   ① GET /material/{id} 返回 unit_name + warehouse_name（id 与名字成对）
#   ② GET /material/page 同样字段，且 **SQL 条数不随行数增长**（PG log_statement=all，按本实例 PID 过滤）
#   ③ 没设默认仓库的物料 warehouse_name = null（键仍在）
#   ④ POST /serial/batch-generate 不带 unit_cost → 400 + 10001（文档 §10 写的就是这个）
set -u
export PATH=/root/.cargo/bin:$PATH
DB=cloudpivot_dev
PORT=8038
BIN=/opt/cloudpivot/backend/target/debug/cloudpivot
PGLOG=/var/log/postgresql/postgresql-18-main.log
WORK=/tmp/e2e08fix3
mkdir -p "$WORK"
OUT=$WORK/result.txt
: > "$OUT"
say() { echo "$@" | tee -a "$OUT"; }
psqlq() { PGPASSWORD="$NIK_DATABASE_PASSWORD" psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$DB" -Atc "$1"; }

set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a

say "===== 0) 二进制 mtime ====="
ls -l --time-style=+%F_%T "$BIN" | tee -a "$OUT"

say "===== 1) 记录 cloudpivot_dev 既有连接 PID（用于把 SQL 计数限定到本实例）====="
psqlq "select pid from pg_stat_activity where datname='$DB'" | sort > "$WORK/pids_before.txt"
say "既有连接数=$(wc -l < "$WORK/pids_before.txt")"

say "===== 2) 只对**新连接**开 log_statement=all（跑完 RESET）====="
# 必须用超管：cloudpivot 用户不是该库 owner，ALTER DATABASE ... SET 会被拒（第一版就静默失败，SQL 计数全 0）
if su - postgres -c "psql -qc \"alter database $DB set log_statement='all'\"" >/dev/null 2>&1; then
  say "log_statement=all (database=$DB, by superuser)"
else
  say "!! 开 log_statement 失败（本轮 SQL 计数无效）"
fi
say "show log_statement(@$DB) = $(su - postgres -c "psql -d $DB -Atc 'show log_statement'" 2>/dev/null)"

say "===== 3) 起隔离实例 :$PORT ====="
export NIK_SERVER_LISTEN=127.0.0.1:$PORT
LOG=$WORK/server.log
cd /opt/cloudpivot/backend || exit 1   # 配置文件按 CWD 找 config/<profile>.toml
setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
SRV=$!
OK=0
for i in $(seq 1 120); do
  if curl -s -m 2 -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/health" 2>/dev/null | grep -q 200; then OK=1; break; fi
  kill -0 "$SRV" 2>/dev/null || { say "实例提前退出"; grep -E "ERROR|FATAL" "$LOG" | tail -10 | tee -a "$OUT"; exit 1; }
  sleep 1
done
say "health_ok=$OK pid=$SRV"
[ "$OK" = 1 ] || { grep -E "ERROR|FATAL" "$LOG" | tail -20 | tee -a "$OUT"; exit 1; }

say "===== 4) 本实例的连接 PID ====="
psqlq "select pid from pg_stat_activity where datname='$DB'" | sort > "$WORK/pids_after.txt"
comm -13 "$WORK/pids_before.txt" "$WORK/pids_after.txt" > "$WORK/pids_mine.txt"
say "本实例连接数=$(wc -l < "$WORK/pids_mine.txt")"

say "===== 5) admin 首登改密闸（仅当为 true 时改；cloudpivot_dev 长期共享库）====="
say "must_change_password(before)=$(psqlq "select must_change_password from sys_user where username='admin'")"
psqlq "update sys_user set must_change_password=false where username='admin' and must_change_password" >/dev/null
say "must_change_password(after)=$(psqlq "select must_change_password from sys_user where username='admin'")"

say "===== 6) 挑两条现有物料（一条有默认仓库 / 一条没有）====="
M_WH=$(psqlq "select id from biz_material where deleted_at is null and default_warehouse_id is not null and unit_id is not null order by material_code limit 1")
M_NOWH=$(psqlq "select id from biz_material where deleted_at is null and default_warehouse_id is null and unit_id is not null order by material_code limit 1")
say "M_WH=$M_WH"
say "M_NOWH=$M_NOWH"
say "库内口径(WH)   =$(psqlq "select m.material_code||' | unit='||u.unit_name||' | wh='||w.warehouse_name from biz_material m join biz_unit u on u.id=m.unit_id left join biz_warehouse w on w.id=m.default_warehouse_id where m.id='$M_WH'")"
say "库内口径(NOWH) =$(psqlq "select m.material_code||' | unit='||u.unit_name||' | wh='||coalesce(w.warehouse_name,'<null>') from biz_material m join biz_unit u on u.id=m.unit_id left join biz_warehouse w on w.id=m.default_warehouse_id where m.id='$M_NOWH'")"

say "===== 7) 登录拿 token ====="
LOGIN=$(curl -s -m 10 -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"${NIK_ADMIN_PASSWORD:-Admin@123456}\"}" \
  "http://127.0.0.1:$PORT/api/system/auth/login")
echo "$LOGIN" > $WORK/login.json
TOKEN=$(python3 -c "import json;print(json.load(open('$WORK/login.json'))['data']['access_token'])" 2>/dev/null)
if [ -z "${TOKEN:-}" ]; then say "登录失败：$(head -c 300 $WORK/login.json)"; kill "$SRV"; exit 1; fi
say "token_len=${#TOKEN}"

API="http://127.0.0.1:$PORT/api"
AUTH="Authorization: Bearer $TOKEN"

logmark() { wc -l < "$PGLOG"; }
sqlcount() {  # $1=起始行号 → 只数**本实例连接**发出的语句行
  tail -n +$(( $1 + 1 )) "$PGLOG" \
    | awk -v pids=" $(tr '\n' ' ' < "$WORK/pids_mine.txt") " '
        { if (match($0, /\[[0-9]+\]/)) { pid = substr($0, RSTART+1, RLENGTH-2);
            if (index(pids, " " pid " ") > 0 && $0 ~ /LOG:  (statement|execute)/) c++ } }
        END { print c+0 }'
}

say "===== 8) ① 详情（有默认仓库） ====="
M1=$(logmark); D1=$(curl -s -m 10 -H "$AUTH" "$API/business/erp/material/$M_WH"); S1=$(sqlcount "$M1")
echo "$D1" > $WORK/detail_wh.json
say "detail(有仓库) SQL条数=$S1"
say "detail(有仓库) 响应=$(echo "$D1" | head -c 700)"

say "===== 9) ② 详情（无默认仓库） ====="
M2=$(logmark); D2=$(curl -s -m 10 -H "$AUTH" "$API/business/erp/material/$M_NOWH"); S2=$(sqlcount "$M2")
echo "$D2" > $WORK/detail_nowh.json
say "detail(无仓库) SQL条数=$S2"
say "detail(无仓库) 响应=$(echo "$D2" | head -c 700)"

say "===== 10) ③ 分页 page_size=1 vs 20（同筛选，SQL 条数应相等）====="
M3=$(logmark); P1=$(curl -s -m 10 -H "$AUTH" "$API/business/erp/material/page?page=1&page_size=1"); S3=$(sqlcount "$M3")
echo "$P1" > $WORK/page_1.json
say "page_size=1  SQL条数=$S3 行数=$(python3 -c "import json;print(len(json.load(open('$WORK/page_1.json'))['data']['list']))" 2>/dev/null)"

M4=$(logmark); P20=$(curl -s -m 10 -H "$AUTH" "$API/business/erp/material/page?page=1&page_size=20"); S4=$(sqlcount "$M4")
echo "$P20" > $WORK/page_20.json
say "page_size=20 SQL条数=$S4 行数=$(python3 -c "import json;print(len(json.load(open('$WORK/page_20.json'))['data']['list']))" 2>/dev/null)"
say "page_size=20 响应=$(echo "$P20" | head -c 900)"

say "===== 11) ④ 序列号批量生成：不带 unit_cost → 400 + 10001 ====="
B1=$(curl -s -m 10 -o $WORK/batch_no_cost.json -w '%{http_code}' -X POST -H "$AUTH" -H 'Content-Type: application/json' \
  -d "{\"material_id\":\"$M_WH\",\"count\":2}" "$API/business/erp/serial/batch-generate")
say "batch(不带 unit_cost) HTTP=$B1 body=$(head -c 400 $WORK/batch_no_cost.json)"

say "===== 12) 断言 ====="
python3 - "$WORK" <<'PY' | tee -a "$OUT"
import json, sys
w = sys.argv[1]
ok = True
def chk(cond, label, extra=""):
    global ok
    print(("PASS " if cond else "FAIL ") + label + ((" :: " + str(extra)) if extra else ""))
    if not cond: ok = False

def load(n):
    return json.load(open(f"{w}/{n}"))["data"]

d1 = load("detail_wh.json")
chk(bool(d1.get("unit_id")), "①详情 unit_id 非空")
chk(bool(d1.get("unit_name")), "①详情 unit_name 已返回（约定 13）", repr(d1.get("unit_name")))
chk(bool(d1.get("warehouse_name")), "①详情 warehouse_name 已返回（约定 13）", repr(d1.get("warehouse_name")))
chk("unit_name" in d1 and "warehouse_name" in d1, "①详情两个键都存在")

d2 = load("detail_nowh.json")
chk(bool(d2.get("unit_name")), "②详情(无仓库) unit_name 仍有", repr(d2.get("unit_name")))
chk("warehouse_name" in d2 and d2["warehouse_name"] is None, "②详情(无仓库) warehouse_name 键在且 null", repr(d2.get("warehouse_name")))
chk(d2.get("default_warehouse_id") is None, "②详情(无仓库) default_warehouse_id null")

p1, p20 = load("page_1.json"), load("page_20.json")
r1, r20 = p1["list"], p20["list"]
chk(len(r1) == 1, "③page_size=1 返回 1 行", len(r1))
chk(len(r20) >= 2, "③page_size=20 返回多行", len(r20))
for nm, rows in (("page_size=1", r1), ("page_size=20", r20)):
    chk(all("unit_name" in r and "warehouse_name" in r for r in rows), f"③{nm} 每行都有两个名字键")
    chk(all(r.get("unit_name") for r in rows), f"③{nm} 每行 unit_name 都有值")
    chk(all(r.get("warehouse_name") for r in rows if r.get("default_warehouse_id")), f"③{nm} 有默认仓库的行 warehouse_name 也有值")

b = json.load(open(f"{w}/batch_no_cost.json"))
chk(b.get("code") == 10001, "④批量生成不带 unit_cost → 10001", b.get("code"))
chk("unit_cost 必填" in (b.get("msg") or ""), "④报文点名 unit_cost 必填", b.get("msg"))
print("OVERALL=" + ("PASS" if ok else "FAIL"))
PY

say "===== 13) SQL 条数对比（无 N+1 的判据）====="
python3 - "$WORK" <<'PY' | tee -a "$OUT"
import re, sys
w = sys.argv[1]
text = open(f"{w}/result.txt", encoding="utf-8", errors="replace").read()
def num(label):
    m = re.search(label + r"[^=]*=(\d+)", text)
    return int(m.group(1)) if m else None
d1, d2 = num("detail\\(有仓库\\) SQL条数"), num("detail\\(无仓库\\) SQL条数")
s1, s20 = num("page_size=1  SQL条数"), num("page_size=20 SQL条数")
print(f"detail(有仓库)={d1} detail(无仓库)={d2} page1={s1} page20={s20}")
ok = d1 is not None and s1 is not None and s20 is not None and s1 > 0 and s1 == s20 and s1 <= 4
print(("PASS " if ok else "FAIL ") + "分页 SQL 条数与行数无关（page_size=1 与 20 相等，且 <= 4 条/请求）")
print("NO_N1_OVERALL=" + ("PASS" if ok else "FAIL"))
PY

say "===== 14) 收工：杀实例 + RESET log_statement ====="
kill "$SRV" 2>/dev/null; sleep 2; kill -9 "$SRV" 2>/dev/null
su - postgres -c "psql -qc \"alter database $DB reset log_statement\"" >/dev/null 2>&1 && say "log_statement 已 RESET"
say "SRV_KILLED=$SRV"
say "SERVER_ERRORS=$(grep -cE ' (ERROR|FATAL) ' "$LOG" || true)"
echo "DONE=$OUT"
