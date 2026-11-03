#!/usr/bin/env python3
"""闸② 解析器的边角探针：cfg 变体、未闭合花括号、同一行混排生产代码。"""
import os
import shutil
import subprocess
import os
import sys
from pathlib import Path

BASE = Path(os.environ.get("PROBE_DIR", "/tmp")) / "scope_edges"
SCRIPT = "/opt/cloudpivot/scripts/lint/scope_gate.py"
REAL = Path(os.environ.get("CP_SRC", "/opt/cloudpivot/backend/src"))

PROBES = {
    # U1: mod tests 花括号未闭合（文件被截断） → 期望：保守，后文生产查询仍报红
    "zz_u1.rs": (
        "pub fn before() -> i32 { 1 }\n"
        "#[cfg(test)]\n"
        "mod tests {\n"
        "    fn t() -> i32 { 1 }\n"
        "pub fn after() {\n"
        '    let _sql = "SELECT * FROM sys_notice";\n'
        "}\n"
    ),
    # U2: 生产代码与 #[cfg(test)] mod 同一行 → 行粒度跳过是否把生产查询吞掉
    "zz_u2.rs": (
        'pub fn q() { let _sql = "SELECT * FROM sys_notice"; } #[cfg(test)] mod tests { fn t() {} }\n'
    ),
    # U3: cfg(all(test, feature)) → 只在测试编译，应跳过（不报红）
    "zz_u3.rs": (
        "#[cfg(all(test, feature = \"db\"))]\n"
        "mod tests {\n"
        '    fn q() { let _sql = "SELECT * FROM sys_notice"; }\n'
        "}\n"
    ),
    # U4: cfg(not(test)) → 只会在生产编译，**不许**跳过（漏接必须报红）
    "zz_u4.rs": (
        "#[cfg(not(test))]\n"
        "mod prod_only {\n"
        '    fn q() { let _sql = "SELECT * FROM sys_notice"; }\n'
        "}\n"
    ),
    # U5: cfg(any(test, feature)) → 可能进生产，不许跳过
    "zz_u5.rs": (
        "#[cfg(any(test, feature = \"db\"))]\n"
        "mod maybe_prod {\n"
        '    fn q() { let _sql = "SELECT * FROM sys_notice"; }\n'
        "}\n"
    ),
    # U6: 内联测试区里嵌套一层生产风味代码 + 后续生产函数（区间收尾复核）
    "zz_u6.rs": (
        "#[cfg(test)]\n"
        "mod tests {\n"
        "    mod inner {\n"
        '        fn q() { let _sql = "SELECT * FROM biz_task"; }\n'
        "    }\n"
        "}\n"
        "pub fn tail() {\n"
        '    let _sql = "SELECT * FROM sys_user";\n'
        "}\n"
    ),
}


def main():
    if BASE.exists():
        shutil.rmtree(BASE)
    (BASE / "backend").mkdir(parents=True)
    shutil.copytree(REAL, BASE / "backend" / "src")
    for name, body in PROBES.items():
        (BASE / "backend/src/business/task" / name).write_text(body, encoding="utf-8")
    mod = BASE / "backend/src/business/task/mod.rs"
    mod.write_text(
        mod.read_text(encoding="utf-8") + "\n" + "".join(f"mod {n[:-3]};\n" for n in PROBES),
        encoding="utf-8",
    )
    out = subprocess.run(
        [sys.executable, SCRIPT, "--root", str(BASE), "--list-skipped"],
        capture_output=True, text=True,
    )
    print(f"$ scope_gate.py --root {BASE}   EXIT={out.returncode}")
    for ln in (out.stdout + out.stderr).splitlines():
        if "FAIL" in ln or "扫描生产面" in ln or "zz_u" in ln:
            if "←" in ln or "FAIL:" in ln or "扫描生产面" in ln:
                print("  " + ln.strip())


if __name__ == "__main__":
    sys.exit(main())
