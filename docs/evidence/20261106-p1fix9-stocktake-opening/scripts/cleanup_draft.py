import json, sys
sys.path.insert(0, '/root/st9-run')
from apilib import Api

a = Api(); a.login("admin", "${ST9_ADMIN_PASSWORD}")
page = a.ok("GET", "/business/erp/stock-take/page?page=1&page_size=100")
rows = page.get("list") or []
for r in rows:
    if r.get("status") == "DRAFT":
        res = a.call("DELETE", f"/business/erp/stock-take/{r['id']}")
        print("delete", r.get("take_no"), res["_ok"])
print("remaining:", [ (r.get('take_no'), r.get('status')) for r in (a.ok("GET","/business/erp/stock-take/page?page=1&page_size=100").get("list") or [])])
