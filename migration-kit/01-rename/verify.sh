#!/usr/bin/env bash
# =============================================================================
# 01-rename/verify.sh —— 改名校验：残留扫描 + 关键文件抽查 + 幂等自检（双向）
#
# 用法：bash 01-rename/verify.sh [--root <repo>] [--reverse] [--strict]
#   --reverse  校验「原名版」树（cailu 标识）；默认校验「云枢版」树（cloudpivot 标识）
#   --strict   发现失败项时退出码 1（CI 用）；默认只报告，退出码 0
#
# 判定标准：
#   正向（云枢版）：`cailu|菜芦` 只允许出现在两类位置 ——
#     1) .gitleaks.toml 规则 3（有意保留旧前缀，见 special-cases.md 特例 1）
#     2) 少量「历史基线引用」：cailu-src 参考仓路径 / 旧 crate 名的基线描述
#        （见 rename.sh 的 STICKY_BY_FILE，逐 span 精确放行）
#   反向（原名版）：`cloudpivot|CloudPivot|云枢` 必须为 **0**（不得有改名痕迹）
#   migration-kit 目录是改名工具自身，两个方向都排除在残留扫描之外。
# =============================================================================
set -uo pipefail

ROOT=""
STRICT=0
REVERSE=0
while [ $# -gt 0 ]; do
  case "$1" in
    --root) ROOT="${2:?}"; shift 2 ;;
    --root=*) ROOT="${1#*=}"; shift ;;
    --strict) STRICT=1; shift ;;
    --reverse) REVERSE=1; shift ;;
    -h|--help) sed -n '2,19p' "$0"; exit 0 ;;
    *) echo "未知参数: $1" >&2; exit 2 ;;
  esac
done
[ -z "$ROOT" ] && ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "$ROOT" || { echo "进不去 $ROOT" >&2; exit 2; }

RED=$'\033[31m'; GRN=$'\033[32m'; YEL=$'\033[33m'; RST=$'\033[0m'
fail=0; warn=0
ok()   { printf "  ${GRN}✓${RST} %s\n" "$*"; }
bad()  { printf "  ${RED}✘${RST} %s\n" "$*"; fail=$((fail+1)); }
note() { printf "  ${YEL}!${RST} %s\n" "$*"; warn=$((warn+1)); }

EXCLUDES=(--exclude-dir=node_modules --exclude-dir=target --exclude-dir=.git
          --exclude-dir=dist --exclude-dir=migration-kit --exclude-dir=.rename-backup
          --exclude-dir=.pnpm-store
          --exclude=双版本说明.md)  # 中性文档：两版内容相同，必然同时出现新旧标识，不参与残留扫描

if [ "$REVERSE" -eq 1 ]; then
  DIRNAME="反向（校验原名版 / cailu 标识）"
  OLD_RE='cloudpivot|CloudPivot|云枢'
else
  DIRNAME="正向（校验云枢版 / cloudpivot 标识）"
  OLD_RE='cailu|Cailu|菜芦'
fi
echo "== 0) 方向：${DIRNAME}  root=$ROOT"

# 「历史基线引用」逐 span 白名单（与 rename.sh 的 STICKY_BY_FILE 保持一致）
STICKY_E='crate `cailu`|@cailu/web|`cailu`|DATABASE cailu|host[[:space:]]+cailu|dbname=cailu|-d cailu|cailu-src'
STICKY_FILES='^\./docs/大数据-技术架构\.md:|^\./docs/ERP-批次1-技术架构\.md:|^\./docs/ERP-批次1-开发计划\.md:|^\./docs/大数据-产品需求PRD\.md:|^\./docs/ERP-批次1-产品需求PRD\.md:'

echo "== 1) 旧标识残留扫描（排除 node_modules/target/.git/dist/migration-kit）=="
RAW="$(grep -rniE "$OLD_RE" . "${EXCLUDES[@]}" 2>/dev/null || true)"
TOTAL="$(printf '%s\n' "$RAW" | grep -c . || true)"
if [ "$REVERSE" -eq 1 ]; then
  echo "    cloudpivot/云枢 命中: ${TOTAL} 行"
  if [ "$TOTAL" -eq 0 ]; then
    ok "原名版无任何 cloudpivot/云枢 残留（红线达标）"
  else
    bad "原名版仍有 ${TOTAL} 行新标识残留（红线不达标）："
    printf '%s\n' "$RAW" | head -20 | sed 's/^/      /'
  fi
else
  GL="$(printf '%s\n' "$RAW" | grep -c '^\./\.gitleaks\.toml:' || true)"
  STICKY_HIT="$(printf '%s\n' "$RAW" | grep -E "$STICKY_FILES" | grep -cE "$STICKY_E" || true)"
  OTHER="$(printf '%s\n' "$RAW" | grep -v '^\./\.gitleaks\.toml:' | grep -vE "$STICKY_FILES" || true)"
  STICKY_LEFTOVER="$(printf '%s\n' "$RAW" | grep -E "$STICKY_FILES" | grep -vE "$STICKY_E" || true)"
  echo "    .gitleaks.toml（允许）: ${GL} 行"
  echo "    历史基线引用（允许）: ${STICKY_HIT} 行"
  echo "    合计: ${TOTAL} 行"
  if [ -n "$OTHER" ]; then
    bad "存在不被允许的残留："; printf '%s\n' "$OTHER" | sed 's/^/      /'
  elif [ -n "$STICKY_LEFTOVER" ]; then
    bad "历史文档里有「非基线引用」的旧标识（sticky 白名单之外）："
    printf '%s\n' "$STICKY_LEFTOVER" | sed 's/^/      /'
  else
    ok "无非法残留（只剩 .gitleaks.toml 规则 3 与历史基线引用）"
  fi
  if [ "$GL" -lt 1 ]; then
    bad ".gitleaks.toml 里旧前缀 cailu 不见了 —— 历史泄露检测被削弱（见 special-cases.md 特例 1）"
  elif [ "$GL" -le 8 ]; then
    ok ".gitleaks.toml 旧前缀保留（${GL} 行）"
  else
    note ".gitleaks.toml 命中 ${GL} 行（预期 1~8 行；多出来通常是注释被重复插入）"
  fi
fi

echo "== 2) .gitleaks.toml 规则 3 形态 =="
if [ "$REVERSE" -eq 1 ]; then
  grep -q "regex = '''cailu-(dev|test|prod)-jwt-secret-" .gitleaks.toml 2>/dev/null \
    && ok "规则 3 正则为纯 cailu 前缀（上游形态）" \
    || bad "规则 3 正则不是纯 cailu 前缀"
  grep -q '(?:cloudpivot|cailu)-' .gitleaks.toml 2>/dev/null \
    && bad "原名版仍带双前缀正则（应还原为纯 cailu）" \
    || ok "规则 3 无双前缀残留"
  grep -q '故意保留旧前缀' .gitleaks.toml 2>/dev/null \
    && bad "原名版仍带「改名」guard 注释（应还原）" \
    || ok "规则 3 无改名 guard 注释"
else
  grep -q '(?:cloudpivot|cailu)-' .gitleaks.toml 2>/dev/null \
    && ok "规则 3 正则同时匹配新旧前缀（(?:cloudpivot|cailu)）" \
    || bad "规则 3 正则没同时匹配新旧前缀"
  grep -q '故意保留旧前缀' .gitleaks.toml 2>/dev/null \
    && ok "规则 3 带 guard 注释（说明为何保留旧前缀）" \
    || note "规则 3 缺 guard 注释（不影响检测，但少了说明）"
fi

echo "== 3) 关键文件抽查（期望值 = 本方向目标标识）=="
if [ "$REVERSE" -eq 1 ]; then
  EXP_SLUG='cailu'; EXP_WEB='@cailu'; EXP_TITLE='菜芦'; EXP_KEY='cailu-setting'; EXP_BARE='/opt/git/cailu.git'
else
  EXP_SLUG='cloudpivot'; EXP_WEB='@cloudpivot'; EXP_TITLE='云枢'; EXP_KEY='cloudpivot-setting'; EXP_BARE='/opt/git/'
fi
check_eq() { # file pattern expect desc
  local f="$1" pat="$2" exp="$3" desc="$4"
  if [ ! -f "$f" ]; then bad "$desc：文件缺失 $f"; return; fi
  local got; got="$(grep -E "$pat" "$f" | head -1)"
  if printf '%s' "$got" | grep -q -- "$exp"; then ok "$desc  ($f)"
  else bad "$desc：$f 里 '$pat' 期望含 '$exp'，实际: ${got:-<无匹配>}"; fi
}
check_eq backend/Cargo.toml   '^name[[:space:]]*='          "$EXP_SLUG"  'Rust crate 名'
check_eq frontend/package.json '"name"'                     "$EXP_WEB"   '前端包名'
check_eq frontend/src/stores/setting.ts 'SETTING_KEY'        "$EXP_KEY"   'localStorage 键'
check_eq frontend/index.html  '<title>'                      "$EXP_TITLE" '页面标题'
check_eq backend/config/config.toml '^app_name'              "$EXP_SLUG"  'app_name'
check_eq backend/config/config.toml 'file_prefix'            "$EXP_SLUG"  '日志前缀'
check_eq backend/config/config.toml '^name[[:space:]]*='     "$EXP_SLUG"  '数据库名'
check_eq .githooks/post-commit "ORIGIN_URL|/opt/git"         "$EXP_BARE"  'bare 白名单'
if [ -f scripts/patch-alun-web.sh ]; then
  grep -q 'let mut best: Option<(&PermissionRule, u8)>' scripts/patch-alun-web.sh \
    && ok "patch-alun-web.sh 判重已按补丁代码签名（改名不再触发重复插入）" \
    || note "patch-alun-web.sh 判重仍依赖 marker 字面（f3fc278 未应用）"
fi

echo "== 4) 文件名残留 =="
if [ "$REVERSE" -eq 1 ]; then PAT='*cloudpivot*'; else PAT='*cailu*'; fi
FN="$(find . \( -name node_modules -o -name target -o -name dist -o -name .git \
        -o -name migration-kit -o -name logs -o -name uploads -o -name downloads \
        -o -name .rename-backup -o -name .pnpm-store \) -prune \
        -o -iname "$PAT" -print 2>/dev/null || true)"
[ -z "$FN" ] && ok "无含旧标识的文件/目录名" || { bad "仍有旧名文件："; printf '%s\n' "$FN" | sed 's/^/      /'; }

echo "== 5) 幂等自检（同方向再跑一次 dry-run 应为 0 改动）=="
DRY_ARGS=(--dry-run --no-backup --root "$ROOT")
[ "$REVERSE" -eq 1 ] && DRY_ARGS=(--reverse "${DRY_ARGS[@]}")
DRY_OUT="$(bash "$ROOT/migration-kit/01-rename/rename.sh" "${DRY_ARGS[@]}" 2>&1 || true)"
if printf '%s' "$DRY_OUT" | grep -qE '内容改动文件: 0 +替换次数: 0 +重命名: 0'; then
  ok "rename.sh 幂等：二次 dry-run 0 改动"
else
  bad "rename.sh 不幂等（二次 dry-run 仍有改动）："
  printf '%s\n' "$DRY_OUT" | tail -20 | sed 's/^/      /'
fi

echo "---------------------------------------------------------------------"
echo "结果: 失败 ${fail} 项, 提示 ${warn} 项"
if [ "$fail" -gt 0 ] && [ "$STRICT" -eq 1 ]; then exit 1; fi
exit 0
