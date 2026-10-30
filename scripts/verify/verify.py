#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""云枢验证脚本统一入口。

用法：
    python3 verify.py all              # 跑全部（smoke + a + b + c + task + flow）
    python3 verify.py smoke            # 只读冒烟（A/B/C/D，35 项）
    python3 verify.py task             # C+ 任务管理（52 项）
    python3 verify.py flow             # 批次 D 审批流读写
    python3 verify.py a b c            # 按批次
    python3 verify.py smoke --batches a,c      # 冒烟只跑指定批次
    python3 verify.py task --modules t1,t2     # 任务管理只跑指定模块

环境变量：BASE_URL / TEST_USER / TEST_PASS / DB_* （见 common.py 顶部表）

退出码：0 = 全通过；1 = 有 FAIL。CI 直接看退出码。
"""

import argparse
import importlib
import sys

from common import Ctx, report, sweep

# 子命令 → 模块名
COMMANDS = {
    "smoke": "v_smoke",
    "task": "v_task",
    "flow": "v_flow",
    "a": "v_a",
    "b": "v_b",
    "c": "v_c",
}

# all 的执行顺序：只读先跑（快速发现服务不可用），再跑写操作
ALL_ORDER = ["smoke", "a", "b", "c", "task", "flow"]


def _run_one(name, args):
    """跑单个子命令，返回 (ctx, err)。"""
    module_name = COMMANDS[name]
    ctx = Ctx(label=name)
    if not ctx.login():
        return ctx, "登录失败（检查 TEST_USER / TEST_PASS / BASE_URL）"
    mod = importlib.import_module(module_name)
    if name == "smoke":
        batches = [b.strip() for b in (args.batches or "").split(",") if b.strip()]
        mod.run(ctx, batches or None)
    elif name == "task":
        modules = [m.strip() for m in (args.modules or "").split(",") if m.strip()]
        mod.run(ctx, modules or None)
    else:
        mod.run(ctx)
    return ctx, None


def main(argv=None):
    parser = argparse.ArgumentParser(
        prog="verify.py", description="云枢验证脚本统一入口",
        formatter_class=argparse.RawDescriptionHelpFormatter, epilog=__doc__)
    parser.add_argument("commands", nargs="*", default=[],
                        help="子命令：all / smoke / task / flow / a / b / c")
    parser.add_argument("--batches", default="", help="smoke 只跑指定批次，如 a,c")
    parser.add_argument("--modules", default="", help="task 只跑指定模块，如 t1,t2")
    parser.add_argument("--list", action="store_true", help="列出全部子命令")
    args = parser.parse_args(argv)

    if args.list:
        print("子命令：")
        print("  all    跑全部（%s）" % " + ".join(ALL_ORDER))
        for key, mod in COMMANDS.items():
            print("  %-6s %s" % (key, mod))
        return 0

    wanted = args.commands or ["all"]
    if "all" in wanted:
        wanted = list(ALL_ORDER)
    unknown = [c for c in wanted if c not in COMMANDS]
    if unknown:
        print("未知子命令：%s（可用：all %s）" % (unknown, " ".join(COMMANDS)))
        return 2

    total = Ctx(label="total")   # 只用来汇总
    # 用库时间当窗口起点（脚本机与 DB 时区可能不同，别用本地时间）
    since_ts = total.q("select now()") if total.db_ok() else ""
    clean_failed = []
    for name in wanted:
        print("\n" + "#" * 66)
        print("# 子命令：%s" % name)
        print("#" * 66)
        ctx, err = _run_one(name, args)
        if err:
            print("  \u274c " + err)
            total.failed += 1
            total.fails.append("%s: %s" % (name, err))
            continue
        _done, errs = ctx.cleanup()
        clean_failed.extend(errs)
        print("\n-- %s: PASS=%d FAIL=%d SKIP=%d" % (name, ctx.passed, ctx.failed, ctx.skipped))
        total.passed += ctx.passed
        total.failed += ctx.failed
        total.skipped += ctx.skipped
        total.fails.extend(ctx.fails)

    if clean_failed:
        print("\n\u26a0 清理告警：")
        for item in clean_failed:
            print("  - " + item)
    if since_ts:
        sweep(total, since_ts)
    return report(total, "汇总（%s）" % " ".join(wanted))


if __name__ == "__main__":
    sys.exit(main())
