#!/usr/bin/env bash
# =============================================================================
# 云枢(CloudPivot) 改名脚本 —— Pro 版迁移准备包 / 01-rename
#
# 作用：把「上游旧标识」整体替换成「云枢标识」，覆盖文件名 + 文件内容。
#       旧标识：cailu / Cailu / 菜芦        新标识：cloudpivot / CloudPivot / 云枢
#
# 特性：
#   * 幂等       —— 重复执行不会二次破坏（已经是 cloudpivot 的地方不再动，二跑 0 改动）
#   * dry-run    —— --dry-run 只打印将要改的文件/行，不落盘
#   * 备份       —— 默认把被改文件的原件备份到 <repo>/.rename-backup/<时间戳>/
#   * 排除       —— node_modules / target / dist / .git / migration-kit 等一律不动
#   * 特例保护   —— .gitleaks.toml 的旧前缀、外部仓库 URL、cailu-src 参考仓路径、历史文档
#                   详见 special-cases.md
#
# 用法：
#   bash 01-rename/rename.sh --dry-run            # 先看要改什么（务必先跑）
#   bash 01-rename/rename.sh                      # 真正执行
#   bash 01-rename/rename.sh --root /path/to/repo
#   bash 01-rename/rename.sh --no-backup          # 不备份（git 工作区干净时可用）
#
# 退出码：0 成功（含「无改动」）；2 参数错误；3 特例锚点未命中（需人工看 special-cases.md）
# =============================================================================
set -uo pipefail

ROOT=""
DRY=0
BACKUP=1
QUIET=0

while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run)   DRY=1; shift ;;
    --no-backup) BACKUP=0; shift ;;
    --root)      ROOT="${2:?--root 需要参数}"; shift 2 ;;
    --root=*)    ROOT="${1#*=}"; shift ;;
    -q|--quiet)  QUIET=1; shift ;;
    -h|--help)   sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "未知参数: $1（--help 看用法）" >&2; exit 2 ;;
  esac
done

RENAME_ROOT="$ROOT" \
RENAME_DRY="$DRY" \
RENAME_BACKUP="$BACKUP" \
RENAME_QUIET="$QUIET" \
RENAME_KIT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" \
exec python3 - <<'PY'
# -*- coding: utf-8 -*-
"""rename.sh 的实体逻辑（内嵌 python3，避免 sed 在 UTF-8/二进制/多行上的坑）。"""
import json
import os
import re
import shutil
import subprocess
import sys
import time

ROOT = os.environ.get("RENAME_ROOT") or ""
DRY = os.environ.get("RENAME_DRY") == "1"
BACKUP = os.environ.get("RENAME_BACKUP") == "1"
QUIET = os.environ.get("RENAME_QUIET") == "1"
KIT = os.environ.get("RENAME_KIT") or ""

if not ROOT:
    try:
        ROOT = subprocess.check_output(
            ["git", "rev-parse", "--show-toplevel"], text=True, stderr=subprocess.DEVNULL
        ).strip()
    except Exception:
        ROOT = os.getcwd()
ROOT = os.path.abspath(ROOT)

# ---------------------------------------------------------------- 常量
EXCLUDE_DIRS = {
    ".git", "node_modules", "target", "dist", ".rename-backup",
    ".pnpm-store", "__pycache__", ".venv", "venv", ".mypy_cache",
    "migration-kit",  # 准备包自身含大量旧标识字面量，必须排除（否则自我污染）
}
MAX_BYTES = 8 * 1024 * 1024

# 逐字替换（长串在前，避免 Cailu 被 cailu 规则先吃掉）
SUBS = [("Cailu", "CloudPivot"), ("cailu", "cloudpivot"), ("菜芦", "云枢")]

# 需要「先打令牌保护起来、替换完再还原」的串：这些旧标识是外部/历史语义，不能改
PROTECT = [
    (r"github\.com/wild1024/cailu", "@@KEEP_GH_URL@@"),   # 外部上游仓库 URL
    (r"wild1024/cailu", "@@KEEP_GH_SLUG@@"),              # 外部上游仓库 slug
    (r"cailu-src", "@@KEEP_REFSRC@@"),                    # 独立参考仓目录名（另一台机器上的路径）
]

# .gitleaks.toml 规则 3 的旧前缀：这是「有意保留」的历史泄露检测器，绝不能改名。
# 逐串保护（不能按行保护：同一行里 id = "cailu-jwt-secret-literal" 是要改的，
#   而注释/描述里的 cailu 是要留的）。这些串已覆盖「终态文件」里全部合法 cailu。
PROTECT_GITLEAKS = [
    "(?:cloudpivot|cailu)",        # 终态正则：新旧前缀双匹配
    "cailu-<env>-jwt-secret-* 形态",  # 仅 guard 注释里出现；header 行的（cailu-<env>…）要改
    "{cloudpivot,cailu}",          # 终态 description 的前缀集合
    "旧前缀 cailu",                 # 注释里的说明文字
]

# 整文件跳过（相对路径 glob）：历史文档里对「旧基线」的描述性文字，改了就是篡改历史
SKIP_FILE_GLOBS = [
    "docs/ERP-批次1-*.md",
]

# ---------------------------------------------------------------- 工具
def rel(p):
    return os.path.relpath(p, ROOT)


def skipped_by_glob(relpath):
    for pat in SKIP_FILE_GLOBS:
        rx = re.escape(pat).replace(r"\*", ".*").replace(r"\?", ".")
        if re.fullmatch(rx, relpath):
            return pat
    return None


def is_binary(data: bytes) -> bool:
    return b"\x00" in data[:8192]


def walk():
    for dirpath, dirnames, filenames in os.walk(ROOT):
        dirnames[:] = sorted(d for d in dirnames if d not in EXCLUDE_DIRS)
        for fn in sorted(filenames):
            yield os.path.join(dirpath, fn)


def protect(text, relpath=""):
    for pat, tok in PROTECT:
        text = re.sub(pat, tok, text)
    if relpath == ".gitleaks.toml":
        for i, s in enumerate(PROTECT_GITLEAKS):
            text = text.replace(s, f"@@KEEP_GL{i}@@")
    return text


def unprotect(text, relpath=""):
    if relpath == ".gitleaks.toml":
        for i, s in enumerate(PROTECT_GITLEAKS):
            text = text.replace(f"@@KEEP_GL{i}@@", s)
    for pat, tok in PROTECT:
        text = text.replace(tok, re.sub(r"\\(.)", r"\1", pat))
    return text


def count_hits(text):
    return sum(text.count(a) for a, _ in SUBS)


# ---------------------------------------------------------------- 特例 1：.gitleaks.toml 规则 3
def fixup_gitleaks(path, text, report):
    """规则 3 的历史密钥正则必须保留旧前缀 cailu，否则历史泄露会被藏进扫描器。

    幂等：已含 `(?:cloudpivot|cailu)` 则跳过。
    """
    if "(?:cloudpivot|cailu)-" in text:
        report.append(("special-skip", rel(path), ".gitleaks.toml 规则3 已保留旧前缀（已处理）"))
        return text

    lines = text.splitlines(keepends=True)
    out, hit_header, hit_desc, hit_regex = [], False, False, False
    guard = [
        "# ⚠ 正则里**故意保留旧前缀 cailu**：git 历史里已泄露的字面量就是 cailu-<env>-jwt-secret-* 形态，\n",
        "#   只写新前缀等于把已知历史泄露从扫描器里藏掉（历史扫描会由红变绿，属于安全回归）。\n",
        "#   密钥本身已轮换，这条规则只用来拦住「历史泄露被重新引入」，跟项目标识无关。\n",
    ]
    for ln in lines:
        if "历史事故的直接防线" in ln:
            out.append(ln)
            out.extend(guard)
            hit_header = True
            continue
        if ln.startswith("description = ") and "jwt-secret-... 的历史密钥字面量" in ln:
            out.append('description = "形如 {cloudpivot,cailu}-{dev,test,prod}-jwt-secret-... 的历史密钥字面量（含改名前的旧前缀）"\n')
            hit_desc = True
            continue
        if ln.startswith("regex = '''") and "jwt-secret-[A-Za-z0-9_" in ln:
            out.append("regex = '''(?:cloudpivot|cailu)-(dev|test|prod)-jwt-secret-[A-Za-z0-9_\\-]+'''\n")
            hit_regex = True
            continue
        out.append(ln)

    if not (hit_header and hit_desc and hit_regex):
        report.append((
            "special-fail", rel(path),
            f".gitleaks.toml 规则3 锚点未命中 header={hit_header} desc={hit_desc} regex={hit_regex}"
            " —— 请按 special-cases.md 手工补，或用 overrides/gitleaks.toml 覆盖",
        ))
        return text
    report.append(("special", rel(path), ".gitleaks.toml 规则3 已恢复旧前缀 cailu 检测"))
    return "".join(out)


# ---------------------------------------------------------------- 特例 2：.githooks/post-commit
def fixup_post_commit(path, text, report):
    """把 bare 白名单从「写死 cailu.git」改成 `/opt/git/*.git`，目录再改名也不失效。"""
    if "/opt/git/*.git" in text:
        report.append(("special-skip", rel(path), "post-commit 白名单已是通配（已处理）"))
        return text
    if "/opt/git/cloudpivot.git*" not in text:
        report.append(("special-skip", rel(path), "post-commit 未见 bare 白名单硬编码，跳过"))
        return text
    text = text.replace(
        "  /opt/git/cloudpivot.git*) ;;",
        "  /opt/git/*.git) ;;",
    )
    text = text.replace(
        "ORIGIN_URL=$(git remote get-url origin 2>/dev/null) || exit 0",
        "# 只接受 /opt/git/*.git 这种本机路径：既排除 GitHub 远端，也不把 bare 目录名写死\n"
        "# （目录改名前后都能命中，避免改名窗口内 post-commit 静默失效）\n"
        "ORIGIN_URL=$(git remote get-url origin 2>/dev/null) || exit 0",
        1,
    )
    report.append(("special", rel(path), "post-commit 白名单改为 /opt/git/*.git"))
    return text


# ---------------------------------------------------------------- 特例 3：patch-alun-web.sh 覆盖
def copy_override(name, target_rel, report):
    """scripts/patch-alun-web.sh 的幂等判重逻辑与旧标记字面耦合，改名后必须整套替换。

    该文件 100% 是本项目自研脚本（不是上游文件），整文件覆盖是安全且精确的做法。
    """
    src = os.path.join(KIT, "overrides", name)
    dst = os.path.join(ROOT, target_rel)
    if not os.path.isfile(src):
        report.append(("special-fail", target_rel, f"找不到 overrides/{name}，请人工按 special-cases.md 处理"))
        return
    if not os.path.isfile(dst):
        report.append(("special-skip", target_rel, "目标树里没有该文件（未应用 02 补丁序列？）跳过覆盖"))
        return
    with open(src, "rb") as f:
        new = f.read()
    with open(dst, "rb") as f:
        old = f.read()
    if old == new:
        report.append(("special-skip", target_rel, f"已与 overrides/{name} 一致（已处理）"))
        return
    if DRY:
        report.append(("special-dry", target_rel, f"将用 overrides/{name} 整文件覆盖（判重改为按补丁代码签名）"))
        return
    shutil.copyfile(dst, dst + ".bak-rename")
    with open(dst, "wb") as f:
        f.write(new)
    os.unlink(dst + ".bak-rename")
    report.append(("special", target_rel, f"已用 overrides/{name} 覆盖（旧标记判重 → 补丁代码签名判重）"))


# ---------------------------------------------------------------- 特例 4：Cargo.lock 包块顺序
def reorder_lock(path, pkg_names, report):
    """crate 改名后，Cargo.lock 里那个 package 块要排到「新名字」的字母序位置。

    为什么必须做：cargo 写 lock 时按 name 排序，`cailu` 的块序位置不等于 `cloudpivot` 的。
    纯文本替换会让 lock 块序不符合 cargo 规范 —— 语义等价，但会让「重放结果 == 历史 commit」
    的逐字节校验失败，也会让下一次 `cargo build` 重写 lock（产生无意义 diff）。
    """
    try:
        text = open(path, encoding="utf-8").read()
    except OSError:
        return
    lines = text.splitlines(keepends=True)
    head, blocks, cur = [], [], None
    for ln in lines:
        if ln.startswith("[[package]]"):
            if cur is not None:
                blocks.append(cur)
            cur = [ln]
        elif cur is None:
            head.append(ln)
        else:
            cur.append(ln)
    if cur is not None:
        blocks.append(cur)

    def nm(b):
        for l in b:
            m = re.match(r'name = "(.*)"', l)
            if m:
                return m.group(1)
        return ""

    changed = False
    for pkg in pkg_names:
        idx = [i for i, b in enumerate(blocks) if nm(b) == pkg]
        if len(idx) != 1:
            continue
        i = idx[0]
        blk = blocks[i]
        rest = blocks[:i] + blocks[i + 1:]
        pos = len(rest)
        for j, b in enumerate(rest):
            if nm(b) > pkg:
                pos = j
                break
        ordered = rest[:pos] + [blk] + rest[pos:]
        if ordered != blocks:
            blocks = ordered
            changed = True
    if changed:
        if DRY:
            report.append(("special-dry", rel(path), "Cargo.lock 包块将按 cargo 字母序重排"))
        else:
            with open(path, "w", encoding="utf-8") as f:
                f.write("".join(head) + "".join("".join(b) for b in blocks))
            report.append(("special", rel(path), "Cargo.lock 包块已按 cargo 字母序重排"))


def crate_locks():
    """收集「Cargo.toml [package] name + 同目录 Cargo.lock」对，用于 lock 块重排。"""
    for dirpath, dirnames, filenames in os.walk(ROOT):
        dirnames[:] = [d for d in dirnames if d not in EXCLUDE_DIRS]
        if "Cargo.toml" not in filenames or "Cargo.lock" not in filenames:
            continue
        try:
            txt = open(os.path.join(dirpath, "Cargo.toml"), encoding="utf-8").read()
        except OSError:
            continue
        m = re.search(r'(?ms)^\[package\]\s*\n(?:[^\[].*\n)*?name\s*=\s*"([^"]+)"', txt)
        if m:
            yield os.path.join(dirpath, "Cargo.lock"), m.group(1)


# ---------------------------------------------------------------- 主流程
def main():
    ts = time.strftime("%Y%m%d-%H%M%S")
    backup_dir = os.path.join(ROOT, ".rename-backup", ts) if BACKUP else ""
    report, changed, renamed = [], [], []
    total_occ = 0
    skipped = []

    if not os.path.exists(os.path.join(ROOT, ".git")) and subprocess.call(
        ["git", "-C", ROOT, "rev-parse", "--git-dir"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    ) != 0:
        print(f"[warn] {ROOT} 不像 git 仓库根，继续但在非 git 目录上无法回滚", file=sys.stderr)

    # ---- 1) 内容替换
    for path in walk():
        rp = rel(path)
        glob_hit = skipped_by_glob(rp)
        if glob_hit:
            skipped.append((rp, f"SKIP_FILE_GLOBS {glob_hit}（历史文档描述旧基线，改了等于篡改）"))
            continue
        try:
            with open(path, "rb") as f:
                raw = f.read()
        except OSError as e:
            skipped.append((rp, f"读不了: {e}"))
            continue
        if len(raw) > MAX_BYTES:
            skipped.append((rp, f"超过 {MAX_BYTES}B，跳过"))
            continue
        if is_binary(raw):
            continue
        try:
            text = raw.decode("utf-8")
        except UnicodeDecodeError:
            skipped.append((rp, "非 UTF-8 文本，跳过"))
            continue

        guarded = protect(text, rp)
        if count_hits(guarded) == 0:
            continue

        new = guarded
        for a, b in SUBS:
            new = new.replace(a, b)
        new = unprotect(new, rp)

        # 特例修整
        if rp == ".gitleaks.toml":
            new = fixup_gitleaks(path, new, report)
        elif rp == ".githooks/post-commit":
            new = fixup_post_commit(path, new, report)

        if new == text:
            continue

        occ = sum(text.count(a) for a, _ in SUBS) - sum(new.count(a) for a, _ in SUBS)
        total_occ += max(occ, 0)
        changed.append((rp, occ))
        if DRY:
            # 打印差异样本
            old_lines, new_lines = text.splitlines(), new.splitlines()
            shown = 0
            for i in range(min(len(old_lines), len(new_lines))):
                if old_lines[i] != new_lines[i] and shown < 3:
                    print(f"  ~ {rp}:{i + 1}")
                    print(f"      - {old_lines[i][:160]}")
                    print(f"      + {new_lines[i][:160]}")
                    shown += 1
            if len(old_lines) != len(new_lines) and shown < 3:
                print(f"  ~ {rp}: 行数 {len(old_lines)} → {len(new_lines)}（特例修整）")
            continue

        if BACKUP:
            dst = os.path.join(backup_dir, rp)
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            with open(dst, "wb") as f:
                f.write(raw)
        with open(path, "wb") as f:
            f.write(new.encode("utf-8"))

    # ---- 2) 特例 3：整文件覆盖（仅当补丁序列已应用）
    copy_override("patch-alun-web.sh", "scripts/patch-alun-web.sh", report)

    # ---- 2b) Cargo.lock 包块重排（改名后块序要与 cargo 的字母序一致）
    for lock_path, pkg in crate_locks():
        reorder_lock(lock_path, [pkg], report)

    # ---- 3) 文件名/目录名重命名（自底向上，避免父子路径错位）
    for dirpath, dirnames, filenames in os.walk(ROOT, topdown=False):
        if any(part in EXCLUDE_DIRS for part in rel(dirpath).split(os.sep)):
            continue
        for name in sorted(filenames) + sorted(dirnames):
            if name in EXCLUDE_DIRS:
                continue
            nn = name
            for a, b in SUBS:
                nn = nn.replace(a, b)
            if nn == name:
                continue
            src = os.path.join(dirpath, name)
            dstp = os.path.join(dirpath, nn)
            renamed.append((rel(src), rel(dstp)))
            if DRY:
                continue
            if BACKUP and os.path.isfile(src):
                dst = os.path.join(backup_dir, rel(src))
                os.makedirs(os.path.dirname(dst), exist_ok=True)
                shutil.copy2(src, dst)
            if os.path.exists(dstp):
                print(f"[warn] 重命名目标已存在，跳过: {rel(dstp)}", file=sys.stderr)
                continue
            os.rename(src, dstp)

    # ---- 报告
    print("=" * 78)
    print(f"rename.sh  {'DRY-RUN（未落盘）' if DRY else '已执行'}   root={ROOT}")
    print("-" * 78)
    print(f"内容改动文件: {len(changed)}    替换次数: {total_occ}    重命名: {len(renamed)}")
    for rp, occ in changed:
        if not QUIET:
            print(f"  M {rp}  ({occ} 处)")
    for rp in renamed:
        print(f"  R {rp[0]} → {rp[1]}")
    for kind, rp, msg in report:
        print(f"  [{kind}] {rp}: {msg}")
    if skipped and not QUIET:
        print(f"跳过文件 {len(skipped)} 个（只列前 10）:")
        for rp, why in skipped[:10]:
            print(f"  S {rp}: {why}")
    if BACKUP and changed and not DRY:
        print(f"备份目录: {backup_dir}")
        manifest = {
            "root": ROOT,
            "ts": ts,
            "changed": [{"path": p, "replacements": o} for p, o in changed],
            "renamed": renamed,
            "special": [{"kind": k, "path": p, "msg": m} for k, p, m in report],
        }
        os.makedirs(backup_dir, exist_ok=True)
        with open(os.path.join(backup_dir, "manifest.json"), "w", encoding="utf-8") as f:
            json.dump(manifest, f, ensure_ascii=False, indent=2)
    fails = [r for r in report if r[0] == "special-fail"]
    print("=" * 78)
    if fails:
        print(f"✘ 有 {len(fails)} 个特例需要人工处理（见 special-cases.md）", file=sys.stderr)
        return 3
    if DRY and (changed or renamed):
        print("DRY-RUN 结束：上面是将要发生的改动，确认后去掉 --dry-run 再跑。")
    elif not changed and not renamed:
        print("无改动：本树已是 cloudpivot/云枢 标识（脚本幂等）或属于被排除路径。")
    return 0


sys.exit(main())
PY
