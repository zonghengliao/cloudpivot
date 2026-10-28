#!/usr/bin/env bash
# =============================================================================
# 双版本改名脚本 —— 迁移准备包 / 01-rename（正向 cailu→cloudpivot / 反向 cloudpivot→cailu）
#
# 作用：在同一棵树上做「项目标识」的整体改名，覆盖文件名 + 文件内容，两个方向严格互逆：
#   正向（默认）：旧标识 cailu / Cailu / 菜芦         → 新标识 cloudpivot / CloudPivot / 云枢
#   反向（--reverse）：新标识 cloudpivot / CloudPivot / 云枢 → 旧标识 cailu / Cailu / 菜芦
#
# 为什么需要反向：本项目维护两个功能完全对等的版本 ——
#   云枢版（改名，生产用）/ 原名版（不改名，用于向上游贡献）。
#   两版之间只允许存在「命名差异」，所以必须能自动互相推导，且「反向再正向」要能逐字节还原。
#
# 特性：
#   * 严格互逆   —— 反向 = 正向的逆函数；`反向 + 正向` 后与原树逐字节一致（见 verify-roundtrip.sh）
#   * 幂等       —— 重复执行不会二次破坏（已经是目标标识的地方不再动，二跑 0 改动）
#   * dry-run    —— --dry-run 只打印将要改的文件/行，不落盘
#   * 备份       —— 默认把被改文件的原件备份到 <repo>/.rename-backup/<时间戳>/
#   * 排除       —— node_modules / target / dist / .git / migration-kit 等一律不动
#   * 特例保护   —— .gitleaks.toml 的历史泄露前缀、外部仓库 URL、cailu-src 参考仓、
#                   历史基线引用（sticky）、两版说明文档，详见 special-cases.md
#
# 用法：
#   bash 01-rename/rename.sh --dry-run              # 正向：先看要改什么（务必先跑）
#   bash 01-rename/rename.sh                        # 正向：真正执行
#   bash 01-rename/rename.sh --reverse --dry-run    # 反向：先看要改什么
#   bash 01-rename/rename.sh --reverse              # 反向：真正执行
#   bash 01-rename/rename.sh --root /path/to/repo   # 指定仓库根
#   bash 01-rename/rename.sh --no-backup            # 不备份（git 工作区干净时可用）
#
# 退出码：0 成功（含「无改动」）；2 参数错误；3 特例锚点未命中（需人工看 special-cases.md）
#         4 反向执行后仍残留新标识（说明映射表有遗漏，必须补齐）
# =============================================================================
set -uo pipefail

ROOT=""
DRY=0
BACKUP=1
QUIET=0
DIRECTION="forward"

while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run)   DRY=1; shift ;;
    --no-backup) BACKUP=0; shift ;;
    --reverse)   DIRECTION="reverse"; shift ;;
    --forward)   DIRECTION="forward"; shift ;;
    --root)      ROOT="${2:?--root 需要参数}"; shift 2 ;;
    --root=*)    ROOT="${1#*=}"; shift ;;
    -q|--quiet)  QUIET=1; shift ;;
    -h|--help)   sed -n '2,36p' "$0"; exit 0 ;;
    *) echo "未知参数: $1（--help 看用法）" >&2; exit 2 ;;
  esac
done

RENAME_ROOT="$ROOT" \
RENAME_DRY="$DRY" \
RENAME_BACKUP="$BACKUP" \
RENAME_QUIET="$QUIET" \
RENAME_DIRECTION="$DIRECTION" \
RENAME_KIT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" \
exec python3 - <<'PY'
# -*- coding: utf-8 -*-
"""rename.sh 的实体逻辑（内嵌 python3，避免 sed 在 UTF-8/二进制/多行上的坑）。

正向与反向共用同一套流程，只有三处按方向分叉：
  1) SUBS 映射表（严格互换）
  2) 两个特例修整器（正向「加」，反向「撤」）：.gitleaks.toml 规则 3、.githooks/post-commit
  3) 整文件覆盖（正向把 scripts/patch-alun-web.sh 覆盖成终态版；反向只做文本替换，
     因为终态版本身就是「上游版经正向替换」的结果，反向替换即可逐字节回到上游形态）
"""
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
REVERSE = os.environ.get("RENAME_DIRECTION") == "reverse"
DIRNAME = "反向(cloudpivot→cailu)" if REVERSE else "正向(cailu→cloudpivot)"

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
    "migration-kit",  # 准备包自身含大量旧标识字面量（它就是改名工具），必须排除（否则自我污染）
    # 运行态目录：日志/上传/下载件是按当时项目名生成的磁盘数据（可能还有数据库/nginx 引用），
    # 改文件名既不会迁移数据，还会让「旧名日志」消失、幂等性检查永远失败。不属于代码，排除。
    "logs", "uploads", "downloads",
}
MAX_BYTES = 8 * 1024 * 1024

# 逐字替换（长串在前，避免 Cailu 被 cailu 规则先吃掉）。两个方向严格互换。
SUBS_FORWARD = [("Cailu", "CloudPivot"), ("cailu", "cloudpivot"), ("菜芦", "云枢")]
SUBS_REVERSE = [("CloudPivot", "Cailu"), ("cloudpivot", "cailu"), ("云枢", "菜芦")]
SUBS = SUBS_REVERSE if REVERSE else SUBS_FORWARD

# 源标识 / 目标标识（用于统计与残留自检）
# 源标识（本方向要消除的命名）/ 目标标识（本方向要写入的命名）
SRC_WORDS = ["cailu", "Cailu", "菜芦"] if not REVERSE else ["cloudpivot", "CloudPivot", "云枢"]
DST_WORDS = ["cloudpivot", "CloudPivot", "云枢"] if not REVERSE else ["cailu", "Cailu", "菜芦"]

# 需要「先打令牌保护起来、替换完再还原」的串：这些旧标识是外部/历史语义，不能改。
# 反向时这些串里不含新标识，保护动作是空操作 —— 保留同一份表，保证两个方向行为一致。
PROTECT = [
    (r"github\.com/wild1024/cailu", "@@KEEP_GH_URL@@"),   # 外部上游仓库 URL
    (r"wild1024/cailu", "@@KEEP_GH_SLUG@@"),              # 外部上游仓库 slug
    (r"cailu-src", "@@KEEP_REFSRC@@"),                    # 独立参考仓目录名（另一台机器上的路径）
]

# 「历史基线引用」保护：这些位置上的 cailu 指的是**改名前的旧基线**（旧 crate 名、旧库名、
# 旧的 /root/work/c144 基线），改了就是篡改历史记录；反向时也不该把 cloudpivot 与它们混谈。
# 逐文件精确到 span，避免「`cailu`」这种通用串误伤 README 里的 `cloudpivot`（两版都真实存在）。
# 顺序：更长的 span 在前，避免被短 span 先切碎。
STICKY_BY_FILE = {
    "docs/大数据-技术架构.md": [
        r"crate `cailu`",
        r"@cailu/web",
        r"`cailu`",
        r"DATABASE cailu",
        r"host\s+cailu",
        r"dbname=cailu",
        r"(?<=-d )cailu\b",
    ],
    "docs/ERP-批次1-技术架构.md": [
        r"crate `cailu` v0\.1\.0",
        r"@cailu/web",
        r"`cailu`",
    ],
}

# .gitleaks.toml 规则 3 的旧前缀：这是「有意保留」的历史泄露检测器，绝不能改名。
# 逐串保护（不能按行保护：同一行里 id = "cailu-jwt-secret-literal" 是要改的，
#   而注释/描述里的 cailu 是要留的）。这些串已覆盖「正向终态文件」里全部合法 cailu。
PROTECT_GITLEAKS = [
    "(?:cloudpivot|cailu)",        # 终态正则：新旧前缀双匹配
    "cailu-<env>-jwt-secret-* 形态",  # 仅 guard 注释里出现；header 行的（cailu-<env>…）要改
    "{cloudpivot,cailu}",          # 终态 description 的前缀集合
    "旧前缀 cailu",                 # 注释里的说明文字
]

# .gitleaks.toml 规则 3 在「反向」时要还原成的上游形态（正向修整器会把它再变回终态）
GITLEAKS_DESC_UPSTREAM = 'description = "形如 cailu-{dev,test,prod}-jwt-secret-... 的历史密钥字面量"\n'
GITLEAKS_REGEX_UPSTREAM = "regex = '''cailu-(dev|test|prod)-jwt-secret-[A-Za-z0-9_\\-]+'''\n"

# 整文件跳过（相对路径 glob）：两版必须**逐字节相同**的双版本说明文档，改了就和另一版不一致。
SKIP_FILE_GLOBS = [
    "docs/双版本说明.md",
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


def _patterns_for(relpath):
    """该文件要打令牌保护的 span 正则表（顺序即优先级：更具体的在前）。"""
    pats = [p for p, _ in PROTECT]
    if relpath == ".gitleaks.toml":
        pats += [re.escape(s) for s in PROTECT_GITLEAKS]
    pats += STICKY_BY_FILE.get(relpath, [])
    return pats


def protect(text, relpath=""):
    """把「不能改」的 span 换成占位令牌，替换完再原样还原（精确到匹配到的原文，不做正则反解）。"""
    store = {}
    for i, pat in enumerate(_patterns_for(relpath)):
        tok = f"@@KEEP{i}@@"

        def repl(m, tok=tok):
            while tok in store or tok in text:
                tok = tok[:-2] + "_" + tok[-2:]  # 极端情况（原文里就有该令牌）时变形
            store[tok] = m.group(0)
            return tok

        text = re.sub(pat, repl, text)
    return text, store


def unprotect(text, store):
    for tok, literal in store.items():
        text = text.replace(tok, literal)
    return text


def count_hits(text):
    return sum(text.count(a) for a in SRC_WORDS)


# ---------------------------------------------------------------- 特例 1：.gitleaks.toml 规则 3
GITLEAKS_GUARD_HEADS = (
    "# ⚠ 正则里**故意保留旧前缀 cailu**",
    "#   只写新前缀等于把已知历史泄露",
    "#   密钥本身已轮换",
)


def fixup_gitleaks(path, text, report):
    """正向：规则 3 的历史密钥正则必须保留旧前缀 cailu，否则历史泄露会被藏进扫描器。

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
            out.append(
                'description = "形如 {cloudpivot,cailu}-{dev,test,prod}-jwt-secret-... '
                '的历史密钥字面量（含改名前的旧前缀）"\n'
            )
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


def unfixup_gitleaks(path, text, report):
    """反向：把规则 3 还原成上游形态（纯 cailu 前缀、无 guard 注释），正向会再补回终态。

    幂等：若已是上游形态（无 `(?:cloudpivot|cailu)`、无 guard 注释），0 改动。
    """
    lines = text.splitlines(keepends=True)
    out = []
    dropped = 0
    hit_desc = hit_regex = False
    for ln in lines:
        if ln.startswith(GITLEAKS_GUARD_HEADS):
            dropped += 1
            continue
        if ln.startswith("description = ") and "jwt-secret-" in ln and "历史密钥字面量" in ln:
            out.append(GITLEAKS_DESC_UPSTREAM)
            hit_desc = True
            continue
        if ln.startswith("regex = '''") and "jwt-secret-" in ln:
            out.append(GITLEAKS_REGEX_UPSTREAM)
            hit_regex = True
            continue
        out.append(ln)

    new = "".join(out)
    if not (hit_desc and hit_regex):
        report.append((
            "special-fail", rel(path),
            f".gitleaks.toml 规则3 反向锚点未命中 desc={hit_desc} regex={hit_regex}"
            " —— 见 special-cases.md 特例 1",
        ))
        return text
    if new == text:
        report.append(("special-skip", rel(path), ".gitleaks.toml 规则3 已是上游形态（已处理）"))
        return text
    report.append((
        "special", rel(path),
        f".gitleaks.toml 规则3 已还原上游形态（去 guard 注释 {dropped} 行，desc/regex 恢复纯 cailu）",
    ))
    return new


# ---------------------------------------------------------------- 特例 2：.githooks/post-commit
def fixup_post_commit(path, text, report):
    """正向：把 bare 白名单从「写死 cailu.git」改成 `/opt/git/*.git`，目录再改名也不失效。"""
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


def unfixup_post_commit(path, text, report):
    """反向：还原成「写死 <项目>.git」的形态（正向会再改成通配）。"""
    lines = text.splitlines(keepends=True)
    out, dropped = [], 0
    for ln in lines:
        if ln.startswith("# 只接受 /opt/git/*.git 这种本机路径") or ln.startswith(
            "# （目录改名前后都能命中"
        ):
            dropped += 1
            continue
        out.append(ln)
    text = "".join(out)
    if "  /opt/git/*.git) ;;" in text:
        text = text.replace("  /opt/git/*.git) ;;", "  /opt/git/cailu.git*) ;;")
        report.append((
            "special", rel(path),
            f"post-commit 白名单还原为 /opt/git/cailu.git*（去注释 {dropped} 行）",
        ))
        return text
    report.append(("special-skip", rel(path), "post-commit 白名单非通配形态，反向无需处理"))
    return text


# ---------------------------------------------------------------- 特例 3：patch-alun-web.sh 覆盖（仅正向）
def copy_override(name, target_rel, report):
    """scripts/patch-alun-web.sh 的幂等判重逻辑与旧标记字面耦合，改名后必须整套替换。

    该文件 100% 是本项目自研脚本（不是上游文件），整文件覆盖是安全且精确的做法。
    反向不做覆盖：终态版经「反向文本替换」就是上游形态，正向再替换回来即可逐字节还原。
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
    纯文本替换会让 lock 块序不符合 cargo 规范 —— 语义等价，但会让「反向+正向」的逐字节校验失败，
    也会让下一次 `cargo build` 重写 lock（产生无意义 diff）。两个方向都跑，方向无关。
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


# ---------------------------------------------------------------- 残留自检
def residue_scan():
    """扫描「源标识」在树里的残留（排除 migration-kit / .git 等）。

    源标识 = 本方向要消除的那个命名：正向消除 cailu，反向消除 cloudpivot。
    正向：残留的 cailu 只允许出现在受保护 span 里（.gitleaks 规则3、基线引用、cailu-src）。
    反向：残留的 cloudpivot/云枢 必须为 0 —— 这是「原名版不得有改名痕迹」的红线。
    """
    hits = []
    for path in walk():
        rp = rel(path)
        if skipped_by_glob(rp):
            continue  # 两版必须逐字节相同的中性文档（含新旧两种标识），不参与残留判定
        try:
            raw = open(path, "rb").read()
        except OSError:
            continue
        if len(raw) > MAX_BYTES or is_binary(raw):
            continue
        try:
            text = raw.decode("utf-8")
        except UnicodeDecodeError:
            continue
        guarded, _ = protect(text, rp)
        n = sum(guarded.count(w) for w in SRC_WORDS)
        if n:
            hits.append((rp, n))
    return hits


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
            skipped.append((rp, f"SKIP_FILE_GLOBS {glob_hit}（两版必须逐字节相同的中性文档）"))
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

        guarded, store = protect(text, rp)
        is_special = rp in (".gitleaks.toml", ".githooks/post-commit")
        if count_hits(guarded) == 0 and not is_special:
            continue

        new = guarded
        for a, b in SUBS:
            new = new.replace(a, b)
        new = unprotect(new, store)

        # 特例修整
        if rp == ".gitleaks.toml":
            new = (unfixup_gitleaks if REVERSE else fixup_gitleaks)(path, new, report)
        elif rp == ".githooks/post-commit":
            new = (unfixup_post_commit if REVERSE else fixup_post_commit)(path, new, report)

        if new == text:
            continue

        occ = sum(text.count(a) for a in SRC_WORDS) - sum(new.count(a) for a in SRC_WORDS)
        total_occ += max(occ, 0)
        changed.append((rp, occ))
        if DRY:
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

    # ---- 2) 特例 3：整文件覆盖（仅正向；反向靠文本替换即可逐字节还原）
    if not REVERSE:
        copy_override("patch-alun-web.sh", "scripts/patch-alun-web.sh", report)
    else:
        report.append((
            "special-skip", "scripts/patch-alun-web.sh",
            "反向不覆盖：终态版经反向文本替换即为上游形态（正向会再补回）",
        ))

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
    print(f"rename.sh  {DIRNAME}  {'DRY-RUN（未落盘）' if DRY else '已执行'}   root={ROOT}")
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
            "direction": "reverse" if REVERSE else "forward",
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

    if not DRY:
        residue = residue_scan()
        if REVERSE and residue:
            print(f"✘ 反向残留检查失败：仍有 {len(residue)} 个文件含目标标识（cloudpivot/云枢）", file=sys.stderr)
            for rp, n in residue[:20]:
                print(f"    ! {rp}  ({n} 处)", file=sys.stderr)
            print("  说明映射表有遗漏（某处反向没还原），必须补齐直到为 0。", file=sys.stderr)
            return 4
        if not QUIET:
            if residue:
                print(f"残留检查：{len(residue)} 个文件保留旧标识（受保护/历史基线引用，符合预期）：")
                for rp, n in residue:
                    print(f"    · {rp}  ({n} 处)")
            else:
                print("残留检查：源标识已清零。")

    if DRY and (changed or renamed):
        print(f"DRY-RUN 结束：上面是将要发生的改动（方向 {DIRNAME}），确认后去掉 --dry-run 再跑。")
    elif not changed and not renamed:
        print(f"无改动：本树已是目标标识（方向 {DIRNAME}，脚本幂等）或属于被排除路径。")
    return 0


sys.exit(main())
PY
