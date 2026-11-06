#!/usr/bin/env bash
# 用云枢隔离库跑 pflow1a::publish_regression_keeps_graph_hash
# 目的：V0298 把 so_approve 标成 is_system=TRUE 后，它会进入该用例的「存量已发布流程」遍历。
#       该用例要求「另存草稿 → 重新发布」后 graph_hash 逐字节不变；若有漂移，这里会红。
set -uo pipefail
DB=${1:-cloudpivot_pflow1a}
export PGPASSWORD=alun
su_psql() { su postgres -s /bin/sh -c "psql -v ON_ERROR_STOP=1 -At $*"; }
su_psql "-c \"select pg_terminate_backend(pid) from pg_stat_activity where datname='$DB'\"" >/dev/null
su_psql "-c 'DROP DATABASE IF EXISTS $DB'" >/dev/null
su_psql "-c 'CREATE DATABASE $DB TEMPLATE cloudpivot_so23fresh OWNER alun'" >/dev/null || exit 1
echo "== $DB 建好（模板 cloudpivot_so23fresh，含 V0298）"

cd /opt/cloudpivot/backend || exit 1
PFLOW_DB_URL="postgres://alun:alun@127.0.0.1:5432/$DB" \
  timeout 900 cargo test --test pflow1a -- --test-threads=1 publish_regression 2>&1 | tail -25
