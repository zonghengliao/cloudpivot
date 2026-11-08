#!/usr/bin/env bash
# =============================================================================
# CloudPivot PDF 导出基础设施安装脚本（幂等，可重复执行）
#
# 装三样东西：
#   ① 中文字体  fonts-noto-cjk + fonts-noto-cjk-extra（没有它 PDF 全是方框）
#   ② Chromium  优先 snap（Ubuntu 24.04+ 官方 deb 已改为 snap 过渡包），兜底 apt
#   ③ 常驻 CDP 服务 chromium-pdf.service（:9222，每天 3 点自动重启）
#
# 用法：
#   sudo bash scripts/pdf/install-pdf-infra.sh            # 安装 + 自检
#   sudo bash scripts/pdf/install-pdf-infra.sh --check    # 只自检，不改系统
#
# 退出码：0 全部通过；1 有检查项未通过（逐行打印 FAIL）。
#
# 说明：本脚本只改本机系统状态（apt / snap / systemd），不碰数据库、不碰业务进程，
# 可在测试机与生产机重复执行。生产机（144）由运维手工执行，CI 不自动跑。
# =============================================================================
set -uo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
UNIT_SRC="${SCRIPT_DIR}/chromium-pdf.service"
UNIT_DST="/etc/systemd/system/chromium-pdf.service"
CDP_PORT=9222
CHECK_ONLY=0
[[ "${1:-}" == "--check" ]] && CHECK_ONLY=1

FAILED=0
pass() { printf '  \033[32mPASS\033[0m  %s\n' "$*"; }
fail() { printf '  \033[31mFAIL\033[0m  %s\n' "$*"; FAILED=1; }
info() { printf '\n== %s ==\n' "$*"; }

[[ $EUID -eq 0 ]] || { echo "必须以 root 运行"; exit 1; }

# ---------------------------------------------------------------------------
# ① 中文字体
# ---------------------------------------------------------------------------
if [[ $CHECK_ONLY -eq 0 ]]; then
    info "① 安装中文字体（fonts-noto-cjk / -extra）"
    export DEBIAN_FRONTEND=noninteractive
    if dpkg -s fonts-noto-cjk >/dev/null 2>&1; then
        echo "  已安装，跳过"
    else
        apt-get install -y -qq fontconfig fonts-noto-cjk fonts-noto-cjk-extra 2>&1 | tail -3
    fi
    # 装完必须刷新字体缓存，否则 fc-list 看不到新字体
    fc-cache -f >/dev/null 2>&1 || true
fi

info "② 校验中文字体"
ZH_COUNT=$(fc-list :lang=zh 2>/dev/null | wc -l)
if [[ "$ZH_COUNT" -gt 0 ]]; then
    pass "fc-list :lang=zh 命中 ${ZH_COUNT} 个字体切片"
else
    fail "fc-list :lang=zh 命中 0 —— PDF 中文会变方框"
fi
MATCH=$(fc-match "Noto Sans CJK SC" 2>/dev/null)
if [[ "$MATCH" == *NotoSansCJK* || "$MATCH" == *"Noto Sans CJK"* ]]; then
    pass "fc-match \"Noto Sans CJK SC\" -> ${MATCH}"
else
    fail "fc-match \"Noto Sans CJK SC\" -> ${MATCH}（未命中 Noto Sans CJK）"
fi

# ---------------------------------------------------------------------------
# ③ Chromium
# ---------------------------------------------------------------------------
CHROME_BIN=""
if command -v chromium >/dev/null 2>&1 && chromium --version >/dev/null 2>&1; then
    CHROME_BIN="$(command -v chromium)"
elif command -v chromium-browser >/dev/null 2>&1 && chromium-browser --version >/dev/null 2>&1; then
    CHROME_BIN="$(command -v chromium-browser)"
fi

if [[ -z "$CHROME_BIN" && $CHECK_ONLY -eq 0 ]]; then
    info "③ 安装 Chromium"
    if command -v snap >/dev/null 2>&1; then
        snap install chromium 2>&1 | tail -2 || true
        # snap 在容器里经常卡在 apparmor profile 未加载 → chromium 报
        # "missing profile snap-update-ns.chromium"，下面两步是通用解法。
        systemctl start snapd.apparmor >/dev/null 2>&1 || true
        apparmor_parser -r /var/lib/snapd/apparmor/profiles/snap-update-ns.chromium >/dev/null 2>&1 || true
        CHROME_BIN="/snap/bin/chromium"
    else
        apt-get install -y -qq chromium 2>&1 | tail -3
        CHROME_BIN="$(command -v chromium || echo /usr/bin/chromium)"
    fi
fi

info "④ 校验 Chromium"
if [[ -n "$CHROME_BIN" ]] && "$CHROME_BIN" --version >/dev/null 2>&1; then
    pass "$("$CHROME_BIN" --version 2>/dev/null | head -1)（$CHROME_BIN）"
else
    fail "chromium 不可执行"
    CHROME_BIN="${CHROME_BIN:-/snap/bin/chromium}"
fi
# 无头 DOM 导出（证明渲染进程能真正跑起来，而不只是二进制存在）
DOM_OUT=$("$CHROME_BIN" --headless --no-sandbox --disable-gpu --dump-dom about:blank 2>/dev/null | head -1)
if [[ "$DOM_OUT" == *"<html"* ]]; then
    pass "chromium --headless --dump-dom about:blank 输出正常：${DOM_OUT:0:48}"
else
    fail "chromium --headless --dump-dom about:blank 无有效输出"
fi

# ---------------------------------------------------------------------------
# ⑤ CDP 常驻服务
# ---------------------------------------------------------------------------
if [[ $CHECK_ONLY -eq 0 ]]; then
    info "⑤ 安装并启动 chromium-pdf.service"
    [[ -f "$UNIT_SRC" ]] || { echo "缺少 unit 模板：$UNIT_SRC"; exit 1; }
    # 把模板里的 /snap/bin/chromium 替换成本机实际路径后落地
    sed "s#^ExecStart=[^ ]*#ExecStart=${CHROME_BIN}#" "$UNIT_SRC" > "$UNIT_DST"
    systemctl daemon-reload
    systemctl enable chromium-pdf >/dev/null 2>&1 || true
    systemctl restart chromium-pdf
    for _ in $(seq 1 30); do
        curl -sf -m 1 "http://127.0.0.1:${CDP_PORT}/json/version" >/dev/null 2>&1 && break
        sleep 0.5
    done
fi

info "⑥ 校验 CDP 端点"
if systemctl is-active --quiet chromium-pdf; then
    pass "systemd: chromium-pdf 正在运行（PID $(systemctl show -p MainPID --value chromium-pdf)）"
else
    fail "systemd: chromium-pdf 未运行（journalctl -u chromium-pdf -n 50 看原因）"
fi
VER_JSON=$(curl -sf -m 3 "http://127.0.0.1:${CDP_PORT}/json/version" 2>/dev/null)
if [[ -n "$VER_JSON" ]]; then
    BROWSER=$(echo "$VER_JSON" | sed -n 's/.*"Browser": *"\([^"]*\)".*/\1/p')
    WSURL=$(echo "$VER_JSON" | sed -n 's/.*"webSocketDebuggerUrl": *"\([^"]*\)".*/\1/p')
    pass "CDP /json/version 可用：${BROWSER}  ws=${WSURL}"
else
    fail "CDP /json/version 连不上 127.0.0.1:${CDP_PORT}"
fi

info "结果"
if [[ $FAILED -eq 0 ]]; then
    echo "  全部通过 ✅  chromium=${CHROME_BIN}  cdp=ws://127.0.0.1:${CDP_PORT}"
    echo "  下一步：curl -s http://127.0.0.1:${CDP_PORT}/json/version 检查常驻，"
    echo "          并在后端 config.toml 里确认 [pdf] cdp_url 指向本机 9222。"
    exit 0
else
    echo "  有检查项未通过 ❌ 见上面 FAIL 行"
    exit 1
fi
