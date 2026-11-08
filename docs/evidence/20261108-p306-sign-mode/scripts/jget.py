#!/usr/bin/env python3
"""从 stdin 读 JSON，按 `a.b.0.c` 取值打印（不存在打印空串）。

为什么单独一个文件：`lib.sh` 里的 `jget` 要能在未设置 `python3 -c`（安全策略拦 heredoc）
的环境里工作 —— 把脚本落到仓库里既可复跑，也可审计。
"""
import json
import sys


def main() -> int:
    path = sys.argv[1] if len(sys.argv) > 1 else ""
    try:
        data = json.load(sys.stdin)
    except Exception:
        print("")
        return 0
    cur = data
    for key in path.split("."):
        if isinstance(cur, dict):
            cur = cur.get(key)
        elif isinstance(cur, list):
            try:
                cur = cur[int(key)]
            except Exception:
                cur = None
        else:
            cur = None
        if cur is None:
            break
    if cur is None:
        print("")
    elif isinstance(cur, (dict, list)):
        print(json.dumps(cur, ensure_ascii=False))
    else:
        print(cur)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
