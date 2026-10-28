#!/usr/bin/env bash
# =============================================================================
# 01-rename/verify.sh —— 改名校验：残留扫描 + 关键文件抽查 + 幂等自检
#
# 用法：bash 01-rename/verify.sh [--root <repo>] [--strict]
#   --strict  发现残留时退出码 1（CI 用）；默认只报告，退出码 0
#
# 判定标准（本仓库当前状态）：
#   `cailu|菜芦` 只允许出现在两处：
#     1) .gitleaks.toml 规则 3（有意保留旧前缀，见 special-cases.md）
#     2) docs/ERP-批次1-*.md（历史文档对旧基线的描述性文字，改了等于篡改历史）
#   其余任何命中都算失败。
# =============================================================================
set -uo pipefail

ROOT=""
STRICT=0
while [ $# -gt 0 ]; do
  case "$1" in
    --root) ROOT="${2:?}"; shift 2 ;;
    --root=*) ROOT="${1#*=}"; shift ;;
    --strict) STRICT=1; shift ;;
    -h|--help) sed -n '2,16p' "$0"; exit 0 ;;
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
          --exclude-dir=.pnpm-store)

echo "== 1) 旧标识残留扫描（排除 node_modules/target/.git/dist/migration-kit）=="
RAW="$(grep -rniE "cailu|菜芦" . "${EXCLUDES[@]}" 2>/dev/null || true)"
ALLOWED_GITLEAKS="$(printf '%s\n' "$RAW" | grep -c '^\./\.gitleaks\.toml:' || true)"
ALLOWED_ERP="$(printf '%s\n' "$RAW" | grep -c '^\./docs/ERP-批次1-' || true)"
OTHER="$(printf '%s\n' "$RAW" | grep -v '^\./\.gitleaks\.toml:' | grep -v '^\./docs/ERP-批次1-' || true)"
TOTAL="$(printf '%s\n' "$RAW" | grep -c . || true)"
echo "    .gitleaks.toml（允许）: ${ALLOWED_GITLEAKS} 行"
echo "    docs/ERP-批次1-*.md（允许，历史文档）: ${ALLOWED_ERP} 行"
echo "    合计: ${TOTAL} 行"
if [ -n "$OTHER" ]; then
  bad "存在不被允许的残留："
  printf '%s\n' "$OTHER" | sed 's/^/      /'
else
  ok "无非法残留（只剩 .gitleaks.toml 规则 3 与历史 ERP 文档）"
fi
if [ "$ALLOWED_GITLEAKS" -lt 1 ]; then
  bad ".gitleaks.toml 里旧前缀 cailu 不见了 —— 历史泄露检测被削弱，见 special-cases.md 特例 1"
elif [ "$ALLOWED_GITLEAKS" -eq 1 ] || [ "$ALLOWED_GITLEAKS" -eq 3 ]; then
  ok ".gitleaks.toml 旧前缀保留（${ALLOWED_GITLEAKS} 行）"
else
  note ".gitleaks.toml 命中 ${ALLOWED_GITLEAKS} 行（预期 1~3 行；多出来通常是注释被重复插入）"
fi
grep -q '(?:cloudpivot|cailu)-' .gitleaks.toml 2>/dev/null \
  && ok ".gitleaks.toml 规则 3 正则同时匹配新旧前缀（(?:cloudpivot|cailu)）" \
  || bad ".gitleaks.toml 规则 3 正则没同时匹配新旧前缀"

echo "== 2) 关键文件抽查（期望值 = 云枢标识）=="
check_eq() { # file pattern expect desc
  local f="$1" pat="$2" exp="$3" desc="$4"
  if [ ! -f "$f" ]; then bad "$desc：文件缺失 $f"; return; fi
  local got; got="$(grep -E "$pat" "$f" | head -1)"
  if printf '%s' "$got" | grep -q -- "$exp"; then ok "$desc  ($f)"
  else bad "$desc：$f 里 '$pat' 期望含 '$exp'，实际: ${got:-<无匹配>}"; fi
}
check_eq backend/Cargo.toml   '^name[[:space:]]*='                    'cloudpivot'  'Rust crate 名'
check_eq frontend/package.json '"name"'                               '@cloudpivot' '前端包名'
check_eq frontend/src/stores/setting.ts 'SETTING_KEY'                'cloudpivot-setting' 'localStorage 键'
check_eq frontend/index.html  '<title>'                               '云枢'        '页面标题'
check_eq backend/config/config.toml '^app_name'                       'cloudpivot'  'app_name'
check_eq backend/config/config.toml 'file_prefix'                     'cloudpivot'  '日志前缀'
check_eq backend/config/config.toml '^name[[:space:]]*='              'cloudpivot'  '数据库名'
check_eq .githooks/post-commit "ORIGIN_URL|/opt/git"                  '/opt/git'    'bare 白名单'
[ -f scripts/patch-alun-web.sh ] && grep -q '/opt/git/cailu' scripts/patch-alun-web.sh \
  && bad "scripts/patch-alun-web.sh 仍写死旧 bare 路径" || ok "scripts/patch-alun-web.sh 无旧路径"
if [ -f scripts/patch-alun-web.sh ]; then
  grep -q 'let mut best: Option<(&PermissionRule, u8)>' scripts/patch-alun-web.sh \
    && ok "patch-alun-web.sh 判重已按补丁代码签名（认知：改名不再触发重复插入）" \
    || note "patch-alun-web.sh 判重仍依赖 marker 字面（f3fc278 未应用）"
fi

echo "== 3) 文件名残留 =="
FN="$(find . -path ./node_modules -prune -o -path ./target -prune -o -path ./.git -prune \
        -o -path ./migration-kit -prune -o -iname '*cailu*' -print 2>/dev/null || true)"
[ -z "$FN" ] && ok "无含旧标识的文件/目录名" || { bad "仍有旧名文件："; printf '%s\n' "$FN" | sed 's/^/      /'; }

echo "== 4) 幂等自检（再跑一次 dry-run 应为 0 改动）=="
DRY_OUT="$(bash "$ROOT/migration-kit/01-rename/rename.sh" --dry-run --no-backup --root "$ROOT" 2>&1 || true)"
if printf '%s' "$DRY_OUT" | grep -qE '内容改动文件: 0 +替换次数: 0 +重命名: 0'; then
  ok "rename.sh 幂等：二次 dry-run 0 改动"
else
  bad "rename.sh 不幂等（二次 dry-run 仍有改动）："
  printf '%s\n' "$DRY_OUT" | tail -20 | sed 's/^/      /'
fi

echo "== 5) 外部 URL / 参考仓路径保护 =="
if git ls-files 2>/dev/null | grep -q .; then
  if git grep -q 'github.com/wild1024/cailu' -- . ':!migration-kit' 2>/dev/null; then
    note "树里仍有外部上游 URL（属于保护串，未被改名——符合预期，但需确认 Pro 基线是否还要它）"
  else
    ok "树里没有外部上游 URL 残留（阶段3已彻底脱离）"
  fi
fi

echo "---------------------------------------------------------------------"
echo "结果: 失败 ${fail} 项, 提示 ${warn} 项"
if [ "$fail" -gt 0 ] && [ "$STRICT" -eq 1 ]; then exit 1; fi
exit 0
