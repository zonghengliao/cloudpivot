#!/usr/bin/env python3
"""生成一份**全新**的 3 行员工花名册（2 合法 + 1 部门不存在），给 UI 验收用。

为什么要单独一个脚本：UI 验收要看到「成功 2 行」，而 CLI E2E 已经把这批人导进库了 ——
同一份文件再导会全部按重复跳过（那是幂等用例的语义）。所以每次跑 UI 前现造一份
（姓名 / 身份证 / 手机号都带随机后缀）。

用法：python3 make_roster.py /root/emp-import-e2e/员工花名册_ui.xlsx
"""

from __future__ import annotations

import json
import os
import sys
import urllib.request
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import emp_import_e2e as core  # noqa: E402  （复用同一个 xlsx 写 / 表头读 / 登录实现）


def main() -> int:
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(core.WORK, "员工花名册_ui.xlsx")
    os.makedirs(os.path.dirname(out) or ".", exist_ok=True)
    token = core.login("admin", os.environ.get("ADMIN_PASSWORD", "Admin@123456"))

    status, raw, _ = core.request(
        "GET", "/api/business/hr/employee/import-template", None, {"Authorization": f"Bearer {token}"}
    )
    if status != 200:
        print(f"FATAL: 模板下载失败 status={status}", file=sys.stderr)
        return 2
    template = os.path.join(core.WORK, "员工档案导入模板.xlsx")
    with open(template, "wb") as fh:
        fh.write(raw)
    headers = core.read_template_headers(template)

    status, data = core.api_json("GET", "/api/system/dept/tree", None, token)
    dept_name = data["data"][0]["dept_name"] if status == 200 and data.get("data") else ""
    if not dept_name:
        print("FATAL: 库里没有部门", file=sys.stderr)
        return 2

    tag = uuid.uuid4().hex[:6]
    cards = []
    for prefix, birth in (("440203", "19900520"), ("110105", "19491231")):
        body = prefix + birth + f"{uuid.uuid4().int % 1000:03d}"
        cards.append(core.id_card(body))
    phones = [f"{prefix}{uuid.uuid4().int % 100_000_000:08d}" for prefix in ("138", "139")]

    def row(name: str, card: str, dept: str, mobile: str) -> list[str]:
        values = [""] * len(headers)
        values[headers.index("姓名*")] = name
        values[headers.index("性别*")] = "男"
        values[headers.index("证件类型*")] = "身份证"
        values[headers.index("身份证号")] = card
        values[headers.index("手机号")] = mobile
        values[headers.index("邮箱")] = f"{mobile}@example.com"
        values[headers.index("部门名称*")] = dept
        values[headers.index("用工类型*")] = "正式"
        values[headers.index("入职日期*")] = "2026-01-15"
        values[headers.index("生日")] = "1990-05-20"
        values[headers.index("学历")] = "本科"
        values[headers.index("工号")] = ""
        return values

    rows = [
        headers,
        row(f"UI张三{tag}", cards[0], dept_name, phones[0]),
        row(f"UI李四{tag}", cards[1], dept_name, phones[1]),
        row(f"UI王五{tag}", "", f"不存在的部门{tag}", f"137{uuid.uuid4().int % 100_000_000:08d}"),
    ]
    core.write_xlsx(out, rows)
    print(json.dumps({"file": out, "tag": tag, "dept": dept_name}, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    sys.exit(main())
