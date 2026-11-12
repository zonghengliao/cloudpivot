#!/usr/bin/env python3
"""员工档案导入（卡 t_b0808afe）HTTP 级 E2E —— 纯标准库。

跑在隔离实例上（库 cp_hr_emp_import / 端口 18089，**非 prod**）：

    python3 emp_import_e2e.py

覆盖（与卡面验收逐条对齐）：
  ① 模板下载：`GET /api/business/hr/employee/import-template` → 真 xlsx
  ② 模板体检：中文表头 / 第 2 行示例 / 列文本格式(`@`) / 下拉(dataValidation) /
     批注(comments1.xml) / 冻结首行(pane)
  ③ 上传 3 行（2 合法 + 1 部门不存在）→ 预检 2 可导入 / 1 错误（带行号与原因）
  ④ 确认入库 → 2 行入库（库里查得到，工号自动取号）
  ⑤ 下载错误行 → xlsx 只含错误行 + 错误原因列
  ⑥ 幂等：同一文件再导一遍 → 0 行入库（2 行按重复跳过）

退出码：0 全绿 / 1 有断言失败 / 2 环境问题。
"""

from __future__ import annotations

import json
import mimetypes
import os
import re
import sys
import urllib.error
import urllib.request
import uuid
import zipfile
from xml.sax.saxutils import escape

BASE = os.environ.get("EMP_BASE", "http://127.0.0.1:18089")
ADMIN_PASSWORD = os.environ.get("ADMIN_PASSWORD", "Admin@123456")
WORK = os.environ.get("EMP_EVIDENCE_DIR", "/root/emp-import-e2e")

PASS = 0
FAIL = 0
CHECKPOINTS: list[str] = []


def ok(name: str, detail: str = "") -> None:
    global PASS
    PASS += 1
    line = f"[PASS] {name}" + (f"（{detail}）" if detail else "")
    CHECKPOINTS.append(line)
    print(line)


def bad(name: str, detail: str) -> None:
    global FAIL
    FAIL += 1
    line = f"[FAIL] {name}：{detail}"
    CHECKPOINTS.append(line)
    print(line)


def expect(name: str, cond: bool, detail: str = "") -> bool:
    if cond:
        ok(name, detail)
    else:
        bad(name, detail or "条件不成立")
    return cond


# ── HTTP ────────────────────────────────────────────────────────────────


def request(method: str, path: str, body: bytes | None = None, headers: dict | None = None):
    url = BASE + path
    req = urllib.request.Request(url, data=body, method=method)
    for key, value in (headers or {}).items():
        req.add_header(key, value)
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return resp.status, resp.read(), dict(resp.headers)
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read(), dict(exc.headers or {})


def api_json(method: str, path: str, payload=None, token: str | None = None):
    body = json.dumps(payload).encode() if payload is not None else None
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    status, raw, _ = request(method, path, body, headers)
    try:
        data = json.loads(raw.decode("utf-8"))
    except Exception:  # noqa: BLE001
        data = {"code": -1, "msg": raw[:200].decode("utf-8", "replace")}
    return status, data


def login(user: str, password: str) -> str:
    status, data = api_json("POST", "/api/system/auth/login", {"username": user, "password": password})
    if status != 200 or data.get("code") != 0:
        print(f"FATAL: 登录失败 status={status} body={data}", file=sys.stderr)
        sys.exit(2)
    token = data["data"]["access_token"]
    if data["data"].get("must_change_password"):
        new_password = password + "-E2e9"
        status, data = api_json(
            "PUT",
            "/api/system/auth/password",
            {"old_password": password, "new_password": new_password},
            token,
        )
        if status != 200 or data.get("code") != 0:
            print(f"FATAL: 改密失败 {data}", file=sys.stderr)
            sys.exit(2)
        status, data = api_json("POST", "/api/system/auth/login", {"username": user, "password": new_password})
        token = data["data"]["access_token"]
    return token


def multipart(field: str, filename: str, content: bytes) -> tuple[bytes, dict]:
    boundary = "----emp" + uuid.uuid4().hex
    mime = mimetypes.guess_type(filename)[0] or "application/octet-stream"
    parts = [
        f"--{boundary}\r\n".encode(),
        f'Content-Disposition: form-data; name="{field}"; filename="{filename}"\r\n'.encode(),
        f"Content-Type: {mime}\r\n\r\n".encode(),
        content,
        f"\r\n--{boundary}--\r\n".encode(),
    ]
    return b"".join(parts), {"Content-Type": f"multipart/form-data; boundary={boundary}"}


# ── 极简 xlsx 写（纯标准库；与 scripts/verify/hr/hr_e2e_xlsx.py 同款思路） ──


def col_name(index: int) -> str:
    name = ""
    index += 1
    while index > 0:
        index, rem = divmod(index - 1, 26)
        name = chr(65 + rem) + name
    return name


def write_xlsx(path: str, rows: list[list[str]]) -> None:
    strings: list[str] = []
    index: dict[str, int] = {}

    def sid(text: str) -> int:
        if text not in index:
            index[text] = len(strings)
            strings.append(text)
        return index[text]

    sheet_rows = []
    for r, row in enumerate(rows, start=1):
        cells = []
        for c, value in enumerate(row):
            if value == "":
                continue
            cells.append(f'<c r="{col_name(c)}{r}" t="s"><v>{sid(value)}</v></c>')
        sheet_rows.append(f'<row r="{r}">{"".join(cells)}</row>')

    shared = "".join(f"<si><t>{escape(s)}</t></si>" for s in strings)
    files = {
        "[Content_Types].xml": (
            '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
            '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
            '<Default Extension="xml" ContentType="application/xml"/>'
            '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
            '<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
            '<Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>'
            "</Types>"
        ),
        "_rels/.rels": (
            '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
            '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>'
            "</Relationships>"
        ),
        "xl/workbook.xml": (
            '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            '<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" '
            'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">'
            '<sheets><sheet name="数据" sheetId="1" r:id="rId1"/></sheets></workbook>'
        ),
        "xl/_rels/workbook.xml.rels": (
            '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
            '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>'
            '<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/>'
            "</Relationships>"
        ),
        "xl/worksheets/sheet1.xml": (
            '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            '<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">'
            f'<sheetData>{"".join(sheet_rows)}</sheetData></worksheet>'
        ),
        "xl/sharedStrings.xml": (
            '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            f'<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="{len(strings)}" uniqueCount="{len(strings)}">{shared}</sst>'
        ),
    }
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as zf:
        for name, content in files.items():
            zf.writestr(name, content)


def read_template_headers(xlsx_path: str) -> list[str]:
    """从下载到的模板里读第 1 行表头（sharedStrings + sheet1 第 1 行）。"""
    with zipfile.ZipFile(xlsx_path) as zf:
        shared_xml = zf.read("xl/sharedStrings.xml").decode("utf-8")
        sheet_xml = zf.read("xl/worksheets/sheet1.xml").decode("utf-8")
    strings = [re.sub(r"<[^>]+>", "", item) for item in re.findall(r"<si>(.*?)</si>", shared_xml, re.S)]
    first_row = re.search(r'<row r="1".*?</row>', sheet_xml, re.S)
    if not first_row:
        raise SystemExit("模板里找不到表头行")
    headers = []
    for cell in re.findall(r'<c r="([A-Z]+)1"[^>]*>(?:<v>(\d+)</v>)?', first_row.group(0)):
        _, value = cell
        headers.append(strings[int(value)] if value else "")
    return headers


def header_of(headers: dict, name: str) -> str:
    """大小写不敏感地取响应头（`dict(resp.headers)` 的键是服务端原样的小写）。"""
    for key, value in headers.items():
        if key.lower() == name.lower():
            return value
    return ""


W_WEIGHTS = [7, 9, 10, 5, 8, 4, 2, 1, 6, 3, 7, 9, 10, 5, 8, 4, 2]
W_TABLE = "10X98765432"


def id_card(body17: str) -> str:
    """按 ISO 7064 MOD 11-2 给 17 位主体补校验位（每一轮跑用不同的号，避免互相撞）。"""
    total = sum(int(ch) * weight for ch, weight in zip(body17, W_WEIGHTS))
    return body17 + W_TABLE[total % 11]


def main() -> int:
    os.makedirs(WORK, exist_ok=True)
    print(f"== 员工档案导入 E2E（{BASE}）==")

    token = login("admin", ADMIN_PASSWORD)
    ok("0. admin 登录")

    # ── ① 模板下载 ──
    status, raw, headers = request(
        "GET", "/api/business/hr/employee/import-template", None, {"Authorization": f"Bearer {token}"}
    )
    template_path = os.path.join(WORK, "员工档案导入模板.xlsx")
    if status != 200:
        bad("① 模板下载", f"status={status} body={raw[:200]}")
        return 1
    with open(template_path, "wb") as fh:
        fh.write(raw)
    ok("① 模板下载（HTTP 200）", f"{len(raw)} 字节")
    disposition = header_of(headers, "Content-Disposition")
    expect(
        "① 文件名带「员工档案导入模板」且是 UTF-8 编码",
        "filename*=UTF-8''" in disposition and "E5%91%98%E5%B7%A5" in disposition,
        disposition,
    )
    expect(
        "① Content-Type = xlsx",
        header_of(headers, "Content-Type").endswith("spreadsheetml.sheet"),
        header_of(headers, "Content-Type"),
    )
    expect("① 文件是 zip/xlsx 容器", raw[:4] == b"PK\x03\x04", raw[:4].hex())

    # ── ② 模板体检 ──
    with zipfile.ZipFile(template_path) as zf:
        names = zf.namelist()
        styles_xml = zf.read("xl/styles.xml").decode("utf-8")
        sheet_xml = zf.read("xl/worksheets/sheet1.xml").decode("utf-8")
        shared_xml = zf.read("xl/sharedStrings.xml").decode("utf-8")
        workbook_xml = zf.read("xl/workbook.xml").decode("utf-8")
    expect("② 列是文本格式（numFmt 含 @）", 'formatCode="@"' in styles_xml, "styles.xml 里应有文本格式")
    expect(
        "② 有下拉（dataValidation + formula1）",
        "<dataValidation" in sheet_xml and "<formula1>" in sheet_xml,
        "sheet1.xml",
    )
    expect("② 下拉是序列（性别/证件类型/用工类型/学历）", sheet_xml.count("<dataValidation ") >= 4, f"实际 {sheet_xml.count('<dataValidation ')} 条")
    expect("② 有批注（comments1.xml）", any("comments" in name for name in names), str([n for n in names if "comment" in n]))
    expect("② 首行冻结（pane）", "<pane" in sheet_xml and "frozen" in sheet_xml.lower(), "freeze")
    expect("② 三个 sheet（数据 / 字段说明 / 填写须知）", "字段说明" in shared_xml and "填写须知" in shared_xml, "sharedStrings")
    expect("② 示例行是灰色斜体", "<i/>" in styles_xml and 'rgb="FF808080"' in styles_xml, "styles.xml")

    headers_list = read_template_headers(template_path)
    print(f"   模板表头（{len(headers_list)} 列）：{headers_list}")
    expect("② 表头 28 列且首位是「姓名*」", len(headers_list) == 28 and headers_list[0] == "姓名*", str(headers_list[:3]))
    expect("② 第 7 列是「部门名称*」", headers_list[6] == "部门名称*", headers_list[6])
    expect("② 末尾追加列是「工号」", headers_list[-1] == "工号", headers_list[-1])

    # ── ③ 上传 3 行（2 合法 + 1 部门不存在） ──
    def row(name: str, id_card: str, dept: str, mobile: str) -> list[str]:
        values = [""] * len(headers_list)
        values[headers_list.index("姓名*")] = name
        values[headers_list.index("性别*")] = "男"
        values[headers_list.index("证件类型*")] = "身份证"
        values[headers_list.index("身份证号")] = id_card
        values[headers_list.index("手机号")] = mobile
        values[headers_list.index("邮箱")] = f"{mobile}@example.com"
        values[headers_list.index("部门名称*")] = dept
        values[headers_list.index("用工类型*")] = "正式"
        values[headers_list.index("入职日期*")] = "2026-01-15"
        values[headers_list.index("试用期结束")] = "2026-04-15"
        values[headers_list.index("生日")] = "1990-05-20"
        values[headers_list.index("学历")] = "本科"
        values[headers_list.index("学校")] = "某某大学"
        values[headers_list.index("银行账号")] = "6222021234567890"
        values[headers_list.index("备注")] = "E2E 导入"
        return values

    # 部门：取库里真实部门名（导入按名称匹配，用户不会填 UUID）
    status, data = api_json("GET", "/api/system/dept/tree", None, token)
    dept_name = ""
    if status == 200 and data.get("code") == 0 and data.get("data"):
        dept_name = data["data"][0].get("dept_name", "")
    if not dept_name:
        bad("③ 取样部门", "部门树为空，无法验证导入")
        return 2
    print(f"   使用部门：{dept_name}")

    u = uuid.uuid4().hex[:6]
    # 身份证每轮不同（固定号会让第二轮跑变成「全部重复」，那是幂等用例 ⑥ 的事，
    # 不能污染 ③④ 的「2 行入库」）
    card1 = id_card("440203" + "1990" + "05" + "20" + uuid.uuid4().int.__str__()[:3].rjust(3, "0"))
    card2 = id_card("110105" + "1949" + "12" + "31" + uuid.uuid4().int.__str__()[:3].rjust(3, "0"))
    # 手机号同理每轮不同：库里已有同号的会被按「重复」跳过（幂等语义）
    phone1 = "138" + f"{uuid.uuid4().int % 100_000_000:08d}"
    phone2 = "139" + f"{uuid.uuid4().int % 100_000_000:08d}"
    good1 = row("张三" + u, card1, dept_name, phone1)
    good2 = row("李四" + u, card2, dept_name, phone2)
    evil = row("王五" + u, "", "不存在的部门" + u, "13700001111")
    upload_path = os.path.join(WORK, "员工花名册_3行.xlsx")
    write_xlsx(upload_path, [headers_list, good1, good2, evil])
    ok("③ 造 3 行测试文件", upload_path)

    with open(upload_path, "rb") as fh:
        content = fh.read()
    body, extra = multipart("file", os.path.basename(upload_path), content)
    headers_req = {"Authorization": f"Bearer {token}", **extra}
    status, raw, _ = request(
        "POST",
        "/api/business/impexp/import/upload?biz_type=hr.employee&mode=PARTIAL&duplicate=SKIP",
        body,
        headers_req,
    )
    report = json.loads(raw.decode("utf-8")) if status == 200 else {}
    if status != 200 or report.get("code") != 0:
        bad("③ 上传预检", f"status={status} body={raw[:300]}")
        return 1
    first_task = report["data"]
    rep = first_task["report"]
    print(f"   预检报告：total={rep['total']} valid={rep['valid']} error={rep['error']} errors={rep['errors']}")
    expect("③ 预检：3 行数据", rep["total"] == 3, str(rep["total"]))
    expect("③ 预检：2 行可导入", rep["valid"] == 2, str(rep["valid"]))
    expect("③ 预检：1 行错误", rep["error"] == 1, str(rep["error"]))
    err = rep["errors"][0] if rep["errors"] else {}
    expect("③ 错误行带 Excel 行号（第 4 行）", err.get("row_no") == 4, str(err.get("row_no")))
    expect("③ 错误原因是部门不存在", "部门" in (err.get("message") or ""), err.get("message", ""))
    expect("③ 预检不写库（任务状态 VALIDATED）", first_task.get("status") == "VALIDATED", first_task.get("status", ""))

    # ── ④ 确认入库 ──
    status, data = api_json("POST", f"/api/business/impexp/import/{first_task['task_id']}/confirm", None, token)
    if status != 200 or data.get("code") != 0:
        bad("④ 确认入库", f"status={status} body={data}")
        return 1
    confirm = data["data"]
    print(f"   确认回执：{confirm}")
    expect("④ 入库 2 行", confirm["imported_rows"] == 2, str(confirm["imported_rows"]))
    expect("④ 状态 PARTIAL（有错行但合法行已入库）", confirm["status"] == "PARTIAL", confirm["status"])

    status, data = api_json("GET", "/api/business/hr/employee/page?page=1&page_size=50&keyword=" + u, None, token)
    rows = data.get("data", {}).get("list", []) if status == 200 else []
    expect("④ 列表能查到 2 人", len(rows) == 2, f"实际 {len(rows)}：{[r.get('name') for r in rows]}")
    emp_nos = sorted(r.get("emp_no") or "" for r in rows)
    expect("④ 工号自动取号（EMP-xxxx）", all(no.startswith("EMP-") for no in emp_nos), str(emp_nos))
    expect(
        "④ 部门名实时 join 回显",
        all(r.get("dept_name") == dept_name for r in rows),
        str([r.get("dept_name") for r in rows]),
    )

    # ── ⑤ 下载错误行 ──
    status, raw, headers = request(
        "GET",
        f"/api/business/impexp/import/{first_task['task_id']}/errors/file",
        None,
        {"Authorization": f"Bearer {token}"},
    )
    errors_path = os.path.join(WORK, "错误行.xlsx")
    if status == 200:
        with open(errors_path, "wb") as fh:
            fh.write(raw)
        ok("⑤ 错误行下载（HTTP 200）", f"{len(raw)} 字节")
        with zipfile.ZipFile(errors_path) as zf:
            err_shared = zf.read("xl/sharedStrings.xml").decode("utf-8")
            err_sheet = zf.read("xl/worksheets/sheet1.xml").decode("utf-8")
        expect("⑤ 错误行文件里有「错误原因」列", "错误原因" in err_shared, "sharedStrings")
        expect("⑤ 错误行文件里有「错误列」列", "错误列" in err_shared, "sharedStrings")
        expect("⑤ 带上原始行数据（王五 / 部门名 / 手机号）", "王五" in err_shared and "13700001111" in err_shared, "sharedStrings")
        expect("⑤ 不含已入库行的姓名（张三 / 李四）", "张三" + u not in err_shared and "李四" + u not in err_shared, "sharedStrings")
        expect("⑤ 只有一行数据（同一行的多条错误合并成一行）", err_sheet.count("<row ") == 2, f"实际 {err_sheet.count('<row ')} 行（含表头）")
    else:
        bad("⑤ 错误行下载", f"status={status}")

    # ── ⑥ 幂等：同一文件再导一遍 ──
    body, extra = multipart("file", os.path.basename(upload_path), content)
    status, raw, _ = request(
        "POST",
        "/api/business/impexp/import/upload?biz_type=hr.employee&mode=PARTIAL&duplicate=SKIP",
        body,
        {"Authorization": f"Bearer {token}", **extra},
    )
    again = json.loads(raw.decode("utf-8")) if status == 200 else {}
    if status != 200 or again.get("code") != 0:
        bad("⑥ 二次上传", f"status={status} body={raw[:300]}")
        return 1
    second_task = again["data"]
    second_rep = second_task["report"]
    expect("⑥ 二次预检：2 行仍通过校验（库内重复不算错误行）", second_rep["valid"] == 2, str(second_rep["valid"]))
    status, data = api_json("POST", f"/api/business/impexp/import/{second_task['task_id']}/confirm", None, token)
    second_confirm = data.get("data", {}) if status == 200 else {}
    print(f"   二次确认回执：{second_confirm}")
    expect("⑥ 二次确认：0 行入库（全部按重复跳过）", second_confirm.get("imported_rows") == 0, str(second_confirm.get("imported_rows")))
    skipped = second_rep["valid"] - (second_confirm.get("imported_rows") or 0)
    expect("⑥ 跳过数 = 预检通过 − 实际入库 = 2（页面显示「已跳过 2 行（重复）」）", skipped == 2, str(skipped))
    status, data = api_json("GET", "/api/business/hr/employee/page?page=1&page_size=50&keyword=" + u, None, token)
    rows2 = data.get("data", {}).get("list", []) if status == 200 else []
    expect("⑥ 库里仍是 2 人（没多出重复档案）", len(rows2) == 2, str(len(rows2)))

    with open(os.path.join(WORK, "00-e2e-checkpoints.txt"), "w", encoding="utf-8") as fh:
        fh.write("\n".join(CHECKPOINTS) + "\n")

    print(f"\n== 结果：{PASS} 通过 / {FAIL} 失败（证据目录 {WORK}）==")
    return 0 if FAIL == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
