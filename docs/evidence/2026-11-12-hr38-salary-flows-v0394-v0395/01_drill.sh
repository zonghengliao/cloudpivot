#!/usr/bin/env bash
# ============================================================================
# HR-38 真库演练：V0394__hr_salary_flow + V0395__hr_adjust_resign_flow
#                 （薪资单 / 调薪 / 离职 三套审批流种子）
#
# 库建在 145（172.18.10.145），跑完 DROP。
#
# S0  基线（不含 V0394/V0395）：流程表在、三套流程一条都没有
# S1  V0394 up            ：1 def / 1 version(v1,is_current) / 6 节点 / 6 边 / 4 审批人
# S2  V0394 up 幂等       ：指纹（行数+hash）不变
# S3  V0394 hash 库内重算 ：canonical(快照) 的 sha256 == 列值 == 迁移常量
# S4  V0395 up            ：调薪 5/4 + 离职 6/6，两条自检 NOTICE 都出现
# S5  V0395 up 幂等       ：指纹不变
# S6  V0395 hash 库内重算 ：两套各自对账
# S7  down 两套 → 幂等 → 重放：全清 → 零异常 → 回到 S1/S4 形态
# S8  守卫①（同名 flow_code）：客户自建 → 只跳过那套、另一套照种、客户行不被覆盖
# S9  守卫②（同 biz_type 默认流）：同上
# S10 守卫③（角色缺失）  ：软删 GENERAL_MANAGER → 离职跳过、调薪照种；恢复后重跑成功
# S11 守卫④（版本递增）  ：已有「非本卡」v1 → 发 v2、不撞 uk、不抢 is_current
# S12 down 安全阀（本流程下还有别的版本）→ 拒绝回滚
# S13 收尾：down 干净、库 DROP
# ============================================================================
set -uo pipefail

ROOT=/opt/cloudpivot
MROOT=$ROOT/backend/migrations
UP_SAL=$MROOT/business/V0394__hr_salary_flow.up.sql
DOWN_SAL=$MROOT/business/V0394__hr_salary_flow.down.sql
UP_ADJ=$MROOT/business/V0395__hr_adjust_resign_flow.up.sql
DOWN_ADJ=$MROOT/business/V0395__hr_adjust_resign_flow.down.sql
DBHOST=${DBHOST:-172.18.10.145}
DBUSER=${DBUSER:-cloudpivot}
DB=${DB:-hr38_flow_drill}

HASH_SAL=0a296af0477bd307f7d7687b447d50f2f813da9eeef1bbe57711b1e8a09bd5d0
HASH_ADJ=a413971a5a5de2bdb11798bfbfa140d927573e541cf4fd7e4f99a99dd6a99171
HASH_RES=ec42328b209441d5ea6e403f8bfaef9b8d5dc13f5a83ec1319a36d9864558496
D_SAL=20000000-0000-4000-8000-0000000e1401; V_SAL=20000000-0000-4000-8000-0000000e1402
D_ADJ=20000000-0000-4000-8000-0000000e1501; V_ADJ=20000000-0000-4000-8000-0000000e1502
D_RES=20000000-0000-4000-8000-0000000e1531; V_RES=20000000-0000-4000-8000-0000000e1532
HR=20000000-0000-4000-8000-0000000f0016
FIN=20000000-0000-4000-8000-0000000f0005
FIND=20000000-0000-4000-8000-0000000f0006
GM=20000000-0000-4000-8000-0000000f0003

q()  { psql -h "$DBHOST" -U "$DBUSER" -d "$DB" -tAc "$1" 2>&1; }
adm(){ psql -h "$DBHOST" -U "$DBUSER" -d postgres -tAc "$1" 2>&1; }
qf() { psql -h "$DBHOST" -U "$DBUSER" -d "$DB" -q -X -v ON_ERROR_STOP=1 --single-transaction -f "$1" 2>&1; }
run_up_sal(){ qf "$UP_SAL"; }
run_down_sal(){ qf "$DOWN_SAL"; }
run_up_adj(){ qf "$UP_ADJ"; }
run_down_adj(){ qf "$DOWN_ADJ"; }

FAILS=0
ok(){  printf '  ✓ %s\n' "$1"; }
bad(){ printf '  ✗ %s\n' "$1"; FAILS=$((FAILS+1)); }
eq(){ if [ "$2" = "$3" ]; then ok "$1（$2）"; else bad "$1：期望 [$3] 实际 [$2]"; fi; }
has(){ case "$2" in *"$3"*) ok "$1";; *) bad "$1：输出里没有 [$3]（实际：$(printf '%s' "$2" | tr '\n' ' ' | cut -c1-260)）";; esac; }
hasnt(){ case "$2" in *"$3"*) bad "$1：输出里不该有 [$3]";; *) ok "$1";; esac; }

CANON="CREATE OR REPLACE FUNCTION pg_temp.cj(v jsonb) RETURNS text LANGUAGE sql IMMUTABLE AS \$fn\$ SELECT CASE jsonb_typeof(v) WHEN 'object' THEN '{'||COALESCE((SELECT string_agg(to_jsonb(k)::text||':'||pg_temp.cj(val), ',' ORDER BY k COLLATE \"C\") FROM jsonb_each(v) AS e(k,val)),'')||'}' WHEN 'array' THEN '['||COALESCE((SELECT string_agg(pg_temp.cj(a),',') FROM jsonb_array_elements(v) AS x(a)),'')||']' ELSE v::text END \$fn\$;"
# psql 每次 -c 都是独立会话（pg_temp 不跨会话），故把建函数与取 hash 放在同一次调用里，
# 再用 tail -1 丢掉 `CREATE FUNCTION` 的命令标签行。
rehash(){ q "$CANON SELECT encode(sha256(convert_to(pg_temp.cj(graph_snapshot),'UTF8')),'hex') FROM biz_flow_def_version WHERE id='$1'" | tail -1; }
fingerprint(){ q "SELECT (SELECT count(*) FROM biz_flow_def)||'/'||(SELECT count(*) FROM biz_flow_def_version)||'/'||(SELECT count(*) FROM biz_flow_node)||'/'||(SELECT count(*) FROM biz_flow_edge)||'/'||(SELECT count(*) FROM biz_flow_node_assignee)||'/'||(SELECT COALESCE(string_agg(graph_hash,',' ORDER BY graph_hash),'') FROM biz_flow_def_version)"; }

cleanup(){ adm "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1; }
trap cleanup EXIT

echo "==> 建演练库 $DB @ $DBHOST（先 DROP 残留）"
cleanup
adm "CREATE DATABASE $DB" >/dev/null 2>&1 || { echo "FAIL: 建库失败"; exit 2; }

echo "==> S0 回放 V0394 之前的全部迁移（按版本号全局升序，含 V0393 请假流程）"
n=0; failed=0
while IFS=$' \t' read -r ver f; do
    if ! qf "$f" >/dev/null 2>/tmp/hr38_mig_err.log; then
        echo "FAIL: 基线迁移 v$ver $(basename "$f")"; grep -E "ERROR|FATAL" /tmp/hr38_mig_err.log | head -3; failed=1; break
    fi
    n=$((n+1))
done < <(ls "$MROOT"/*/*.up.sql | sed -E 's#.*/V([0-9]{4})__.*#\1 &#' | sort -n | awk -v limit=394 '$1 < limit')
[ "$failed" = 1 ] && exit 2
echo "    基线迁移 $n 条完成"
eq "S0 流程表在" "$(q "SELECT to_regclass('biz_flow_def') IS NOT NULL")" "t"
eq "S0 三套流程一条都没有" "$(q "SELECT count(*) FROM biz_flow_def WHERE flow_code IN ('hr_salary','hr_salary_adjust','hr_employee_resign')")" "0"
eq "S0 请假流程（V0393）已在，基线可信" "$(q "SELECT count(*) FROM biz_flow_def WHERE flow_code='hr_leave'")" "1"

echo "==> S1 应用 V0394（薪资单流程 up）"
OUT=$(run_up_sal); printf '%s\n' "$OUT" | grep -E "NOTICE|ERROR" | sed 's/^/    /'
has  "S1 自检 NOTICE 通过" "$OUT" "HR-38 自检通过：hr_salary"
hasnt "S1 无 ERROR" "$OUT" "ERROR"
eq "S1 def 1 行 PUBLISHED+默认+系统+不免审" "$(q "SELECT count(*) FROM biz_flow_def WHERE id='$D_SAL' AND status='PUBLISHED' AND is_default AND is_system AND NOT allow_auto_approve AND category='人力资源'")" "1"
eq "S1 version v1/PUBLISHED/current/6/6" "$(q "SELECT version_no||'/'||version_status||'/'||is_current||'/'||node_count||'/'||edge_count FROM biz_flow_def_version WHERE id='$V_SAL'")" "1/PUBLISHED/true/6/6"
eq "S1 def 指针指向本卡版本" "$(q "SELECT current_version_id FROM biz_flow_def WHERE id='$D_SAL'")" "$V_SAL"
eq "S1 graph_hash 列值" "$(q "SELECT graph_hash FROM biz_flow_def_version WHERE id='$V_SAL'")" "$HASH_SAL"
eq "S1 节点 6 个" "$(q "SELECT count(*) FROM biz_flow_node WHERE version_id='$V_SAL'")" "6"
eq "S1 边 6 条" "$(q "SELECT count(*) FROM biz_flow_edge WHERE version_id='$V_SAL'")" "6"
eq "S1 审批人绑定 4 条" "$(q "SELECT count(*) FROM biz_flow_node_assignee a JOIN biz_flow_node n ON n.id=a.node_id WHERE n.version_id='$V_SAL'")" "4"
eq "S1 财务会签 = COUNTERSIGN+ALL+兜底+超时" "$(q "SELECT node_type||'/'||approval_mode||'/'||empty_assignee||'/'||(timeout_hours IS NOT NULL) FROM biz_flow_node WHERE version_id='$V_SAL' AND node_key='n_fin'")" "COUNTERSIGN/ALL/FALLBACK_ADMIN/true"
eq "S1 会签候选人 = 财务经理+财务总监" "$(q "SELECT string_agg(a.ref_id::text,',' ORDER BY a.sort_order) FROM biz_flow_node_assignee a JOIN biz_flow_node n ON n.id=a.node_id WHERE n.version_id='$V_SAL' AND n.node_key='n_fin'")" "$FIN,$FIND"
eq "S1 两个 APPROVAL 都 ANY+FALLBACK_ADMIN+REMIND" "$(q "SELECT count(*) FROM biz_flow_node WHERE version_id='$V_SAL' AND node_type='APPROVAL' AND approval_mode='ANY' AND empty_assignee='FALLBACK_ADMIN' AND timeout_action='REMIND'")" "2"
eq "S1 没有 APPROVAL 全员会签" "$(q "SELECT count(*) FROM biz_flow_node WHERE version_id='$V_SAL' AND node_type='APPROVAL' AND approval_mode='ALL'")" "0"
eq "S1 金额条件边（gte 500000 on gross_total_num）" "$(q "SELECT condition_expr->>'field'||'/'||(condition_expr->>'cmp')||'/'||(condition_expr->>'value') FROM biz_flow_edge WHERE version_id='$V_SAL' AND edge_key='e_big'")" "form.gross_total_num/gte/500000"
eq "S1 兜底边恰 1 条（e_small）" "$(q "SELECT count(*)||'/'||max(edge_key) FROM biz_flow_edge WHERE version_id='$V_SAL' AND is_default")" "1/e_small"
eq "S1 链路 6 条骨架" "$(q "SELECT count(*) FROM biz_flow_edge e JOIN biz_flow_node f ON f.id=e.from_node_id JOIN biz_flow_node t ON t.id=e.to_node_id WHERE e.version_id='$V_SAL' AND ((f.node_key,t.node_key) IN (('s','n_hr'),('n_hr','n_fin'),('n_fin','c_amt'),('c_amt','n_gm'),('c_amt','e'),('n_gm','e')))")" "6"

echo "==> S2 V0394 up 幂等"
FP1=$(fingerprint)
OUT=$(run_up_sal)
eq "S2 重放后指纹不变" "$(fingerprint)" "$FP1"
has "S2 重放后自检仍通过" "$OUT" "HR-38 自检通过：hr_salary"
eq "S2 本卡版本仍只有 1 个" "$(q "SELECT count(*) FROM biz_flow_def_version WHERE def_id='$D_SAL' AND change_note LIKE 'V0394 种子%'")" "1"

echo "==> S3 V0394 hash 库内重算对账"
eq "S3 canonical 重算 == graph_hash == 常量" "$(rehash "$V_SAL")" "$HASH_SAL"

echo "==> S4 应用 V0395（调薪 + 离职 两套 up）"
OUT=$(run_up_adj); printf '%s\n' "$OUT" | grep -E "NOTICE|ERROR" | sed 's/^/    /'
has  "S4 调薪自检 NOTICE" "$OUT" "HR-38 自检通过：hr_salary_adjust"
has  "S4 离职自检 NOTICE" "$OUT" "HR-38 自检通过：hr_employee_resign"
hasnt "S4 无 ERROR" "$OUT" "ERROR"
eq "S4 调薪 1 def / 5 节点 / 4 边" "$(q "SELECT (SELECT count(*) FROM biz_flow_def WHERE id='$D_ADJ')||'/'||(SELECT node_count FROM biz_flow_def_version WHERE id='$V_ADJ')||'/'||(SELECT edge_count FROM biz_flow_def_version WHERE id='$V_ADJ')")" "1/5/4"
eq "S4 离职 1 def / 6 节点 / 6 边" "$(q "SELECT (SELECT count(*) FROM biz_flow_def WHERE id='$D_RES')||'/'||(SELECT node_count FROM biz_flow_def_version WHERE id='$V_RES')||'/'||(SELECT edge_count FROM biz_flow_def_version WHERE id='$V_RES')")" "1/6/6"
eq "S4 调薪 def PUBLISHED+默认+系统+不免审" "$(q "SELECT count(*) FROM biz_flow_def WHERE id='$D_ADJ' AND status='PUBLISHED' AND is_default AND is_system AND NOT allow_auto_approve")" "1"
eq "S4 离职 def PUBLISHED+默认+系统+不免审" "$(q "SELECT count(*) FROM biz_flow_def WHERE id='$D_RES' AND status='PUBLISHED' AND is_default AND is_system AND NOT allow_auto_approve")" "1"
eq "S4 调薪绑定人事经理+财务经理" "$(q "SELECT string_agg(a.ref_id::text,',' ORDER BY n.sort_order, a.sort_order) FROM biz_flow_node_assignee a JOIN biz_flow_node n ON n.id=a.node_id WHERE n.version_id='$V_ADJ'")" "$HR,$FIN"
eq "S4 离职绑定人事经理+总经理" "$(q "SELECT string_agg(a.ref_id::text,',' ORDER BY n.sort_order, a.sort_order) FROM biz_flow_node_assignee a JOIN biz_flow_node n ON n.id=a.node_id WHERE n.version_id='$V_RES'")" "$HR,$GM"
eq "S4 直属上级 = INITIATOR_SUPERIOR(levels=1) 且无枚举行（调薪/离职各一处）" "$(q "SELECT count(*) FROM biz_flow_node n WHERE n.version_id IN ('$V_ADJ','$V_RES') AND n.node_key='n_sup' AND n.assignee_type='INITIATOR_SUPERIOR' AND n.superior_levels=1 AND NOT EXISTS (SELECT 1 FROM biz_flow_node_assignee a WHERE a.node_id=n.id)")" "2"
eq "S4 两套都没有 ALL 审批节点" "$(q "SELECT count(*) FROM biz_flow_node WHERE version_id IN ('$V_ADJ','$V_RES') AND approval_mode='ALL'")" "0"
eq "S4 关键岗位条件边（eq true on is_key_position）" "$(q "SELECT condition_expr->>'field'||'/'||(condition_expr->>'cmp')||'/'||(condition_expr->>'value') FROM biz_flow_edge WHERE version_id='$V_RES' AND edge_key='e_key'")" "form.is_key_position/eq/true"

echo "==> S5 V0395 up 幂等"
FP2=$(fingerprint)
OUT=$(run_up_adj)
eq "S5 重放后指纹不变" "$(fingerprint)" "$FP2"
has "S5 调薪自检仍通过" "$OUT" "HR-38 自检通过：hr_salary_adjust"
has "S5 离职自检仍通过" "$OUT" "HR-38 自检通过：hr_employee_resign"

echo "==> S6 V0395 hash 库内重算对账"
eq "S6 调薪 canonical 重算 == 常量" "$(rehash "$V_ADJ")" "$HASH_ADJ"
eq "S6 离职 canonical 重算 == 常量" "$(rehash "$V_RES")" "$HASH_RES"

echo "==> S7 down 两套 → down 幂等 → 重放"
OUT=$(run_down_sal); has "S7 薪资单 down 安全阀通过" "$OUT" "安全阀全部通过"
OUT=$(run_down_adj); has "S7 调薪/离职 down 安全阀通过" "$OUT" "安全阀全部通过"
hasnt "S7 down 无 ERROR" "$(run_down_sal)$(run_down_adj)" "ERROR"
eq "S7 三套定义全清" "$(q "SELECT count(*) FROM biz_flow_def WHERE flow_code IN ('hr_salary','hr_salary_adjust','hr_employee_resign')")" "0"
eq "S7 版本/节点/边/审批人全清" "$(q "SELECT (SELECT count(*) FROM biz_flow_def_version WHERE def_id IN ('$D_SAL','$D_ADJ','$D_RES'))||'/'||(SELECT count(*) FROM biz_flow_node WHERE version_id IN ('$V_SAL','$V_ADJ','$V_RES'))||'/'||(SELECT count(*) FROM biz_flow_edge WHERE version_id IN ('$V_SAL','$V_ADJ','$V_RES'))")" "0/0/0"
OUT=$(run_up_sal)$(run_up_adj)
has "S7 重放后薪资单自检通过" "$OUT" "HR-38 自检通过：hr_salary"
has "S7 重放后调薪自检通过" "$OUT" "HR-38 自检通过：hr_salary_adjust"
has "S7 重放后离职自检通过" "$OUT" "HR-38 自检通过：hr_employee_resign"
eq "S7 重放后回到 3 def" "$(q "SELECT count(*) FROM biz_flow_def WHERE flow_code IN ('hr_salary','hr_salary_adjust','hr_employee_resign')")" "3"

echo "==> S8 守卫①：客户自建同名 flow_code（只跳过那一套）"
OUT=$(run_down_sal)$(run_down_adj)
q "INSERT INTO biz_flow_def (id, flow_code, flow_name, biz_type, category, status, current_version_id, current_version_no, is_default, allow_auto_approve)
   VALUES ('11111111-1111-4111-8111-111111111111','hr_salary_adjust','客户自建调薪流','HR_SALARY_ADJUST_X','人力资源','DRAFT',NULL,0,FALSE,FALSE)" >/dev/null
OUT=$(run_up_adj)
has "S8 守卫①命中（提示已存在同名流程）" "$OUT" "flow_code=hr_salary_adjust 的流程"
eq "S8 客户行未被覆盖（名字仍是客户的）" "$(q "SELECT flow_name FROM biz_flow_def WHERE flow_code='hr_salary_adjust' AND deleted_at IS NULL")" "客户自建调薪流"
eq "S8 没有种出本卡的调薪 def" "$(q "SELECT count(*) FROM biz_flow_def WHERE id='$D_ADJ'")" "0"
eq "S8 另一套（离职）照种" "$(q "SELECT count(*) FROM biz_flow_def WHERE id='$D_RES'")" "1"
q "DELETE FROM biz_flow_node_assignee WHERE node_id IN (SELECT id FROM biz_flow_node WHERE version_id='$V_RES')" >/dev/null
q "DELETE FROM biz_flow_edge WHERE version_id='$V_RES'" >/dev/null
q "DELETE FROM biz_flow_node WHERE version_id='$V_RES'" >/dev/null
q "DELETE FROM biz_flow_def_version WHERE id='$V_RES'" >/dev/null
q "DELETE FROM biz_flow_def WHERE id='$D_RES'" >/dev/null
q "DELETE FROM biz_flow_def WHERE id='11111111-1111-4111-8111-111111111111'" >/dev/null

echo "==> S9 守卫②：同 biz_type 已有默认流（只跳过那一套）"
q "INSERT INTO biz_flow_def (id, flow_code, flow_name, biz_type, category, status, current_version_id, current_version_no, is_default, allow_auto_approve)
   VALUES ('22222222-2222-4222-8222-222222222222','hr_employee_resign_custom','客户默认离职流','HR_EMPLOYEE_RESIGN','人力资源','DRAFT',NULL,0,TRUE,FALSE)" >/dev/null
OUT=$(run_up_adj)
has "S9 守卫②命中（提示同 biz_type 已有默认流）" "$OUT" "biz_type=HR_EMPLOYEE_RESIGN 的默认流程"
eq "S9 没有种出本卡的离职 def" "$(q "SELECT count(*) FROM biz_flow_def WHERE id='$D_RES'")" "0"
eq "S9 另一套（调薪）照种" "$(q "SELECT count(*) FROM biz_flow_def WHERE id='$D_ADJ'")" "1"
q "DELETE FROM biz_flow_node_assignee WHERE node_id IN (SELECT id FROM biz_flow_node WHERE version_id='$V_ADJ')" >/dev/null
q "DELETE FROM biz_flow_edge WHERE version_id='$V_ADJ'" >/dev/null
q "DELETE FROM biz_flow_node WHERE version_id='$V_ADJ'" >/dev/null
q "DELETE FROM biz_flow_def_version WHERE id='$V_ADJ'" >/dev/null
q "DELETE FROM biz_flow_def WHERE id='$D_ADJ'" >/dev/null
q "DELETE FROM biz_flow_def WHERE id='22222222-2222-4222-8222-222222222222'" >/dev/null

echo "==> S10 守卫③：审批角色缺失（离职要总经理）"
q "UPDATE sys_role SET deleted_at = now() WHERE id='$GM'" >/dev/null
OUT=$(run_up_adj)
has "S10 守卫③命中（角色不齐 → 该套不种）" "$OUT" "只找到 2/3"
eq "S10 没有种出离职 def" "$(q "SELECT count(*) FROM biz_flow_def WHERE id='$D_RES'")" "0"
eq "S10 调薪（不依赖总经理）照种" "$(q "SELECT count(*) FROM biz_flow_def WHERE id='$D_ADJ'")" "1"
q "UPDATE sys_role SET deleted_at = NULL WHERE id='$GM'" >/dev/null
OUT=$(run_up_adj); has "S10 角色恢复后重跑成功" "$OUT" "HR-38 自检通过：hr_employee_resign"

echo "==> S11 守卫④：版本递增（已有「非本卡」版本）"
# 造一个「客户自建调薪 v1」：删掉本卡调薪版本与子行（保留 def），插 v1 DRAFT 且非 current
q "DELETE FROM biz_flow_node_assignee WHERE node_id IN (SELECT id FROM biz_flow_node WHERE version_id='$V_ADJ')" >/dev/null
q "DELETE FROM biz_flow_edge WHERE version_id='$V_ADJ'" >/dev/null
q "DELETE FROM biz_flow_node WHERE version_id='$V_ADJ'" >/dev/null
q "DELETE FROM biz_flow_def_version WHERE id='$V_ADJ'" >/dev/null
q "INSERT INTO biz_flow_def_version (id, def_id, version_no, version_status, is_current, node_count, edge_count, change_note)
   VALUES ('33333333-3333-4333-8333-333333333333','$D_ADJ',1,'DRAFT',FALSE,0,0,'客户自建 v1')" >/dev/null
q "UPDATE biz_flow_def SET current_version_id='33333333-3333-4333-8333-333333333333', current_version_no=1 WHERE id='$D_ADJ'" >/dev/null
OUT=$(run_up_adj)
hasnt "S11 未撞唯一索引（无 ERROR）" "$OUT" "ERROR"
eq "S11 发的是 v2 而不是 v1" "$(q "SELECT version_no FROM biz_flow_def_version WHERE def_id='$D_ADJ' AND id='$V_ADJ'")" "2"
eq "S11 新版本 is_current（客户那版本来就不是 current）" "$(q "SELECT is_current FROM biz_flow_def_version WHERE id='$V_ADJ'")" "t"
eq "S11 不抢客户版本：def 指针仍是客户那版" "$(q "SELECT current_version_id FROM biz_flow_def WHERE id='$D_ADJ'")" "33333333-3333-4333-8333-333333333333"
eq "S11 本卡调薪版本只多这一份" "$(q "SELECT count(*) FROM biz_flow_def_version WHERE def_id='$D_ADJ' AND change_note LIKE 'V0395 种子%'")" "1"

echo "==> S12 down 安全阀：本流程下还有别的版本 → 拒绝回滚"
OUT=$(run_down_adj)
has "S12 down 被安全阀拦下" "$OUT" "还有 1 个非本卡版本"
q "DELETE FROM biz_flow_def_version WHERE id='33333333-3333-4333-8333-333333333333'" >/dev/null
q "UPDATE biz_flow_def SET current_version_id='$V_ADJ', current_version_no=2 WHERE id='$D_ADJ'" >/dev/null
OUT=$(run_down_adj); has "S12 清掉客户版本后 down 成功" "$OUT" "安全阀全部通过"
eq "S12 down 后两套定义已删" "$(q "SELECT count(*) FROM biz_flow_def WHERE id IN ('$D_ADJ','$D_RES')")" "0"

echo "==> S13 收尾"
cleanup
eq "S13 演练库已 DROP" "$(adm "SELECT count(*) FROM pg_database WHERE datname='$DB'")" "0"

echo
if [ "$FAILS" = 0 ]; then echo "ALL PASS（0 失败）"; exit 0; else echo "FAILED: $FAILS 项"; exit 1; fi
