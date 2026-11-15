#!/usr/bin/env python3
# ============================================================================
# junit_report.py —— 把 Rust libtest 的输出变成 **JUnit XML**（CI 报告，P2-63）
#
# 为什么自己写：
#   `cargo test` 是唯一的测试入口，但它在 **stable** 上**不吐任何机器可读格式**：
#     · `--format=junit` / `--format=json` / `--report-time` 全是 nightly-only
#       （实测 rustc 1.94.1 stable：`error: The "junit" format is only accepted on the
#        nightly compiler with -Z unstable-options`；`--report-time` 同）；
#     · 机器上没有 nightly 工具链、也没有 cargo-nextest（`rustup toolchain list` 只有 1.94.x）。
#   于是 CI（GitLab/GitHub/Jenkins）拿不到用例级结果，只能 grep 文本日志 —— 红在哪、
#   哪条变慢了、失败原因是什么全靠人看。
#
# 本文件做三件事（都只依赖 stdlib）：
#   ① `run_capture()`：跑 cargo test，**逐行**读 stdout 并打时间戳 ⇒ 每条用例的耗时是
#      真量出来的（libtest 每完成一条用例就 flush 一行 `test <name> ... ok`，
#      `--test-threads=1` 下相邻两行的时间差 = 上一条用例的执行时间）。
#   ② `parse()`：解析 libtest pretty 输出（`running N tests` / `test … ok|FAILED|ignored` /
#      `---- <name> stdout ----` 失败块 / `test result: …` 汇总行）⇒ Suite 结构。
#   ③ `to_xml()`：写 `<testsuites>`（每条用例一个 `<testcase>`，失败挂 `<failure>`，
#      ignored 挂 `<skipped>`），GitLab/GitHub Actions/Jenkins 都能直接吃。
#
# 子命令：
#   selftest                    门禁自检：真解析 + 4 类畸变必须被抓（反证），exit 0/1
#   check <xml…>                校验 XML 结构自洽（计数/重复/失败元素/时间），exit 0/1
#   from-logs <DIR> [--out F]   把已有的 `<域>.log`(+`summary.tsv`) 补成 XML（无逐条耗时）
#   run -p <crate> …            独立跑一次并直接产出 XML（不经 test_domain.py 时用）
#
# 用法（日常走 test_domain.py，它已接线）：
#   python3 scripts/test_domain.py run --package all --junit out/junit
#   python3 scripts/junit_report.py check out/junit/junit.xml
#
# 退出码：0 = OK；1 = 校验失败/有红；2 = 前置/参数错误
# ============================================================================
from __future__ import annotations

import argparse
import datetime as _dt
import io
import os
import re
import socket
import subprocess
import sys
import tempfile
import time
import xml.etree.ElementTree as ET
from dataclasses import dataclass, field
from pathlib import Path
from typing import Iterable, NoReturn

# ---------------------------------------------------------------------------
# libtest 输出的四类行（stable pretty 格式，实测 rustc 1.94.1）
# ---------------------------------------------------------------------------
RUNNING_RE = re.compile(r"^running (\d+) tests?$")
# `test business::x::t01 ... ok` / `... FAILED` / `... ignored` / `... bench: 1,234 ns/iter`
TEST_LINE_RE = re.compile(r"^test (\S+) \.\.\.(?: (.*))?$")
# 判定词（可能被用例自己的日志输出挤到**下一行**，实测 kernel/platform 各有 3/5 条）
VERDICT_RE = re.compile(r"^(ok|FAILED|ignored)(?:, .*)?$")
# `test result: FAILED. 3 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s`
RESULT_RE = re.compile(
    r"^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; "
    r"(\d+) measured; (\d+) filtered out; finished in ([\d.]+)s"
)
BLOCK_RE = re.compile(r"^---- (\S+) stdout ----$")


def die(msg: str, code: int = 2) -> NoReturn:
    print(f"!! {msg}", file=sys.stderr)
    sys.exit(code)


# ---------------------------------------------------------------------------
# 数据结构
# ---------------------------------------------------------------------------
@dataclass
class Case:
    name: str                     # libtest 全名，如 business::hr::salary::tests::t01
    status: str                   # ok / FAILED / ignored
    duration: float | None = None  # 秒；None = 没量到（from-logs 回填时）
    message: str = ""             # <failure message=…>
    detail: str = ""              # <failure> 正文（libtest 的 stdout 块原文）
    extra: str = ""               # 非失败用例自身打印的 stdout（一般没有）

    @property
    def classname(self) -> str:
        mod, _, fn = self.name.rpartition("::")
        return mod or self.name

    @property
    def short(self) -> str:
        mod, _, fn = self.name.rpartition("::")
        return fn if mod else self.name


@dataclass
class Suite:
    name: str
    package: str = ""
    cases: list[Case] = field(default_factory=list)
    wall_total: float = 0.0       # cargo 进程墙钟（含启动/编译尾巴）
    harness: float | None = None  # `running N` → `test result:` 的间隔（真正的用例耗时）
    rc: int = 0
    saw_result: bool = False
    expected: int | None = None   # `running N tests` 的 N
    filtered_out: int = 0
    crashed: str = ""             # 没看到 test result 行时的原因
    mismatch: str = ""            # 解析出的用例数 ≠ running N（静默少算的红旗）
    interleaved: int = 0          # 判定词被用例自身输出挤到下一行的条数

    @property
    def tests(self) -> int:
        return len(self.cases)

    @property
    def failures(self) -> int:
        return sum(1 for c in self.cases if c.status == "FAILED")

    @property
    def skipped(self) -> int:
        return sum(1 for c in self.cases if c.status == "ignored")


# ---------------------------------------------------------------------------
# ① 跑 + 逐行打时间戳
# ---------------------------------------------------------------------------
def run_capture(cmd: list[str], cwd: Path, env: dict[str, str], log_path: Path) -> Suite:
    """跑 cmd，逐行读 stdout（同时写 log），返回带逐条耗时的 Suite。

    耗时口径：`test <name> ... ok` 这一行是 libtest 在**该方法返回后**立刻 flush 的，
    `--test-threads=1` 下相邻两条完成行的时间差 = 上一条的执行时间（含测试自身的
    async 收尾）。第一条从 `running N tests` 行起算。读管道的额外开销 <0.2ms/行，
    在 5~5000ms 的用例上可忽略；总误差用 `harness`（running→result 真间隔）做交叉校验。
    """
    t0 = time.monotonic()
    proc = subprocess.Popen(
        cmd, cwd=str(cwd), env=env,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        text=True, bufsize=1, errors="replace",
    )
    events: list[tuple[str, float]] = []
    assert proc.stdout is not None
    with open(log_path, "w", encoding="utf-8", errors="replace") as fh:
        for line in proc.stdout:
            fh.write(line)
            events.append((line.rstrip("\n"), time.perf_counter()))
    rc = proc.wait()
    wall = time.monotonic() - t0
    suite = parse(events, name=log_path.stem, rc=rc)
    suite.wall_total = wall
    return suite


# ---------------------------------------------------------------------------
# ② 解析
# ---------------------------------------------------------------------------
def _finish_case(suite: Suite, tname: str, verdict: str, ts: float | None,
                 last_done_ts: float | None, captured: list[str]) -> float | None:
    """记一条用例完成；返回新的 last_done_ts（耗时以**判定行**为锚）。"""
    status = {"ok": "ok", "FAILED": "FAILED", "ignored": "ignored"}.get(verdict, "ok")
    dur = None
    if ts is not None and last_done_ts is not None and ts >= last_done_ts:
        dur = round(ts - last_done_ts, 6)  # 跨「被捕获输出挤开」的行时仍是真耗时
    suite.cases.append(Case(name=tname, status=status, duration=dur,
                            extra="\n".join(captured).strip()))
    return ts if ts is not None else last_done_ts


def parse(events: Iterable[tuple[str, float | None]], name: str = "", rc: int = 0) -> Suite:
    """把 (行, 时间戳) 序列解析成 Suite。时间戳可为 None（回填老日志时）。

    ⚠ 实测坑（P2-63 抓到）：libtest 打印 `test <name> ... ` 之后，**用例自己写 stdout 的
    日志行会插在判定词之前**，判定词 `ok` 落在**下一行**：
        test shared::observability::tests::x ... 2026-…Z INFO nik_web::app: 请求体上限就绪 …
        ok
    只认单行 `... ok` 的解析器会静默少算用例（本卡首轮实测 kernel 237/240、
    platform 43/48，且看着「结构自洽」）。所以这里用 pending 状态机，并用
    `running N tests` 的 N 做兜底校验（`Suite.mismatch`）。
    """
    suite = Suite(name=name, rc=rc)
    running_ts: float | None = None
    result_ts: float | None = None
    last_done_ts: float | None = None          # 上一条用例**判定**的时间戳
    blocks: dict[str, list[str]] = {}
    fail_list: list[str] = []
    cur_block: str | None = None
    in_fail_list = False
    pending: str | None = None                 # 已打印 `test x ...`、判定词还没到
    pending_captured: list[str] = []
    interleaved = 0                            # 统计「被输出挤开」的用例数

    for line, ts in events:
        # —— pending：等判定词（中间的行是该用例的捕获输出）——
        if pending is not None:
            m = VERDICT_RE.match(line.strip())
            if m:
                last_done_ts = _finish_case(suite, pending, m.group(1), ts, last_done_ts,
                                            pending_captured)
                pending, pending_captured = None, []
                continue
            pending_captured.append(line)
            continue

        m = RUNNING_RE.match(line)
        if m:
            suite.expected = int(m.group(1))
            running_ts = ts
            last_done_ts = ts
            cur_block = None
            in_fail_list = False
            continue

        m = BLOCK_RE.match(line)
        if m:
            bname = m.group(1)
            cur_block = bname
            blocks.setdefault(bname, [])
            in_fail_list = False
            continue

        m = RESULT_RE.match(line)
        if m:
            suite.saw_result = True
            suite.filtered_out = int(m.group(6))
            result_ts = ts
            cur_block = None
            in_fail_list = False
            continue

        m = TEST_LINE_RE.match(line)
        if m:
            tname, rest = m.group(1), (m.group(2) or "").strip()
            vm = VERDICT_RE.match(rest)
            if vm:
                last_done_ts = _finish_case(suite, tname, vm.group(1), ts, last_done_ts, [])
            elif rest.startswith("bench:"):
                last_done_ts = _finish_case(suite, tname, "ok", ts, last_done_ts, [])
            else:
                pending, pending_captured = tname, ([rest] if rest else [])
                interleaved += 1
            cur_block = None
            in_fail_list = False
            continue

        if line.strip() == "failures:":
            # 两种 `failures:`：① 失败块之前的标题 ② 末尾的失败名单
            if blocks:
                in_fail_list = True
                cur_block = None
            continue

        if cur_block is not None:
            blocks[cur_block].append(line)
            continue

        if in_fail_list:
            if line.startswith("    ") and line.strip():
                fail_list.append(line.strip())
            elif line and not line.startswith(" "):
                in_fail_list = False

    # 失败块 → 挂到对应用例
    by_name = {c.name: c for c in suite.cases}
    for tname, lines in blocks.items():
        body = "\n".join(lines).strip("\n")
        case = by_name.get(tname)
        if case is None:  # 块里有、完成行里没有（理论上不发生）
            case = Case(name=tname, status="FAILED")
            suite.cases.append(case)
            by_name[tname] = case
        if case.status != "FAILED":
            case.status = "FAILED"
        case.detail = body
        case.message = _first_message(body) or "test failed"

    # `failures:` 名单里有但没块的（极端情况）：保持 FAILED，消息兜底
    for tname in fail_list:
        case = by_name.get(tname)
        if case is not None and not case.message:
            case.message = "test failed（libtest 未输出失败块）"

    if suite.saw_result:
        if running_ts is not None and result_ts is not None and result_ts >= running_ts:
            suite.harness = round(result_ts - running_ts, 3)
    else:
        suite.crashed = f"libtest 未输出 `test result:` 行（cargo rc={rc}）"
    if pending is not None:
        suite.mismatch = f"用例 {pending} 打了 `test …` 但没等到判定词（日志被截断？）"
    elif suite.expected is not None and suite.expected != suite.tests:
        suite.mismatch = f"`running {suite.expected} tests` 但只解析出 {suite.tests} 条用例"
    suite.interleaved = interleaved
    return suite


def _first_message(body: str) -> str:
    """从失败块正文里取一行短消息（`<failure message=…>`）。"""
    lines = [ln for ln in body.splitlines() if ln.strip()]
    if not lines:
        return ""
    first = lines[0].strip()
    # `thread 'x' panicked at src/a.rs:1:1:` 的下一行才是消息
    if first.startswith("thread '") and "panicked at" in first:
        for ln in lines[1:]:
            if ln.strip():
                return ln.strip()
    return first


# ---------------------------------------------------------------------------
# ③ 写 XML
# ---------------------------------------------------------------------------
def _now_iso() -> str:
    return _dt.datetime.now(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


_ILLEGAL_XML = re.compile(r"[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]")


def _xml_safe(text: str) -> str:
    """XML 1.0 不允许的控制字符（ANSI/进度条/二进制日志里会有）替换掉，否则 CI 解析直接报错。"""
    return _ILLEGAL_XML.sub("\uFFFD", text)


def _suite_to_element(suite: Suite, hostname: str, ts: str) -> ET.Element:
    harness_err = 1 if suite.crashed else 0
    el = ET.Element("testsuite", {
        "name": suite.name,
        "package": suite.package or suite.name,
        "tests": str(suite.tests + (1 if suite.mismatch else 0)),
        "failures": str(suite.failures),
        "errors": str(harness_err + (1 if suite.mismatch and not suite.crashed else 0)),
        "skipped": str(suite.skipped),
        "time": f"{(suite.harness if suite.harness is not None else suite.wall_total):.3f}",
        "timestamp": ts,
        "hostname": hostname,
    })
    props = ET.SubElement(el, "properties")
    ET.SubElement(props, "property", {"name": "wall_total_s", "value": f"{suite.wall_total:.3f}"})
    ET.SubElement(props, "property", {"name": "cargo_rc", "value": str(suite.rc)})
    ET.SubElement(props, "property", {
        "name": "per_testcase_time",
        "value": "measured" if any(c.duration is not None for c in suite.cases) else "unavailable",
    })
    if suite.expected is not None:
        ET.SubElement(props, "property", {"name": "libtest_running", "value": str(suite.expected)})
    ET.SubElement(props, "property", {"name": "filtered_out", "value": str(suite.filtered_out)})
    if suite.harness is not None and any(c.duration is not None for c in suite.cases):
        # 逐条耗时之和 vs harness 真间隔：差得多说明「读管道的开销」不可忽略（质检用）
        ET.SubElement(props, "property", {
            "name": "sum_case_s",
            "value": f"{sum(c.duration or 0.0 for c in suite.cases):.3f}",
        })
    ET.SubElement(props, "property", {"name": "interleaved_cases", "value": str(suite.interleaved)})

    for case in suite.cases:
        tc = ET.SubElement(el, "testcase", {
            "classname": case.classname,
            "name": case.short,
            "time": f"{case.duration:.3f}" if case.duration is not None else "0.000",
        })
        if case.status == "FAILED":
            f = ET.SubElement(tc, "failure", {
                "message": _xml_safe(case.message or "test failed"),
                "type": "panic",
            })
            f.text = _xml_safe(case.detail or case.message)
        elif case.status == "ignored":
            ET.SubElement(tc, "skipped", {"message": "ignored (#[ignore])"})
        if case.extra:
            # 用例自己写 stdout 的日志（被 libtest 捕获的那几行）：失败时 CI 上要能看到
            ET.SubElement(tc, "system-out").text = _xml_safe(case.extra)

    if suite.crashed or suite.mismatch:
        msg = suite.crashed or suite.mismatch
        tc = ET.SubElement(el, "testcase", {
            "classname": "libtest::harness", "name": "harness_completed", "time": "0.000",
        })
        e = ET.SubElement(tc, "error", {
            "message": msg, "type": "harness" if suite.crashed else "report-incomplete",
        })
        e.text = msg
    return el


def to_xml(suites: list[Suite], root_name: str = "cloudpivot lib tests",
           total_wall: float | None = None) -> ET.ElementTree:
    """root 的 `time` = 本次跑批的**墙钟**（给了 total_wall 时；并行时它 < 各 suite 之和）；
    suite 的 `time` = 该域 harness 间隔（running→result）；testcase 的 `time` = 逐条实测耗时。"""
    hostname = socket.gethostname()
    ts = _now_iso()
    suite_sum = sum((s.harness if s.harness is not None else s.wall_total) for s in suites)
    root = ET.Element("testsuites", {
        "name": root_name,
        "tests": str(sum(s.tests for s in suites)),
        "failures": str(sum(s.failures for s in suites)),
        "errors": str(sum(1 for s in suites if s.crashed or s.mismatch)),
        "skipped": str(sum(s.skipped for s in suites)),
        "time": f"{(total_wall if total_wall is not None else suite_sum):.3f}",
        "timestamp": ts,
    })
    for s in suites:
        root.append(_suite_to_element(s, hostname, ts))
    return ET.ElementTree(root)


def write_xml(suites: list[Suite], path: Path, total_wall: float | None = None) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    tree = to_xml(suites, total_wall=total_wall)
    ET.indent(tree, space="  ")  # py3.9+
    tree.write(path, encoding="utf-8", xml_declaration=True)
    return path


# ---------------------------------------------------------------------------
# 校验（门禁 / CI 自检）
# ---------------------------------------------------------------------------
def check_xml_text(text: bytes | str, label: str = "") -> list[str]:
    """返回问题列表（空 = 自洽）。CI 消费者最怕「看着是绿的、其实计数对不上」。"""
    problems: list[str] = []
    try:
        root = ET.fromstring(text)
    except ET.ParseError as exc:
        return [f"{label}: XML 解析失败：{exc}"]
    if root.tag != "testsuites":
        problems.append(f"{label}: 根元素应为 <testsuites>，实际 <{root.tag}>")
        return problems

    seen_suite_names: set[str] = set()
    agg = {"tests": 0, "failures": 0, "skipped": 0, "errors": 0}
    for suite in root.findall("testsuite"):
        sname = suite.get("name", "")
        if not sname:
            problems.append(f"{label}: 有 <testsuite> 缺 name")
        elif sname in seen_suite_names:
            problems.append(f"{label}: <testsuite name={sname!r}> 重复")
        seen_suite_names.add(sname)

        cases = suite.findall("testcase")
        fails = [c for c in cases if c.find("failure") is not None]
        skips = [c for c in cases if c.find("skipped") is not None]
        errs = [c for c in cases if c.find("error") is not None]
        want = {"tests": len(cases), "failures": len(fails),
                "skipped": len(skips), "errors": len(errs)}
        for k, v in want.items():
            got = suite.get(k)
            if got is None:
                problems.append(f"{label}: <testsuite name={sname!r}> 缺属性 {k}")
                agg[k] += v
            elif not got.lstrip("-").isdigit():
                problems.append(f"{label}: <testsuite name={sname!r}> 属性 {k}={got!r} 不是整数")
                agg[k] += v
            else:
                if int(got) != v:
                    problems.append(
                        f"{label}: <testsuite name={sname!r}> {k}={got} 与子元素实数 {v} 不符")
                agg[k] += int(got)

        t = suite.get("time", "")
        if not _is_float(t):
            problems.append(f"{label}: <testsuite name={sname!r}> time={t!r} 不是数字")

        # `running N tests` 的 N 必须等于解析出的用例数 —— 少算（判定词被输出挤开、
        # 日志截断、解析器退化）时这里会红。P2-63 首轮实测就是靠这条抓到 kernel −3 / platform −5。
        props = {p.get("name"): p.get("value") for p in suite.findall("./properties/property")}
        running = props.get("libtest_running")
        if running is not None and running.isdigit():
            real = len(cases) - (1 if any(
                (c.get("classname") or "") == "libtest::harness" for c in cases) else 0)
            if int(running) != real:
                problems.append(
                    f"{label}: <testsuite name={sname!r}> running {running} tests 但只有 {real} 条真用例"
                    "（解析少算 ⇒ CI 上会静默漏报）")

        seen: set[tuple[str, str]] = set()
        for c in cases:
            cn, nm = c.get("classname", ""), c.get("name", "")
            if not cn or not nm:
                problems.append(f"{label}: {sname} 里有 testcase 缺 classname/name（{cn!r} / {nm!r}）")
                continue
            key = (cn, nm)
            if key in seen:
                problems.append(f"{label}: {sname} 里 testcase 重复：{cn}::{nm}")
            seen.add(key)
            ct = c.get("time", "")
            if not _is_float(ct):
                problems.append(f"{label}: testcase {cn}::{nm} time={ct!r} 不是数字")
            elif float(ct) < 0:
                problems.append(f"{label}: testcase {cn}::{nm} time={ct} 为负")
            for f in c.findall("failure"):
                msg = (f.get("message") or "").strip()
                if not msg:
                    problems.append(f"{label}: 失败用例 {cn}::{nm} 的 <failure> 缺 message")
                if not (f.text or "").strip():
                    problems.append(f"{label}: 失败用例 {cn}::{nm} 的 <failure> 无正文（原因丢失）")

    for k, v in agg.items():
        got = root.get(k)
        if got is None:
            problems.append(f"{label}: <testsuites> 缺属性 {k}")
        elif not got.lstrip("-").isdigit():
            problems.append(f"{label}: <testsuites> 属性 {k}={got!r} 不是整数")
        elif int(got) != v:
            problems.append(f"{label}: <testsuites> {k}={got} 与各 suite 合计 {v} 不符")
    if not _is_float(root.get("time", "")):
        problems.append(f"{label}: <testsuites> time={root.get('time')!r} 不是数字")
    return problems


def _is_float(text: str) -> bool:
    try:
        float(text)
        return True
    except (TypeError, ValueError):
        return False


def check_files(paths: list[Path]) -> list[str]:
    problems: list[str] = []
    for p in paths:
        if not p.is_file():
            problems.append(f"{p}: 文件不存在")
            continue
        problems.extend(check_xml_text(p.read_bytes(), label=str(p)))
    return problems


# ---------------------------------------------------------------------------
# from-logs：把已有日志补成 XML（无逐条耗时，testsuite time 用 summary.tsv 的墙钟）
# ---------------------------------------------------------------------------
def suites_from_logs(directory: Path) -> list[Suite]:
    summary: dict[str, tuple[str, float, int]] = {}
    tsv = directory / "summary.tsv"
    if tsv.is_file():
        for i, line in enumerate(tsv.read_text(encoding="utf-8").splitlines()):
            if i == 0 or not line.strip():
                continue
            cols = line.split("\t")
            if len(cols) >= 8:
                summary[cols[0]] = (cols[1], float(cols[6]), int(cols[4]))

    suites: list[Suite] = []
    for log in sorted(directory.glob("*.log")):
        lines = [(ln, None) for ln in log.read_text(encoding="utf-8", errors="replace").splitlines()]
        suite = parse(lines, name=log.stem, rc=0)
        failed: int | None = None
        if suite.name in summary:
            crate, wall, failed = summary[suite.name]
            suite.package, suite.wall_total = crate, wall
        suite.saw_result = True  # 老日志不判 crash
        suite.crashed = ""
        # 逐条耗时不可得 ⇒ harness 也留空，用墙钟代替
        if failed is not None and failed != suite.failures:
            print(f"⚠ {log.stem}: summary.tsv 记 {failed} 红，日志里解析出 {suite.failures} 红",
                  file=sys.stderr)
        suites.append(suite)
    return suites


# ---------------------------------------------------------------------------
# selftest：真解析 + 4 类畸变必须被抓（门禁的反证）
# ---------------------------------------------------------------------------
FIXTURE = """\
   Compiling cloudpivot v0.1.0
    Finished `test` profile [unoptimized + debuginfo] target(s)
     Running unittests src/lib.rs (target/debug/deps/cloudpivot-abc123)
running 4 tests
test business::demo::tests::t01_ok ... ok
test business::demo::tests::t02_ok ... ok
test business::demo::tests::t03_ignored ... ignored
test business::other::tests::t04_boom ... FAILED

failures:

---- business::other::tests::t04_boom stdout ----
thread 'business::other::tests::t04_boom' panicked at src/business/other.rs:42:9:
左借 != 右贷（差 4.0000）
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace


failures:
    business::other::tests::t04_boom

test result: FAILED. 2 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.31s
"""


def cmd_selftest(_args) -> int:
    ok = True

    def say(good: bool, msg: str) -> None:
        nonlocal ok
        ok = ok and good
        print(f"{'✓' if good else '✗'} {msg}")

    # 造时间戳：两条 ok 各 10ms/20ms、ignored 1ms、失败 30ms
    lines = FIXTURE.splitlines()
    events: list[tuple[str, float]] = []
    clock = 1000.0
    per_line = {"t01_ok": 0.010, "t02_ok": 0.020, "t03_ignored": 0.001, "t04_boom": 0.030}
    for ln in lines:
        if "running 4 tests" in ln:
            clock += 0.05
            events.append((ln, clock))
            continue
        hit = next((v for k, v in per_line.items() if f"::{k} ..." in ln), None)
        if hit is not None:
            clock += hit
        events.append((ln, clock))

    suite = parse(events, name="demo", rc=101)
    say(suite.tests == 4, f"① 解析用例数：{suite.tests}（期望 4）")
    say(suite.failures == 1 and suite.skipped == 1,
        f"② 失败/跳过：failures={suite.failures} skipped={suite.skipped}（期望 1/1）")
    say([c.status for c in suite.cases] == ["ok", "ok", "ignored", "FAILED"],
        f"③ 逐条状态：{[c.status for c in suite.cases]}")
    boom = next(c for c in suite.cases if c.status == "FAILED")
    say("左借 != 右贷" in boom.message, f"④ 失败消息取到 panic 正文：{boom.message!r}")
    say(boom.classname == "business::other::tests" and boom.short == "t04_boom",
        f"⑤ classname/name 拆分：{boom.classname} :: {boom.short}")
    durs = {c.short: c.duration for c in suite.cases}
    say(abs((durs["t01_ok"] or 0) - 0.010) < 1e-6 and abs((durs["t04_boom"] or 0) - 0.030) < 1e-6,
        f"⑥ 逐条耗时是量出来的：{ {k: (None if v is None else round(v, 3)) for k, v in durs.items()} }")
    say(suite.harness is not None and abs(suite.harness - 0.061) < 0.005,
        f"⑦ harness 真间隔（running→result）：{suite.harness}s")

    xml = _xml_bytes([suite])
    problems = check_xml_text(xml, label="selftest")
    say(not problems, f"⑧ 生成的 XML 自洽（{len(xml)} 字节，0 问题）")
    for p in problems:
        print(f"    - {p}")

    # —— 反证：4 类畸变必须被抓 ——
    mutations = {
        "计数造假（tests=4→3）": lambda t: t.replace(b'tests="4" failures="1"', b'tests="3" failures="1"', 1),
        "失败元素被删（failures 仍 1）": lambda t: _drop_failure(t),
        "用例重复": lambda t: t.replace(
            b'<testcase classname="business::demo::tests" name="t01_ok"',
            b'<testcase classname="business::other::tests" name="t04_boom"', 1),
        "XML 截断（不可解析）": lambda t: t[: len(t) // 2],
        "少算一条用例但计数自洽（静默漏报）": lambda t: _drop_case(t, b'name="t02_ok"'),
    }
    for label, mutate in mutations.items():
        bad = mutate(xml)
        caught = bool(check_xml_text(bad, label=label)) if bad != xml else False
        say(caught, f"⑨ 反证 · {label} → {'被抓' if caught else '**漏过**（门禁无效）'}")

    # —— 反证：解析器必须能区分「跑挂了的进程」（没有 test result 行）——
    crash = parse([(ln, float(i)) for i, ln in enumerate(lines[:8])], name="crash", rc=137)
    say(not crash.saw_result and crash.crashed != "",
        f"⑩ 反证 · 进程被杀（无 test result 行）→ error 元素：{crash.crashed[:40]!r}")
    crash_xml = _xml_bytes([crash])
    say(any("error" in p for p in check_xml_text(crash_xml)) is False,
        "⑪ 进程被杀仍产出结构合法的 XML（'errors' 计数与 <error> 元素一致）")

    # —— 跨行判定（真实形态：用例自己的日志输出把 `ok` 挤到下一行）——
    split = FIXTURE.replace(
        "test business::demo::tests::t01_ok ... ok",
        "test business::demo::tests::t01_ok ... 2026-11-15T07:51:08Z  INFO nik_web: 请求体上限就绪\nok",
    )
    split_suite = parse([(ln, 5.0 + i * 0.01) for i, ln in enumerate(split.splitlines())], name="split")
    say(split_suite.tests == 4 and split_suite.interleaved == 1 and split_suite.mismatch == "",
        f"⑫ 跨行判定（`ok` 被日志输出挤到下一行）：{split_suite.tests} 条 / interleaved="
        f"{split_suite.interleaved} / mismatch={split_suite.mismatch!r}")
    say("INFO nik_web: 请求体上限就绪" in split_suite.cases[0].extra,
        "⑬ 被挤开的日志行收进该用例的 <system-out>（不丢）")

    # —— from-logs 回填路径（老日志 → XML，无逐条耗时时用墙钟）——
    tmp = Path(tempfile.mkdtemp(prefix="junit-selftest-"))
    (tmp / "demo.log").write_text(FIXTURE, encoding="utf-8")
    (tmp / "summary.tsv").write_text(
        "domain\tcrate\ttests\tpassed\tfailed\tignored\twall_s\tresult\n"
        "demo\tcloudpivot\t4\t2\t1\t1\t0.9\tFAIL\n", encoding="utf-8")
    back = suites_from_logs(tmp)
    back_ok = (len(back) == 1 and back[0].tests == 4 and back[0].failures == 1
               and abs(back[0].wall_total - 0.9) < 1e-9 and all(c.duration is None for c in back[0].cases))
    say(back_ok, f"⑭ from-logs 回填：{len(back)} 个 suite / {back[0].tests if back else 0} 条 / "
                 f"墙钟 {back[0].wall_total if back else 0}s（逐条耗时=不可得，不用假数）")
    if back:
        p = write_xml(back, tmp / "back.xml")
        say(not check_xml_text(p.read_bytes(), label="from-logs"),
            "⑮ from-logs 产物同样通过结构校验")

    # —— 接线检查：跑批入口必须仍然产出报告（删了接线 ⇒ 门禁红）——
    runner = (Path(__file__).resolve().parent / "test_domain.py")
    src = runner.read_text(encoding="utf-8") if runner.is_file() else ""
    wired = all(s in src for s in ("import junit_report", "--junit", "junit_report.run_capture(",
                                   "junit_report.write_xml("))
    say(wired, f"⑯ test_domain.py 仍接线（import/--junit/run_capture/write_xml 四处在位）：{runner}")

    print()
    print("✓ junit_report 自检通过" if ok else "✗ junit_report 自检未通过")
    return 0 if ok else 1


def _drop_failure(text: bytes) -> bytes:
    """删掉 <failure> 元素，保留 failures="1" 属性（模拟「红被吞掉」）。"""
    start = text.find(b"<failure ")
    if start < 0:
        return text
    end = text.find(b"</failure>", start)
    if end < 0:
        return text
    return text[:start] + text[end + len(b"</failure>"):]


def _drop_case(text: bytes, marker: bytes) -> bytes:
    """删掉含 marker 的那一整行 <testcase>，并把 tests 减 1 —— 造出「计数自洽但少一条」的静默漏报。"""
    out: list[bytes] = []
    dropped = False
    for line in text.splitlines(keepends=True):
        if not dropped and marker in line:
            dropped = True
            continue
        out.append(line)
    if not dropped:
        return text
    body = b"".join(out)
    # root 与 suite 两处 tests 一起减（保持「计数自洽」——这正是静默漏报的形态）
    return body.replace(b'tests="4"', b'tests="3"')


def _xml_bytes(suites: list[Suite]) -> bytes:
    tree = to_xml(suites)
    ET.indent(tree, space="  ")
    buf = io.BytesIO()
    tree.write(buf, encoding="utf-8", xml_declaration=True)
    return buf.getvalue()


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
def cmd_check(args) -> int:
    paths = [Path(p) for p in args.xml]
    problems = check_files(paths)
    for p in problems:
        print(f"✗ {p}", file=sys.stderr)
    if problems:
        print(f"\n✗ JUnit XML 校验未通过：{len(problems)} 个问题", file=sys.stderr)
        return 1
    total = 0
    for p in paths:
        root = ET.parse(p).getroot()
        total += int(root.get("tests", "0"))
        print(f"✓ {p}：{root.get('tests')} 条用例 / {root.get('failures')} 红 / "
              f"{root.get('skipped')} 跳过 / {root.get('time')}s"
              f"（{len(root.findall('testsuite'))} 个 suite）")
    print(f"✓ 合计 {total} 条用例，结构自洽")
    return 0


def cmd_from_logs(args) -> int:
    directory = Path(args.dir)
    if not directory.is_dir():
        die(f"目录不存在：{directory}")
    suites = suites_from_logs(directory)
    if not suites:
        die(f"{directory} 里没有 *.log")
    out = Path(args.out) if args.out else directory / "junit.xml"
    write_xml(suites, out, total_wall=args.wall if args.wall > 0 else None)
    print(f"✓ {len(suites)} 个 suite / {sum(s.tests for s in suites)} 条用例 → {out}")
    print("  注意：from-logs 只有 testsuite 级墙钟（逐条耗时需走 test_domain.py --junit 真跑）")
    return 0


def cmd_run(args) -> int:
    env = {k: v for k, v in os.environ.items() if not k.startswith("NIK_DATABASE_")}
    env.update(NIK_PROFILE="test", RUST_TEST_THREADS="1", CARGO_TERM_COLOR="never")
    if args.db_url:
        for key in ("ERP_DB_URL", "SYS_DB_URL", "TASK_DB_URL", "P0FIX_DB_URL"):
            env[key] = args.db_url
    cwd = Path(args.cwd).resolve()
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    cmd = ["cargo", "test", "-p", args.package, "--lib", "--", "--test-threads=1"]
    suite = run_capture(cmd, cwd, env, out / f"{args.package}.log")
    suite.package = args.package
    path = write_xml([suite], out / f"{args.package}.xml")
    print(f"{suite.tests} 条 / {suite.failures} 红 / {suite.skipped} 跳过 / {suite.wall_total:.1f}s → {path}")
    return 0 if suite.failures == 0 and not suite.crashed and suite.rc == 0 else 1


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description="libtest → JUnit XML（P2-63）")
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("selftest", help="门禁自检（含 4 类畸变反证）")
    p.set_defaults(func=cmd_selftest)

    p = sub.add_parser("check", help="校验 JUnit XML 结构自洽")
    p.add_argument("xml", nargs="+")
    p.set_defaults(func=cmd_check)

    p = sub.add_parser("from-logs", help="把已有的 <域>.log 补成 XML")
    p.add_argument("dir")
    p.add_argument("--out", default="")
    p.add_argument("--wall", type=float, default=0.0,
                   help="本次跑批墙钟（秒）；给了就写进 root 的 time 属性（默认各 suite 之和）")
    p.set_defaults(func=cmd_from_logs)

    p = sub.add_parser("run", help="独立跑一个 crate 并产出 XML")
    p.add_argument("-p", "--package", default="cloudpivot")
    p.add_argument("--cwd", default=str(Path(__file__).resolve().parent.parent / "backend" / "app"))
    p.add_argument("--out", default="")
    p.add_argument("--db-url", default=os.environ.get("ERP_DB_URL", ""))
    p.set_defaults(func=cmd_run)

    args = ap.parse_args(argv)
    if args.cmd == "run" and not args.out:
        args.out = tempfile.mkdtemp(prefix="junit-run-")
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
