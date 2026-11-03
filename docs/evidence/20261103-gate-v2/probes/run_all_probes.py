#!/usr/bin/env python3
"""把复核期间所有探针的原始输出汇总成一个证据文件（可复跑）。

用法：python3 run_all_probes.py <输出文件>
"""
import os
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRATCH = Path(os.environ.get("PROBE_DIR", "/tmp"))
PROBES = {
    "G5 自建负例（5 条必核 + 我的额外探针）": [sys.executable, str(HERE / "g5_build_and_run.py")],
    "旧 G5 报的 37 处逐条归类": [sys.executable, str(HERE / "classify37.py")],
    "闸② 测试面判定绕过（T0–T7）": [sys.executable, str(HERE / "scope_bypass.py")],
    "闸② 解析器边角（U1–U6）": [sys.executable, str(HERE / "scope_edges.py")],
    "50 个跳过文件的 Rust 语义审计": [sys.executable, str(HERE / "audit_skips.py")],
    "抽检 3 个豁免文件的查询↔注释映射": [sys.executable, str(HERE / "sample_exempt.py")],
}


def main(out_path: str) -> int:
    env = dict(os.environ, TMPDIR=str(SCRATCH))
    lines = []
    for title, cmd in PROBES.items():
        lines.append("=" * 100)
        lines.append(f"## {title}")
        lines.append(f"$ {' '.join(cmd)}")
        r = subprocess.run(cmd, capture_output=True, text=True, env=env, cwd=str(SCRATCH))
        lines.append(f"EXIT={r.returncode}")
        lines.append(r.stdout.rstrip())
        if r.stderr.strip():
            lines.append("--- stderr ---")
            lines.append(r.stderr.rstrip())
        lines.append("")
        print(f"[done] {title}")
    Path(out_path).write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"写出 {out_path}（{len(lines)} 行）")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1]))
