#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""生成 `docs/测试用例总表.md` 与 `docs/测试覆盖率-YYYYMMDD.md`。

用法（在仓根）：
    python3 scripts/verify/gen_test_cases.py                 # 写文档 + 打印统计
    python3 scripts/verify/gen_test_cases.py --check         # 只校验数量与格式，不写文件（可进 CI）
    python3 scripts/verify/gen_test_cases.py --date 20261031 # 指定覆盖率报告文件名里的日期

为什么用脚本生成而不是手写表格：
  358 行手写一定会有「第 137 行期望结果漏写」这种脏数据；
  状态机部分是 N×N 全组合（25/9/36/49/16/9），手抄必漏边。
  脚本生成 = 数量、覆盖率、编号连续性能机器校验（`--check` 就是那条闸）。
"""
from __future__ import annotations

import argparse
import re
import sys
from datetime import date
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(HERE))

from testcase_gen import rest, state_machines as sm  # noqa: E402
from testcase_gen.flows import ROLE_ORDER, SCENARIOS  # noqa: E402

HEADER = "| 编号 | 模块 | 场景 | 前置条件 | 操作步骤 | 期望结果 | 优先级 | 自动化状态 |"
SEP = "|---|---|---|---|---|---|---|---|"


# ==================== 活体计数（文档里的数字必须来自代码，不许手填）====================
# 教训：本文件第一版的「后端 1355 个 test」「不变量 22 条」「E2E 12 spec / 29 test」
# 全是手填的，代码一长就变成假话。文档说假话比没文档更糟——它会让人以为验过了。
def _read(p: Path) -> str:
    return p.read_text(encoding="utf-8", errors="ignore")


def count_backend_tests() -> int:
    pat = re.compile(r"^\s*#\[(?:tokio::)?test\]", re.M)
    files = list((ROOT / "backend" / "src").rglob("*.rs")) + \
            list((ROOT / "backend" / "tests").rglob("*.rs"))
    return sum(len(pat.findall(_read(p))) for p in files)


def count_e2e() -> tuple[int, int]:
    """(spec 文件数, 顶层 test() 数)。

    注意：`22-pages-*.spec.ts` 走 `_pagerunner.ts::definePageChecks` 用 `it()` 动态生成，
    不在 `test(` 计数里；所以这里同时返回「动态生成器」是否存在的标记，文档里如实写明。
    """
    specs = sorted((ROOT / "frontend" / "e2e").glob("*.spec.ts"))
    pat = re.compile(r"^\s*test\(", re.M)
    return len(specs), sum(len(pat.findall(_read(p))) for p in specs)


def count_unit_files() -> int:
    return len(list((ROOT / "frontend" / "tests" / "unit").glob("*.test.ts")))


def count_invariants() -> tuple[int, list[str]]:
    """`erp_invariants.py` 里 check() 的**唯一编号数**（F1/F2 有重复调用，按编号去重）。"""
    src = _read(ROOT / "scripts" / "verify" / "erp_invariants.py")
    ids = [m.split()[0] for m in re.findall(r'check\(\s*"([^"]+)"', src)]
    uniq = sorted(set(ids), key=lambda x: (x[0], int(re.sub(r"\D", "", x) or 0)))
    return len(uniq), uniq


def group_ranges(ids: list[str]) -> str:
    """把 ['A1','A2','B1'] 折成 'A1–A2 / B1' 这种写法给文档用。"""
    from itertools import groupby
    out = []
    for head, group in groupby(ids, key=lambda x: x[0]):
        g = list(group)
        out.append(f"{g[0]}–{g[-1]}" if len(g) > 1 else g[0])
    return " / ".join(out)


DONE, PART, TODO = sm.AUTO_DONE, sm.AUTO_PART, sm.AUTO_TODO


def cell(text) -> str:
    """表格单元格清洗：竖线转义、换行压平（否则整张表塌）。"""
    return str(text).replace("|", "\\|").replace("\n", " ").strip()


def row(cid, module, scenario, pre, steps, expect, prio, auto) -> str:
    return "| " + " | ".join(
        cell(x) for x in (cid, module, scenario, pre, steps, expect, prio, auto)
    ) + " |"


def auto_cell(status: str, pointer: str) -> str:
    pointer = (pointer or "").strip()
    if not pointer:
        return status
    return f"{status} · {pointer}"


# ==================== ① 状态机 ====================
def _pre(ent, from_code, n):
    """`pre_tpl` 里用 `{from}` / `{i}` 占位；不能用 str.format（`from` 是关键字，且 body 里有 JSON 花括号）。"""
    return ent["pre_tpl"].replace("{from}", from_code).replace("{i}", f"{n:03d}")


def state_rows():
    rows = []
    for ent in sm.ENTITIES:
        terminal = sm.TERMINAL[ent["prefix"]]
        ledger = ent["log_check"] is not None
        n = 0
        for from_code, from_label in ent["statuses"]:
            for to_code, to_label in ent["statuses"]:
                n += 1
                cid = f"{ent['prefix']}-{n:03d}"
                edge = (from_code, to_code)
                api = ent["api"].replace("{id}", "<单号>")
                body = ent["body"].replace("{to}", to_code)
                log_sql = ent["log_check"].replace("{id}", "<单号>") if ledger else ""
                # 任务/序列号不涉及账务，别让用例去查库存
                fin = ent["prefix"] in ("TC-STKIN", "TC-XFER", "TC-PO", "TC-AP")
                tail = "查账务三处（库存/凭证/应付）" if fin else "查业务表本身（状态/时间戳）"
                if ent["prefix"] == "TC-AP":
                    # 应付单没有通用 status 端点：按目标态选 pay / cancel
                    if to_code == "CANCELLED":
                        api = "POST /api/business/erp/payable/<单号>/cancel"
                        body = '{"reason":"用例作废"}'
                    elif to_code == from_code == "OPEN":
                        api = "POST /api/business/erp/payable/<单号>/pay"
                        body = '{"amount":"0","pay_method":"BANK"}'
                    else:
                        api = "POST /api/business/erp/payable/<单号>/pay"
                        body = '{"amount":"300.00","pay_method":"BANK"}'

                if edge in ent["legal"]:
                    extra_pre, eff = ent["legal"][edge]
                    scenario = f"合法边 {from_label}（{from_code}）→ {to_label}（{to_code}）"
                    pre = _pre(ent, from_code, n) + "；" + extra_pre
                    steps = f"1.{api} 2.body `{body}` 3.复查单据 status"
                    if ledger:
                        steps += f" 4.{tail} 5.`{log_sql}`"
                    else:
                        steps += f" {tail}"
                    expect = "200；" + eff
                    if ledger and "biz_status_log" not in eff and "日志" not in eff:
                        expect += f"；`biz_status_log` 新增 1 行（{from_code}→{to_code}）"
                    prio = ent["prio_legal"].get(edge, ent["prio_legal_default"])
                    over = ent.get("auto_overrides", {}).get(edge)
                    if over:
                        status, pointer = over
                    else:
                        status, pointer = ent["auto_default"], ent["auto_note"]
                else:
                    is_self = from_code == to_code
                    scenario = (
                        f"自环 {from_label}（{from_code}→{from_code}，非法）" if is_self
                        else f"非法边 {from_label}（{from_code}）→ {to_label}（{to_code}）"
                    )
                    pre = _pre(ent, from_code, n)
                    steps = f"1.{api} 2.body `{body}` 3.复查单据 status 4.{tail}"
                    if ledger:
                        steps += f" 5.`{log_sql}`"
                    expect = (
                        ent.get("self_msgs", {}).get(edge)
                        if is_self else None
                    ) or (ent["self_msg"] if is_self else f"{ent['illegal_code']}；{ent['illegal_msg']}")
                    if not is_self:
                        # 非法边文案里点名「当前态允许去哪」——用真实出边填，不写「…」省略号
                        lbl = dict(ent["statuses"])
                        outs = " / ".join(lbl[dst] for (src, dst) in ent["legal"] if src == from_code) \
                            or "（终态，无可流转去向）"
                        expect = (expect
                                  .replace("{FROMCN}", from_label.split("（")[0])
                                  .replace("{TOCN}", to_label.split("（")[0])
                                  .replace("{FROM}", from_code)
                                  .replace("{TO}", to_code)
                                  .replace("{OUT}", outs))
                    prio = "P0" if from_code in terminal else "P1"
                    if edge in ent.get("auto_overrides", {}):
                        status, pointer = ent["auto_overrides"][edge]
                    else:
                        status, pointer = ent["auto_default"], ent["auto_note"]

                rows.append(row(cid, ent["module"], scenario, pre, steps, expect, prio,
                                auto_cell(status, pointer)))
    return rows


# ==================== ② 审批流 ====================
def flow_rows():
    rows = []
    n = 0
    for sc in SCENARIOS:
        for role in ROLE_ORDER:
            n += 1
            cid = f"TC-FLOW-{n:03d}"
            steps, expect = sc["views"][role]
            status, pointer = sc["auto_map"][role]
            rows.append(row(cid, "审批流", f"{sc['key']} {sc['title']} · {role}视角",
                            sc["pre"], steps, expect, sc["prio"], auto_cell(status, pointer)))
    return rows


# ==================== ③ 其余六类 ====================
def module_rows(mod):
    rows = []
    for i, case in enumerate(mod["cases"], start=1):
        cid = f"{mod['prefix']}-{i:03d}"
        rows.append(row(cid, mod["module"], case["scenario"], case["pre"],
                        case["steps"], case["expect"], case["prio"],
                        auto_cell(case["auto"], case["note"])))
    return rows


def all_sections():
    """[(模块名, 前缀, 行列表)] —— 顺序即文档顺序。"""
    sections = []
    for ent in sm.ENTITIES:
        rows = [r for r in state_rows() if r.startswith(f"| {ent['prefix']}-")]
        sections.append((ent["module"], ent["prefix"], rows))
    sections.append(("审批流", "TC-FLOW", flow_rows()))
    for mod in rest.MODULES:
        sections.append((mod["module"], mod["prefix"], module_rows(mod)))
    return sections


def stats(sections):
    out = []
    for name, prefix, rows in sections:
        total = len(rows)
        done = sum(1 for r in rows if "✅ 已自动" in r)
        part = sum(1 for r in rows if "🟡" in r)
        todo = sum(1 for r in rows if "⬜ 待写" in r)
        out.append(dict(name=name, prefix=prefix, total=total, done=done,
                        part=part, todo=todo, cov=done / total * 100 if total else 0.0))
    return out


# ==================== 文档 1：用例总表 ====================
def render_cases(sections, st):
    total = sum(s["total"] for s in st)
    inv_n, inv_ids = count_invariants()
    L = []
    L.append("# 云枢 · 全系统 E2E 测试用例总表")
    L.append("")
    L.append("> 本文档由 `python3 scripts/verify/gen_test_cases.py` 生成，**不要手改**——")
    L.append("> 改用例请改 `scripts/verify/testcase_gen/`，再重跑脚本；`--check` 会在 CI 里校验数量与编号连续性。")
    L.append("")
    L.append(f"**用例总数：{total} 条**（卡面要求 ≥200）。"
             f"自动化状态：✅ 已自动 {sum(s['done'] for s in st)} 条 / "
             f"🟡 部分（仅单测或纯逻辑）{sum(s['part'] for s in st)} 条 / "
             f"⬜ 待写 {sum(s['todo'] for s in st)} 条。")
    L.append("")
    L.append("## §0 口径与来源（先读，避免「这用例是不是拍的」）")
    L.append("")
    L.append("| 项 | 口径 |")
    L.append("|---|---|")
    L.append("| 状态机的合法边 | 抄自代码，不是我编的：`backend/src/business/shared/state_machine.rs`（STOCK_IN 3 / TRANSFER 6 / PURCHASE_ORDER 10 / PAYABLE 4）、"
             "`task/state.rs::TRANSITIONS`（`[Transition; 10]`）、`serial_model.rs`（3） |")
    L.append("| 非法边断言 | 409 + 具体错误码 + 文案**必须点名当前态可选去向**：任务 `63010`、ERP 四单 `10003`、序列号 `66008` |")
    L.append("| 审批流场景 | 接口与常量来自 `flow/handler.rs` / `flow/dto.rs` / `flow/timeout.rs` / `flow/task_bridge.rs`，错误码逐个核对 |")
    L.append(f"| 一致性检查编号 | `scripts/verify/erp_invariants.py` 实有 **{inv_n} 条唯一编号**（{group_ranges(inv_ids)}；本行由脚本数 `check()` 得出，不手填） |")
    L.append("| 前端选择器 | 全部是代码里真实存在的 `data-testid`（如 `btn-stockin-post-confirm`），可直接被 Playwright 定位 |")
    L.append("| ✅/🟡 判定 | ✅ = 有已提交的自动化断言（单测/接口脚本/E2E 任一，且注明文件与函数）；🟡 = 只有纯逻辑单测、接口层没覆盖；"
             "⬜ = 一行测试都没有。**没跑过的绝不标 ✅。** |")
    L.append("")
    L.append("### 与卡面的两处口径差异（诚实说明）")
    L.append("")
    L.append("1. 卡面写「`scan_all_links.py` 的 32 项」，但**本仓库不存在这个文件**"
             "（`find . -name 'scan_all_links*'` 零命中）。联动扫描实际由 "
             "`p9_cancel_e2e.py` / `t11_cancel_cascade.sh` / `t13_flow_cascade.sh` / `flow_e2e.py` 承担，"
             f"所以 CONS 类对齐全量不变量检查器的 {inv_n} 条 + 10 条新增联动/负向用例（F1–F10）。")
    L.append("2. 卡面要求采购单 7×7，其中 `SUBMITTED/APPROVED→PARTIAL_RECEIVED/RECEIVED` 四条边的**真实写入方是收货链路**"
             "（`stockin_model::sync_purchase_status`），人工 PATCH 也能过；这是已知缺口 K-01/K-02，用例里按「先断言现状」写，"
             "收口后改断言即可。")
    L.append("")
    L.append("### 编号前缀规范（`grep TC-STKIN-` 就能统计某块覆盖）")
    L.append("")
    L.append("| 前缀 | 模块 | 条数 |")
    L.append("|---|---|---|")
    for s in st:
        L.append(f"| `{s['prefix']}-` | {s['name']} | {s['total']} |")
    L.append(f"| **合计** | | **{total}** |")
    L.append("")
    L.append("## 用例总览")
    L.append("")
    L.append("| 维度 | 模块 | 条数 | 覆盖内容 |")
    L.append("|---|---|---|---|")

    overview = [
        ("① 状态机全组合", "任务 / 收货 / 调拨 / 采购 / 应付 / 序列号 / 凭证",
         "N×N 全排列（25+9+36+49+16+9+8）：合法边测通过+副作用+日志，非法边与自环逐个断言 409 + 错误码 + 点名可选去向",
         ("TC-TASK", "TC-STKIN", "TC-XFER", "TC-PO", "TC-AP", "TC-SERIAL", "TC-VCH")),
        ("② 审批流全场景", "审批流", "A1–A15 × 4 视角（提交人/审批人/超管/无关人）= 60 条",
         ("TC-FLOW",)),
        ("③ 权限边界", "权限", "菜单访问矩阵 / 数据范围 / 字段脱敏 / 7 个写接口越权撞",
         ("TC-PERM",)),
        ("④ 数据一致性", "数据一致性", f"{inv_n} 条检查器逐个负向注入 + F1–F10 联动缺口 + 假红护栏 + 造数必清",
         ("TC-CONS",)),
        ("⑤ 边界与异常", "边界与异常", "数量/金额/日期/字符串/分页/UUID/并发/幂等/事务/大数据量",
         ("TC-BOUND",)),
        ("⑥ 前端交互", "前端交互", "10 组 ERP 页面核心操作（收货/调拨/采购/应付/凭证/总账/库存/仓库/库位/基础数据）"
                                   " + 任务与流程页 8 条已自动化 + 9 类通用交互（选择器全为真实 testid）",
         ("TC-FE",)),
        ("⑦ 登录会话", "登录与会话", "登录/锁定/限流/吊销/改密/token 生命周期",
         ("TC-AUTH",)),
        ("⑧ 系统底座", "系统底座", "用户/角色/菜单/字典/配置/文件/日志/在线/定时任务/公告/通知/变更日志",
         ("TC-BASE",)),
    ]
    for dim, mod, desc, prefixes in overview:
        cnt = sum(s["total"] for s in st if s["prefix"] in prefixes)
        L.append(f"| {dim} | {mod} | {cnt} | {desc} |")
    L.append("")
    L.append("## §1 状态机全组合（每张单据 N×N 一条不落）")
    L.append("")
    L.append(HEADER)
    L.append(SEP)
    for name, prefix, rows in sections:
        if prefix.startswith("TC-") and prefix in ("TC-TASK", "TC-STKIN", "TC-XFER", "TC-PO",
                                                   "TC-AP", "TC-VCH", "TC-SERIAL"):
            L.extend(rows)
    L.append("")
    L.append("## §2 审批流全场景（A1–A15 × 4 视角）")
    L.append("")
    L.append(HEADER)
    L.append(SEP)
    for name, prefix, rows in sections:
        if prefix == "TC-FLOW":
            L.extend(rows)
    L.append("")
    L.append("## §3 权限 / 一致性 / 边界 / 前端 / 会话 / 底座")
    L.append("")
    for name, prefix, rows in sections:
        if prefix in ("TC-PERM", "TC-CONS", "TC-BOUND", "TC-FE", "TC-AUTH", "TC-BASE"):
            L.append(f"### {name}（`grep {prefix}-`）")
            L.append("")
            L.append(HEADER)
            L.append(SEP)
            L.extend(rows)
            L.append("")
    L.append("## §4 已知缺口（用例里标了 K-xx 的地方，都是「代码现状 vs 应有行为」的差）")
    L.append("")
    L.append("| 编号 | 缺口 | 证据 | 影响的用例 |")
    L.append("|---|---|---|---|")
    gaps = [
        ("K-01", "采购订单 `SUBMITTED→APPROVED` 的写入方不明（审批未接入采购单，目前靠超管直改）",
         "`state_machine.rs` 注释「`APPROVED` 没有任何写入方」", "TC-PO-010"),
        ("K-02", "采购单 `→PARTIAL_RECEIVED/RECEIVED` 允许人工 PATCH（正常只该由收货链路写）",
         "`PURCHASE_ORDER_EDGES` 含这两条边，`purchase_handler` 不区分调用方",
         "TC-PO-011 / 012 / 018 / 019（合法边）；TC-PO-004/005 是「DRAFT 直接收货」的非法边，必须一直 409"),
        ("K-03", "ERP 五张单据表**没有接数据权限**（`scripts/check.sh` 的 `BATCHB_SCOPED_TABLES` 里没有 biz_stock_in/transfer/payable）",
         "`grep -c biz_stock_in scripts/check.sh` → 0", "TC-PERM-019"),
        ("K-04", "0 成本入库是否允许未定（PRD 未写死；当前允许但 C3 会报 WARN）",
         "`erp_invariants.py` C3 只报 WARN", "TC-BOUND-004"),
        ("K-05", "同物料并发过账是否有行锁未验证（移动加权在并发下可能算错金额）",
         "`stockin_handler` 未见 `FOR UPDATE` 的显式路径", "TC-BOUND-021"),
    ]
    for g in gaps:
        L.append("| " + " | ".join(cell(x) for x in g) + " |")
    L.append("")
    L.append("## §5 怎么用这张表")
    L.append("")
    L.append("```bash")
    L.append("grep -c '^| TC-STKIN-' docs/测试用例总表.md        # 某模块用例数")
    L.append("grep '^| TC-XFER-' docs/测试用例总表.md | grep -c '⬜'   # 某模块还剩多少没写")
    L.append("python3 scripts/verify/gen_test_cases.py --check   # 编号连续性 / 期望结果非空 / 条数 ≥200")
    L.append("```")
    L.append("")
    return "\n".join(L) + "\n"


# ==================== 文档 2：覆盖率报告 ====================
def render_coverage(st, day: str):
    total = sum(s["total"] for s in st)
    done = sum(s["done"] for s in st)
    part = sum(s["part"] for s in st)
    todo = sum(s["todo"] for s in st)
    be_tests = count_backend_tests()
    e2e_specs, e2e_tests = count_e2e()
    unit_files = count_unit_files()
    inv_n, inv_ids = count_invariants()
    L = []
    L.append(f"# 测试覆盖率报告 · {day[:4]}-{day[4:6]}-{day[6:]}")
    L.append("")
    L.append("> 本表由 `python3 scripts/verify/gen_test_cases.py` 从用例数据**算出**，不是手填的。")
    L.append("> `已自动化` 只算 ✅（有已提交的自动化断言并注明位置）；🟡 是「只有纯逻辑单测」，单独列一栏，")
    L.append("> 因为纯逻辑测过 ≠ 接口/页面测过（TSK-149 就是这么漏掉的）。")
    L.append("")
    L.append("## 一、按模块")
    L.append("")
    L.append("| 模块 | 用例总数 | 已自动化 ✅ | 部分 🟡 | 待写 ⬜ | 覆盖率（✅/总数） | 备注 |")
    L.append("|---|---|---|---|---|---|---|")
    remarks = {
        "任务管理": "状态机 5×5 全覆盖（含自环）；⚠️ 但全部是纯逻辑单测，接口层 0 条",
        "收货入库单": "3×3 全覆盖；过账/作废链路有单测，非法边接口层 0 条",
        "库存调拨单": "6×6 全覆盖；`DONE` 的 6 条非法边有单测，其余 ⬜",
        "采购订单": "7×7 全覆盖；只有 1 条边有单测，★ 最大空白",
        "应付单": "4×4 全覆盖；付款/作废冲突有单测",
        "会计凭证": "红冲 8 条；单测较扎实（P7 那批）",
        "物料序列号": "3×3 全覆盖；4 条边有单测",
        "审批流": "15 场景 × 4 视角；只有 A2 审批人视角进了 E2E",
        "权限边界": "★ 完全空白（30+ 条一条没写）",
        "数据一致性": "检查器有了，但「负向注入」一条没写 —— 等于没验证检查器有效",
        "边界与异常": "★ 几乎空白（只有 1 条幂等被单测覆盖）",
        "前端交互": "7 个任务/流程 spec 已入 E2E；10 组 ERP 页面核心操作全空白",
        "登录与会话": "只有 1 条（正确登录）进了 E2E",
        "系统底座": "接口冒烟脚本 `verify.py smoke` 覆盖一部分（准确条数看脚本输出，别抄旧数字）",
    }
    for s in st:
        L.append(f"| {s['name']} | {s['total']} | {s['done']} | {s['part']} | {s['todo']} | "
                 f"{s['cov']:.1f}% | {remarks.get(s['name'], '')} |")
    L.append(f"| **合计** | **{total}** | **{done}** | **{part}** | **{todo}** | "
             f"**{done / total * 100:.1f}%** | 含 🟡 的口径：{(done + part) / total * 100:.1f}% |")
    L.append("")
    L.append("## 二、按维度（卡面要求的六个维度）")
    L.append("")
    L.append("| 维度 | 卡面下限 | 本表条数 | 达标 |")
    L.append("|---|---|---|---|")
    dims = [
        ("① 状态机全组合", 60, sum(s["total"] for s in st if s["prefix"] in
                                   ("TC-TASK", "TC-STKIN", "TC-XFER", "TC-PO", "TC-AP", "TC-VCH", "TC-SERIAL"))),
        ("② 审批流全场景", 40, sum(s["total"] for s in st if s["prefix"] == "TC-FLOW")),
        ("③ 权限边界", 30, sum(s["total"] for s in st if s["prefix"] == "TC-PERM")),
        ("④ 数据一致性", 30, sum(s["total"] for s in st if s["prefix"] == "TC-CONS")),
        ("⑤ 边界与异常", 30, sum(s["total"] for s in st if s["prefix"] == "TC-BOUND")),
        ("⑥ 前端交互", 20, sum(s["total"] for s in st if s["prefix"] == "TC-FE")),
    ]
    for name, low, cnt in dims:
        L.append(f"| {name} | {low} | {cnt} | {'✅' if cnt >= low else '❌'} |")
    extra = sum(s["total"] for s in st if s["prefix"] in ("TC-AUTH", "TC-BASE"))
    L.append(f"| （附加）登录会话 + 系统底座 | — | {extra} | — |")
    L.append(f"| **合计** | **≥200** | **{total}** | **{'✅' if total >= 200 else '❌'}** |")
    L.append("")
    L.append("## 三、现有自动化资产盘点（覆盖率的分母从哪来）")
    L.append("")
    L.append("| 资产 | 规模 | 入口 |")
    L.append("|---|---|---|")
    L.append(f"| 后端 Rust 单测/集成测 | {be_tests} 个 `#[test]`/`#[tokio::test]`（脚本实时统计 `backend/src` + `backend/tests`） | `cd backend && cargo test` |")
    L.append(f"| 前端 Playwright E2E | {e2e_specs} 个 spec / {e2e_tests} 个顶层 `test()`（另有 `22-pages-*.spec.ts` 用 `_pagerunner.ts` 动态生成 `it`，未计入） | `cd frontend && pnpm test:e2e` |")
    L.append(f"| 前端纯函数单测 | {unit_files} 个文件（`frontend/tests/unit/`） | `pnpm test:unit` |")
    L.append("| 接口验证套件 | `verify.py` 6 个子命令（`smoke` / `a` / `b` / `c` / `task` / `flow`），`all` 串跑 | `python3 scripts/verify/verify.py all` |")
    L.append(f"| ERP 不变量审计 | **{inv_n}** 条唯一检查（{group_ranges(inv_ids)}；实时数 `check()`） | `python3 scripts/verify/erp_invariants.py` |")
    L.append("| 状态机统一验收（P10） | 6 组断言（`scripts/verify/p10_state_machine.sh`，已入库） | `bash scripts/verify/p10_state_machine.sh` |")
    L.append("| 提交前检查 | fmt / clippy / check / typecheck + 数据权限两道闸 | `bash scripts/check.sh` |")
    L.append("")
    L.append("## 四、实测现状（`bash scripts/verify/run_all.sh` 真跑，两轮）")
    L.append("")
    L.append("| Stage | 11:56 轮 | 12:03 轮（修完环境探测后） | 现场 |")
    L.append("|---|---|---|---|")
    L.append(f"| ① 用例表一致性闸 | PASS | PASS | {total} 条 / 编号连续 / 期望结果无模糊词 / 指针真实 |")
    L.append("| ② 后端 cargo test | FAIL | FAIL | 主体 **1367 passed**；红的 2 条是 `testd::mutex_help_ai::h01_help_doc_crud` / `h02_help_search`（`实际 total=0`，别人在途的 `tests/testd/`），另有未入库探针 `zz_probe_tmp.rs` 一起挂 |")
    L.append(f"| ③ 前端纯函数单测 | PASS | PASS | {unit_files} 个文件，`fail 0` |")
    L.append("| ④ 接口验证套件 | SKIP | **SKIP** | 后端未起（`:8023/health` 不通）→ 按环境缺失跳过 |")
    L.append("| ⑤ 不变量审计 | FAIL（**旧脚本误报**） | **SKIP** | 旧脚本 psql 没口令也照跑 → 报 6 个「执行异常 BLOCKER」；改成先探库再跑，现在是 SKIP |")
    L.append("| ⑥ 联动扫描 | FAIL（**旧脚本误报**） | **SKIP** | 同上：登录失败/缺 env 文件时先探接口可达性再决定跑不跑 |")
    L.append("| ⑦ 前端 E2E（生产） | FAIL | 未重跑（原因见下） | 12 passed / 5 failed / 4 skipped；红的是 `06-task-detail-tabs`、`08-t8b-task-tree`、`09-t8c-root-only`×2、`10-erp-warehouse` |")
    L.append("| ⑧ 静态检查 | FAIL | 未重跑 | `cargo fmt` 卡在**别人在途**的 `backend/tests/linkage/task_flow_linkage.rs`（本卡一个文件都没改业务代码） |")
    L.append("")
    L.append("**⑦ 为什么 12:03 轮没重跑 E2E**：同一时刻「测试E · 52 页全量」那张卡正在打**同一台生产**跑 9 个 spec，")
    L.append("生产全局限流 300 req / 60s 按**客户端 IP** 计，两轮叠一起必然 429（本仓 README 有复现记录）。")
    L.append("宁可只报 11:56 那一轮的真数，也不制造一次假红。合并跑法：等那边跑完 ≥65s 再 `--only e2e`。")
    L.append("")
    L.append("**读法**：①③ 真绿；② 是别人在途测试红（不是本卡引入）；④⑤⑥ 是环境未就绪（**SKIP 不等于通过**，")
    L.append("只是不冒充失败）；⑦ 4 条 spec 真红要修；⑧ 是在途改动没 fmt。")
    L.append("改这份文档之前，8 个 stage 里只有 **2 个能真跑绿**——这就是为什么必须先有系统性用例，而不是继续加零散断言。")
    L.append("")
    L.append("## 五、下一步派卡建议（按 ROI 排序）")
    L.append("")
    L.append("| 顺序 | 补什么 | 依据 | 预计条数 |")
    L.append("|---|---|---|---|")
    L.append(f"| 1 | **一致性负向注入**（TC-CONS 全套 37 条）：先把检查器证明有效 | 后端 {be_tests} 个测试也没抓住 TSK-149，根因是「检查器本身没被验证过」 | 37 |")
    L.append("| 2 | **权限矩阵**（TC-PERM 全套，尤其 PERM-26..32 七个写接口越权） | 30 条全空白，且 ERP 表没接数据范围（K-03） | 32 |")
    L.append("| 3 | **状态机非法边接口层**（TC-XFER / TC-PO 的非法边） | 单测只测纯逻辑，接口层 409 文案没人验过 | ~50 |")
    L.append("| 4 | **审批流 A3/A4/A7/A8/A13/A14/A15** | 会签/或签/加签/条件分支只有引擎纯逻辑测 | ~28 |")
    L.append("| 5 | **前端交互**（TC-FE 剩余 22 条） | 10 组 ERP 页面核心操作一条 E2E 都没有（只有仓库/库位/基础数据有） | 22 |")
    L.append("| 6 | **边界异常**（TC-BOUND 全套） | 几乎空白，并发/幂等/事务最危险 | 32 |")
    L.append("")
    L.append("## 六、怎么跑全部检查")
    L.append("")
    L.append("```bash")
    L.append("bash scripts/verify/run_all.sh          # 单元 + 接口 + E2E + 不变量 + 闸门（一键）")
    L.append("bash scripts/verify/run_all.sh --fast   # 跳过 E2E（本地改代码时用）")
    L.append("python3 scripts/verify/gen_test_cases.py --check   # 用例表自身的一致性闸")
    L.append("```")
    L.append("")
    return "\n".join(L) + "\n"


# ==================== 校验闸 ====================
def check(sections, st):
    problems = []
    total = sum(s["total"] for s in st)
    if total < 200:
        problems.append(f"用例总数 {total} < 200（卡面硬红线）")
    seen = set()
    for name, prefix, rows in sections:
        nums = []
        for r in rows:
            parts = [p.strip() for p in r.strip("|").split(" | ")]
            cid = parts[0]
            if cid in seen:
                problems.append(f"编号重复：{cid}")
            seen.add(cid)
            # 列数（8 列）
            if len(parts) != 8:
                problems.append(f"{cid} 列数 {len(parts)} != 8")
            # 期望结果非空且不是模糊词
            expect = parts[5]
            if len(expect) < 8:
                problems.append(f"{cid} 期望结果过短：{expect!r}")
            for vague in ("正常", "OK", "没问题"):
                if expect.strip() == vague:
                    problems.append(f"{cid} 期望结果是模糊词：{vague}")
            # 未替换的模板占位符（{FROM} 之类漏进表里，等于期望结果没写完）
            if re.search(r"\{(FROM|TO|OUT|FROMCN|TOCN|拒绝原因|白名单)", expect):
                problems.append(f"{cid} 期望结果里有未替换的占位符")
            # 优先级
            if parts[6] not in ("P0", "P1", "P2"):
                problems.append(f"{cid} 优先级非法：{parts[6]}")
            # 自动化状态
            if not any(k in parts[7] for k in ("✅", "🟡", "⬜")):
                problems.append(f"{cid} 自动化状态缺失")
            # 指针真实性：✅/🟡 行里反引号引用的仓库文件必须真的存在，
            # 否则「已自动化」就是空口白话（本闸就是为了拦住这种脏数据）
            if ("✅" in parts[7] or "🟡" in parts[7]):
                for ref in re.findall(r"`([^`]+)`", parts[7]):
                    file_part, _, symbol = ref.partition("::")
                    file_part = file_part.strip()
                    if not file_part.startswith(("backend/", "frontend/", "scripts/")):
                        continue
                    if "*" in file_part:      # 通配引用（如 specs 批）不查
                        continue
                    target = ROOT / file_part
                    if not target.exists():
                        # 有的引用带命令参数/子命令（如 `scripts/verify/verify.py smoke`）：
                        # 逐段回退看前缀是不是真文件，别把「写法带参数」误判成「文件不存在」。
                        toks = file_part.split()
                        target = next((ROOT / " ".join(toks[:i]) for i in range(len(toks) - 1, 0, -1)
                                       if (ROOT / " ".join(toks[:i])).exists()), None)
                        if target is None:
                            problems.append(f"{cid} 引用的文件不存在：{file_part}")
                            continue
                    # `path::func` 形式：函数/测试名必须真在该文件里，防「文件对、名字错」
                    if symbol.strip() and target.suffix in (".rs", ".ts", ".py", ".sh"):
                        sig = symbol.strip()
                        if sig not in _read(target):
                            problems.append(f"{cid} 引用的符号不存在：{file_part}::{sig}")
            # 编号连续性
            try:
                nums.append(int(cid.rsplit("-", 1)[1]))
            except ValueError:
                problems.append(f"{cid} 编号格式非法")
        if nums and nums != list(range(1, len(nums) + 1)):
            problems.append(f"{prefix} 编号不连续：{nums[:5]}…")
    return problems


def check_drift(sections, st, day: str):
    """落盘文档 vs 生成器现状：不一致就报红。

    为什么必须有这条：`--check` 原来只校验内存里的数据，所以「文档写着 359 条、
    生成器已经 366 条」这种漂移它一声不响（本卡第一轮就是这个坑）。
    CI 里挂的闸，必须比对的**是提交进仓库的那份字节**。
    """
    problems = []
    pairs = [
        (ROOT / "docs" / "测试用例总表.md", render_cases(sections, st)),
        (ROOT / "docs" / f"测试覆盖率-{day}.md", render_coverage(st, day)),
    ]
    for path, fresh in pairs:
        if not path.exists():
            problems.append(f"缺文档：{path.relative_to(ROOT)}（先跑一次不带 --check 的生成）")
            continue
        if path.read_text(encoding="utf-8") != fresh:
            problems.append(f"文档与生成器不一致：{path.relative_to(ROOT)}（有人手改或忘了重生成，跑一次即可修）")
    return problems


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--check", action="store_true", help="只校验，不写文件")
    ap.add_argument("--date", default=date.today().strftime("%Y%m%d"))
    args = ap.parse_args()

    sections = all_sections()
    st = stats(sections)
    problems = check(sections, st)
    if args.check:
        problems += check_drift(sections, st, args.date)

    if problems:
        print("❌ 用例表校验失败：")
        for p in problems[:40]:
            print("  -", p)
        return 1

    total = sum(s["total"] for s in st)
    print(f"✅ 校验通过：{total} 条用例，编号连续，8 列齐备，期望结果无模糊词")
    for s in st:
        print(f"   {s['prefix']:12} {s['name']:8} 总 {s['total']:3}  ✅{s['done']:3} 🟡{s['part']:3} ⬜{s['todo']:3}  覆盖 {s['cov']:5.1f}%")

    if args.check:
        return 0

    cases_md = render_cases(sections, st)
    cov_md = render_coverage(st, args.date)
    p1 = ROOT / "docs" / "测试用例总表.md"
    p2 = ROOT / "docs" / f"测试覆盖率-{args.date}.md"
    p1.write_text(cases_md, encoding="utf-8")
    p2.write_text(cov_md, encoding="utf-8")
    print(f"\n写出：{p1}（{len(cases_md)} 字符）")
    print(f"写出：{p2}（{len(cov_md)} 字符）")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
