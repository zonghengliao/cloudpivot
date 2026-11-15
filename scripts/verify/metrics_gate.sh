#!/usr/bin/env bash
# ======================================================================
# metrics_gate.sh —— /metrics 端点自检（P1-09 · 卡 t_7264e6ff）
#
# 一句话：**端点活着不代表监控活着**。P1-05 踩过的真实故障是
#   `curl /metrics` 返回 200，但 body 里一条 `cloudpivot_` 都没有
#   （业务指标从未 describe，只有框架的 `nik_*`）——
# 只看 HTTP 状态码发现不了，而 `scripts/ops/alert_check.sh` 的 ③（巡检结论）
# 与 ⑥（导入导出积压）两项**正是读这些指标**。所以本闸判三条：
#
#   ① HTTP 200（curl 连不上 / 404 / 403 / 5xx 都红）
#   ② body 里有 `^# HELP cloudpivot_`（业务指标已登记且已写入）
#   ③ 只在「该有服务的机器」上强制 —— 见下面的 auto 语义
#
# ============================ 四种模式 ============================
#   auto（默认）  本机有「部署证据」→ 按 block 跑；否则连不上就跳过
#   block         必须真跑，失败即红（CI / 144 / 145 / 演练）
#   warn          真跑但不阻断（存量环境过渡期用）
#   off           跳过（`SKIP_METRICS_CHECK=1` 等价，卡面要求的逃生门）
#
# 「部署证据」= `CI=1` / 存在 `/etc/cloudpivot*/*.env` / systemd 里注册过 cloudpivot 服务。
# 为什么要 auto 而不是一刀切：
#   · `check.sh --fast` 是 **pre-push 钩子**，开发机上服务通常没起；
#     一律 block ⇒「没起服务」=「不能 push」，门禁就成了噪声，最后一定被绕过；
#   · 一律 skip ⇒ 145 / 144 上「服务挂了」这件事被静默放过（正是本闸要抓的）。
# 所以判据是「这台机器该不该有服务」，不是「服务此刻在不在」。
# 测试/演练可以显式覆盖：`METRICS_EVIDENCE=0|1`（0 = 当开发机，1 = 当部署机）。
#
# ========================= 地址解析顺序 =========================
#   METRICS_URL > NIK_METRICS_URL > NIK_SERVER_LISTEN(环境变量)
#     > env 文件里的 NIK_SERVER_LISTEN > backend/config/config.toml 的 [server].listen > 8023
#
# ⚠ 145 的实例监听 **127.0.0.1:8033**（`/etc/cloudpivot/cloudpivot-dev.env` 的
#   `NIK_SERVER_LISTEN`），不是 8023 —— 所以这里必须读 env 文件；但**只 grep 那一行，
#   绝不 source 整个文件**（里面是 `NIK_DATABASE_PASSWORD`，别顺手导进检查进程）。
#   监听 `0.0.0.0` / `[::]` 的实例，回环照样可达，统一打 `127.0.0.1`。
#
# ============================== 用法 ==============================
#   bash scripts/verify/metrics_gate.sh                # auto（check.sh --l0 就是这么调的）
#   bash scripts/verify/metrics_gate.sh block          # 强制真跑
#   SKIP_METRICS_CHECK=1 bash scripts/check.sh --l0    # 跳过
#   bash scripts/verify/metrics_gate.sh --self-test    # 判据自检（本地起假服务，8 种情形）
#
# 退出码：0 = 通过 / 跳过    1 = 不过（红）    2 = 用法错误
# ======================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SELF="${BASH_SOURCE[0]}"

# 模式：命令行参数优先，其次 METRICS_GATE（check.sh 传参也用同一个变量）
MODE="${1:-${METRICS_GATE:-auto}}"
SELF_TEST=0
case "$MODE" in
    --self-test) SELF_TEST=1 ;;
    -h|--help) sed -n '2,45p' "$SELF" | sed -E 's/^# ?//'; exit 0 ;;
    auto|block|warn|off) ;;
    *)
        echo "未知模式：$MODE（支持 auto / block / warn / off / --self-test）" >&2
        exit 2
        ;;
esac

# ----------------------------------------------------------------------
# 本机是不是「该有服务的机器」
# ----------------------------------------------------------------------
metrics_env_evidence() {
    local f
    case "${METRICS_EVIDENCE:-}" in
        1|true|yes) return 0 ;;
        0|false|no) return 1 ;;
    esac
    if [ "${CI:-}" = "1" ] || [ "${CI:-}" = "true" ]; then
        return 0
    fi
    for f in /etc/cloudpivot/cloudpivot.env /etc/cloudpivot/cloudpivot-dev.env \
             /etc/cloudpivot/cloudpivot-upg.env /etc/cloudpivot-base/base.env; do
        if [ -f "$f" ]; then
            return 0
        fi
    done
    if command -v systemctl >/dev/null 2>&1; then
        if systemctl list-units --all --type=service --no-legend 2>/dev/null | grep -q 'cloudpivot'; then
            return 0
        fi
    fi
    return 1
}

# ----------------------------------------------------------------------
# 把 listen 的各种写法（"8023" / "127.0.0.1:8033" / "0.0.0.0:8023" / "[::]:8023"）
# 归一成能 curl 的回环地址
# ----------------------------------------------------------------------
metrics_url() {
    local listen="${NIK_SERVER_LISTEN:-}" f port
    if [ -z "$listen" ]; then
        for f in /etc/cloudpivot/cloudpivot.env /etc/cloudpivot/cloudpivot-dev.env /etc/cloudpivot-base/base.env; do
            if [ -f "$f" ]; then
                # 只取那一行，不 source（文件里有库口令）
                listen="$(grep -m1 -E '^[[:space:]]*(export[[:space:]]+)?NIK_SERVER_LISTEN=' "$f" 2>/dev/null |
                    sed -E "s/^[^=]*=//; s/^[\"']//; s/[\"']\$//" || true)"
                if [ -n "$listen" ]; then
                    break
                fi
            fi
        done
    fi
    if [ -z "$listen" ] && [ -f "$ROOT/backend/config/config.toml" ]; then
        listen="$(awk -F'"' '
            /^\[server\]/ { insec = 1; next }
            /^\[/        { insec = 0 }
            insec && $1 ~ /^[[:space:]]*listen[[:space:]]*=/ { print $2; exit }
        ' "$ROOT/backend/config/config.toml")"
    fi
    listen="${listen:-8023}"
    port="${listen##*:}"           # "127.0.0.1:8033" / "[::]:8023" / "8023" 都落到最后一段
    case "$port" in ''|*[!0-9]*) port="8023" ;; esac
    printf 'http://127.0.0.1:%s/metrics\n' "$port"
}

# ----------------------------------------------------------------------
# 主判据
# ----------------------------------------------------------------------
metrics_gate() {
    local mode="$MODE"

    if [ "${SKIP_METRICS_CHECK:-}" = "1" ]; then
        echo "==> L0 /metrics 自检：SKIP_METRICS_CHECK=1 → 跳过"
        return 0
    fi
    case "$mode" in
        off)
            echo "==> L0 /metrics 自检：METRICS_GATE=off → 跳过"
            return 0
            ;;
        auto)
            if metrics_env_evidence; then mode="block"; else mode="skip"; fi
            ;;
        block|warn) ;;
    esac

    local url code out helps total
    url="${METRICS_URL:-${NIK_METRICS_URL:-$(metrics_url)}}"
    echo "==> L0 /metrics 自检（$url，模式 $mode）"

    if ! command -v curl >/dev/null 2>&1; then
        if [ "$mode" = "block" ]; then
            echo "  ✘ 本机没有 curl，无法自检 /metrics" >&2
            echo "    修法：装 curl（apt-get install -y curl）；CI 镜像必须带 curl" >&2
            return 1
        fi
        echo "  ℹ 本机没有 curl 且模式为 $mode → 跳过（装 curl 后才会真跑）"
        return 0
    fi

    out="$(mktemp "${TMPDIR:-/tmp}/cloudpivot_metrics.XXXXXX")"
    code="$(curl -s -o "$out" -w '%{http_code}' --max-time 5 "$url" 2>/dev/null || true)"
    code="${code:-000}"

    # ① 连不上
    if [ "$code" = "000" ]; then
        rm -f "$out"
        if [ "$mode" = "skip" ]; then
            echo "  ℹ 本机无部署证据（无 /etc/cloudpivot*/*.env、systemd 里无 cloudpivot 服务），且 $url 连不上 → 跳过"
            echo "    开发机没起服务属正常；要强制校验：METRICS_GATE=block bash scripts/check.sh --l0"
            return 0
        fi
        echo "  ✘ /metrics 连不上（curl 返回 000）：$url" >&2
        echo "    可能是：① 服务没起（systemctl status 'cloudpivot*' / 手工 cargo run）" >&2
        echo "             ② 端口不对（地址按 METRICS_URL > NIK_SERVER_LISTEN > env 文件 > config.toml 推出）" >&2
        echo "             ③ 回环被防火墙/代理挡了（检查 no_proxy 是否含 127.0.0.1）" >&2
        echo "    服务该在的机器上这条红就是对的：/metrics 是 alert_check.sh 的唯一数据源。" >&2
        if [ "$mode" = "warn" ]; then
            echo "  ⚠ METRICS_GATE=warn → 不阻断（问题仍需修）" >&2
            return 0
        fi
        return 1
    fi

    # ② 非 200
    if [ "$code" != "200" ]; then
        rm -f "$out"
        echo "  ✘ /metrics 返回 HTTP $code（期望 200）：$url" >&2
        case "$code" in
            404)
                echo "    ⇒ 端点没开：backend/config/config.toml 的 [metrics] enabled=true / path=\"/metrics\"；" >&2
                echo "      老版本二进制没有该端点（需重新构建部署）；启动日志应有「指标端点已启用」" >&2
                ;;
            403)
                echo "    ⇒ 来源被拒：[metrics] allow_ips 只允许回环（本检查走 127.0.0.1）；" >&2
                echo "      过 nginx 时 location = /metrics 必须清空 X-Forwarded-For" >&2
                ;;
            5??)
                echo "    ⇒ 服务内部错误，先看 journalctl -u 'cloudpivot*' 与后端日志" >&2
                ;;
            *)
                echo "    ⇒ 非预期状态码：确认 $url 后面确实是后端，而不是别的服务 / SPA 回退" >&2
                ;;
        esac
        if [ "$mode" = "warn" ]; then
            echo "  ⚠ METRICS_GATE=warn → 不阻断（问题仍需修）" >&2
            return 0
        fi
        return 1
    fi

    # ③ 200 但一条业务指标都没有（P1-05 的真实故障形态）
    helps="$(grep -c '^# HELP cloudpivot_' "$out" 2>/dev/null || true)"
    helps="${helps:-0}"
    total="$(wc -l < "$out" 2>/dev/null | tr -d ' ')"
    total="${total:-0}"
    if [ "$helps" -eq 0 ]; then
        rm -f "$out"
        echo "  ✘ /metrics 返回 200（$total 行），但没有一条 cloudpivot_ 指标" >&2
        echo "    ⇒ 名字/HELP 没登记：backend/app/src/shared/observability.rs 的 CATALOG；" >&2
        echo "    ⇒ 或采样器没跑：看 cloudpivot_metrics_sampler_last_success_timestamp 是否前进；" >&2
        echo "    ⇒ 只有 nik_* 序列 = 业务侧从未写入（「200 但是空的」比 404 更难发现，所以判红）" >&2
        if [ "$mode" = "warn" ]; then
            echo "  ⚠ METRICS_GATE=warn → 不阻断（问题仍需修）" >&2
            return 0
        fi
        return 1
    fi

    rm -f "$out"
    echo "  ✓ $url → 200，$helps 条 cloudpivot_ 指标（带 HELP），共 $total 行"
    return 0
}

# ----------------------------------------------------------------------
# 判据自检：本地起假 /metrics 服务，逐情形断言退出码与关键输出
# （不需要真后端 / 库 / 外网；与 check.sh --l0 用的是同一段判据代码）
# ----------------------------------------------------------------------
self_test() {
    local dir port dead_port pass=0 fail=0 out rc
    dir="$(mktemp -d "${TMPDIR:-/tmp}/p109_gate.XXXXXX")"
    port="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')"
    dead_port="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')"

    # 好样例：业务指标 + 框架指标
    {
        echo '# HELP cloudpivot_invariant_blockers 巡检 BLOCKER 数'
        echo '# TYPE cloudpivot_invariant_blockers gauge'
        echo 'cloudpivot_invariant_blockers 0'
        echo '# HELP nik_http_requests_total 框架请求数'
        echo '# TYPE nik_http_requests_total counter'
        echo 'nik_http_requests_total 3'
    } > "$dir/metrics"
    # 坏样例：200 但只有框架指标（P1-05 故障形态）
    {
        echo '# HELP nik_http_requests_total 框架请求数'
        echo '# TYPE nik_http_requests_total counter'
        echo 'nik_http_requests_total 7'
    } > "$dir/plain.metrics"

    python3 -m http.server "$port" --bind 127.0.0.1 --directory "$dir" >/dev/null 2>&1 &
    # 用全局变量承接：trap 在 self_test 返回之*后*才执行，local 那时已出作用域（set -u 会炸）
    SRV=$!
    DIR="$dir"
    trap 'kill "${SRV:-}" 2>/dev/null; rm -rf "${DIR:-}"' EXIT
    local i
    for i in $(seq 1 50); do
        if curl -s -o /dev/null --max-time 1 "http://127.0.0.1:$port/metrics" 2>/dev/null; then
            break
        fi
        sleep 0.1
    done

    # expect <期望rc> <说明> <期望在输出里出现的字样> -- env... args...
    expect() {
        local want="$1" desc="$2" needle="$3"
        shift 3
        out="$("$@" 2>&1)"
        rc=$?
        if [ "$rc" -ne "$want" ]; then
            echo "  ✗ $desc：期望 rc=$want，实得 rc=$rc"
            echo "$out" | sed 's/^/      /'
            fail=$((fail + 1))
            return 0
        fi
        if [ -n "$needle" ] && ! printf '%s' "$out" | grep -qF "$needle"; then
            echo "  ✗ $desc：输出里没有「$needle」"
            echo "$out" | sed 's/^/      /'
            fail=$((fail + 1))
            return 0
        fi
        echo "  ✓ $desc（rc=$rc）"
        pass=$((pass + 1))
    }

    echo "==> 判据自检（假服务 127.0.0.1:$port）"
    # 1. 200 + cloudpivot_ → 绿
    expect 0 "200 且有 cloudpivot_ 指标 → 通过" "✓" \
        env METRICS_URL="http://127.0.0.1:$port/metrics" bash "$SELF" block
    # 2. 200 但没有 cloudpivot_ → 红
    expect 1 "200 但只有 nik_* 指标 → 判红（P1-05 故障形态）" "没有一条 cloudpivot_" \
        env METRICS_URL="http://127.0.0.1:$port/plain.metrics" bash "$SELF" block
    # 3. 404 → 红，且提示端点没开
    expect 1 "404（端点没开）→ 判红并提示 [metrics]" "404" \
        env METRICS_URL="http://127.0.0.1:$port/nope" bash "$SELF" block
    # 4. 连不上 → 红（就是「故意停掉服务」的场景）
    expect 1 "服务没起（curl 000）→ 判红" "连不上" \
        env METRICS_URL="http://127.0.0.1:$dead_port/metrics" bash "$SELF" block
    # 5. 同一条连不上，auto + 无部署证据 → 跳过（开发机不能因此不能 push）
    expect 0 "auto + 无部署证据 + 连不上 → 跳过" "跳过" \
        env METRICS_EVIDENCE=0 METRICS_URL="http://127.0.0.1:$dead_port/metrics" bash "$SELF" auto
    # 6. auto + 有部署证据 → 与 block 同判（145 停服务必须红）
    expect 1 "auto + 有部署证据 + 连不上 → 判红（145 停服务场景）" "连不上" \
        env METRICS_EVIDENCE=1 METRICS_URL="http://127.0.0.1:$dead_port/metrics" bash "$SELF" auto
    # 7. warn → 不阻断但说清楚
    expect 0 "warn 模式 → 不阻断" "不阻断" \
        env METRICS_URL="http://127.0.0.1:$dead_port/metrics" bash "$SELF" warn
    # 8. SKIP_METRICS_CHECK=1 / off → 跳过
    expect 0 "SKIP_METRICS_CHECK=1 → 跳过" "跳过" \
        env SKIP_METRICS_CHECK=1 METRICS_URL="http://127.0.0.1:$dead_port/metrics" bash "$SELF" block
    expect 0 "METRICS_GATE=off → 跳过" "跳过" \
        env METRICS_URL="http://127.0.0.1:$dead_port/metrics" bash "$SELF" off
    # 9. 地址解析：只给 NIK_SERVER_LISTEN（模拟 145 的 env 文件那一行）也能拼对
    expect 0 "NIK_SERVER_LISTEN=127.0.0.1:<port> → 不靠 METRICS_URL 也能拼对地址" "✓" \
        env METRICS_EVIDENCE=1 NIK_SERVER_LISTEN="127.0.0.1:$port" bash "$SELF" block
    expect 0 "NIK_SERVER_LISTEN=<port>（纯端口）→ 拼成 127.0.0.1:<port>" "✓" \
        env METRICS_EVIDENCE=1 NIK_SERVER_LISTEN="$port" bash "$SELF" block
    expect 0 "NIK_SERVER_LISTEN=0.0.0.0:<port> → 归一成回环" "✓" \
        env METRICS_EVIDENCE=1 NIK_SERVER_LISTEN="0.0.0.0:$port" bash "$SELF" block

    echo ""
    if [ "$fail" -gt 0 ]; then
        echo "判据自检失败：$fail 条不通过（$pass 通过）"
        return 1
    fi
    echo "判据自检通过：$pass/$pass"
    return 0
}

if [ "$SELF_TEST" = "1" ]; then
    self_test
    exit $?
fi

metrics_gate
exit $?
