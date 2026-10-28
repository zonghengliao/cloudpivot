#!/usr/bin/env bash
# =============================================================================
# 06-verify/verify.sh —— 云枢迁移验收（安全 14 项 + 功能 6 项 + 改名残留 + 构建）
#
# 用法：
#   bash 06-verify/verify.sh                # 全部（含 cargo test / pnpm typecheck）
#   bash 06-verify/verify.sh --no-build     # 只跑静态断言（几十秒级）
#   bash 06-verify/verify.sh --root <repo>
#
# 退出码：0 全通过；1 有失败项
# 说明：静态断言都是「读代码判定」，不碰数据库、不重启服务，可在任何副本上跑。
# =============================================================================
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
KIT="$(cd "$HERE/.." && pwd)"
ROOT=""
BUILD=1
while [ $# -gt 0 ]; do
  case "$1" in
    --root) ROOT="${2:?}"; shift 2 ;;
    --root=*) ROOT="${1#*=}"; shift ;;
    --no-build) BUILD=0; shift ;;
    -h|--help) sed -n '2,14p' "$0"; exit 0 ;;
    *) echo "未知参数: $1" >&2; exit 2 ;;
  esac
done
[ -z "$ROOT" ] && ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
cd "$ROOT" || { echo "进不去 $ROOT" >&2; exit 2; }

RED=$'\033[31m'; GRN=$'\033[32m'; YEL=$'\033[33m'; RST=$'\033[0m'
PASS=0; FAIL=0; SKIP=0
pass() { PASS=$((PASS+1)); printf "  ${GRN}✓${RST} %s\n" "$*"; }
fail() { FAIL=$((FAIL+1)); printf "  ${RED}✘${RST} %s\n" "$*"; }
skip() { SKIP=$((SKIP+1)); printf "  ${YEL}–${RST} %s\n" "$*"; }
# t "<描述>" "<shell 断言>"
t() { if eval "$2" >/dev/null 2>&1; then pass "$1"; else fail "$1  [$2]"; fi; }
sec() { echo; echo "== $* =="; }

sec "S1 改名残留（委托 01-rename/verify.sh）"
if [ -f "$KIT/01-rename/verify.sh" ]; then
  if bash "$KIT/01-rename/verify.sh" --root "$ROOT" --strict > /tmp/rename-verify.log 2>&1; then
    pass "01-rename/verify.sh --strict 通过"
  else
    fail "01-rename/verify.sh --strict 失败（详情 /tmp/rename-verify.log）"
    sed 's/^/      /' /tmp/rename-verify.log | tail -25
  fi
else
  skip "找不到 01-rename/verify.sh"
fi

sec "S2 安全断言（14 项）"
# 1 JWT 密钥不落盘：config 里 jwt_secret 只能为空串或注释
t "1) 配置文件无明文 jwt_secret（只允许空串/注释）" \
  '! grep -rnE "^[[:space:]]*jwt_secret[[:space:]]*=[[:space:]]*\"[^\"]+\"" backend/config/*.toml'
# 2 启动守卫 fail-closed
t "2) 启动守卫存在（shared/startup_guard.rs）" 'test -f backend/src/shared/startup_guard.rs'
t "2b) 启动守卫 fail-closed（密钥缺失/过短即拒绝启动）" \
  'grep -qE "拒绝启动|fail.?closed|exit\(1\)|process::exit" backend/src/shared/startup_guard.rs'
t "2c) lib.rs 在启动链上调用守卫" 'grep -q "startup_guard" backend/src/lib.rs'
# 3 去掉 docker 默认口令
t "3) config.toml 无 docker 默认库口令 alun" '! grep -qE "^\s*password\s*=\s*\"alun\"" backend/config/config.toml'
t "3b) config.toml 库口令为空串（由环境变量注入）" 'grep -qE "^\s*password\s*=\s*\"\"" backend/config/config.toml'
# 4 gitleaks 三级防线
t "4) .gitleaks.toml 存在" 'test -f .gitleaks.toml'
t "4b) scripts/secret-scan.sh 存在且可执行" 'test -x scripts/secret-scan.sh || test -f scripts/secret-scan.sh'
t "4c) pre-commit 钩子接入扫描（fail-closed）" 'grep -q "secret-scan.sh" .githooks/pre-commit'
t "4d) CI 工作流 secret-scan.yml 存在" 'test -f .github/workflows/secret-scan.yml'
# 5 规则3 保留旧前缀
t "5) gitleaks 规则3 同时匹配新旧前缀（保留历史泄露检测）" \
  'grep -q "(?:cloudpivot|cailu)-" .gitleaks.toml'
# 6 用户管理提权拦截
t "6) 用户管理权限守卫存在（system/user/guard.rs）" 'test -f backend/src/system/user/guard.rs'
t "6b) 建号/改密/授角色 三处都过守卫" \
  'grep -q "guard::" backend/src/system/user/handler.rs'
t "6c) 角色授权只能授予自己已持有的权限码（role/handler.rs::ensure_menus_grantable）" \
  'grep -q "ensure_menus_grantable" backend/src/system/role/handler.rs'
# 7 首登强制改密（后端）
t "7) 首登改密标记进入常量/中间件（must_change_password）" \
  'grep -rq "must_change_password" backend/src/shared/constants.rs backend/src/middleware/session_guard.rs'
t "7b) 会话守卫对未改密用户只放行改密/登出" \
  'grep -q "change-password\|change_password" backend/src/middleware/session_guard.rs'
# 8 首登改密前端闭环
t "8) 前端改密页存在" 'test -f frontend/src/views/user/change-password.vue'
t "8b) 路由守卫接入强制改密跳转" 'grep -q "change-password" frontend/src/router/index.ts'
# 9 会话吊销
t "9) 会话守卫中间件存在（middleware/session_guard.rs）" 'test -f backend/src/middleware/session_guard.rs'
t "9b) token_version / 会话失效字段迁移在位（V0022）" \
  'grep -q "token_version" backend/migrations/system/V0022__add_session_security_flags.up.sql'
# 10 短 TTL + 绝对上限
t "10) access TTL 15 分钟（900s）" 'grep -qE "^\s*access_token_expire_secs\s*=\s*900" backend/config/config.toml'
t "10b) refresh TTL 24 小时（86400s）" 'grep -qE "^\s*refresh_token_expire_secs\s*=\s*86400" backend/config/config.toml'
# 11 复用检测
t "11) refresh 复用检测落地（reuse_detected）" 'grep -q "reuse_detected" backend/src/system/auth/handler.rs'
t "11b) 会话守卫做令牌版本比对（middleware/session_guard.rs 的 token_version）" \
  'test -f backend/src/middleware/session_guard.rs && grep -q "token_version" backend/src/middleware/session_guard.rs'
t "11c) 吊销复检 fail-closed（middleware/auth_guard.rs：缓存不可用即 401）" \
  'test -f backend/src/middleware/auth_guard.rs && grep -qi "fail-closed\|fail closed" backend/src/middleware/auth_guard.rs'
# 12 Vite 收口
t "12) Vite 默认只监听 127.0.0.1（可用 VITE_HOST 显式放开）" \
  'grep -qE "VITE_HOST \|\| .127\.0\.0\.1." frontend/vite.config.ts'
# 13 compose 密钥全走环境变量必需项
t "13) prod compose 的 JWT 密钥为必需环境变量（:? 语法）" \
  'grep -q "ALUN_MIDDLEWARE_AUTH_JWT_SECRET:?" docker/docker-compose.prod.yml'
t "13b) prod compose 无明文数据库口令" '! grep -qE "POSTGRES_PASSWORD:\s*[a-zA-Z0-9]+" docker/docker-compose.prod.yml'
# 14 patch-alun-web 补丁链
t "14) patch-alun-web.sh 存在" 'test -f scripts/patch-alun-web.sh'
t "14b) 判重不依赖标记字面（按补丁代码签名判重）" \
  'grep -q "let mut best: Option<(&PermissionRule, u8)>" scripts/patch-alun-web.sh'
if [ -d "$HOME/.cargo/registry/src" ]; then
  if bash scripts/patch-alun-web.sh --check >/tmp/patch-check.log 2>&1; then
    pass "14c) patch-alun-web.sh --check: PATCH-STATE OK（registry 补丁在位）"
  else
    skip "14c) patch-alun-web.sh --check 未通过（cargo registry 里 alun-web 未打补丁；开发/构建前需跑一次，详见 /tmp/patch-check.log）"
  fi
else
  skip "14c) 无 cargo registry，跳过 alun-web 补丁在位检查"
fi

sec "S3 功能断言"
t "F1) business monitor 四件套完整" \
  'test -f backend/src/business/monitor/mod.rs -a -f backend/src/business/monitor/model.rs -a -f backend/src/business/monitor/dto.rs -a -f backend/src/business/monitor/handler.rs'
t "F2) 接口权限码与迁移里的 auth_code 一致" \
  'test -n "$(grep -o "business:monitor:server-status" backend/src/business/monitor/handler.rs | head -1)" && grep -q "business:monitor:server-status" backend/migrations/business/V0020__add_biz_monitor_permission.up.sql'
t "F3) 所有迁移 up/down 成对" \
  'test -z "$(for f in $(find backend/migrations -name "*.up.sql"); do d="${f%.up.sql}.down.sql"; [ -f "$d" ] || echo "$f"; done)"'
t "F4) 状态卡仅超管渲染（isSuperAdmin 门）" \
  'grep -q "isSuperAdmin" frontend/src/views/dashboard/index.vue'
t "F5) 品牌主色生效（#6F8AB8 晨雾蓝 = --td-brand-color-7）" \
  'grep -qi -- "--td-brand-color-7: #6f8ab8" frontend/src/style/theme.less'
t "F6) 品牌资源替换（logo 512x512 / favicon 64x64）" \
  'test -f frontend/public/logo.png -a -f frontend/public/favicon.png'

# ---------------------------------------------------------------------------
# 品牌 / 配色（03-brand 的「最终态」断言）
# 这些断言就是「重放出来的东西和线上视觉一致」的机器判据，缺一不可。
# ---------------------------------------------------------------------------
sec "S3b 品牌与配色（主题变量 / 组件色值 / 资源 / 旧青色残留）"
PYBIN="${PYTHON:-python3}"
BRAND_PY="$KIT/03-brand/replace-colors.py"
if [ -f "$BRAND_PY" ]; then
  if "$PYBIN" "$BRAND_PY" --root "$ROOT" --check > /tmp/brand-check.log 2>&1; then
    pass "B1) 03-brand 自检通过：上游青色系已清零 + 6 个关键变量到位"
  else
    fail "B1) 03-brand 自检失败（详情 /tmp/brand-check.log）"
    sed 's/^/      /' /tmp/brand-check.log | tail -22
  fi
else
  fail "B1) 缺少 03-brand/replace-colors.py（配色无法自证）"
fi
t "B2) theme.less == 03-brand/theme-final.less（逐字节，防被本地手改）" \
  'cmp -s frontend/src/style/theme.less "$KIT/03-brand/theme-final.less"'
t "B3) --td-bg-color-page = #f2f2ee（霜月白）" \
  'grep -qi -- "--td-bg-color-page: #f2f2ee" frontend/src/style/theme.less'
t "B4) --td-component-border = #aab7c2（雾蓝灰描边）" \
  'grep -qi -- "--td-component-border: #aab7c2" frontend/src/style/theme.less'
t "B5) --app-brand-secondary: #51697c（深海蓝转辅助色）" \
  'grep -qi -- "--app-brand-secondary: #51697c" frontend/src/style/theme.less'
t "B6) --app-brand-strong: #56729f（严格 AA 白字底）" \
  'grep -qi -- "--app-brand-strong: #56729f" frontend/src/style/theme.less'
t "B7) 横幅渐变 --app-gradient-brand 仍是三档（深蓝→晨雾蓝→hover）" \
  'test "$(grep -m1 -- "--app-gradient-brand" frontend/src/style/theme.less | grep -o "#[0-9a-fA-F]\{6\}" | wc -l)" = "3"'
t "B8) 横幅渐变没有被改成五色（历史 1172658 已被 revert，不许回来）" \
  '! grep -q "app-gradient-brand-five\|gradient-brand.*#c1ac7b.*#c17b7e" frontend/src/style/theme.less'
t "B9) 上游青色系清零（6 位 hex，frontend/src）" \
  '! grep -rqiE "#(0580a2|0398c1|017190|035e77|054d61|90d6ea|f6fafb|ecf6f8|ddf0f5|c6e7f1|03a3cc)" frontend/src'
t "B10) 上游青色系清零（8 位 hex 带 alpha，frontend/src）" \
  '! grep -rqiE "#(0580a2|0398c1|017190)[0-9a-f]{2}\\b" frontend/src'
t "B11) 上游青色系清零（rgb() 现代语法：rgb(5 128 162 / …)）" \
  '! grep -rqiE "rgb\\(0*5[ ,]+128[ ,]+162" frontend/src'
t "B12) 组件里旧的蓝/紫/红散落色清零（rgb 形式：59 109 255 / 94 112 180 / 122 142 255 / 15 30 70）" \
  '! grep -rqiE "rgb\\((59 109 255|94 112 180|122 142 255|15 30 70)" frontend/src'
t "B13) 语义色未被改成灰度（成功绿 #3e9b72 / 警告橙 #d98b4a / 危险绛红 #c17b7e 都在）" \
  'grep -q "#3e9b72" frontend/src/views/dashboard/index.vue && grep -q "#d98b4a" frontend/src/views/dashboard/index.vue && grep -q "#c17b7e" frontend/src/views/dashboard/index.vue'
t "B14) 登录页渐变走变量（--app-login-bg / --app-login-brand），不是写死 hex" \
  'grep -q "var(--app-login-bg)" frontend/src/views/login/index.vue && grep -q "var(--app-login-brand)" frontend/src/views/login/index.vue'
t "B15) 登录页柔光改流金（rgb(193 172 123 …)）" \
  'grep -q "rgb(193 172 123 / 26%)" frontend/src/views/login/index.vue'

if [ "$BUILD" = "1" ]; then
  sec "S4 构建与测试"
  # cargo 常常不在非登录 shell 的 PATH 里（rustup 装在 ~/.cargo/bin）
  CARGO_BIN="$(command -v cargo || true)"
  [ -z "$CARGO_BIN" ] && [ -x "$HOME/.cargo/bin/cargo" ] && CARGO_BIN="$HOME/.cargo/bin/cargo"
  echo "  -- cargo test（backend，$CARGO_BIN）--"
  T0=$(date +%s)
  if [ -n "$CARGO_BIN" ] && ( cd backend && "$CARGO_BIN" test --all-targets > /tmp/cargo-test.log 2>&1 ); then
    pass "cargo test 全绿（$(grep -cE '^test .* \.\.\. ok$' /tmp/cargo-test.log) 个用例，$(( $(date +%s) - T0 ))s）"
  else
    fail "cargo test 失败（/tmp/cargo-test.log 末尾）"
    tail -25 /tmp/cargo-test.log | sed 's/^/      /'
  fi
  echo "  -- pnpm typecheck（frontend）--"
  T0=$(date +%s)
  if ( cd frontend && pnpm typecheck > /tmp/typecheck.log 2>&1 ); then
    pass "pnpm typecheck 零错误（$(( $(date +%s) - T0 ))s）"
  else
    fail "pnpm typecheck 有错误（/tmp/typecheck.log 末尾）"
    tail -25 /tmp/typecheck.log | sed 's/^/      /'
  fi
else
  skip "S4 构建与测试（--no-build）"
fi

echo
echo "====================================================================="
echo "验收结果: 通过 ${PASS}, 失败 ${FAIL}, 跳过 ${SKIP}"
echo "====================================================================="
[ "$FAIL" -eq 0 ] && exit 0 || exit 1
