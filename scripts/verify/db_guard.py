#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""e2e / 验收脚本的环境守卫：**禁止把测试打到生产库**
==========================================================================
为什么有这个文件
----------------
2026-10-30/31 两天，生产库（144 / `cloudpivot`）被写进了一批测试残留：

* `biz_flow_instance` 37 行活实例（`biz_type` = `E2E_AP_<12位随机>`，用户能在「我发起的」
  里看见），
* `biz_flow_def` 85 行定义（82 个 `E2E_*` + 3 个 `D2_SMOKE`）+ 85 个版本 / 330 节点 /
  245 条边，
* 合计让 `biz_type_domain.py --strict` 常年红。

成因不是某一条命令写错，而是**没有任何一道闸**：脚本的库名可以从
`/etc/cloudpivot/cloudpivot.env`（生产那套）里解析出来，`source` 一下就指向生产。
`frontend/e2e/target-guard.mjs` 管住了「浏览器打哪个 HTTP 目标」，但**管不住「脚本连哪个库」**
（Python 侧直连 psql 的验收脚本完全不经过它）。本模块补的就是这一层。

判据（唯一的真相：**生产库身份从 env 文件解析，不写死库名**）
------------------------------------------------------------
1. 生产库身份 = `/etc/cloudpivot/cloudpivot.env` 里的 `NIK_DATABASE_NAME` /
   `NIK_DATABASE_HOST` / `NIK_DATABASE_PORT`（该文件不存在 → 本机不是生产机，判据退化为
   「库名带 prod 段就算生产」）；
2. 目标库判定为「生产」的任一条件成立即**拒绝**：
   * 库名 == 生产库名；
   * 库名含 `prod` 段（`cloudpivot_prod` / `prod_erp` / 单独 `prod`）；
   * 库名解析不出来（空）→ **fail-closed 拒绝**（宁可误杀，不可放过）；
3. 唯一放行口（与 `frontend/e2e/target-guard.mjs` 同一对变量，两个运行时同一套规矩）：

       E2E_ALLOW_PROD=1 E2E_PROD_REASON='<谁批准/为什么，至少 8 字>'

   少给一个照样拒绝；放行后打醒目横幅，并 append 到 `/tmp/e2e-prod-target.log` 留痕。

用法
----
代码里（脚本启动第一步）：

    from db_guard import require_non_prod
    require_non_prod(script=__file__)

命令行：

    python3 scripts/verify/db_guard.py --check cloudpivot        # 非 0 = 拒绝
    python3 scripts/verify/db_guard.py --check cloudpivot_d2v    # 0 = 放行
    python3 scripts/verify/db_guard.py --self-test               # 判据自测（不连库）

退出码：0 = 放行；3 = 拒绝（生产库 / 库名缺失）；2 = 参数错。
"""
from __future__ import annotations

import argparse
import os
import re
import sys
from pathlib import Path

PROD_ENV_FILE = Path(os.environ.get("CLOUDPIVOT_ENV_FILE", "/etc/cloudpivot/cloudpivot.env"))
# 生产库名兜底：本机看不到 env 文件时用它（见 prod_identity 的注释）
BUILTIN_PROD_DB = "cloudpivot"

# 会被 `source /etc/cloudpivot/cloudpivot.env` 填上的「全局库名」变量，按优先级排列。
# 脚本自己的 `<PREFIX>_DB_NAME` 默认值一律是测试库（scripts/verify/*_e2e.py 逐个核过），
# 所以「打进生产」的唯一现实路径就是这几个全局变量被 source 进来。
GLOBAL_DB_ENV = ("DB_NAME", "NIK_DATABASE_NAME", "ALUN_DATABASE_NAME")
GLOBAL_HOST_ENV = ("DB_HOST", "NIK_DATABASE_HOST", "ALUN_DATABASE_HOST")
GLOBAL_PORT_ENV = ("DB_PORT", "NIK_DATABASE_PORT", "ALUN_DATABASE_PORT")

PROD_MARK = re.compile(r"(^|[_\-])prod([_\-]|$)|^prod|prod$", re.IGNORECASE)
TRAIL_LOG = Path("/tmp/e2e-prod-target.log")


def read_env_file(path: Path) -> dict[str, str]:
    """读 systemd EnvironmentFile 风格键值（容忍引号 / 注释 / 空行）。"""
    out: dict[str, str] = {}
    try:
        with path.open(encoding="utf-8") as fh:
            for line in fh:
                line = line.strip()
                if not line or line.startswith("#") or "=" not in line:
                    continue
                key, _, val = line.partition("=")
                out[key.strip()] = val.strip().strip('"').strip("'")
    except OSError:
        pass
    return out


def prod_identity(env_file: Path | None = None) -> dict[str, str]:
    """生产库身份：{db, host, port}（解析不到就是空串，调用方按 fail-closed 处理）。

    本机没有环境文件时（开发机 / 容器）退化为**内置兜底**：生产库名 `cloudpivot`。
    这一条很重要 —— 守卫不能因为「这台机器看不到生产 env」就失效，
    否则开发机上 `DB_NAME=cloudpivot python3 scripts/verify/x_e2e.py` 仍然能打到生产。
    """
    path = env_file or PROD_ENV_FILE
    env = read_env_file(path)
    db = env.get("NIK_DATABASE_NAME") or env.get("ALUN_DATABASE_NAME") or ""
    host = env.get("NIK_DATABASE_HOST") or env.get("ALUN_DATABASE_HOST") or ""
    port = env.get("NIK_DATABASE_PORT") or env.get("ALUN_DATABASE_PORT") or ""
    if not db:
        return {"db": BUILTIN_PROD_DB, "host": host.strip(), "port": port.strip(),
                "env_file": f"内置兜底（{path} 不存在或没有 NIK_DATABASE_NAME）"}
    return {"db": db.strip(), "host": host.strip(), "port": port.strip(), "env_file": str(path)}


def _first_env(names: tuple[str, ...]) -> str:
    for name in names:
        val = os.environ.get(name)
        if val:
            return val.strip()
    return ""


def classify_db(db_name: str, host: str = "", port: str = "", prod: dict[str, str] | None = None) -> dict:
    """判定目标库：返回 {prod: bool, reason: str, db: str}。纯函数，不连库、不读环境。"""
    prod = prod or {"db": "", "host": "", "port": ""}
    db = (db_name or "").strip()
    if not db:
        return {"prod": True, "db": db, "reason": "目标库名解析不出来（fail-closed：库不明就当生产处理）"}
    if prod["db"] and db == prod["db"]:
        return {
            "prod": True,
            "db": db,
            "reason": f"库名 == 生产库名 {prod['db']}（{prod.get('env_file', 'env 文件')}）",
        }
    if PROD_MARK.search(db):
        return {"prod": True, "db": db, "reason": f"库名 {db} 含 prod 段"}
    if (
        prod["db"]
        and db != prod["db"]
        and host
        and prod["host"]
        and host == prod["host"]
        and (not port or not prod["port"] or port == prod["port"])
        and db == "postgres"
    ):
        return {"prod": True, "db": db, "reason": "目标是生产库所在实例的 postgres 库"}
    return {"prod": False, "db": db, "reason": f"库名 {db} 不是生产库"}


def escape_hatch() -> dict:
    """放行生产所需的两个条件（与 frontend/e2e/target-guard.mjs 同一对变量）。"""
    flag = os.environ.get("E2E_ALLOW_PROD") == "1"
    reason = (os.environ.get("E2E_PROD_REASON") or "").strip()
    return {"granted": flag and len(reason) >= 8, "flag": flag, "reason": reason}


def _banner(script: str, verdict: dict, reason: str) -> None:
    lines = [
        "",
        "!" * 78,
        f"!! [db-guard] 已按 E2E_ALLOW_PROD=1 放行生产库：{verdict['db']}",
        f"!! 脚本：{script}",
        f"!! 原因：{reason}",
        f"!! 判据结论：{verdict['reason']}",
        "!! 跑完必须清掉自己造的夹具，并复跑：",
        "!!   python3 scripts/verify/biz_type_domain.py --strict",
        "!!   python3 scripts/verify/e2e_fixture_residue.py",
        "!" * 78,
        "",
    ]
    print("\n".join(lines), file=sys.stderr)
    try:
        with TRAIL_LOG.open("a", encoding="utf-8") as fh:
            fh.write(f"script={script} db={verdict['db']} reason={reason}\n")
    except OSError:
        pass


def require_non_prod(
    db_name: str | None = None,
    host: str | None = None,
    port: str | None = None,
    *,
    script: str = "",
    env_file: Path | None = None,
) -> dict:
    """目标库是生产就 `SystemExit(3)`。

    * 不传 `db_name` 时按 `GLOBAL_DB_ENV`（DB_NAME / NIK_DATABASE_NAME / ALUN_DATABASE_NAME）
      里第一个有值的变量判断 —— 那正是 `source /etc/cloudpivot/cloudpivot.env` 会填上的变量；
      全都没设 → 交给脚本自己的默认值（`scripts/verify/*_e2e.py` 的默认值逐个核过，全是测试库），放行。
    * 传了 `db_name` 就是硬判据，空串也拒绝（fail-closed）。
    """
    prod = prod_identity(env_file)
    if db_name is None:
        db_name = _first_env(GLOBAL_DB_ENV)
        if not db_name:
            return {"prod": False, "db": "", "reason": "未设置全局库名变量，交给脚本自身默认值（测试库）"}
    if host is None:
        host = _first_env(GLOBAL_HOST_ENV)
    if port is None:
        port = _first_env(GLOBAL_PORT_ENV)

    verdict = classify_db(db_name, host, port, prod)
    if not verdict["prod"]:
        print(f"[db-guard] OK 目标库 {verdict['db']}（{verdict['reason']}）", file=sys.stderr)
        return verdict
    if escape_hatch()["granted"]:
        _banner(script or "", verdict, escape_hatch()["reason"])
        return verdict
    sys.stderr.write(
        "\n!! [db-guard] 拒绝执行：%s\n"
        "!! 目标库 = %s%s\n"
        "!! e2e / 验收脚本不允许打到生产库（2026-10-30/31 的 37 行 E2E_AP_* 实例 + 85 条 E2E 流程定义\n"
        "!! 就是这么进的库，见 t_61dca492）。\n"
        "!! 正确做法：\n"
        "!!   ① 起隔离实例：bash scripts/verify/e2e_isolated_instance.sh，或用它的库名\n"
        "!!   ② 只连测试库：DB_NAME=cloudpivot_<tag> python3 %s\n"
        "!!   ③ 确需打生产（线上验收）：E2E_ALLOW_PROD=1 E2E_PROD_REASON='<谁批准/为什么>' python3 %s\n"
        "!!      跑完复跑 biz_type_domain.py --strict && e2e_fixture_residue.py\n"
        % (verdict["reason"], verdict["db"], f"（{host}:{port}）" if host else "", script or "<script>", script or "<script>")
    )
    raise SystemExit(3)


# ---------------------------------------------------------------------------
# 自测（不连库、不读真实 env 文件；用假 prod 身份钉住判据）
# ---------------------------------------------------------------------------
def self_test() -> int:
    fake_prod = {"db": "cloudpivot", "host": "127.0.0.1", "port": "5432", "env_file": "/etc/cloudpivot/cloudpivot.env"}
    cases = [
        # (db, host, port, 期望 prod?)
        ("cloudpivot", "", "", True),            # 生产库名本身
        ("cloudpivot_prod", "", "", True),       # 含 prod 段
        ("prod_erp", "", "", True),
        ("prod", "", "", True),
        ("cloudpivot_d2v", "", "", False),       # 测试库
        ("cloudpivot_e2eres_drill", "", "", False),
        ("cp_dc01_e2e", "", "", False),
        ("cloudpivot_test_9476", "", "", False),
        ("", "", "", True),                       # fail-closed
        ("postgres", "127.0.0.1", "5432", True),  # 生产实例上的 postgres 库
        ("postgres", "10.0.0.5", "5432", False),  # 别的实例的 postgres 库
    ]
    failed = 0
    for db, host, port, want in cases:
        got = classify_db(db, host, port, fake_prod)["prod"]
        flag = "ok  " if got == want else "FAIL"
        if got != want:
            failed += 1
        print(f"[{flag}] db={db!r:<26} host={host or '-'!s:<10} port={port or '-':<5} prod={got} want={want}")
    print(f"---- self-test：{len(cases) - failed}/{len(cases)} 通过 ----")
    return 0 if failed == 0 else 1


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="e2e / 验收脚本的环境守卫：禁止把测试打到生产库")
    parser.add_argument("--check", metavar="DB", help="检查一个库名：0=放行，3=拒绝")
    parser.add_argument("--host", default="")
    parser.add_argument("--port", default="")
    parser.add_argument("--env-file", default=str(PROD_ENV_FILE), help="生产身份来源（默认 /etc/cloudpivot/cloudpivot.env）")
    parser.add_argument("--self-test", action="store_true", help="跑判据自测（不连库）")
    args = parser.parse_args(argv)

    if args.self_test:
        return self_test()
    if args.check is None:
        parser.print_help()
        return 2
    verdict = require_non_prod(args.check, args.host, args.port, script="db_guard.py --check",
                               env_file=Path(args.env_file))
    print(("SAFE " if not verdict["prod"] else "PROD ") + f"{verdict['db']} — {verdict['reason']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
