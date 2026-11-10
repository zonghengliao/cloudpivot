#!/usr/bin/env bash
# ============================================================================
# B3-2 空库全链重放：把全部迁移（含本卡 V0381）从零跑一遍
#
# 目的：证明 V0381 在**空库首启**路径上不出错（客户装机 / CI 全新环境走的就是这条路），
#       并且种子结果与设计一致：流程 1 条 PUBLISHED + 4 节点 / 3 边 / 2 审批人，
#       approval_mode 全是 ANY（不是全员会签）。
# 用法：bash 01_replay_fresh.sh
# 退出码：0 = 全部通过
# ============================================================================
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
MROOT="$ROOT/backend/migrations"
DBHOST="${DBHOST:-127.0.0.1}"; DBPORT="${DBPORT:-5432}"
DBUSER="${DBUSER:-alun}";      DBPASS="${DBPASS:-alun}"
DB="mig_costb32_fresh_$(date +%m%d%H%M%S)"
export PGPASSWORD="$DBPASS"
q() { psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "${2:-$DB}" -tAc "$1" 2>&1; }

FAILS=0
ok(){ printf '  ✓ %s\n' "$1"; }
bad(){ printf '  ✗ %s\n' "$1"; FAILS=$((FAILS+1)); }
eq(){ if [ "$2" = "$3" ]; then ok "$1（$2）"; else bad "$1：期望 [$3] 实际 [$2]"; fi; }

cleanup(){ su - postgres -c "dropdb --force $DB" >/dev/null 2>&1 || q "DROP DATABASE IF EXISTS $DB" postgres >/dev/null 2>&1; }
trap cleanup EXIT

su - postgres -c "createdb -O $DBUSER $DB" >/dev/null 2>&1 || { echo "FAIL: 建库失败"; exit 2; }
echo "==> 重放库：$DB"

n=0
while IFS=$'\t' read -r ver f; do
    if ! PGOPTIONS='-c client_min_messages=warning' \
         psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -q -X -v ON_ERROR_STOP=1 \
              --single-transaction -f "$f" >/dev/null 2>/tmp/b32_replay_err.log; then
        echo "FAIL: v$ver $(basename "$f")"
        grep -E "ERROR|FATAL" /tmp/b32_replay_err.log | head -5
        exit 2
    fi
    n=$((n + 1))
done < <(find "$MROOT" -name 'V*__*.up.sql' | awk -F'/V' '{split($NF,a,"__"); print a[1]+0 "\t" $0}' | sort -n -k1,1)
echo "  重放迁移数：$n 条"
eq "重放迁移数 301 条（B1-7 时 300，+1）" "$n" "301"

D='20000000-0000-4000-8000-0000000b0381'
V='20000000-0000-4000-8000-0000000b0382'
eq "流程定义 1 条且 PUBLISHED / is_default / 不免审" \
   "$(q "select count(*) from biz_flow_def where flow_code='cost_settle_approve' and status='PUBLISHED' and is_default and not allow_auto_approve and biz_type='COST_SETTLE' and deleted_at is null")" "1"
eq "def.current_version_id 指向本卡版本" \
   "$(q "select current_version_id::text from biz_flow_def where id='$D'")" "$V"
eq "版本 1 条：PUBLISHED / is_current / 4 节点 / 3 边" \
   "$(q "select count(*) from biz_flow_def_version where id='$V' and version_status='PUBLISHED' and is_current and node_count=4 and edge_count=3 and version_no=1")" "1"
eq "节点 4 条" "$(q "select count(*) from biz_flow_node where version_id='$V'")" "4"
eq "边 3 条"   "$(q "select count(*) from biz_flow_edge where version_id='$V'")" "3"
eq "审批人绑定 2 条（角色型）" \
   "$(q "select count(*) from biz_flow_node_assignee a join biz_flow_node n on n.id=a.node_id where n.version_id='$V' and a.assignee_type='ROLE'")" "2"
eq "两级审批 = 2 个 APPROVAL 节点" \
   "$(q "select count(*) from biz_flow_node where version_id='$V' and node_type='APPROVAL'")" "2"
eq "审批节点全是或签 ANY（全员会签 ALL 为 0 条）" \
   "$(q "select count(*) from biz_flow_node where version_id='$V' and node_type='APPROVAL' and approval_mode='ANY' and assignee_type='ROLE'")/$(q "select count(*) from biz_flow_node where version_id='$V' and approval_mode='ALL'")" "2/0"
eq "节点顺序：START→财务经理→财务总监→END" \
   "$(q "select string_agg(node_name, '→' order by sort_order) from biz_flow_node where version_id='$V'")" \
   "开始→财务经理审批→财务总监审批→结束"
eq "graph_hash = sha256(canonical(快照))（迁移自检 §6.4 已重算，这里核常量）" \
   "$(q "select graph_hash from biz_flow_def_version where id='$V'")" \
   "8a4fbd64cf5fb5cfc3b133ef2e4ff6103cd14a9afbabfae2b6f63f5845eb4171"
eq "COST_SETTLE 默认流程全库唯一（uk_biz_flow_def_default）" \
   "$(q "select count(*) from biz_flow_def where biz_type='COST_SETTLE' and is_default and deleted_at is null")" "1"

echo "=== 结果：$FAILS 条断言失败 ==="
[ "$FAILS" -eq 0 ] && exit 0 || exit 1
