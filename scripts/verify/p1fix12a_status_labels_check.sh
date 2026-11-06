#!/usr/bin/env bash
# 【P1-FIX-12A】联动规则「触发状态」中文名（status_labels）接口自检。
#
# 判据（任一不满足 → 非零退出）：
#   1. GET /business/flow/linkage 的 triggers[] 每项都有 status_labels；
#   2. status_labels 的 code 序列与 statuses **逐项对齐**（前端按下标兜底也不会错位）；
#   3. 每条 label 非空、且不等于 code（等于 code = 字典缺项，等于没修）；
#   4. rules[] 每行带 trigger_status_label，且与字典一致；
#   5. statuses 仍在（向后兼容，老前端不炸）。
#
# 用法：
#   BASE=http://127.0.0.1:18023 NIK_ADMIN_PASSWORD='Admin@123456' \
#     bash scripts/verify/p1fix12a_status_labels_check.sh
#   （首次登录会被 must_change_password 拦成 403 —— 脚本会自动 PUT 改密再重登；
#     新口令由 NIK_ADMIN_NEW_PASSWORD 指定，默认 Admin@654321）
set -uo pipefail
BASE="${BASE:-http://127.0.0.1:18023}"
WORK="${WORK:-/root/.cloudpivot/p1fix12a}"
: "${NIK_ADMIN_PASSWORD:?需要 admin 口令：NIK_ADMIN_PASSWORD=<口令>}"
NEW_PASSWORD="${NIK_ADMIN_NEW_PASSWORD:-Admin@654321}"
export NIK_ADMIN_PASSWORD NIK_ADMIN_NEW_PASSWORD="$NEW_PASSWORD"
mkdir -p "$WORK"

json_field() {
  python3 -c "import json,sys;d=json.load(open(sys.argv[1]));print(d.get('data',{}).get(sys.argv[2],''))" "$1" "$2"
}

login() {
  # 注意：`-w` 的输出必须走 stderr —— 本函数在 `$(...)` 里被调用，stdout 只能是令牌本身。
  curl -sS -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
    --data-binary @<(python3 -c 'import json,os;print(json.dumps({"username":"admin","password":os.environ["NIK_ADMIN_PASSWORD"]}))') \
    -o "$WORK/login.json" -w 'login_http=%{http_code}\n' 1>&2
  json_field "$WORK/login.json" access_token
}

echo "== ① 登录"
TOKEN=$(login)
if [ -z "$TOKEN" ]; then
  cat "$WORK/login.json"; echo; echo "登录失败"; exit 1
fi

echo "== ② 首登强制改密（403「首次登录必须先修改初始密码」时必经）"
curl -sS -X PUT "$BASE/api/system/auth/password" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  --data-binary @<(python3 -c 'import json,os;print(json.dumps({"old_password":os.environ["NIK_ADMIN_PASSWORD"],"new_password":os.environ["NIK_ADMIN_NEW_PASSWORD"]}))') \
  -o "$WORK/change-pwd.json" -w 'change_pwd_http=%{http_code}\n'
if grep -q '"code":0' "$WORK/change-pwd.json"; then
  export NIK_ADMIN_PASSWORD="$NEW_PASSWORD"
  TOKEN=$(login)
fi

echo "== ③ GET /business/flow/linkage"
curl -sS "$BASE/api/business/flow/linkage" -H "Authorization: Bearer $TOKEN" \
  -o "$WORK/linkage.json" -w 'linkage_http=%{http_code}\n'

echo "== ④ 断言"
python3 - "$WORK/linkage.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
res = d.get("data") or {}
triggers, rules = res.get("triggers") or [], res.get("rules") or []
problems = []
if d.get("code") != 0:
    problems.append(f"接口返回 code={d.get('code')} msg={d.get('msg')!r}")
if len(triggers) != 15:
    problems.append(f"触发点数量 {len(triggers)} ≠ 15")

by_entity = {}
for t in triggers:
    sl = t.get("status_labels")
    if not sl:
        problems.append(f"{t['entity']}: 缺 status_labels")
        continue
    if [x["code"] for x in sl] != t["statuses"]:
        problems.append(f"{t['entity']}: status_labels 与 statuses 不逐项对齐")
    for x in sl:
        if not x["label"].strip() or x["label"] == x["code"]:
            problems.append(f"{t['entity']}/{x['code']}: label 未中文化")
    if not t["statuses"]:
        problems.append(f"{t['entity']}: statuses 丢空（向后兼容被破坏）")
    by_entity[t["entity"]] = {x["code"]: x["label"] for x in sl}

for r in rules:
    want = by_entity.get(r["trigger_entity"], {}).get(r["trigger_status"], r["trigger_status"])
    if r.get("trigger_status_label") != want:
        problems.append(
            f"规则 {r['id'][:8]}…: trigger_status_label={r.get('trigger_status_label')!r} 期望 {want!r}"
        )

if problems:
    print("红：")
    for p in problems:
        print("  -", p)
    sys.exit(1)
print(f"绿：{len(triggers)} 个触发点全部带中文名、与 statuses 逐项对齐、{len(rules)} 条规则带 trigger_status_label")
PY
