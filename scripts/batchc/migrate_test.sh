#!/usr/bin/env bash
# ======================================================
# 批次 C · C2 迁移往返验证（范式沿用 B7 的 migrate_test.sh）
#
# 用法：
#   bash scripts/batchc/migrate_test.sh up        # 应用到测试库
#   bash scripts/batchc/migrate_test.sh down      # 执行 down 脚本 + 清 schema_migrations 行
#   bash scripts/batchc/migrate_test.sh verify    # 打印结构 / 索引 / 权限种子现状
#
# ── 为什么单独建库 cloudpivot_c2v ────────────────────────────────
# `cloudpivot_test` 是**多张卡共用**的验证库（B7 等卡的夹具在里面），
# 我在里面跑 down 会删掉别人正在用的表；`cloudpivot` / `cloudpivot_dev` 更不能碰。
# 所以本脚本永远只操作 C2_DB 指定的库（默认 cloudpivot_c2v）。
#
# ── 为什么 down 用 psql 执行 .down.sql 而不是 Migrator::rollback ──
# 框架的 rollback 会先撞上 `S__*.sql` 种子迁移（rerunnable、没有 down 脚本），
# 报 `migration 'xxx' has no down script`（B7 已踩）。本脚本的往返口径是：
#   ① psql 执行 V0087/V0081/V0080 的 down 脚本（**就是** rollback 会执行的那些文件）；
#   ② 删掉这三条 schema_migrations 记录（等价于 rollback 的簿记动作）；
#   ③ 再跑一次应用（up）→ 迁移重新应用成功 = 往返闭合。
# ======================================================
set -uo pipefail

ACTION="${1:-verify}"
C2_DB="${C2_DB:-cloudpivot_c2v}"
ENVF=/etc/cloudpivot/cloudpivot-test.env
RUN_DIR="${RUN_DIR:-/var/tmp/c2run}"
BIN="${BIN:-/var/tmp/c2-target/debug/cloudpivot}"
MIG_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/backend/migrations/business"

for k in ALUN_DATABASE_HOST ALUN_DATABASE_PORT ALUN_DATABASE_USER ALUN_DATABASE_PASSWORD \
         ALUN_DATABASE_SSL_MODE ALUN_MIDDLEWARE_AUTH_JWT_SECRET CLOUDPIVOT_STORAGE_MASTER_KEY \
         ALUN_ADMIN_PASSWORD ALUN_REDIS_URL; do
    v=$(grep -m1 "^$k=" "$ENVF" | cut -d= -f2-)
    if [ -n "${v:-}" ]; then export "$k=$v"; fi
done
export ALUN_PROFILE=test
export ALUN_DATABASE_NAME="$C2_DB"
export ALUN_SERVER_LISTEN=127.0.0.1:8029
export RUST_LOG=info

psql_c2() {
    PGPASSWORD="$ALUN_DATABASE_PASSWORD" psql -h "$ALUN_DATABASE_HOST" -U "$ALUN_DATABASE_USER" \
        -d "$C2_DB" -v ON_ERROR_STOP=1 "$@"
}

ensure_db() {
    if ! PGPASSWORD="$ALUN_DATABASE_PASSWORD" psql -h "$ALUN_DATABASE_HOST" -U "$ALUN_DATABASE_USER" \
        -lqt | cut -d'|' -f1 | grep -qw "$C2_DB"; then
        echo "==> 建库 $C2_DB（owner=cloudpivot）"
        su postgres -c "createdb -O cloudpivot $C2_DB"
    else
        echo "==> 库 $C2_DB 已存在"
    fi
}

run_app() {
    echo "==> 应用启动（profile=test db=$C2_DB :8029），让框架执行迁移"
    cd "$RUN_DIR" || exit 1
    timeout 60 "$BIN" 2>&1 | grep -E "迁移|migration|启动|ERROR" | head -25
}

case "$ACTION" in
    up)
        ensure_db
        run_app
        ;;
    down)
        echo "==> 执行 down 脚本（逆序：V0087 → V0081 → V0080）"
        psql_c2 -f "$MIG_DIR/V0087__seed_announcement_read_permission.down.sql"
        psql_c2 -f "$MIG_DIR/V0081__announcement_indexes.down.sql"
        psql_c2 -f "$MIG_DIR/V0080__create_announcement.down.sql"
        echo "==> 清 schema_migrations 记录（等价 rollback 簿记）"
        psql_c2 -c "DELETE FROM schema_migrations WHERE version IN ('80', '81', '87');"
        psql_c2 -c "SELECT version, name, success FROM schema_migrations ORDER BY version DESC LIMIT 3;"
        ;;
    verify)
        echo "==> 四张表"
        psql_c2 -c "SELECT table_name FROM information_schema.tables
                     WHERE table_schema='public' AND table_name LIKE 'sys_announcement%' ORDER BY 1;"
        echo "==> 7 个索引"
        psql_c2 -c "SELECT indexname FROM pg_indexes
                     WHERE schemaname='public' AND tablename LIKE 'sys_announcement%'
                       AND indexname NOT LIKE '%pkey%' AND indexname NOT LIKE 'uk_sys_announcement_scope%'
                       AND indexname NOT LIKE 'uk_sys_announcement_read%'
                       AND indexname NOT LIKE 'uk_sys_announcement_attachment%'
                     ORDER BY 1;"
        echo "==> 迁移记录（80/81/87）"
        psql_c2 -c "SELECT version, name, success FROM schema_migrations WHERE version IN ('80','81','87') ORDER BY version;"
        echo "==> 权限种子（读权限覆盖的角色数 / 未覆盖角色数）"
        psql_c2 -c "SELECT count(*) FILTER (WHERE granted) AS granted_roles,
                           count(*) FILTER (WHERE NOT granted) AS missing_roles
                      FROM (SELECT r.id, EXISTS (
                                  SELECT 1 FROM sys_role_menu rm
                                   WHERE rm.role_id = r.id
                                     AND rm.menu_id = '20000000-0000-0000-0000-000000000701'::uuid
                              ) AS granted
                              FROM sys_role r WHERE r.deleted_at IS NULL) t;"
        ;;
    *)
        echo "未知动作：$ACTION（up / down / verify）"
        exit 2
        ;;
esac
