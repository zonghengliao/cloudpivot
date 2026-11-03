#!/usr/bin/env python3
"""审计：真实仓库里被闸② 跳过的 50 个文件，其声明位置是否**符合 Rust 模块语义**。

scope_gate.child_module_paths() 用 `parent.parent` 当子模块目录，这只对 `foo/mod.rs` 与
`src/lib.rs`/`src/main.rs` 成立；对 `foo.rs`（非 mod.rs）会把子模块目录算成**父目录**而不是 `foo/`。
本脚本按 Rust 真实语义重算，找出「被误跳」的生产文件。
"""
import importlib.util
import os
import sys
from pathlib import Path

SCRIPT = "/opt/cloudpivot/scripts/lint/scope_gate.py"
SRC = Path(os.environ.get("CP_SRC", "/opt/cloudpivot/backend/src"))

spec = importlib.util.spec_from_file_location("scope_gate", SCRIPT)
sg = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sg)


def rust_parent_dir(parent: Path) -> Path:
    """Rust 语义：mod.rs / lib.rs / main.rs 的子模块在自身目录；其余 foo.rs 的子模块在 foo/。"""
    if parent.name in ("mod.rs", "lib.rs", "main.rs"):
        return parent.parent
    return parent.parent / parent.stem


def main() -> int:
    files = sg.load_files(SRC)
    skipped = sg.collect_test_surface(files)

    # 反查：每个被跳过的文件，是谁声明的
    decls_by_target: dict[Path, list[tuple[Path, object]]] = {}
    for resolved, info in files.items():
        for decl in info.decls:
            for cand in sg.child_module_paths(resolved, decl.name, decl.path_attr):
                t = cand.resolve()
                if t in files:
                    decls_by_target.setdefault(t, []).append((resolved, decl))

    print(f"被跳过的测试面文件：{len(skipped)} 个")
    bad = []
    for target in sorted(skipped):
        rel = target.relative_to(SRC.resolve()).as_posix()
        entries = decls_by_target.get(target, [])
        detail = []
        for parent, decl in entries:
            prel = parent.relative_to(SRC.resolve()).as_posix()
            # 脚本算出来的路径
            script_paths = [p.resolve() for p in sg.child_module_paths(parent, decl.name, decl.path_attr)]
            # Rust 真实语义
            if decl.path_attr:
                rust_paths = [(parent.parent / decl.path_attr).resolve()]
            else:
                rp = rust_parent_dir(parent)
                rust_paths = [(rp / f"{decl.name}.rs").resolve(), (rp / decl.name / "mod.rs").resolve()]
            ok = target in rust_paths
            detail.append(f"{prel}:{decl.line} mod {decl.name}; → Rust={[str(p.relative_to(SRC.resolve())) for p in rust_paths if p.exists()] or 'none'} {'OK' if ok else 'MISRESOLVED'}")
            if not ok:
                bad.append((rel, detail[-1]))
        print(f"  {rel}")
        for d in detail:
            print(f"      ← {d}")

    print("\n=== 结论 ===")
    if bad:
        print(f"!! {len(bad)} 个文件按 Rust 语义**不该**被跳过（解析器误判）：")
        for rel, d in bad:
            print(f"   {rel}  ←  {d}")
    else:
        print("OK：所有跳过都能按 Rust 语义追溯到真正的 #[cfg(test)] 声明（无误跳）")

    # 额外：仓库里有没有「非 mod.rs 的文件」声明子模块（潜在误解析土壤）
    print("\n=== 非 mod.rs/lib.rs/main.rs 里声明 mod 的文件（误解析土壤） ===")
    n = 0
    for resolved, info in files.items():
        if not info.decls:
            continue
        if resolved.name in ("mod.rs", "lib.rs", "main.rs"):
            continue
        rel = resolved.relative_to(SRC.resolve()).as_posix()
        for d in info.decls:
            print(f"  {rel}:{d.line}  mod {d.name};   (parent.stem={resolved.stem})")
            n += 1
    print(f"共 {n} 条")
    return 0


if __name__ == "__main__":
    sys.exit(main())
