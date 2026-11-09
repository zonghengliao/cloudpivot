#!/usr/bin/env bash
# =============================================================================
# flow_critical_edges.sh — 主链路「必需边」只读巡检（卡 t_c99b63cd / E2E-FIX-1）
#
# 用法：
#   bash scripts/verify/flow_critical_edges.sh [dbname]
#       dbname 省略时取 $NIK_DATABASE_NAME，再退回 cloudpivot_dev
#   连接参数：PGHOST / PGPORT / PGUSER / PGPASSWORD，未设时回落 NIK_DATABASE_*
#
# 退出码：
#   0 = 主链路边全部可用（绿）
#   1 = 有主链路边被禁用（红，明细已打印）
#   2 = 连库 / 查询失败（基础设施问题，不是业务红）
#   4 = biz_flow_critical_edge 表不存在（V0354 未跑：**不许假装绿**）
#
# # 为什么需要这个巡检
#
#   2026-11-09 03:34（E2E-02 演练期间）`biz_flow_state_edge` 里
#   `STOCK_OUT DRAFT→POSTED` 被置成 `enabled = false`（属主不明）：
#     · 发货过账 409「流转 DRAFT → POSTED 已被状态机配置禁用」
#     · **整条销售链断在发货**，演练的人现场手工改库才继续
#     · 没有任何检查发现它 —— 配置页要人主动点进去一条条看才看得见
#
#   本脚本只干一件事：拿 `biz_flow_critical_edge`（V0354 种进库的主链白名单，
#   与 `state_machine::CRITICAL_EDGES` 逐行一致，真库单测比对）去比对
#   `biz_flow_state_edge`，**任何一条 critical 边存在禁用行就报警**。
#
# # 刻意不做的事
#
#   · **不自动改回 `enabled = true`**：禁用可能是人工加严（例如某月临时关掉
#     「免审直过」逼着走审批），自动改回会把人的决定悄悄推翻。报警 + 人来判断。
#   · **不判非 critical 边**：那些边本来就能禁用（`mandatory` 边在 API 层就禁不掉，
#     只有绕过 API 直接写库才会出现，运行时也会忽略 + WARN），这里只打印一行提示。
#   · **不写库**：整脚本只有 SELECT（生产 / 只读副本都能跑）。
#
# # 手工验证脚本有效（**只在测试库**做，做完立刻复原；生产只读，别注入）
#
#   psql ... -c "update biz_flow_state_edge set enabled=false
#                 where entity='STOCK_OUT' and from_status='DRAFT' and to_status='POSTED';"
#   bash scripts/verify/flow_critical_edges.sh <db>; echo rc=$?   # 期望 rc=1，明细点名 STOCK_OUT
#   psql ... -c "update biz_flow_state_edge set enabled=true
#                 where entity='STOCK_OUT' and from_status='DRAFT' and to_status='POSTED';"
#   bash scripts/verify/flow_critical_edges.sh <db>; echo rc=$?   # 期望 rc=0
# =============================================================================
set -uo pipefail

DB="${1:-${NIK_DATABASE_NAME:-cloudpivot_dev}}"
PGHOST="${PGHOST:-${NIK_DATABASE_HOST:-127.0.0.1}}"
PGPORT="${PGPORT:-${NIK_DATABASE_PORT:-5432}}"
PGUSER="${PGUSER:-${NIK_DATABASE_USER:-cloudpivot}}"
PGPASSWORD="${PGPASSWORD:-${NIK_DATABASE_PASSWORD:-}}"
export PGPASSWORD PGHOST PGPORT PGUSER

# ⚠ locale 必须钉死：本机 psql 是 Debian pg_wrapper（Perl），调用方带着目标机没装的
#   locale 时会往 stderr 刷 "Setting locale failed…"，被 2>&1 并进结果就会把
#   「连得上库」误判成 rc=2（menu_invariants.sh 2026-11-08 踩过同一个坑）。
export LC_ALL=C LANG=C PERL_BADLANG=0

q() { psql -q -t -A -F'|' -d "$DB" -c "$1"; }

echo "==> 库：$DB（$PGUSER@$PGHOST:$PGPORT）"

# ① 表在不在（V0354 未跑 → rc=4，不假装绿）
TABLE_OK="$(q "SELECT to_regclass('public.biz_flow_critical_edge') IS NOT NULL;" 2>/dev/null)"
RC_CONN=$?
if [ "$RC_CONN" -ne 0 ]; then
    echo "✗ 连库失败（psql rc=$RC_CONN）：检查 PGHOST/PGUSER/PGPASSWORD 与库名" >&2
    exit 2
fi
case "$TABLE_OK" in
    t) ;;
    *)
        echo "✗ biz_flow_critical_edge 不存在：该库还没跑 V0354__flow_critical_edge_whitelist" >&2
        echo "  → 主链路白名单未知，巡检无法判断，按「未部署」处理（rc=4）" >&2
        exit 4
        ;;
esac

TOTAL="$(q "SELECT count(*) FROM biz_flow_critical_edge;" 2>/dev/null)"
[ -z "$TOTAL" ] && { echo "✗ 读 biz_flow_critical_edge 失败" >&2; exit 2; }
echo "    主链路白名单：$TOTAL 条（真相在 backend/src/business/shared/state_machine.rs::CRITICAL_EDGES）"

# ② 被禁用的主链路边（唯一判红条件）
DISABLED="$(q "
    SELECT c.entity || ' ' || c.from_status || '→' || c.to_status || ' | ' || c.impact
      FROM biz_flow_critical_edge c
      JOIN biz_flow_state_edge e
        ON e.entity = c.entity
       AND e.from_status = c.from_status
       AND e.to_status = c.to_status
       AND e.deleted_at IS NULL
     WHERE e.enabled = false
     ORDER BY c.entity, c.from_status, c.to_status;" 2>/dev/null)"

# ③ 提示项：非主链路边被禁用（合法动作，只打印不判红）
OTHER_DISABLED="$(q "
    SELECT e.entity || ' ' || e.from_status || '→' || e.to_status
      FROM biz_flow_state_edge e
     WHERE e.enabled = false
       AND e.deleted_at IS NULL
       AND NOT EXISTS (
           SELECT 1 FROM biz_flow_critical_edge c
            WHERE c.entity = e.entity
              AND c.from_status = e.from_status
              AND c.to_status = e.to_status)
     ORDER BY e.entity, e.from_status, e.to_status;" 2>/dev/null)"

if [ -n "$OTHER_DISABLED" ]; then
    echo "    提示：以下非主链路边被禁用（合法配置，不判红）："
    printf '%s\n' "$OTHER_DISABLED" | sed 's/^/      · /'
fi

if [ -n "$DISABLED" ]; then
    echo ""
    echo "✗ 主链路边被禁用（功能会断）："
    printf '%s\n' "$DISABLED" | while IFS= read -r line; do
        edge="${line%% | *}"
        impact="${line#* | }"
        printf '  · %s 被禁用 → %s\n' "$edge" "$impact"
    done
    echo ""
    echo "  处置：确认不是误操作后恢复（**本脚本不会自动改回**）："
    printf '%s\n' "$DISABLED" | while IFS= read -r line; do
        edge="${line%% | *}"
        ent="${edge%% *}"
        rest="${edge#* }"
        pair="${rest%% *}"
        from="${pair%%→*}"
        to="${pair##*→}"
        printf "    UPDATE biz_flow_state_edge SET enabled = true, updated_at = now()\n     WHERE entity = '%s' AND from_status = '%s' AND to_status = '%s' AND deleted_at IS NULL;\n" \
            "$ent" "$from" "$to"
    done
    echo ""
    echo "  为什么不能自动改回：禁用可能是人工加严（临时关掉免审直过），自动改回会推翻人的决定。"
    exit 1
fi

echo "✓ 主链路边全部可用（$TOTAL 条白名单，0 条被禁用）"
exit 0
