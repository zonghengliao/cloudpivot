#!/usr/bin/env python3
"""把旧 G5（pre-v2）实际报的 37 处，逐处回读源码行，独立归类：
   A=方法调用（前一个非空字符 '.'）  B=本地 fn record 遮蔽下的裸调
   C=首参形参声明为 Transaction   D=其他（=真实违规候选）
"""
import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path("/opt/cloudpivot")
OLD = str(Path(os.environ.get("PROBE_DIR", "/tmp")) / "g5_old.py")
if not Path(OLD).exists():
    import subprocess as _sp
    Path(OLD).write_text(
        _sp.run(["git", "-C", str(ROOT), "show",
                 "5408859^:scripts/lint/changelog_in_tx.py"],
                capture_output=True, text=True, check=True).stdout,
        encoding="utf-8",
    )

out = subprocess.run([sys.executable, OLD, "--root", str(ROOT)], capture_output=True, text=True).stdout
lines = [ln for ln in out.splitlines() if re.match(r"^\s+backend/src/.+:\d+: ", ln)]
print(f"旧脚本报告的调用点数：{len(lines)}")

classified = {"A_method_call": [], "B_local_shadow": [], "C_typed_param": [], "D_UNKNOWN": []}
shadow_files = set()

for ln in lines:
    m = re.match(r"^\s+(backend/src/[^:]+):(\d+): (.*)$", ln)
    rel, lineno, rest = m.group(1), int(m.group(2)), m.group(3)
    src = (ROOT / rel).read_text(encoding="utf-8").splitlines()
    text = src[lineno - 1]
    # 找该行上方最近的 fn 定义，若本文件有 `fn record(` 且本处是裸调 → B
    prev_char_idx = None
    col = text.find("record")
    while col != -1:
        before = text[:col].rstrip()
        if before and before[-1] not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_:":
            prev_char_idx = before[-1]
            break
        col = text.find("record", col + 1)
    whole = "\n".join(src)
    is_method = prev_char_idx == "."
    # 形参类型：往上找最近的 fn 行（含多行签名）
    fn_line = None
    for i in range(lineno - 1, max(-1, lineno - 30), -1):
        if re.search(r"\bfn\s+[A-Za-z_]\w*\s*[<(]", src[i]):
            fn_line = i
            break
    sig = ""
    if fn_line is not None:
        sig = " ".join(x.strip() for x in src[fn_line : min(fn_line + 8, len(src))])
    # 首参
    after = text[text.find("record"):]
    arg = ""
    if "(" in after:
        arg = after.split("(", 1)[1]
        depth = 1
        buf = []
        for ch in arg:
            if ch in "([{":
                depth += 1
            elif ch in ")]}":
                depth -= 1
                if depth == 0:
                    break
            if depth == 1 and ch == ",":
                break
            buf.append(ch)
        arg = "".join(buf).strip()

    has_local_def = bool(re.search(r"\bfn\s+record\s*\(", whole))
    qual = "changelog::record" in text[: col + 20] if col >= 0 else False

    if is_method:
        classified["A_method_call"].append((rel, lineno, text.strip()[:90]))
    elif has_local_def and not qual:
        shadow_files.add(rel)
        classified["B_local_shadow"].append((rel, lineno, text.strip()[:90]))
    elif re.search(r":\s*&?(?:mut\s+)?(?:[A-Za-z_]\w*::)*Transaction\b", sig) and re.fullmatch(
        r"[A-Za-z_]\w*", arg
    ):
        classified["C_typed_param"].append((rel, lineno, f"arg={arg} | sig={sig[:120]}"))
    else:
        classified["D_UNKNOWN"].append((rel, lineno, f"arg={arg} | sig={sig[:120]} | line={text.strip()[:80]}"))

for name, items in classified.items():
    print(f"\n--- {name}: {len(items)} 处 ---")
    for rel, lineno, extra in items:
        print(f"    {rel}:{lineno}   {extra}")
print("\n=== 汇总 ===")
print({k: len(v) for k, v in classified.items()}, "合计", sum(len(v) for v in classified.values()))
print("有本地 fn record 的文件：", sorted(shadow_files))
