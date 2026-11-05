"""F2/F3 后端契约核对：盘点 page/detail 是否回填 voucher_no / reverse_voucher_no。"""
import json
import os
import urllib.request

API = "http://127.0.0.1:18191/api"


def post(path, body, token=None):
    req = urllib.request.Request(
        API + path,
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    if token:
        req.add_header("Authorization", "Bearer " + token)
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.load(resp)


def get(path, token):
    req = urllib.request.Request(API + path, headers={"Authorization": "Bearer " + token})
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.load(resp)


login = post("/system/auth/login", {"username": "admin", "password": os.environ.get("FIX9B_PASSWORD", "")})
token = login["data"]["access_token"]
print("login ok, token len =", len(token))

page = get("/business/erp/stock-take/page?page=1&page_size=10", token)["data"]
print("total =", page["total"])
fails = []
for row in page["list"]:
    print(
        row["take_no"],
        row["status"],
        "voucher_id=",
        (row.get("voucher_id") or "-")[:8],
        "voucher_no=",
        row.get("voucher_no"),
        "reverse_voucher_no=",
        row.get("reverse_voucher_no"),
    )
    if row.get("voucher_id") and not row.get("voucher_no"):
        fails.append(row["take_no"] + " 有 voucher_id 但 voucher_no 为空")
    if row.get("reverse_voucher_id") and not row.get("reverse_voucher_no"):
        fails.append(row["take_no"] + " 有 reverse_voucher_id 但 reverse_voucher_no 为空")

# 详情（列表第一条有凭证的）
target = next((r for r in page["list"] if r.get("voucher_id")), None)
if target:
    detail = get("/business/erp/stock-take/" + target["id"], token)["data"]
    print(
        "detail",
        detail["take_no"],
        "voucher_no=",
        detail.get("voucher_no"),
        "reverse_voucher_no=",
        detail.get("reverse_voucher_no"),
    )
    if not detail.get("voucher_no"):
        fails.append("详情 voucher_no 为空")
    if detail.get("reverse_voucher_id") and not detail.get("reverse_voucher_no"):
        fails.append("详情 reverse_voucher_no 为空")

# 凭证号确实能在凭证列表按号检索到（财务核账链路）
for row in page["list"]:
    no = row.get("voucher_no")
    if not no:
        continue
    found = get("/business/erp/voucher/page?page=1&page_size=5&voucher_no=" + no, token)["data"]
    hit = any(v["voucher_no"] == no for v in found["list"])
    print("voucher_no", no, "→ 凭证列表命中:", hit, "total=", found["total"])
    if not hit:
        fails.append(no + " 在凭证列表检索不到")

print("\nFAILS:", fails if fails else "无")
raise SystemExit(1 if fails else 0)
