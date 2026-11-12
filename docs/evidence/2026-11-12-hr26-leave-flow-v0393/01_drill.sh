#!/usr/bin/env bash
# ============================================================================
# HR-26 真库演练：V0393__hr_leave_flow（请假审批流种子）
#
# 库建在 145（172.18.10.145），跑完 DROP。
#
# S0  基线（不含 V0393）        ：流程表在、没有 hr_leave
# S1  up                        ：1 def / 1 version(v1,is_current) / 5 节点 / 5 边 / 1 审批人
# S2  up 幂等                   ：行数不变 + graph_hash 不变
# S3  形态断言                  ：PUBLISHED / is_default / is_system / 不免审 / 或签 / 条件边
# S4  down                      ：全清
# S5  down 幂等                 ：第二遍零异常
# S6  回滚后重放                ：形态回到 S1
# S7  守卫①（同名 flow_code）  ：客户自建 → 整条跳过、客户行不被覆盖
# S8  守卫②（同 biz_type 默认）：客户自建默认流 → 整条跳过
# S9  守卫③（角色缺失）        ：软删 HR_MANAGER → 跳过；恢复后重跑成功
# S10 守卫④（版本递增）        ：已有「非本卡」版本 → 发 v2、不撞 uk、不抢 is_current
# S11 down 安全阀 c（本流程下还有别的版本）→ 拒绝回滚
# S12 收尾：down 干净、库 DROP
# ============================================================================
set -uo pipefail

ROOT=/opt/cloudpivot
MROOT=$ROOT/backend/migrations
UP=$MROOT/business/V0393__hr_leave_flow.up.sql
DOWN=$MROOT/business/V0393__hr_leave_flow.down.sql
DBHOST=${DBHOST:-172.18.10.145}
DBUSER=${DBUSER:-cloudpivot}
DB=${DB:-hr26_leave_flow_drill}
HASH=4fc6722abf629a8a9a84580942dd2f6a0a44431786b2e500905531affc0e1f97
D=20000000-0000-4000-8000-0000000e0393
V=20000000-0000-4000-8000-0000000e0394
HR=20000000-0000-4000-8000-0000000f0016

q()  { psql -h "$DBHOST" -U "$DBUSER" -d "$DB" -tAc "$1" 2>&1; }
adm(){ psql -h "$DBHOST" -U "$DBUSER" -d postgres -tAc "$1" 2>&1; }
qf() { psql -h "$DBHOST" -U "$DBUSER" -d "$DB" -q -X -v ON_ERROR_STOP=1 --single-transaction -f "$1" 2>&1; }
run_up(){ qf "$UP"; }
run_down(){ qf "$DOWN"; }

FAILS=0
ok(){  printf '  \u2713 %s\n' "$1"; }
bad(){ printf '  \u2717 %s\n' "$1"; FAILS=$((FAILS+1)); }
eq(){ if [ "$2" = "$3" ]; then ok "$1（$2）"; else bad "$1：期望 [$3] 实际 [$2]"; fi; }
has(){ case "$2" in *"$3"*) ok "$1";; *) bad "$1：输出里没有 [$3]（实际：$(printf '%s' "$2" | tr '\n' ' ' | cut -c1-200)）";; esac; }
hasnt(){ case "$2" in *"$3"*) bad "$1：输出里不该有 [$3]";; *) ok "$1";; esac; }

cleanup(){ adm "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1; }
trap cleanup EXIT

echo "==> 建演练库 $DB @ $DBHOST（先 DROP 残留）"
cleanup
adm "CREATE DATABASE $DB" >/dev/null 2>&1 || { echo "FAIL: 建库失败"; exit 2; }

echo "==> S0 回放 V0393 之前的全部迁移（按版本号全局升序）"
n=0; failed=0
while IFS=$' \t' read -r ver f; do
    if ! qf "$f" >/dev/null 2>/tmp/hr26_mig_err.log; then
        echo "FAIL: 基线迁移 v$ver $(basename "$f")"; grep -E "ERROR|FATAL" /tmp/hr26_mig_err.log | head -3; failed=1; break
    fi
    n=$((n+1))
done < <(ls "$MROOT"/*/*.up.sql | sed -E 's#.*/V([0-9]{4})__.*#\1 &#' | sort -n | awk -v limit=393 '$1 < limit')
[ "$failed" = 1 ] && exit 2
echo "    基线迁移 $n 条完成"
eq "S0 流程表在、无 hr_leave" "$(q "SELECT count(*) FROM biz_flow_def WHERE flow_code='hr_leave'")" "0"

echo "==> S1 应用 V0393（up）"
OUT=$(run_up); echo "$OUT" | grep -E "NOTICE|ERROR" | sed 's/^/    /'
has "S1 自检 NOTICE 通过"  "$OUT" "HR-26 自检通过"
hasnt "S1 无 ERROR"        "$OUT" "ERROR"
eq "S1 def 1 行 PUBLISHED+默认+系统" "$(q "SELECT count(*) FROM biz_flow_def WHERE id='$D' AND status='PUBLISHED' AND is_default AND is_system AND NOT allow_auto_approve")" "1"
eq "S1 version v1 is_current" "$(q "SELECT version_no||'/'||version_status||'/'||is_current||'/'||node_count||'/'||edge_count FROM biz_flow_def_version WHERE id='$V'")" "1/PUBLISHED/true/5/5"
eq "S1 def 指针指向本卡版本" "$(q "SELECT current_version_id FROM biz_flow_def WHERE id='$D'")" "$V"
eq "S1 graph_hash 列值" "$(q "SELECT graph_hash FROM biz_flow_def_version WHERE id='$V'")" "$HASH"
eq "S1 节点 5 个" "$(q "SELECT count(*) FROM biz_flow_node WHERE version_id='$V'")" "5"
eq "S1 边 5 条" "$(q "SELECT count(*) FROM biz_flow_edge WHERE version_id='$V'")" "5"
eq "S1 审批人绑定 1 条（n_hr→HR_MANAGER）" "$(q "SELECT count(*) FROM biz_flow_node_assignee a JOIN biz_flow_node n ON n.id=a.node_id WHERE n.version_id='$V' AND n.node_key='n_hr' AND a.ref_id='$HR'")" "1"
eq "S1 直属上级 = INITIATOR_SUPERIOR(levels=1) 且无枚举行" "$(q "SELECT n.assignee_type||'/'||n.superior_levels||'/'||(SELECT count(*) FROM biz_flow_node_assignee a WHERE a.node_id=n.id) FROM biz_flow_node n WHERE n.version_id='$V' AND n.node_key='n_sup'")" "INITIATOR_SUPERIOR/1/0"
eq "S1 两个审批节点都 ANY + FALLBACK_ADMIN + REMIND" "$(q "SELECT count(*) FROM biz_flow_node WHERE version_id='$V' AND node_type='APPROVAL' AND approval_mode='ANY' AND empty_assignee='FALLBACK_ADMIN' AND timeout_action='REMIND'")" "2"
eq "S1 没有 ALL 审批节点" "$(q "SELECT count(*) FROM biz_flow_node WHERE version_id='$V' AND approval_mode='ALL'")" "0"
eq "S1 条件边（gte 3 on duration_days_num）" "$(q "SELECT condition_expr->>'field'||'/'||(condition_expr->>'cmp')||'/'||(condition_expr->>'value') FROM biz_flow_edge WHERE version_id='$V' AND edge_key='e_long'")" "form.duration_days_num/gte/3"
eq "S1 兜底边恰 1 条（e_short）" "$(q "SELECT count(*)||'/'||max(edge_key) FROM biz_flow_edge WHERE version_id='$V' AND is_default")" "1/e_short"
eq "S1 链路 5 条骨架" "$(q "SELECT count(*) FROM biz_flow_edge e JOIN biz_flow_node f ON f.id=e.from_node_id JOIN biz_flow_node t ON t.id=e.to_node_id WHERE e.version_id='$V' AND ((f.node_key,t.node_key) IN (('s','n_sup'),('n_sup','c_days'),('c_days','n_hr'),('c_days','e'),('n_hr','e')))")" "5"

echo "==> S2 up 幂等"
FP1=$(q "SELECT (SELECT count(*) FROM biz_flow_def)||'/'||(SELECT count(*) FROM biz_flow_def_version)||'/'||(SELECT count(*) FROM biz_flow_node)||'/'||(SELECT count(*) FROM biz_flow_edge)||'/'||(SELECT count(*) FROM biz_flow_node_assignee)||'/'||(SELECT string_agg(graph_hash,E',') FROM biz_flow_def_version)")
OUT=$(run_up)
eq "S2 重放后指纹不变" "$(q "SELECT (SELECT count(*) FROM biz_flow_def)||'/'||(SELECT count(*) FROM biz_flow_def_version)||'/'||(SELECT count(*) FROM biz_flow_node)||'/'||(SELECT count(*) FROM biz_flow_edge)||'/'||(SELECT count(*) FROM biz_flow_node_assignee)||'/'||(SELECT string_agg(graph_hash,E',') FROM biz_flow_def_version)")" "$FP1"
has "S2 重放后自检仍通过（未重复插版本）" "$OUT" "HR-26 自检通过"
eq "S2 本卡版本仍只有 1 个" "$(q "SELECT count(*) FROM biz_flow_def_version WHERE def_id='$D' AND change_note LIKE 'V0393 种子%'")" "1"

echo "==> S3 快照 hash 库内重算对账"
has "S3 canonical 重算 == graph_hash" "$(q "CREATE OR REPLACE FUNCTION pg_temp.cj(v jsonb) RETURNS text LANGUAGE sql IMMUTABLE AS \$fn\$ SELECT CASE jsonb_typeof(v) WHEN 'object' THEN '{'||COALESCE((SELECT string_agg(to_jsonb(k)::text||':'||pg_temp.cj(val), ',' ORDER BY k COLLATE \"C\") FROM jsonb_each(v) AS e(k,val)),'')||'}' WHEN 'array' THEN '['||COALESCE((SELECT string_agg(pg_temp.cj(a),',' ) FROM jsonb_array_elements(v) AS x(a)),'')||']' ELSE v::text END \$fn\$; SELECT encode(sha256(convert_to(pg_temp.cj(graph_snapshot),'UTF8')),'hex') FROM biz_flow_def_version WHERE id='$V'")" "$HASH"

echo "==> S4/S5/S6 down → down 幂等 → 重放"
OUT=$(run_down); has "S4 down 安全阀通过" "$OUT" "安全阀全部通过"
hasnt "S4 down 无 ERROR" "$OUT" "ERROR"
eq "S4 定义已删" "$(q "SELECT count(*) FROM biz_flow_def WHERE id='$D'")" "0"
eq "S4 版本/节点/边/审批人全清" "$(q "SELECT (SELECT count(*) FROM biz_flow_def_version WHERE id='$V')||'/'||(SELECT count(*) FROM biz_flow_node WHERE version_id='$V')||'/'||(SELECT count(*) FROM biz_flow_edge WHERE version_id='$V')||'/'||(SELECT count(*) FROM biz_flow_node_assignee WHERE node_id IN (SELECT id FROM biz_flow_node WHERE version_id='$V'))")" "0/0/0/0"
OUT=$(run_down); hasnt "S5 down 幂等（第二遍零异常）" "$OUT" "ERROR"
OUT=$(run_up); has "S6 重放后自检通过" "$OUT" "HR-26 自检通过"
eq "S6 重放后回到 1 def / 1 version / 5 节点 / 5 边" "$(q "SELECT (SELECT count(*) FROM biz_flow_def WHERE id='$D')||'/'||(SELECT count(*) FROM biz_flow_def_version WHERE def_id='$D')||'/'||(SELECT count(*) FROM biz_flow_node WHERE version_id='$V')||'/'||(SELECT count(*) FROM biz_flow_edge WHERE version_id='$V')")" "1/1/5/5"

echo "==> S7 守卫①：客户自建同名 flow_code"
OUT=$(run_down); [ -n "$(printf '%s' "$OUT" | grep ERROR)" ] && bad "S7 前置 down 失败：$OUT"
q "INSERT INTO biz_flow_def (id, flow_code, flow_name, biz_type, category, status, current_version_id, current_version_no, is_default, allow_auto_approve) VALUES ('11111111-1111-4111-8111-111111111111','hr_leave','客户自建请假流','HR_LEAVE','人力资源','DRAFT',NULL,0,FALSE,FALSE)" >/dev/null
OUT=$(run_up)
has "S7 守卫①命中（提示已存在同名流程）" "$OUT" "守卫①跳过"
eq "S7 客户行未被覆盖（名字还是客户的）" "$(q "SELECT flow_name FROM biz_flow_def WHERE flow_code='hr_leave' AND deleted_at IS NULL")" "客户自建请假流"
eq "S7 没有种出本卡的 def" "$(q "SELECT count(*) FROM biz_flow_def WHERE id='$D'")" "0"
q "DELETE FROM biz_flow_def WHERE id='11111111-1111-4111-8111-111111111111'" >/dev/null

echo "==> S8 守卫②：同 biz_type 已有默认流"
q "INSERT INTO biz_flow_def (id, flow_code, flow_name, biz_type, category, status, current_version_id, current_version_no, is_default, allow_auto_approve) VALUES ('22222222-2222-4222-8222-222222222222','hr_leave_custom','客户默认请假流','HR_LEAVE','人力资源','DRAFT',NULL,0,TRUE,FALSE)" >/dev/null
OUT=$(run_up)
has "S8 守卫②命中（同 biz_type 已有默认流）" "$OUT" "守卫②跳过"
eq "S8 没有种出本卡的 def" "$(q "SELECT count(*) FROM biz_flow_def WHERE id='$D'")" "0"
q "DELETE FROM biz_flow_def WHERE id='22222222-2222-4222-8222-222222222222'" >/dev/null

echo "==> S9 守卫③：审批角色缺失"
q "UPDATE sys_role SET deleted_at = now() WHERE id='$HR'" >/dev/null
OUT=$(run_up)
has "S9 守卫③命中（角色不齐 → 不种）" "$OUT" "守卫③跳过"
eq "S9 没有种出本卡的 def" "$(q "SELECT count(*) FROM biz_flow_def WHERE id='$D'")" "0"
q "UPDATE sys_role SET deleted_at = NULL WHERE id='$HR'" >/dev/null
OUT=$(run_up); has "S9 角色恢复后重跑成功" "$OUT" "HR-26 自检通过"

echo "==> S10 守卫④：版本递增（已有非本卡版本）"
# 造一个「客户自建」的 v1：删掉本卡版本与子行（保留 def），插一版 DRAFT 且 is_current
q "DELETE FROM biz_flow_node_assignee WHERE node_id IN (SELECT id FROM biz_flow_node WHERE version_id='$V')" >/dev/null
q "DELETE FROM biz_flow_edge WHERE version_id='$V'" >/dev/null
q "DELETE FROM biz_flow_node WHERE version_id='$V'" >/dev/null
q "DELETE FROM biz_flow_def_version WHERE id='$V'" >/dev/null
q "INSERT INTO biz_flow_def_version (id, def_id, version_no, version_status, is_current, node_count, edge_count, change_note) VALUES ('33333333-3333-4333-8333-333333333333','$D',1,'DRAFT',FALSE,0,0,'客户自建 v1')" >/dev/null
q "UPDATE biz_flow_def SET current_version_id='33333333-3333-4333-8333-333333333333', current_version_no=1 WHERE id='$D'" >/dev/null
OUT=$(run_up)
hasnt "S10 未撞唯一索引（无 ERROR）" "$OUT" "ERROR"
eq "S10 发的是 v2 而不是 v1" "$(q "SELECT version_no FROM biz_flow_def_version WHERE def_id='$D' AND id='$V'")" "2"
eq "S10 新版本 is_current = true（客户那版本来就不是 current）" "$(q "SELECT is_current FROM biz_flow_def_version WHERE id='$V'")" "t"
eq "S10 不抢客户版本：def 指针仍是客户那版" "$(q "SELECT current_version_id FROM biz_flow_def WHERE id='$D'")" "33333333-3333-4333-8333-333333333333"
eq "S10 本卡版本只多了这一份（v2）" "$(q "SELECT count(*) FROM biz_flow_def_version WHERE def_id='$D' AND change_note LIKE 'V0393 种子%'")" "1"

echo "==> S11 down 安全阀 c：本流程下还有别的版本 → 拒绝回滚"
OUT=$(run_down)
has "S11 down 被安全阀 c 拦下" "$OUT" "还有 1 个非本卡版本"
q "DELETE FROM biz_flow_def_version WHERE id='33333333-3333-4333-8333-333333333333'" >/dev/null
q "UPDATE biz_flow_def SET current_version_id='$V', current_version_no=2 WHERE id='$D'" >/dev/null
OUT=$(run_down); has "S11 清掉客户版本后 down 成功" "$OUT" "安全阀全部通过"
eq "S11 down 后定义已删" "$(q "SELECT count(*) FROM biz_flow_def WHERE id='$D'")" "0"

echo "==> S12 收尾"
cleanup
eq "S12 演练库已 DROP" "$(adm "SELECT count(*) FROM pg_database WHERE datname='$DB'")" "0"

echo
if [ "$FAILS" = 0 ]; then echo "ALL PASS（0 失败）"; exit 0; else echo "FAILED: $FAILS 项"; exit 1; fi
