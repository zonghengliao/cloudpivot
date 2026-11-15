#!/usr/bin/env python3
# ============================================================================
# test_domain.py —— 按域跑 Rust lib 测试套件（P2-61）
#
# 为什么：
#   `backend/` 现在是 workspace（app + crates/*），但**全量 lib 用例仍然只有两个入口**：
#   `cargo test -p cloudpivot --lib`（5448 条）/ `-p cloudpivot-kernel`（240）/ `-p cloudpivot-doc`（19）。
#   一个 target 只能串行跑、一处红要重跑全量、也没法几个域同时跑。
#   本脚本用登记表 `backend/test-domains.tsv` 把用例**按域切开**，于是可以：
#     · `--package hr` 只跑 HR 那一套（快、红在哪一目了然）；
#     · `--package all --jobs 4` 各域并行（每域一份自己的库克隆，互不踩夹具）；
#     · `--package cloudpivot` 仍然是「单进程全量」—— 这是改前口径，用于 A/B 对比。
#
# 选中语义（关键，别改成「前缀当 cargo 过滤器」）：
#   · 域归属由登记表的前缀**在 Python 里算**（`resolve_owner`：最长前缀者胜，和门禁同一份实现）；
#   · 跑的时候把该域的**用例全名**逐个传给 libtest（`--exact <name…>`）。
#     不能写成 `cargo test <前缀>`：libtest 的过滤器是**子串**匹配，`task::` 会把
#     `business::task::` 的 250 条一起吞掉（实测 platform 从 43 变 293、全量多跑 252 条重复）。
#
# 三个子命令：
#   list    列出登记表里的域（不编译、不连库）
#   check   门禁用：用例↔域**恰好一一对应**（无遗漏/无重复/域非空/crate 真实存在）
#   run     真跑：克隆库 → 编译热身 → 并行跑 → 汇总失败名到 failures.txt
#
# 用法：
#   python3 scripts/test_domain.py list
#   python3 scripts/test_domain.py check
#   python3 scripts/test_domain.py run --package hr
#   python3 scripts/test_domain.py run --package erp-cost,hr,flow --jobs 3
#   python3 scripts/test_domain.py run --package all --jobs 4
#   python3 scripts/test_domain.py run --package cloudpivot            # 单进程全量（改前口径）
#
# 库：默认模板 `cloudpivot_test_full`（`bash scripts/verify/make_full_suite_db.sh` 造一次），
#     每域 `CREATE DATABASE … TEMPLATE` 克隆一份，名字必含 `cloudpivot_test`（HR/flow 的库名闸）。
#     结束后自动 DROP（有红或 `--keep` 时保留现场）。
#     99 上默认连 172.18.10.145，145 上默认连 127.0.0.1 —— 与卡面「按运行位置分地址」一致。
#
# 退出码：0 = 全绿；1 = 有红；2 = 前置/参数错误（模板库缺失、crate 不存在、登记表坏）
# ============================================================================
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
import time
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from pathlib import Path
from typing import NoReturn

REPO = Path(__file__).resolve().parent.parent
BACKEND = REPO / "backend"
APP_DIR = BACKEND / "app"  # CWD：全量套件必须在 backend/app（scope_model::d8c 扫 src/）
MANIFEST = BACKEND / "test-domains.tsv"

PG_USER = os.environ.get("PGUSER", "cloudpivot")
PG_PASS = os.environ.get("PGPASS", "fbb1263691ff75c31ec99eaf258138b4")
PG_PORT = os.environ.get("PGPORT", "5432")
TEMPLATE = os.environ.get("TPL", "cloudpivot_test_full")
CLONE_PREFIX = "cloudpivot_test_p261_"

RESULT_RE = re.compile(r"^test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored")


# ---------------------------------------------------------------------------
# 登记表
# ---------------------------------------------------------------------------
@dataclass
class Domain:
    name: str
    crate: str
    filters: list[str]
    note: str = ""

    @property
    def whole_crate(self) -> bool:
        return not self.filters


def die(msg: str, code: int = 2) -> NoReturn:
    print(f"!! {msg}", file=sys.stderr)
    sys.exit(code)


def load_domains(path: Path = MANIFEST) -> list[Domain]:
    if not path.is_file():
        die(f"登记表不存在：{path}")
    doms: list[Domain] = []
    seen: dict[str, int] = {}
    for lineno, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw.rstrip("\n")
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        cols = line.split("\t")
        if len(cols) < 3:
            die(f"{path}:{lineno} 列数不足（要 TAB 分隔的 ≥3 列）：{line!r}")
        name, crate, filt = cols[0].strip(), cols[1].strip(), cols[2].strip()
        note = cols[3].strip() if len(cols) > 3 else ""
        if not name or not crate:
            die(f"{path}:{lineno} 域或 crate 为空：{line!r}")
        if name in seen:
            die(f"{path}:{lineno} 域名重复：{name}（上次在 {seen[name]} 行）")
        seen[name] = lineno
        filters = [] if filt in ("-", "") else filt.split()
        for f in filters:
            if not f.endswith("::"):
                # 不带尾部 `::` 的前缀会误吞兄弟域（business::task:: 吞 business::taskflow::）
                die(f"{path}:{lineno} 过滤前缀必须以 `::` 结尾：{f!r}（域 {name}）")
        doms.append(Domain(name, crate, filters, note))
    if not doms:
        die(f"登记表为空：{path}")
    return doms


def cargo_meta_packages() -> set[str]:
    """workspace 里真实存在的 crate 名（判「crate 列写得对不对」）。"""
    try:
        out = subprocess.run(
            ["cargo", "metadata", "--no-deps", "--format-version", "1"],
            cwd=APP_DIR, capture_output=True, text=True, check=True,
        ).stdout
    except (subprocess.CalledProcessError, OSError) as exc:  # pragma: no cover
        die(f"cargo metadata 失败：{exc}")
    return {p["name"] for p in json.loads(out)["packages"]}


# ---------------------------------------------------------------------------
# 用例清单与归属
# ---------------------------------------------------------------------------
def list_tests(crate: str) -> list[str]:
    """`cargo test -p <crate> --lib -- --list` 的全部用例名（不连库）。"""
    env = dict(os.environ, NIK_PROFILE="test", CARGO_TERM_COLOR="never")
    p = subprocess.run(["cargo", "test", "-p", crate, "--lib", "--", "--list"],
                       cwd=APP_DIR, capture_output=True, text=True, env=env)
    if p.returncode != 0:
        die(f"列用例失败（{crate}）：\n{p.stderr[-2000:]}")
    return [ln[: -len(": test")] for ln in p.stdout.splitlines() if ln.endswith(": test")]


def resolve_owner(test: str, doms: list[Domain]) -> tuple[Domain | None, list[Domain]]:
    """最长前缀者胜；返回 (胜者, 所有命中者)。无前缀（整 crate）域以长度 0 参与，作为该 crate 的兜底。"""
    hits: list[tuple[int, Domain]] = []
    for d in doms:
        if d.whole_crate:
            hits.append((0, d))
            continue
        for f in d.filters:
            if test.startswith(f):
                hits.append((len(f), d))
                break
    if not hits:
        return None, []
    best = max(h[0] for h in hits)
    winners = [d for ln, d in hits if ln == best]
    return winners[0], winners


# ---------------------------------------------------------------------------
# DB 前置
# ---------------------------------------------------------------------------
def pg_host() -> str:
    if os.environ.get("PGHOST"):
        return os.environ["PGHOST"]
    try:
        ips = subprocess.run(["hostname", "-I"], capture_output=True, text=True).stdout.split()
    except OSError:
        ips = []
    return "127.0.0.1" if "172.18.10.145" in ips else "172.18.10.145"


def psql(db: str, sql: str, host: str) -> str:
    env = dict(os.environ, PGPASSWORD=PG_PASS)
    p = subprocess.run(
        ["psql", "-h", host, "-p", PG_PORT, "-U", PG_USER, "-d", db, "-Atc", sql],
        capture_output=True, text=True, env=env,
    )
    if p.returncode != 0:
        die(f"psql 失败（{host}:{PG_PORT}/{db}）：{sql}\n{p.stderr.strip()}")
    return p.stdout.strip()


def db_url(host: str, db: str) -> str:
    return f"postgres://{PG_USER}:{PG_PASS}@{host}:{PG_PORT}/{db}"


def clone_name(domain: str) -> str:
    return CLONE_PREFIX + re.sub(r"[^a-z0-9]+", "_", domain.lower())


def provision_dbs(names: list[str], host: str) -> dict[str, str]:
    if psql("postgres", "select 1", host) != "1":
        die(f"连不上 PG（{host}:{PG_PORT}）")
    if psql("postgres", f"select 1 from pg_database where datname='{TEMPLATE}'", host) != "1":
        die(
            f"模板库 {TEMPLATE} 不存在。先造一次（约 20~40 s，需已构建的二进制）：\n"
            f"    PGHOST={host} DB_USER={PG_USER} DB_PASS=*** ERP_DB=cloudpivot_test_full \\\n"
            f"      bash scripts/verify/make_full_suite_db.sh"
        )

    env = dict(os.environ, PGPASSWORD=PG_PASS)

    def one(dname: str) -> tuple[str, str]:
        db = clone_name(dname)
        subprocess.run(["dropdb", "-h", host, "-p", PG_PORT, "-U", PG_USER, "--if-exists", db],
                       env=env, check=False, capture_output=True)
        p = subprocess.run(["createdb", "-h", host, "-p", PG_PORT, "-U", PG_USER, "-T", TEMPLATE, db],
                           env=env, capture_output=True, text=True)
        if p.returncode != 0:
            die(f"克隆库失败（{db}）：{p.stderr.strip()}")
        return dname, db

    with ThreadPoolExecutor(max_workers=min(8, len(names))) as ex:
        got = dict(ex.map(one, names))
    return {name: db_url(host, db) for name, db in got.items()}


def drop_dbs(dbs: list[str], host: str) -> None:
    env = dict(os.environ, PGPASSWORD=PG_PASS)
    for db in dbs:
        subprocess.run(["dropdb", "-h", host, "-p", PG_PORT, "-U", PG_USER, "--if-exists", "--force", db],
                       env=env, check=False, capture_output=True)


# ---------------------------------------------------------------------------
# 跑
# ---------------------------------------------------------------------------
def build_env(url: str | None) -> dict[str, str]:
    # NIK_DATABASE_* 一律删掉：那些是「造模板库」时用的整库连接串，
    # 留在环境里会让部分用例连错库（本脚本只用四条 *_DB_URL 精确定位）。
    env = {k: v for k, v in os.environ.items() if not k.startswith("NIK_DATABASE_")}
    env.update(NIK_PROFILE="test", RUST_TEST_THREADS="1", CARGO_TERM_COLOR="never")
    if url:
        env.update(ERP_DB_URL=url, SYS_DB_URL=url, TASK_DB_URL=url, P0FIX_DB_URL=url)
    return env


def run_cmd(d: Domain, tests: list[str]) -> list[str]:
    # ⚠ 用例名必须跟着 `--exact` 放在 `--` 之后：
    #   ① cargo 自己只接受**一个**位置参数 TESTNAME，第二个就报 `unexpected argument`；
    #   ② libtest 的过滤器是**子串**匹配（`task::` 会吞 `business::task::`），
    #      加 `--exact` 才是「全名精确相等」，和登记表的归属口径一致。
    cmd = ["cargo", "test", "-p", d.crate, "--lib", "--"]
    if tests:
        cmd += ["--exact", *tests]
    cmd += ["--test-threads=1"]
    return cmd


def run_one(d: Domain, tests: list[str], url: str | None, out: Path, idx: int, total: int) -> dict:
    cmd = run_cmd(d, tests)
    log = out / f"{d.name}.log"
    t0 = time.monotonic()
    with open(log, "w", encoding="utf-8") as fh:
        rc = subprocess.run(cmd, cwd=APP_DIR, stdout=fh, stderr=subprocess.STDOUT,
                            env=build_env(url)).returncode
    wall = time.monotonic() - t0
    passed, failed, ignored, fails = parse_log(log)
    print(f"  [{idx}/{total}] {d.name:<14} rc={rc} {passed:>5} passed / {failed} failed  {wall:6.1f}s",
          flush=True)
    return {"domain": d.name, "crate": d.crate, "rc": rc, "passed": passed, "failed": failed,
            "ignored": ignored, "tests": passed + failed + ignored, "fails": fails, "wall": wall,
            "log": str(log)}


def parse_log(path: Path) -> tuple[int, int, int, list[str]]:
    passed = failed = ignored = 0
    fails: list[str] = []
    in_fail_block = False
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        m = RESULT_RE.match(line)
        if m:
            passed += int(m.group(2))
            failed += int(m.group(3))
            ignored += int(m.group(4))
            in_fail_block = False
            continue
        if line == "failures:":
            in_fail_block = True
            continue
        if in_fail_block:
            if line.startswith("    ") and not line.startswith("     "):
                fails.append(line.strip())
            elif line.startswith("test result:") or (line and not line.startswith(" ")):
                in_fail_block = False
    return passed, failed, ignored, fails


def cmd_run(args) -> int:
    doms = load_domains()
    by_name = {d.name: d for d in doms}
    real_crates = cargo_meta_packages()

    spec = [s.strip() for s in args.package.split(",") if s.strip()]
    if not spec:
        die("--package 为空（可用域名见 `list`，或 all / 某个 crate 名）")
    selected: list[Domain] = []
    for s in spec:
        if s == "all":
            selected = list(doms)
            break
        if s in by_name:
            selected.append(by_name[s])
        elif s in real_crates:
            # crate 名 → 整 crate 一段（`--package cloudpivot` = 单进程全量，改前口径）
            selected.append(Domain(name=s, crate=s, filters=[], note="整 crate"))
        else:
            die(f"未知的 --package：{s!r}。可用域名：{', '.join(sorted(by_name))}；也可用 crate 名或 all")
    # 去重（保留顺序）
    uniq: dict[tuple[str, tuple[str, ...]], Domain] = {}
    for d in selected:
        uniq.setdefault((d.name, tuple(d.filters)), d)
    selected = list(uniq.values())

    host = pg_host()
    print(f"==> 域 {len(selected)} 套 | PG {host}:{PG_PORT} | 模板 {TEMPLATE} | jobs={args.jobs}")

    crates = sorted({d.crate for d in selected})
    t0 = time.monotonic()
    for crate in crates:
        p = subprocess.run(["cargo", "test", "-p", crate, "--lib", "--no-run"],
                           cwd=APP_DIR, capture_output=True, text=True,
                           env=dict(os.environ, NIK_PROFILE="test", CARGO_TERM_COLOR="never"))
        if p.returncode != 0:
            die(f"编译失败（{crate}）：\n{p.stderr[-3000:]}")
    print(f"==> 编译热身 {len(crates)} 个 crate：{time.monotonic() - t0:.1f}s"
          f"（串行做，避免并行跑时抢 target 锁）")

    # 归属在 Python 里算（与门禁同一份 resolve_owner）→ 每个域拿到的是**用例全名**
    t0 = time.monotonic()
    all_tests = {crate: list_tests(crate) for crate in crates}
    picked: dict[str, list[str]] = {}
    for d in selected:
        if d.name not in by_name:  # 整 crate 伪域（--package cloudpivot）
            picked[d.name] = list(all_tests[d.crate])
            continue
        cand = [x for x in doms if x.crate == d.crate]
        picked[d.name] = []
        for t in all_tests[d.crate]:
            winner, winners = resolve_owner(t, cand)
            if winner is not None and len(winners) == 1 and winner.name == d.name:
                picked[d.name].append(t)
    print(f"==> 用例清单 {time.monotonic() - t0:.1f}s：" +
          "，".join(f"{d.name}={len(picked[d.name])}" for d in selected))
    empty = [d.name for d in selected if not picked[d.name]]
    if empty:
        die(f"这些域一条用例都没有（登记表前缀写错？）：{empty}")

    urls: dict[str, str] = {}
    if not args.no_db:
        t0 = time.monotonic()
        urls = provision_dbs([d.name for d in selected], host)
        print(f"==> 克隆库 {len(urls)} 份：{time.monotonic() - t0:.1f}s")

    out = Path(args.out) if args.out else Path(tempfile.mkdtemp(prefix="p261-test-domain-"))
    out.mkdir(parents=True, exist_ok=True)

    order = sorted(selected, key=lambda d: len(picked[d.name]), reverse=True)  # 最长优先（LPT）
    print(f"==> 开始跑（最长优先）：{' → '.join(d.name for d in order)}")
    t_start = time.monotonic()
    with ThreadPoolExecutor(max_workers=max(1, args.jobs)) as ex:
        futures = [ex.submit(run_one, d, picked[d.name], urls.get(d.name), out, i + 1, len(order))
                   for i, d in enumerate(order)]
        results = [f.result() for f in futures]
    wall = time.monotonic() - t_start

    # 汇总
    print()
    hdr = f"{'DOMAIN':<14}{'CRATE':<20}{'TESTS':>6}{'PASS':>7}{'FAIL':>6}{'WALL(s)':>9}  RESULT"
    print(hdr)
    print("-" * len(hdr))
    total_pass = total_fail = 0
    for r in sorted(results, key=lambda r: -r["tests"]):
        total_pass += r["passed"]
        total_fail += r["failed"]
        print(f"{r['domain']:<14}{r['crate']:<20}{r['tests']:>6}{r['passed']:>7}{r['failed']:>6}"
              f"{r['wall']:>9.1f}  {'OK' if r['rc'] == 0 else 'FAIL'}")
    print("-" * len(hdr))
    print(f"{'合计':<14}{'':<20}{total_pass + total_fail:>6}{total_pass:>7}{total_fail:>6}{wall:>9.1f}"
          f"  {'全绿' if total_fail == 0 else '有红'}")

    fails = sorted({n for r in results for n in r["fails"]})
    (out / "failures.txt").write_text("\n".join(fails) + ("\n" if fails else ""), encoding="utf-8")
    with open(out / "summary.tsv", "w", encoding="utf-8") as fh:
        fh.write("domain\tcrate\ttests\tpassed\tfailed\tignored\twall_s\tresult\n")
        for r in results:
            fh.write(f"{r['domain']}\t{r['crate']}\t{r['tests']}\t{r['passed']}\t{r['failed']}\t"
                     f"{r['ignored']}\t{r['wall']:.1f}\t{'OK' if r['rc'] == 0 else 'FAIL'}\n")
    print(f"\n==> 日志与汇总：{out}\n==> 失败用例名：{out / 'failures.txt'}（{'空' if not fails else str(len(fails)) + ' 条'}）")

    if urls and (args.keep or total_fail):
        print(f"==> 保留 {len(urls)} 份克隆库（--keep 或 有红）：{', '.join(Path(u).name for u in urls.values())}")
    elif urls:
        t0 = time.monotonic()
        drop_dbs([Path(u).name for u in urls.values()], host)
        print(f"==> 已删除 {len(urls)} 份克隆库：{time.monotonic() - t0:.1f}s")

    return 0 if total_fail == 0 else 1


# ---------------------------------------------------------------------------
# list / check
# ---------------------------------------------------------------------------
def cmd_list(_args) -> int:
    doms = load_domains()
    w = max(len(d.name) for d in doms) + 2
    print(f"{'DOMAIN':<{w}}{'CRATE':<20}{'PREFIX':>7}  NOTE")
    for d in doms:
        print(f"{d.name:<{w}}{d.crate:<20}{len(d.filters):>7}  {d.note}")
    print(f"\n{len(doms)} 个域；登记表 {MANIFEST}")
    return 0


def cmd_check(args) -> int:
    doms = load_domains()
    real = cargo_meta_packages()

    # ① crate 必须真实存在（否则 cargo 会报 "package ID specification did not match"）
    bad = sorted({d.crate for d in doms if d.crate not in real})
    if bad:
        die(f"登记表里的 crate 在 workspace 里不存在：{bad}（真实 crate：{sorted(real)}）")

    if args.manifest_only:
        # 只校登记表本身（秒级、不编译）：crate 名 / 前缀尾 `::` / 域名唯一 / 非空
        # —— check.sh 的 L0 用这一档；完整性（用例一一对应）要列用例，见不带本参数的全量档。
        print(f"✓ 登记表自洽：{len(doms)} 个域，crate 全部存在"
              f"（{len({d.crate for d in doms})} 个 crate：{', '.join(sorted({d.crate for d in doms}))}）")
        print("  提示：完整性检查（用例↔域一一对应）请跑不带 --manifest-only 的档"
              "（bash scripts/verify/test_domains_gate.sh）")
        return 0

    # ② 每 crate 的用例清单（可用 --tests-from DIR 读预存的 <crate>.txt 免编译）
    per_crate: dict[str, list[str]] = {}
    for crate in sorted({d.crate for d in doms}):
        pre = Path(args.tests_from) / f"{crate}.txt" if args.tests_from else None
        if pre and pre.is_file():
            per_crate[crate] = [l.strip() for l in pre.read_text(encoding="utf-8").splitlines() if l.strip()]
        else:
            per_crate[crate] = list_tests(crate)

    total = 0
    counts: dict[str, int] = {d.name: 0 for d in doms}
    unassigned: list[str] = []
    ambiguous: list[tuple[str, list[str]]] = []
    for crate, names in per_crate.items():
        cand = [d for d in doms if d.crate == crate]
        for t in names:
            total += 1
            winner, winners = resolve_owner(t, cand)
            if winner is None:
                unassigned.append(t)
            elif len(winners) > 1:
                ambiguous.append((t, [w.name for w in winners]))
            else:
                counts[winner.name] += 1

    print(f"用例总数 {total}（{', '.join(f'{c}={len(n)}' for c, n in per_crate.items())}）")
    w = max(len(d.name) for d in doms) + 2
    for d in doms:
        mark = "" if counts[d.name] else "  ← 空域"
        print(f"  {d.name:<{w}}{counts[d.name]:>6}{mark}")

    ok = True
    if unassigned:
        ok = False
        print(f"\n✗ 有 {len(unassigned)} 条用例不属于任何域（新增模块后忘了登记？）：")
        missing = sorted({t.split("::")[0] + "::" + (t.split("::")[1] if "::" in t else "") for t in unassigned})
        for m in missing[:20]:
            print(f"    前缀 {m}::  → 在 backend/test-domains.tsv 里加一列（或并进最近的域）")
        print(f"    前 5 条：{unassigned[:5]}")
    if ambiguous:
        ok = False
        print(f"\n✗ 有 {len(ambiguous)} 条用例同时命中多个域（前缀重叠）：{ambiguous[:5]}")
    empty = [d.name for d in doms if counts[d.name] == 0]
    if empty:
        ok = False
        print(f"\n✗ 空域（前缀匹配不到用例）：{empty}")
    if sum(counts.values()) != total:
        ok = False
        print(f"\n✗ 计数不平：分域合计 {sum(counts.values())} ≠ 用例总数 {total}")
    if not ok:
        return 1
    print(f"\n✓ 分域一一对应：{total} 条用例 / {len(doms)} 个域，无遗漏、无重叠、无空域")
    return 0


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description="按域跑 Rust lib 测试套件（P2-61）")
    sub = ap.add_subparsers(dest="cmd", required=True)

    p_list = sub.add_parser("list", help="列出登记表里的域")
    p_list.set_defaults(func=cmd_list)

    p_check = sub.add_parser("check", help="门禁：用例↔域一一对应")
    p_check.add_argument("--tests-from", default="", help="预存的用例清单目录（<crate>.txt），免编译")
    p_check.add_argument("--manifest-only", action="store_true",
                         help="只校登记表自洽（不列用例、不编译；check.sh L0 用）")
    p_check.set_defaults(func=cmd_check)

    p_run = sub.add_parser("run", help="真跑（克隆库 + 并行）")
    p_run.add_argument("--package", "-p", required=True,
                       help="域名（逗号分隔）/ all / crate 名（cloudpivot = 单进程全量）")
    p_run.add_argument("--jobs", "-j", type=int, default=min(8, 2 * (os.cpu_count() or 4)),
                       help="并行进程数（默认 min(8, 2×nproc)：本套件是 DB 往返瓶颈，"
                            "超订比等于核数更快 —— 实测 4 核上 jobs=4 → 54 s、jobs=8 → 42 s）")
    p_run.add_argument("--keep", action="store_true", help="跑完保留克隆库（排障）")
    p_run.add_argument("--no-db", action="store_true", help="不建库（沿用环境变量里的 *_DB_URL）")
    p_run.add_argument("--out", default="", help="产物目录（默认 mktemp -d）")
    p_run.set_defaults(func=cmd_run)

    args = ap.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
