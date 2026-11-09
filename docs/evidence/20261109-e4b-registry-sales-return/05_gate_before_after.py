#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""t_9c938b6b 证据：**同一个提交状态**下，旧闸判绿、新闸判红。

做法（不改工作区、不连库）：
  · 用 `git show HEAD:<path>` 取出「修前」的三份真相文件到临时目录
    （doc_kind.rs / erp_invariants.py / doc-kinds.ts）；
  · 旧闸逻辑（HEAD 版 erp_invariants_e4_regression.py 的闸①）原样复刻：手写模块清单 +
    「枚举/清单常量 ⊆ 登记表」，对同一状态跑一遍；
  · 新闸逻辑直接 import 当前工作区的 `scan_biz_type_layers`，对同一状态跑一遍。
期望：旧闸 missing=[]（判绿，即真 BLOCKER 被漏掉），新闸在
「写入点 ⊆ 词表」「写入点 ⊆ 登记表」两处点名 SALES_RETURN_IN。
"""
import importlib.util
import re
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
BUSINESS = REPO / "backend/src/business"
REG_MODULE = REPO / "scripts/verify/regression/erp_invariants_e4_regression.py"

# 旧闸的手写清单（HEAD 版原文，见 git show HEAD:scripts/verify/regression/erp_invariants_e4_regression.py）
OLD_MODULES = (
    "stockin_base.rs", "stockin_write.rs", "stockin_cancel.rs", "opening.rs",
    "transfer_settle.rs", "serial_model.rs", "stockout_base.rs", "stockout_model.rs",
    "stocktake_model.rs",
)


def head_bytes(path):
    return subprocess.run(["git", "-C", str(REPO), "show", f"HEAD:{path}"],
                          capture_output=True, check=True).stdout


def old_gate(before):
    """复刻旧闸①：只比「枚举 + 清单模块常量」↔ 登记表。"""
    registered = set(re.findall(r'"([A-Z0-9_]+)"\s*:',
                                re.search(r"^E4_BIZ_TYPE_AXIS = \{(.*?)^\}",
                                          before["audit"], re.M | re.S).group(1)))
    impl = re.search(r"^impl TxnBizType \{(.*?)^\}", before["doc_kind"], re.M | re.S)
    body = re.search(r"pub const fn as_str\(self\) -> &'static str \{(.*?)\n    \}\n",
                     impl.group(1), re.S)
    enum_vals = set(re.findall(r'=>\s*"([A-Z0-9_]+)"', body.group(1)))
    const_vals = set()
    for name in OLD_MODULES:
        const_vals |= set(re.findall(
            r'pub const BIZ_TYPE[A-Z_]*:\s*&str\s*=\s*"([A-Z][A-Z0-9_]*)"',
            (BUSINESS / "erp" / name).read_text(encoding="utf-8")))
    code_vals = enum_vals | const_vals
    return sorted(code_vals - registered), sorted(const_vals)


def main():
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        paths = {
            "doc_kind": "backend/src/business/shared/doc_kind.rs",
            "audit": "scripts/verify/erp_invariants.py",
            "ts": "frontend/src/api/business/erp/doc-kinds.ts",
        }
        before = {}
        for key, rel in paths.items():
            text = head_bytes(rel).decode("utf-8")
            copy = tmp / (key + Path(rel).suffix)
            copy.write_text(text, encoding="utf-8")
            before[key] = text

        spec = importlib.util.spec_from_file_location("e4reg", REG_MODULE)
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)

        old_missing, old_consts = old_gate(before)
        print("=" * 88)
        print("修前状态（HEAD 的三份真相文件）")
        print("=" * 88)
        print(f"[旧闸①] 扫描模块 = 手写清单 {len(OLD_MODULES)} 个；清单里的常量 = {old_consts}")
        print(f"[旧闸①] 未登记的 biz_type = {old_missing}   → {'★ 判绿（漏了真缺口）' if not old_missing else '判红'}")
        assert "SALES_RETURN_IN" not in old_consts, "旧清单居然扫到了 sales_return_posting"

        r = mod.scan_biz_type_layers(BUSINESS, tmp / "audit.py", tmp / "doc_kind.rs", tmp / "ts.ts")
        print()
        print(f"[新闸①] 探测到写入点模块 {len(r['writers'])} 个（含 "
              f"{sorted(p.name for p in r['writers'] if 'sales_return' in p.name)}）")
        print(f"[新闸①] 写入点字面量 {len(r['write_literals'])} 个：{sorted(r['write_literals'])}")
        print(f"[新闸①] L1 写入点 ⊆ 词表  未通过 = {r['not_in_vocabulary']}")
        print(f"[新闸①] L2 写入点 ⊆ 登记表 未通过 = {r['sites_not_in_registry']}")
        print(f"[新闸①] L3 词表 ⊆ 登记表   未通过 = {r['not_in_registry']}")
        ok = ("SALES_RETURN_IN" in r["not_in_vocabulary"]
              and "SALES_RETURN_IN" in r["sites_not_in_registry"])
        print()
        print("说明：L3（词表 ⊆ 登记表）在修前是空的 —— 这正是旧闸的盲区：写入点用了"
              "词表里根本没有的值，枚举侧与登记表侧同时缺这一个值，"
              "只比这两侧永远看不出来。")
        print("结论：" + ("✅ 旧闸漏掉的真缺口，新闸用「写入点」这一侧在 L1/L2 点名 → 以后不会再漏"
                        if ok else "🚨 新闸也没抓到，判据仍需修"))
        return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
