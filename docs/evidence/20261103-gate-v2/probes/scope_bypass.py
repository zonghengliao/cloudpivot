#!/usr/bin/env python3
"""独立复核用：在 backend/src 的**副本**上构造「测试面判定绕过」反例，跑真实 scope_gate.py。

不改 main：整棵树复制到 scratch 下操作。
"""
import os
import shutil
import subprocess
import os
import sys
from pathlib import Path

BASE = Path(os.environ.get("PROBE_DIR", "/tmp")) / "scope_bypass"
SRC = BASE / "backend" / "src"
SCRIPT = "/opt/cloudpivot/scripts/lint/scope_gate.py"
REAL = Path(os.environ.get("CP_SRC", "/opt/cloudpivot/backend/src"))


def run(root, extra=()):
    cmd = [sys.executable, str(SCRIPT), "--root", str(root), *extra]
    out = subprocess.run(cmd, capture_output=True, text=True)
    return out.returncode, out.stdout + out.stderr


def fresh(name):
    root = BASE / name
    if root.exists():
        shutil.rmtree(root)
    (root / "backend").mkdir(parents=True)
    shutil.copytree(REAL, root / "backend" / "src")
    return root


def mount(root, rel_parent, line):
    p = root / "backend" / "src" / rel_parent
    p.write_text(p.read_text(encoding="utf-8") + line, encoding="utf-8")


def show(tag, root, extra=()):
    code, text = run(root, extra)
    keep = [ln for ln in text.splitlines() if ("扫描生产面" in ln or "FAIL" in ln or "PASS" in ln)]
    print(f"\n### {tag}  → exit={code}")
    for ln in keep[:12]:
        print("   " + ln)
    return code, text


def main():
    BASE.mkdir(parents=True, exist_ok=True)

    # ---------------- 基线 ----------------
    root0 = fresh("t0_baseline")
    show("T0 基线（未改动副本）", root0)

    # ---------------- T2: 生产文件里插一句裸 #[cfg(test)]（不给 mod） ----------------
    root2 = fresh("t2_dangling_cfg")
    (root2 / "backend/src/business/task/zz_a.rs").write_text(
        '#[cfg(test)]\npub fn marker() -> i32 { 0 }\n\n'
        'pub fn q() {\n    let _sql = "SELECT * FROM sys_notice";\n}\n',
        encoding="utf-8",
    )
    mount(root2, "business/task/mod.rs", "\nmod zz_a;\n")
    code2, text2 = show("T2 生产文件里裸 #[cfg(test)]（后文生产查询 sys_notice）", root2)
    print("    zz_a 是否被判红：", "zz_a" in text2)

    # ---------------- T3: #[cfg(test)] mod tests {…} 之后再有生产函数 ----------------
    root3 = fresh("t3_region_end")
    (root3 / "backend/src/business/task/zz_b.rs").write_text(
        "pub fn before() -> i32 { 1 }\n"
        "\n"
        "#[cfg(test)]\n"
        "mod tests {\n"
        "    fn q() {\n"
        '        let _sql = "SELECT * FROM biz_task";\n'
        "    }\n"
        "}\n"
        "\n"
        "pub fn after() {\n"
        '    let _sql = "SELECT * FROM sys_user";\n'
        "}\n",
        encoding="utf-8",
    )
    mount(root3, "business/task/mod.rs", "\nmod zz_b;\n")
    code3, text3 = show("T3 cfg(test) mod 之后的生产函数（sys_user）", root3)
    print("    报红内容：", [ln.strip() for ln in text3.splitlines() if "zz_b" in ln])
    print("    biz_task（测试区内）是否被误报：", "biz_task" in "".join(
        ln for ln in text3.splitlines() if "zz_b" in ln))

    # ---------------- T4: 同一文件被 #[cfg(test)] mod 与普通 mod 双指向 ----------------
    root4 = fresh("t4_double_mount")
    (root4 / "backend/src/business/task/zz_c.rs").write_text(
        'pub fn q() {\n    let _sql = "SELECT * FROM sys_notice";\n}\n', encoding="utf-8"
    )
    mount(root4, "business/task/mod.rs", "\nmod zz_c;\n")
    mount(root4, "business/task/model.rs", '\n#[cfg(test)]\nmod zz_c;\n')
    code4, text4 = show("T4 同一文件被普通 mod 与 #[cfg(test)] mod 双指向", root4)
    print("    zz_c 被跳过：", "zz_c" in text4 and "跳过" in text4.split("FAIL")[0])
    print("    zz_c 报红：", "zz_c" in "".join(ln for ln in text4.splitlines() if "FAIL" in ln))
    _c, full4 = run(root4, ("--list-skipped",))
    print("    --list-skipped 里 zz_c 那行：", [ln.strip() for ln in full4.splitlines() if "zz_c" in ln])

    # ---------------- T5: #[cfg(test)] #[path=…] 凭空挂载，把生产文件标成测试面 ----------------
    root5 = fresh("t5_phantom_path")
    (root5 / "backend/src/business/task/zz_d.rs").write_text(
        'pub fn q() {\n    let _sql = "SELECT * FROM sys_notice";\n}\n', encoding="utf-8"
    )
    mount(root5, "business/task/mod.rs", "\nmod zz_d;\n")
    code5a, text5a = show("T5a 先让 zz_d 报红（对照）", root5)
    # 再在别处凭空加一条 #[cfg(test)] #[path] 指向同一个文件
    mount(root5, "business/erp/approval_gate.rs",
          '\n#[cfg(test)]\n#[path = "../task/zz_d.rs"]\nmod ghost_copy;\n')
    code5b, text5b = show("T5b 加一条 #[cfg(test)] #[path=zz_d.rs] mod ghost_copy;", root5)
    print("    zz_d 报红：", "zz_d" in "".join(ln for ln in text5b.splitlines() if "FAIL" in ln))
    _c, full5 = run(root5, ("--list-skipped",))
    print("    --list-skipped 里 zz_d 那行：", [ln.strip() for ln in full5.splitlines() if "zz_d" in ln])

    # ---------------- T6: D1 硬断言（已知生产文件） ----------------
    root6 = fresh("t6_guard")
    p = root6 / "backend/src/business/task/mod.rs"
    if re_mod := __import__("re"):
        body = p.read_text(encoding="utf-8")
        body2 = re_mod.sub(r"^(\s*)mod task_model_query;", r"\1#[cfg(test)]\n\1mod task_model_query;",
                           body, count=1, flags=re_mod.M)
        p.write_text(body2, encoding="utf-8")
    code6, text6 = show("T6a 把 task_model_query.rs 声明成 #[cfg(test)]（改 mod.rs）", root6)
    print("    guard 报错：", [ln.strip() for ln in text6.splitlines() if "防假绿断言" in ln][:3])

    # 6b: 改脚本副本，把文件从扫描集合里剔掉
    root7 = fresh("t6b_drop_from_scan")
    patched = BASE / "scope_gate_drop.py"
    src = SCRIPT
    body = Path(src).read_text(encoding="utf-8")
    body = body.replace(
        "    production = {p: info for p, info in files.items() if p not in skipped}",
        "    production = {p: info for p, info in files.items() if p not in skipped}\n"
        "    production = {p: v for p, v in production.items() if p.name != 'task_model_query.rs'}",
    )
    if body == Path(src).read_text(encoding="utf-8"):
        print("!! 打补丁失败：没找到 production 那一行")
    patched.write_text(body, encoding="utf-8")
    out = subprocess.run(
        [sys.executable, str(patched), "--root", str(root7)], capture_output=True, text=True
    )
    print(f"\n### T6b 脚本副本把 task_model_query.rs 从扫描集合剔除  → exit={out.returncode}")
    for ln in (out.stdout + out.stderr).splitlines():
        if "FAIL" in ln or "扫描生产面" in ln:
            print("   " + ln)

    # ---------------- T7: SCOPE-EXEMPT 的粒度（整文件豁免） ----------------
    root8 = fresh("t7_exempt_blast_radius")
    (root8 / "backend/src/business/task/zz_e.rs").write_text(
        "// SCOPE-EXEMPT: 只有下面这个 aggs 函数是计数，不产出行集\n"
        "pub fn aggs() {\n"
        '    let _sql = "SELECT count(*) FROM biz_task";\n'
        "}\n"
        "\n"
        "/// 用户可见的任务列表分页（本该接 scope::，这里故意不接）\n"
        "pub fn user_visible_list(dept: i64) -> String {\n"
        '    format!("SELECT t.* FROM biz_task t JOIN sys_user u ON u.id = t.owner_id "\n'
        '            + " WHERE t.dept_id = {} ORDER BY t.id DESC LIMIT 20", dept)\n'
        "}\n",
        encoding="utf-8",
    )
    mount(root8, "business/task/mod.rs", "\nmod zz_e;\n")
    code8, text8 = show("T7 一个 SCOPE-EXEMPT 注释是否连带豁免同文件里的用户可见列表查询", root8)
    print("    zz_e 报红：", "zz_e" in "".join(ln for ln in text8.splitlines() if "FAIL" in ln))

    return 0


if __name__ == "__main__":
    sys.exit(main())
