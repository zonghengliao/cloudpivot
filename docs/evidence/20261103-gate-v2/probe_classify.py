"""对照 probe：复现旧闸②（不跳过测试面）的 35 文件，并按口径 v2 逐文件分类差异。"""
import sys
from pathlib import Path

sys.path.insert(0, "/opt/cloudpivot/scripts/lint")
import scope_gate as g

src = Path("/opt/cloudpivot/backend/src")
files = g.load_files(src)
skipped = g.collect_test_surface(files)


def failures(production_only: bool, skip_inline: bool):
    out = {}
    for path, info in files.items():
        if production_only and path in skipped:
            continue
        if "SCOPE-EXEMPT" in info.text:
            continue
        if skip_inline:
            lines = g.code_lines(info)
        else:
            lines = [
                ln
                for ln in info.text.splitlines()
                if not g.COMMENT_LINE_RE.match(ln)
            ]
        hits = [t for t in g.TABLES if any(f"FROM {t}" in ln for ln in lines)]
        if not hits:
            continue
        if any("scope::" in ln for ln in lines):
            continue
        out[info.path.relative_to(src).as_posix()] = sorted(set(hits))
    return out


legacy = failures(production_only=False, skip_inline=False)
v2 = failures(production_only=True, skip_inline=True)
inline_only = failures(production_only=True, skip_inline=False)

print(f"旧闸②（不跳过测试面）：{len(legacy)} 个文件")
print(f"只跳过内联测试区（仍含测试模块文件）：{len(inline_only)} 个文件")
print(f"口径 v2（两种都跳）：{len(v2)} 个文件")
print()
print("差异分类：")
for rel in sorted(set(legacy) - set(v2)):
    if rel in [i.path.relative_to(src).as_posix() for i in files.values() if i.path.resolve() in skipped]:
        print(f"  整文件跳过（父模块 #[cfg(test)] 声明）: {rel}")
    else:
        print(f"  仅内联 #[cfg(test)] 区跳过: {rel}")
print()
print("v2 仍失败（生产面）：")
for rel, ts in sorted(v2.items()):
    print(f"  {rel} ({', '.join(ts)})")
print()
print("v2 失败 == 内联口径失败 的差集：", sorted(set(inline_only) - set(v2)))
