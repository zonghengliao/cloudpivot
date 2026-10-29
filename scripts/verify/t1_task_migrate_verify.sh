#!/usr/bin/env bash
# T1 迁移验收：空库全量 up → down 回滚 → 再 up 重放（逐条打印证据）
#
# 用法（在 172.18.10.144 上）：bash /root/t1_migrate_verify.sh
set -uo pipefail

DB=cloudpivot_t1m
PSQL="su postgres -c"
REPO=/opt/cloudpivot
MIG=$REPO/backend/migrations

q() { # q <sql>  → 单值输出（整数/文本）
    su postgres -c "psql -d $DB -tAc \"$1\""
}

hdr() { echo; echo "=================================================================="; echo "$*"; echo "=================================================================="; }

# ---------- 0) 空库 ----------
hdr "0) 建空库 $DB（DROP + CREATE，确保是空库）"
su postgres -c "psql -q -c 'DROP DATABASE IF EXISTS $DB'" >/dev/null 2>&1
su postgres -c "psql -q -c 'CREATE DATABASE $DB OWNER cloudpivot'"
echo "库已创建；当前业务表数 = $(q "SELECT count(*) FROM information_schema.tables WHERE table_schema='public'")"

# ---------- 1) up ----------
hdr "1) 全量 up（跑 V0001..V0103 + 种子）"
cd "$REPO/backend" || exit 1
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PATH=$HOME/.cargo/bin:$PATH
export ALUN_DATABASE_NAME=$DB
export ALUN_SERVER_LISTEN=127.0.0.1:8099
timeout 60 ./target/debug/cloudpivot >/tmp/t1m_up1.log 2>&1
echo "--- 迁移日志（尾部）---"
grep -iE "迁移|migrat" /tmp/t1m_up1.log | tail -6

hdr "1.1) 7 张表"
su postgres -c "psql -d $DB -c \"SELECT table_name FROM information_schema.tables WHERE table_schema='public' AND table_name IN ('biz_task','biz_task_member','biz_task_comment','biz_task_attachment','biz_task_dependency','biz_task_worklog','biz_project') ORDER BY 1\""

hdr "1.2) 索引（含部分索引）"
su postgres -c "psql -d $DB -c \"SELECT indexname FROM pg_indexes WHERE schemaname='public' AND tablename LIKE 'biz_%' ORDER BY 1\""

hdr "1.3) 部分索引谓词"
su postgres -c "psql -d $DB -tAc \"SELECT indexdef FROM pg_indexes WHERE indexname='idx_biz_task_due_open'\""

hdr "1.4) sys_user.user_type"
su postgres -c "psql -d $DB -c \"SELECT column_name, data_type, column_default FROM information_schema.columns WHERE table_name='sys_user' AND column_name='user_type'\""
echo "存量行 user_type 分布：$(q "SELECT string_agg(user_type||'='||n, ', ') FROM (SELECT user_type, count(*) AS n FROM sys_user GROUP BY 1) s")"

hdr "1.5) 权限种子（5 个菜单节点 + 全员授权）"
su postgres -c "psql -d $DB -c \"SELECT id, name, menu_type, auth_code, path, component FROM sys_menu WHERE id::text LIKE '20000000-0000-0000-0000-00000000076%' ORDER BY id\""
echo "未软删角色数 = $(q "SELECT count(*) FROM sys_role WHERE deleted_at IS NULL")"
echo "缺 business:task:* 授权的角色数（应为 0）= $(q "SELECT count(*) FROM sys_role r WHERE r.deleted_at IS NULL AND EXISTS (SELECT 1 FROM (VALUES ('20000000-0000-0000-0000-000000000761'::uuid),('20000000-0000-0000-0000-000000000762'::uuid),('20000000-0000-0000-0000-000000000763'::uuid),('20000000-0000-0000-0000-000000000764'::uuid)) b(id) WHERE NOT EXISTS (SELECT 1 FROM sys_role_menu rm WHERE rm.role_id=r.id AND rm.menu_id=b.id))")"

hdr "1.6) schema_migrations 里本卡的 4 条"
su postgres -c "psql -d $DB -c \"SELECT kind, name, version, source FROM schema_migrations WHERE version IN ('100','101','102','103') ORDER BY version\""

# ---------- 2) down ----------
hdr "2) down 回滚（V0103 → V0102 → V0101 → V0100，逆序）"
for f in V0103__seed_task_permissions V0102__add_sys_user_type V0101__task_indexes V0100__create_task_tables; do
    path=$(ls $MIG/*/$f.down.sql 2>/dev/null | head -1)
    echo "--- 执行 ${path#$REPO/} ---"
    su postgres -c "psql -v ON_ERROR_STOP=1 -q -d $DB -f $path" && echo "  OK"
done
su postgres -c "psql -q -d $DB -c \"DELETE FROM schema_migrations WHERE version IN ('100','101','102','103') OR name = 'task_permissions'\""
echo "已清除 4 条版本化迁移记录 + 1 条种子记录（task_permissions）"

hdr "2.1) 回滚后残留检查（全部应为 0）"
echo "biz_* 表数            = $(q "SELECT count(*) FROM information_schema.tables WHERE table_schema='public' AND table_name LIKE 'biz_%'")"
echo "biz_* 索引数          = $(q "SELECT count(*) FROM pg_indexes WHERE schemaname='public' AND tablename LIKE 'biz_%'")"
echo "sys_user.user_type 列 = $(q "SELECT count(*) FROM information_schema.columns WHERE table_name='sys_user' AND column_name='user_type'")"
echo "任务菜单节点数        = $(q "SELECT count(*) FROM sys_menu WHERE id::text LIKE '20000000-0000-0000-0000-00000000076%'")"
echo "任务菜单授权行数      = $(q "SELECT count(*) FROM sys_role_menu WHERE menu_id::text LIKE '20000000-0000-0000-0000-00000000076%'")"
echo "本卡迁移记录数        = $(q "SELECT count(*) FROM schema_migrations WHERE version IN ('100','101','102','103') OR name = 'task_permissions'")"

# ---------- 3) up 重放 ----------
hdr "3) 再 up 重放（同一空结构库，验证可重放）"
timeout 60 ./target/debug/cloudpivot >/tmp/t1m_up2.log 2>&1
grep -iE "迁移|migrat" /tmp/t1m_up2.log | tail -4
echo "biz_* 表数            = $(q "SELECT count(*) FROM information_schema.tables WHERE table_schema='public' AND table_name LIKE 'biz_%'")"
echo "biz_* 索引数          = $(q "SELECT count(*) FROM pg_indexes WHERE schemaname='public' AND tablename LIKE 'biz_%'")"
echo "sys_user.user_type 列 = $(q "SELECT count(*) FROM information_schema.columns WHERE table_name='sys_user' AND column_name='user_type'")"
echo "任务菜单节点数        = $(q "SELECT count(*) FROM sys_menu WHERE id::text LIKE '20000000-0000-0000-0000-00000000076%'")"
echo "本卡迁移记录数        = $(q "SELECT count(*) FROM schema_migrations WHERE version IN ('100','101','102','103')")"

hdr "3.1) 重放后表清单"
su postgres -c "psql -d $DB -c \"SELECT table_name FROM information_schema.tables WHERE table_schema='public' AND table_name LIKE 'biz_%' ORDER BY 1\""

echo
echo "T1 迁移验收完成"
