#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""门禁：`sys_config.category` 的取值必须在字典 `config_category` 里存在。

为什么需要它（一次真实事故）
--------------------------
参数配置页（支撑功能 → 参数配置）的 tab 完全由 `sys_config.category` 驱动，
tab 中文名由字典 `config_category` 翻译。两张表的约定是**弱引用**：
category 是自由文本，字典只是个"翻译表"，没有任何数据库约束兜住。

于是每张新卡都会各造新分类：

  · V0405 整理前：erp / erp_budget / erp_internal_control / erp_master_data / erp_cost
    → 5 个 tab，其中 2 个各只有 1 项，且 5 个里 4 个在字典里查不到中文名
    → 页面直接显示英文编码，用户原话「很没规则，简直就是乱七八糟」
  · V0405 合并成 4 类后：M1/M2 的迁移又造了 erp_fx / erp_inventory / erp
    → 又冒出 3 个只有 1 项的英文 tab（V0441 才再合并一次）

**每次都是同一个坑**：迁移作者写 `INSERT INTO sys_config (..., category, ...)
VALUES (..., '某某新分类', ...)` 时，没人要求同时往 `config_category` 字典里
补一条中文名。数据库不拦、页面不报错，只是 tab 里多一个看不懂的英文单词。

本门禁把这条约定变成硬约束：
  **sys_config 里出现过的每个 category，必须在 config_category 字典里有对应项。**

判据
----
连库查两条：
  1. `SELECT DISTINCT category FROM sys_config WHERE deleted_at IS NULL`
  2. `SELECT item_value FROM sys_dict_item WHERE dict_type_id = (config_category 的 id)`

  差集非空 → 失败，并打印「哪个 category 缺、它下面有几条配置、都是什么 key」。

连接方式
--------
按优先级取 DSN：
  1. `$ERP_DB_URL`
  2. `$NIK_DATABASE_*`（从 `$NIK_ENV_FILE` 或默认 env 文件读）
  3. 都没有 → **跳过**（exit 0，CI/无库环境不阻断）

只读：本脚本只做 SELECT。

用法
----
    python3 scripts/lint/config_category_gate.py
    python3 scripts/lint/config_category_gate.py --root .
    python3 scripts/lint/config_category_gate.py --verbose
"""

import argparse
import os
import subprocess
import sys
from pathlib import Path

DEFAULT_ENV_FILES = [
    "/etc/cloudpivot/cloudpivot-dev.env",
    "/etc/cloudpivot/cloudpivot.env",
]


def find_env_file() -> Path | None:
    explicit = os.environ.get("NIK_ENV_FILE")
    if explicit and Path(explicit).is_file():
        return Path(explicit)
    for p in DEFAULT_ENV_FILES:
        if Path(p).is_file():
            return Path(p)
    return None


def read_env(path: Path) -> dict[str, str]:
    out = {}
    try:
        for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            k, v = line.split("=", 1)
            out[k.strip()] = v.strip().strip("'\"")
    except OSError:
        pass
    return out


def resolve_dsn() -> str | None:
    # ① ERP_DB_URL 直给
    if os.environ.get("ERP_DB_URL"):
        return os.environ["ERP_DB_URL"]

    # ② NIK_DATABASE_*
    env = {}
    ef = find_env_file()
    if ef:
        env = read_env(ef)
    for k in ("NIK_DATABASE_HOST", "NIK_DATABASE_PASSWORD", "NIK_DATABASE_NAME", "NIK_DATABASE_USER"):
        if os.environ.get(k):
            env[k] = os.environ[k]

    host = env.get("NIK_DATABASE_HOST")
    name = env.get("NIK_DATABASE_NAME")
    user = env.get("NIK_DATABASE_USER")
    pwd = env.get("NIK_DATABASE_PASSWORD")
    port = env.get("NIK_DATABASE_PORT", "5432")
    if not all([host, name, user]):
        return None
    return f"postgresql://{user}:{pwd}@{host}:{port}/{name}"


def psql(dsn: str, sql: str) -> tuple[int, str]:
    """跑一条 SQL，返回 (rc, stdout+stderr)。用 psql 子进程，避免引入 python 驱动。"""
    r = subprocess.run(
        ["psql", dsn, "-tAF", "|", "-c", sql],
        capture_output=True,
        text=True,
        timeout=30,
    )
    return r.returncode, (r.stdout or "") + (r.stderr or "")


def main() -> int:
    ap = argparse.ArgumentParser(description="sys_config.category 必须在 config_category 字典里")
    ap.add_argument("--root", default=None, help="兼容 check.sh 的 --root，仅用于定位，不改变逻辑")
    ap.add_argument("--verbose", action="store_true")
    args = ap.parse_args()

    dsn = resolve_dsn()
    if not dsn:
        print("  · 无数据库连接（无 ERP_DB_URL / NIK_DATABASE_*），跳过 category 门禁")
        return 0

    # ① 字典 id
    rc, out = psql(dsn, "SELECT id FROM sys_dict_type WHERE dict_code='config_category' AND deleted_at IS NULL LIMIT 1")
    if rc != 0:
        print(f"  ⚠ 连库失败，跳过：{out.strip()[:160]}")
        return 0
    dict_id = out.strip().split("\n")[0] if out.strip() else ""
    if not dict_id:
        print("  ⚠ 库里没有 config_category 字典类型，跳过（这套环境可能还没种字典）")
        return 0

    # ② 用到的 category
    rc, out = psql(dsn, """
        SELECT COALESCE(NULLIF(category,''),'(空)') || '|' || count(*) || '|' ||
               COALESCE(string_agg(config_key, ', ' ORDER BY config_key), '')
        FROM sys_config
        WHERE deleted_at IS NULL
        GROUP BY COALESCE(NULLIF(category,''),'(空)')
        ORDER BY 1
    """)
    if rc != 0:
        print(f"  ⚠ 查 sys_config 失败，跳过：{out.strip()[:160]}")
        return 0
    used = [ln for ln in out.strip().split("\n") if ln.strip()]
    if not used:
        print("  ⚠ sys_config 为空，跳过")
        return 0

    # ③ 字典里有的
    rc, out2 = psql(dsn, f"SELECT item_value FROM sys_dict_item WHERE dict_type_id='{dict_id}' AND status=1")
    if rc != 0:
        print(f"  ⚠ 查字典失败，跳过：{out2.strip()[:160]}")
        return 0
    in_dict = {ln.strip() for ln in out2.strip().split("\n") if ln.strip()}

    # ④ 差集
    missing = []
    for ln in used:
        parts = ln.split("|", 2)
        cat = parts[0]
        n = parts[1] if len(parts) > 1 else "?"
        keys = parts[2] if len(parts) > 2 else ""
        if cat == "(空)":
            missing.append((cat, n, keys, "category 为空 —— 页面不会显示在任何 tab"))
        elif cat not in in_dict:
            missing.append((cat, n, keys, "字典 config_category 里没有它的中文名"))

    print(f"  sys_config 用到 {len(used)} 个分类，字典覆盖 {len(in_dict)} 个")
    if args.verbose:
        for ln in used:
            cat = ln.split("|")[0]
            mark = "✅" if cat in in_dict or cat == "(空)" else "❌"
            print(f"    {mark} {ln}")

    if not missing:
        print("  ✓ 全部 category 都在字典里")
        return 0

    print("", file=sys.stderr)
    print(f"  ✗ {len(missing)} 个分类不合法：", file=sys.stderr)
    for cat, n, keys, reason in missing:
        print(f"      {cat:16} {n:>3} 项   {reason}", file=sys.stderr)
        if keys:
            print(f"       配置项: {keys[:120]}", file=sys.stderr)
    print("", file=sys.stderr)
    print("    修法（二选一）：", file=sys.stderr)
    print("      A. 归并到已有分类（推荐）：UPDATE sys_config SET category='erp_basic' WHERE category='<你的新分类>';", file=sys.stderr)
    print("      B. 确实需要独立 tab：往字典 config_category 插一条中文名", file=sys.stderr)
    print("         INSERT INTO sys_dict_item (id,dict_type_id,item_value,item_label,sort_order,status,created_at,updated_at)", file=sys.stderr)
    print(f"         VALUES (gen_random_uuid(),'<config_category 的 id>','<你的新分类>','<中文名>',<排序>,1,now(),now());", file=sys.stderr)
    print("    并且**必须写成迁移**（幂等），不要手改库。", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
