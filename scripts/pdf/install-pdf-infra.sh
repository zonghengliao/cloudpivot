#!/usr/bin/env bash
# =============================================================================
# CloudPivot PDF 导出基础设施安装脚本（幂等，可重复执行）
#
# 装四样东西：
#   ① 中文字体底座  fonts-noto-cjk + fonts-noto-cjk-extra（fc-match 校验用）
#   ② 静态 glyf 中文字体 Noto Sans SC Regular/Bold（**PDF 体积的关键**，见下）
#   ③ Chromium    优先 snap（Ubuntu 24.04+ 的官方 deb 已改为 snap 过渡包），兜底 apt
#   ④ 常驻 CDP 服务 chromium-pdf.service（:9222，每天 03:00 自动重启）
#
# 为什么 ② 必须单独装（PDF-01 实测，145 / Chromium 157）：
#   · fonts-noto-cjk 提供的是 NotoSansCJK-*.ttc（字体集合 + CFF 轮廓）。Chromium 的
#     Skia PDF 后端无法子集嵌入，每个字形退化成独立 Type3 字体对象 → 20 行对账单
#     单页 311KB；
#   · 换成静态 glyf TTF 后同一页 13KB（24 倍），500 行 14 页 1.27MB（≈90KB/页）。
#   所以模板里的 font-family 首选 "Noto Sans SC"，本步骤负责把这个字体装到位。
#
# 用法：
#   sudo bash scripts/pdf/install-pdf-infra.sh            # 安装 + 自检
#   sudo bash scripts/pdf/install-pdf-infra.sh --check    # 只自检，不改系统
#
# 退出码：0 全部通过；1 有检查项未通过（逐行打印 FAIL）。
#
# 说明：本脚本只改本机系统状态（apt / snap / fonts / systemd），不碰数据库、不碰业务
# 进程，可在测试机与生产机重复执行。生产机（144）由运维手工执行，CI 不自动跑。
# =============================================================================
set -uo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
UNIT_SRC="${SCRIPT_DIR}/chromium-pdf.service"
INSTANCE_PY="${SCRIPT_DIR}/instance_noto_sc.py"
UNIT_DST="/etc/systemd/system/chromium-pdf.service"
FONT_DIR="/usr/share/fonts/truetype/noto-sc"
CDP_PORT=9222
CHECK_ONLY=0
[[ "${1:-}" = "--check" ]] && CHECK_ONLY=1

# 可变字体下载地址（**钉死 commit + sha256**：字体一换 PDF 体积就变，必须可复现）
NOTO_VF_COMMIT="2894aab31764f10f29c421bdfd2340d3b382d384"
NOTO_VF_URL="https://raw.githubusercontent.com/google/fonts/${NOTO_VF_COMMIT}/ofl/notosanssc/NotoSansSC%5Bwght%5D.ttf"
NOTO_VF_SHA256="a3041811a78c361b1de50f953c805e0244951c21c5bd412f7232ef0d899af0da"

FAILED=0
pass() { printf '  \033[32mPASS\033[0m  %s\n' "$*"; }
fail() { printf '  \033[31mFAIL\033[0m  %s\n' "$*"; FAILED=1; }
info() { printf '\n== %s ==\n' "$*"; }

[[ $EUID -eq 0 ]] || { echo "必须以 root 运行"; exit 1; }

# ---------------------------------------------------------------------------
# ① 中文字体底座
# ---------------------------------------------------------------------------
if [[ $CHECK_ONLY -eq 0 ]]; then
    info "① 安装中文字体底座（fonts-noto-cjk / -extra + fontconfig）"
    export DEBIAN_FRONTEND=noninteractive
    if dpkg -s fonts-noto-cjk >/dev/null 2>&1 && dpkg -s fontconfig >/dev/null 2>&1; then
        echo "  已安装，跳过"
    else
        apt-get install -y -qq fontconfig fonts-noto-cjk fonts-noto-cjk-extra 2>&1 | tail -3
    fi
    fc-cache -f >/dev/null 2>&1 || true
fi

info "② 校验中文字体底座"
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
# ③ 静态 glyf 中文字体（PDF 体积关键，见文件头说明）
# ---------------------------------------------------------------------------
if [[ $CHECK_ONLY -eq 0 && ! -f "${FONT_DIR}/NotoSansSC-Regular.ttf" ]]; then
    info "③ 实例化并安装静态 glyf 中文字体 Noto Sans SC（Regular/Bold）"
    dpkg -s python3-fonttools >/dev/null 2>&1 || \
        apt-get install -y -qq python3-fonttools 2>&1 | tail -2
    TMPVF=$(mktemp /var/tmp/NotoSansSC-VF.XXXXXX.ttf)
    echo "  下载可变字体（约 17MB）…"
    if ! curl -sSL -m 300 -o "$TMPVF" "$NOTO_VF_URL"; then
        fail "可变字体下载失败：$NOTO_VF_URL"
    else
        ACTUAL=$(sha256sum "$TMPVF" | awk '{print $1}')
        if [[ "$ACTUAL" != "$NOTO_VF_SHA256" ]]; then
            fail "可变字体 sha256 不匹配（期望 $NOTO_VF_SHA256，实际 $ACTUAL）——上游文件变了，先核对再更新脚本"
        else
            pass "可变字体 sha256 校验通过"
            mkdir -p "$FONT_DIR"
            if python3 "$INSTANCE_PY" "$TMPVF" "$FONT_DIR"; then
                pass "已实例化 NotoSansSC-Regular/Bold 到 ${FONT_DIR}"
            else
                fail "字体实例化失败（见上面 python 输出）"
            fi
        fi
    fi
    rm -f "$TMPVF"
    fc-cache -f >/dev/null 2>&1 || true
fi

info "④ 校验静态 glyf 字体"
if [[ -f "${FONT_DIR}/NotoSansSC-Regular.ttf" && -f "${FONT_DIR}/NotoSansSC-Bold.ttf" ]]; then
    pass "静态字体在位：${FONT_DIR}/NotoSansSC-{Regular,Bold}.ttf"
    if fc-match "Noto Sans SC" 2>/dev/null | grep -q "NotoSansSC"; then
        pass "fc-match \"Noto Sans SC\" -> $(fc-match 'Noto Sans SC' 2>/dev/null)"
    else
        fail "fc-match \"Noto Sans SC\" 未命中 NotoSansSC（PDF 会退回 .ttc → Type3 体积膨胀）"
    fi
    if fc-match "Noto Sans SC:bold" 2>/dev/null | grep -q "Bold"; then
        pass "粗体命中：$(fc-match 'Noto Sans SC:bold' 2>/dev/null)"
    else
        fail "fc-match \"Noto Sans SC:bold\" 未命中粗体"
    fi
else
    fail "静态 glyf 字体缺失（${FONT_DIR}），PDF 会退化成 Type3、单页 300KB+"
fi

# ---------------------------------------------------------------------------
# ⑤ Chromium
# ---------------------------------------------------------------------------
CHROME_BIN=""
if command -v chromium >/dev/null 2>&1 && chromium --version >/dev/null 2>&1; then
    CHROME_BIN="$(command -v chromium)"
elif command -v chromium-browser >/dev/null 2>&1 && chromium-browser --version >/dev/null 2>&1; then
    CHROME_BIN="$(command -v chromium-browser)"
fi

if [[ -z "$CHROME_BIN" && $CHECK_ONLY -eq 0 ]]; then
    info "⑤ 安装 Chromium"
    if command -v snap >/dev/null 2>&1; then
        snap install chromium 2>&1 | tail -2 || true
        # snap 在容器里经常卡在 apparmor profile 未加载 → chromium 报
        # "missing profile snap-update-ns.chromium"，下面两步是通用解法
        systemctl start snapd.apparmor >/dev/null 2>&1 || true
        apparmor_parser -r /var/lib/snapd/apparmor/profiles/snap-update-ns.chromium >/dev/null 2>&1 || true
        CHROME_BIN="/snap/bin/chromium"
    else
        apt-get install -y -qq chromium 2>&1 | tail -3
        CHROME_BIN="$(command -v chromium || echo /usr/bin/chromium)"
    fi
fi

info "⑥ 校验 Chromium"
if [[ -n "$CHROME_BIN" ]] && "$CHROME_BIN" --version >/dev/null 2>&1; then
    pass "$("$CHROME_BIN" --version 2>/dev/null | head -1)（$CHROME_BIN）"
else
    fail "chromium 不可执行"
    CHROME_BIN="${CHROME_BIN:-/snap/bin/chromium}"
fi
# 无头 DOM 导出（证明渲染进程真能跑起来，而不只是二进制存在）
DOM_OUT=$("$CHROME_BIN" --headless --no-sandbox --disable-gpu --dump-dom about:blank 2>/dev/null | head -1)
if [[ "$DOM_OUT" == *"<html"* ]]; then
    pass "chromium --headless --dump-dom about:blank 输出正常：${DOM_OUT:0:48}"
else
    fail "chromium --headless --dump-dom about:blank 无有效输出"
fi

# ---------------------------------------------------------------------------
# ⑦ CDP 常驻服务
# ---------------------------------------------------------------------------
if [[ $CHECK_ONLY -eq 0 ]]; then
    info "⑦ 安装并启动 chromium-pdf.service"
    [[ -f "$UNIT_SRC" ]] || { echo "缺少 unit 模板：$UNIT_SRC"; exit 1; }
    # 把模板里的 /snap/bin/chromium 替换成本机实际路径后落地
    sed "s#^ExecStart=[^ ]*#ExecStart=${CHROME_BIN}#" "$UNIT_SRC" > "$UNIT_DST"
    systemctl daemon-reload
    systemctl enable chromium-pdf >/dev/null 2>&1 || true
    # chromium 进程会缓存 fontconfig 字体列表：装了新字体必须重启才生效
    systemctl restart chromium-pdf
    for _ in $(seq 1 40); do
        curl -sf -m 1 "http://127.0.0.1:${CDP_PORT}/json/version" >/dev/null 2>&1 && break
        sleep 0.5
    done
fi

info "⑧ 校验 CDP 端点"
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
    echo "  下一步：后端注入 PDF_CDP_URL / PDF_MAX_CONCURRENCY / PDF_RENDER_TIMEOUT_MS（可选），"
    echo "          然后 curl -H 'Authorization: Bearer <token>' http://127.0.0.1:8023/api/business/pdf/health"
    exit 0
else
    echo "  有检查项未通过 ❌ 见上面 FAIL 行"
    exit 1
fi
