#!/usr/bin/env bash
# ============================================================================
# scripts/verify/hr/hr_db.sh —— HR 巡检脚本的「连接解析 + 演练库护栏」公共函数
#
# 被 hr_invariants.sh（生产可跑）与 hr_invariants_selftest.sh（造错自检，只许演练库）source。
# 抽出来的唯一理由：**两处必须用同一套解析规则**，否则「自检绿的库」和「巡检跑的库」
# 可能不是同一个 —— 那种假绿比没有自检更糟。
#
# 连接优先级（先到先用）：
#   ① HR_DB_URL      本模块专用（卡面口径，演练库用这个）
#   ② ERP_DB_URL     既有 ERP 真库用例的通用变量（HR-36/37/39 的 db_tests 也认它）
#   ③ NIK_DATABASE_{HOST,PORT,USER,PASSWORD,NAME}   服务自己用的那套
#   ④ 裸 PG* / 默认值（127.0.0.1:5432 / 当前用户 / 库名同用户）
# ============================================================================

# 打印逻辑库名（用于「是不是演练库」的判定）；连不上也不报错，只回显配置里的名字
hr_db_name() {
    if [ -n "${HR_DB_URL:-}" ]; then
        printf '%s' "${HR_DB_URL##*/}"
    elif [ -n "${ERP_DB_URL:-}" ]; then
        printf '%s' "${ERP_DB_URL##*/}"
    elif [ -n "${NIK_DATABASE_NAME:-}" ]; then
        printf '%s' "$NIK_DATABASE_NAME"
    elif [ -n "${PGDATABASE:-}" ]; then
        printf '%s' "$PGDATABASE"
    else
        printf '%s' "${USER:-$(id -un)}"
    fi
}

# psql 包装：自动带上解析出来的连接参数，其余参数原样透传
hr_psql() {
    local url="${HR_DB_URL:-${ERP_DB_URL:-}}"
    if [ -n "$url" ]; then
        psql "$url" -X "$@"
    else
        psql -X \
             -h "${NIK_DATABASE_HOST:-${PGHOST:-127.0.0.1}}" \
             -p "${NIK_DATABASE_PORT:-${PGPORT:-5432}}" \
             -U "${NIK_DATABASE_USER:-${PGUSER:-$(id -un)}}" \
             -d "${NIK_DATABASE_NAME:-${PGDATABASE:-$(id -un)}}" "$@"
    fi
}

# 口令：只从环境/配置推，**不接受命令行参数**（避免进 shell history）
hr_export_password() {
    if [ -n "${NIK_DATABASE_PASSWORD:-}" ]; then
        export PGPASSWORD="$NIK_DATABASE_PASSWORD"
    fi
}

# 演练库护栏：造错自检会**写数据**，绝不许在非演练库上跑
# 判据：库名含 drill / e2e / test（与卡面「演练库必须含 drill/e2e/test」一致）
hr_require_drill_db() {
    local db; db="$(hr_db_name)"
    case "$db" in
        *drill*|*e2e*|*test*|*TEST*|*Drill*|*E2E*) return 0 ;;
    esac
    echo "FATAL: 造错自检会写数据，只许在演练库上跑（库名需含 drill/e2e/test）。当前库名：$db" >&2
    echo "       正确做法：HR_DB_URL=postgres://user:pass@172.18.10.145:5432/hr45_drill bash $0" >&2
    return 2
}
