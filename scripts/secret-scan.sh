#!/usr/bin/env bash
# cailu secret 扫描统一入口（pre-commit 与 CI 共用）
#
#   bash scripts/secret-scan.sh --staged    只扫暂存区（pre-commit）
#   bash scripts/secret-scan.sh --tree      只扫 HEAD 跟踪的文件（默认）
#   bash scripts/secret-scan.sh --history   扫全部历史提交
#
# 退出码：0 = 干净；1 = 发现泄露；2 = 扫描器不可用（fail closed）
#
# 设计要点：
#  - 只扫「git 跟踪的文件」，不扫 backend/target、node_modules 这些构建产物
#    （否则 rpm/rmeta 里的私钥字样会大量误报）；
#  - 输出统一 --redact，命中值不会进入 CI 日志；
#  - gitleaks 不在 PATH 时回退到内置 python 规则（覆盖 backend/config/*.toml），
#    保证「没有网络的机器」也能拦住最关键的这一类泄露。
set -uo pipefail

MODE="${1:---tree}"
ROOT="$(git rev-parse --show-toplevel 2>/dev/null || echo .)"
cd "$ROOT" || exit 2

CONFIG="$ROOT/.gitleaks.toml"
[ -f "$CONFIG" ] || { echo "✘ 缺少 .gitleaks.toml" >&2; exit 2; }

GITLEAKS="${GITLEAKS_BIN:-}"
if [ -z "$GITLEAKS" ]; then
  if command -v gitleaks >/dev/null 2>&1; then
    GITLEAKS="$(command -v gitleaks)"
  elif [ -x /usr/local/bin/gitleaks ]; then
    GITLEAKS=/usr/local/bin/gitleaks
  elif [ -x "$ROOT/scripts/bin/gitleaks" ]; then
    GITLEAKS="$ROOT/scripts/bin/gitleaks"
  fi
fi

# ------------------------------------------------- 内置兜底扫描（gitleaks 缺失时的降级）
# 覆盖 backend/config/*.toml 的 jwt_secret / password / *_secret 明文赋值
fallback_scan() {
  python3 - "$ROOT" "$MODE" <<'PY'
import re, subprocess, sys

root, mode = sys.argv[1], sys.argv[2]
PAT = re.compile(r'''(?i)^\s*(jwt_secret|password|passwd|pwd|secret|secret_key|db_password|api_key|apikey|access_key|private_key|client_secret|[a-z0-9_]+_secret)\s*=\s*["']([^"']{2,})["']''')
ALLOW = re.compile(r'''(?i)^(|changeme|change_me|change-me|placeholder|dummy|todo|xxx+|your[_-]?[a-z]*|<[^>]+>|\$\{[^}]+\}|\$[A-Z_]+)$''')

def files():
    cmd = ["git", "diff", "--cached", "--name-only", "--diff-filter=ACM"] if mode == "--staged" \
          else ["git", "ls-files"]
    out = subprocess.run(cmd, cwd=root, capture_output=True, text=True).stdout
    for f in out.splitlines():
        if re.search(r'(^|/)backend/config/[^/]+\.toml$', f):
            yield f

hits = 0
for f in files():
    try:
        txt = open(f"{root}/{f}", encoding="utf-8", errors="replace").read()
    except OSError:
        continue
    for i, line in enumerate(txt.splitlines(), 1):
        m = PAT.match(line)
        if not m:
            continue
        val = m.group(2)
        if ALLOW.match(val):
            continue
        hits += 1
        print(f"  ✘ {f}:{i}  {m.group(1)} = <REDACTED len={len(val)}>")
print(f"内置规则命中 {hits} 处")
sys.exit(1 if hits else 0)
PY
}

# ---------------------------------------------------------------- gitleaks 存在
if [ -n "$GITLEAKS" ] && [ -x "$GITLEAKS" ]; then
  echo "▶ gitleaks $("$GITLEAKS" version 2>/dev/null)  mode=$MODE"
  case "$MODE" in
    --staged)
      "$GITLEAKS" git "$ROOT" --config "$CONFIG" --staged --redact --no-banner \
        --report-format json --report-path "${TMPDIR:-/tmp}/cailu-secret-scan.json"
      rc=$?
      ;;
    --history)
      "$GITLEAKS" git "$ROOT" --config "$CONFIG" --log-opts="--all" --redact --no-banner \
        --report-format json --report-path "${TMPDIR:-/tmp}/cailu-secret-scan.json"
      rc=$?
      ;;
    --tree|*)
      TMPTREE="$(mktemp -d)"
      git archive HEAD | tar -x -C "$TMPTREE"
      "$GITLEAKS" dir "$TMPTREE" --config "$CONFIG" --redact --no-banner \
        --report-format json --report-path "${TMPDIR:-/tmp}/cailu-secret-scan.json"
      rc=$?
      rm -rf "$TMPTREE"
      ;;
  esac
  if [ "$rc" -eq 0 ]; then
    echo "✔ 未发现泄露"
  else
    echo "✘ 发现疑似泄露（report: ${TMPDIR:-/tmp}/cailu-secret-scan.json，值已 redact）" >&2
  fi
  # 兜底规则同时跑一遍（git 跟踪文件），防止自定义 toml 规则被改坏
  echo "▶ 内置 toml 规则复核（git 跟踪文件）"
  fallback_scan || rc=1
  exit "$rc"
fi

# ---------------------------------------------------------------- 无 gitleaks
echo "! 未找到 gitleaks（本机未安装）。生产建议：sudo install -m0755 gitleaks /usr/local/bin/" >&2
echo "▶ 内置 toml 规则扫描（降级模式，仅覆盖 backend/config/*.toml）"
fallback_scan
