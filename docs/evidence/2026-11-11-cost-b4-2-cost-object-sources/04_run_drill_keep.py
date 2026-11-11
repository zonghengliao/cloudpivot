#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""复用 B4-5 的端到端演练**造真实链路**，但**保留夹具**（不跑收尾清场）。

为什么要这一步：COST-B4-2 的反查页要回答「这个批的料从哪来、货去哪了」，
需要有**真实的**领料单 + 完工入库单 + 已结算计算单 —— 而这些只有跑完
`calc → 审 → 结算` 才存在。B4-5 的演练脚本 `scripts/verify/cost/cost_e2e.py`
正好造这一整套（且在末尾把夹具删干净，好让「清场后残留 = 0」成立）。

本文件不改那个脚本（它是别的卡的交付物），只把它当库用，并把 `clean_fixture`
打成空函数，于是：

* 跑的是演练**同一套代码路径**（calc 探针 + 真接口审批 + 真接口结算）；
* 跑完库里留着 1 个成本对象 / 1 张领料单 / 2 张完工入库单（1 有效 + 1 红冲）/ 3 份凭证。

用法（实例先由 `02_up_instance.sh` 起好）：

    BASE=http://127.0.0.1:18142 DB=cost_b42_e2e \
        NIK_ADMIN_PASSWORD='Admin@123456' python3 04_run_drill_keep.py
"""

from __future__ import annotations

import importlib.util
import os
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]                       # docs/evidence/<card>/ → 仓库根
DRILL = ROOT / "scripts/verify/cost/cost_e2e.py"

if not DRILL.exists():
    sys.exit(f"找不到演练脚本：{DRILL}")

spec = importlib.util.spec_from_file_location("cost_e2e", DRILL)
if spec is None or spec.loader is None:
    sys.exit(f"无法加载演练脚本：{DRILL}")
drill = importlib.util.module_from_spec(spec)
spec.loader.exec_module(drill)


def _keep_fixture(quiet: bool = False) -> None:
    """替代演练收尾的清场：什么都不删（反查页要靠这份夹具取证）。"""
    del quiet
    print("  ⏭  跳过清场（本脚本要保留夹具给反查页验收）")


# `main()` 里按模块全局名调用 clean_fixture ⇒ 覆盖模块属性即可生效
setattr(drill, "clean_fixture", _keep_fixture)

os.environ.setdefault("COST_E2E_BASE", "http://127.0.0.1:18142")
sys.exit(drill.main())
