#!/usr/bin/env bash
# =============================================================================
# 05-conflicts/gen-changed-files.sh —— 生成「我们动过的上游文件清单」
#
# 用途：
#   1) 首次生成 05-conflicts/changed-upstream-files.md（默认）
#   2) Pro 到手后对「新基线 vs 我们的改动」再生成一份，用来逐条核对冲突
#
# 用法：
#   bash 05-conflicts/gen-changed-files.sh [--base <commit>] [--head <commit>] [--out <file>]
#   默认 --base c4e8950（上游基线） --head HEAD
# =============================================================================
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BASE="c4e8950"
HEADREV="HEAD"
OUT="$HERE/changed-upstream-files.md"
while [ $# -gt 0 ]; do
  case "$1" in
    --base) BASE="${2:?}"; shift 2 ;;
    --head) HEADREV="${2:?}"; shift 2 ;;
    --out)  OUT="${2:?}"; shift 2 ;;
    -h|--help) sed -n '2,14p' "$0"; exit 0 ;;
    *) echo "未知参数: $1" >&2; exit 2 ;;
  esac
done
ROOT="$(git rev-parse --show-toplevel)"; cd "$ROOT" || exit 2
git rev-parse "$BASE" >/dev/null 2>&1 || { echo "找不到基线 commit: $BASE" >&2; exit 2; }

python3 - "$BASE" "$HEADREV" "$OUT" <<'PY'
import subprocess, sys, collections
base, head, out = sys.argv[1], sys.argv[2], sys.argv[3]

def sh(*a):
    return subprocess.check_output(a, text=True)

# 1) 文件级 stat
stat = sh("git", "diff", "--numstat", "-M", f"{base}..{head}")
files = []
for line in stat.splitlines():
    if not line.strip():
        continue
    add, dele, path = line.split("\t", 2)
    files.append((path, add, dele))

# 2) 每个文件被哪些提交动过 → 归类
commits = [l.split(" ", 1) for l in sh("git", "log", "--reverse", "--format=%H %s", f"{base}..{head}").splitlines()]
touched = collections.defaultdict(list)
for sha, subj in commits:
    names = sh("git", "show", "--pretty=", "--name-only", "-M", "--first-parent", sha).splitlines()
    for n in names:
        if n.strip():
            touched[n].append((sha[:7], subj))

CAT_RULES = [
    ("改名",      lambda s: s.startswith("refactor(rename)")),
    ("配色",      lambda s: s.startswith("style(theme)")),
    ("品牌资源",  lambda s: s.startswith("feat(brand)")),
    ("安全",      lambda s: s.startswith("security")),
    ("状态卡功能", lambda s: s.startswith("feat(dashboard)")),
    ("清理",      lambda s: s.startswith("chore(clean)")),
    ("工程/钩子", lambda s: s.startswith(("chore(hooks)", "chore(dev-env)", "chore(lint)"))),
    ("文档",      lambda s: s.startswith(("docs", "chore(docs)")) or s.startswith("docs")),
]

def cats(path):
    cs = touched.get(path, [])
    got = []
    for name, pred in CAT_RULES:
        if any(pred(subj) for _, subj in cs):
            got.append(name)
    return got or ["其它"]

lines = []
lines.append(f"# 我们动过的上游文件清单（`git diff --numstat -M {base}..{head}`）\n")
lines.append(f"> 基线 `{base}`，对照 `{head}`（`git rev-parse {head[:12] if len(head)>12 else head}`）。")
lines.append(f"> 共 {len(files)} 个文件。合并/重放时按本表逐条核对：每个文件「为什么改」，决定它是**必须保留**还是**可以丢掉**。\n")
lines.append("列说明：`+/-` = 增删行数；`性质` = 改动分类；`涉及提交` = 哪些 commit 碰过它。\n")

by_cat = collections.Counter()
for path, add, dele, *rest in [(p, a, d) for p, a, d in files]:
    for c in cats(path):
        by_cat[c] += 1
lines.append("## 0. 按性质统计\n")
lines.append("| 性质 | 文件数 |")
lines.append("|------|-------:|")
for c, n in by_cat.most_common():
    lines.append(f"| {c} | {n} |")
lines.append("")

lines.append("## 1. 逐文件清单\n")
lines.append("| # | 文件 | +/- | 性质 | 涉及提交 |")
lines.append("|--:|------|-----|------|----------|")
for i, (path, add, dele) in enumerate(files, 1):
    cs = touched.get(path, [])
    cs_s = ", ".join(sorted({c for c, _ in cs})) or "—"
    lines.append(f"| {i} | `{path}` | +{add}/-{dele} | {'/'.join(cats(path))} | {cs_s} |")
lines.append("")

lines.append("## 2. 合并时怎么用这张表\n")
lines.append("1. **改名类（`refactor(rename)`）**：不要 merge，直接用 `01-rename/rename.sh` 在新基线上重放。")
lines.append("   凡本表标了「改名」的文件，Pro 基线里的同名文件都要重新过一遍脚本。")
lines.append("2. **安全类**：`02-security/patches` 按序 `git am`；打不上的按 `design-notes.md` 逐条手工重做。")
lines.append("   ★ 这类改动**不能丢**，丢了等于把已知漏洞重新引入。")
lines.append("3. **配色/品牌**：`03-brand/apply.sh`（patch）或整文件替换；视觉类，冲突了以新基线的结构为准。")
lines.append("4. **状态卡功能**：`04-modules/business-monitor/copy.sh` + `wiring.md` 的四处接线。")
lines.append("5. **清理类（公众号二维码/关于卡片）**：如果 Pro 又重新加了推广位，需要再删一次 —— 逐个 `grep` 关键字确认。")
lines.append("6. **文档类**：可以最后补，不阻塞编译。")

open(out, "w", encoding="utf-8").write("\n".join(lines) + "\n")
print(f"已生成 {out}（{len(files)} 个文件）")
PY
