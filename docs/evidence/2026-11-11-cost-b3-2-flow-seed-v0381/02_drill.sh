#!/usr/bin/env bash
# ============================================================================
# B3-2 八段真库演练：V0381 成本结算审批流种子
#
# S0 基线（不含 V0381）        ：库里有流程表、没有 cost_settle_approve
# S1 up                        ：1 def / 1 version / 4 节点 / 3 边 / 2 审批人
# S2 up 幂等                   ：行数不变 + **指纹逐字节相等**（不重复插版本）
# S3 down + down 幂等          ：全清；第二遍零异常
# S4 回滚后重放                ：指纹回到 S1
# S5 守卫①（同名 flow_code）  ：客户自建 → 整条跳过、客户行不被覆盖
# S6 守卫②（同 biz_type 默认）：客户自建默认流 → 整条跳过
# S7 守卫③（角色缺失）        ：软删 FINANCE_DIRECTOR → 跳过；恢复后重跑成功
# S8 守卫④（版本递增）        ：已有同 def 的「非本卡」版本 → 发 v2，不撞 uk、不抢 is_current
# S9 down 安全阀 b（结算批次已挂实例）→ 拒绝回滚，库零变化
# S10 down 安全阀 c（本流程下还有别的版本）→ 拒绝回滚
# S11 收尾：down 干净、临时行清空
#
# 用法：bash 02_drill.sh
# 退出码：0 = 全部断言通过
# ============================================================================
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
MROOT="$ROOT/backend/migrations"
UP="$MROOT/business/V0381__cost_flow_def.up.sql"
DOWN="$MROOT/business/V0381__cost_flow_def.down.sql"
DBHOST="${DBHOST:-127.0.0.1}"; DBPORT="${DBPORT:-5432}"
DBUSER="${DBUSER:-alun}";      DBPASS="${DBPASS:-alun}"
DB="mig_costb32_drill_$(date +%m%d%H%M%S)"
export PGPASSWORD="$DBPASS"

D='20000000-0000-4000-8000-0000000b0381'
V='20000000-0000-4000-8000-0000000b0382'
FM='20000000-0000-4000-8000-0000000f0005'
FD='20000000-0000-4000-8000-0000000f0006'

q()  { psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -tAc "$1" 2>&1; }
qf() { psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -q -X -v ON_ERROR_STOP=1 -f "$1" 2>&1; }

FAILS=0
ok(){ printf '  ✓ %s\n' "$1"; }
bad(){ printf '  ✗ %s\n' "$1"; FAILS=$((FAILS+1)); }
eq(){ if [ "$2" = "$3" ]; then ok "$1（$2）"; else bad "$1：期望 [$3] 实际 [$2]"; fi; }
has(){ case "$2" in *"$3"*) ok "$1";; *) bad "$1：输出里没有 [$3]（实际：$(printf '%s' "$2" | tr '\n' ' ' | cut -c1-220)）";; esac; }
hasnt(){ case "$2" in *"$3"*) bad "$1：输出里不该有 [$3]";; *) ok "$1";; esac; }

cleanup(){ su - postgres -c "dropdb --force $DB" >/dev/null 2>&1 || q "DROP DATABASE IF EXISTS $DB" postgres >/dev/null 2>&1; }
trap cleanup EXIT

su - postgres -c "createdb -O $DBUSER $DB" >/dev/null 2>&1 || { echo "FAIL: 建库失败"; exit 2; }
echo "==> 演练库：$DB"

# ---------- 基线：跑 V0381 之前的全部迁移 ----------
n=0
while IFS=$'\t' read -r ver f; do
    [ "$ver" -ge 381 ] && continue
    if ! PGOPTIONS='-c client_min_messages=warning' \
         psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -q -X -v ON_ERROR_STOP=1 \
              --single-transaction -f "$f" >/dev/null 2>/tmp/b32_drill_err.log; then
        echo "FAIL: 基线 v$ver $(basename "$f")"; grep -E "ERROR|FATAL" /tmp/b32_drill_err.log | head -3; exit 2
    fi
    n=$((n + 1))
done < <(find "$MROOT" -name 'V*__*.up.sql' | awk -F'/V' '{split($NF,a,"__"); print a[1]+0 "\t" $0}' | sort -n -k1,1)
echo "  基线迁移数：$n 条（< V0381）"

# 指纹：def/version/node/edge/assignee 五张表里属于本流程的所有行
fingerprint() {
  q "select md5(string_agg(t, '|' order by t)) from (
       select 'D|'||id||'|'||flow_code||'|'||status||'|'||current_version_no::text||'|'||is_default::text
                ||'|'||allow_auto_approve::text||'|'||coalesce(current_version_id::text,'-') as t
         from biz_flow_def where flow_code='cost_settle_approve'
       union all
       select 'V|'||id||'|'||version_no::text||'|'||version_status||'|'||is_current::text||'|'||graph_hash
         from biz_flow_def_version where def_id='$D'
       union all
       select 'N|'||node_key||'|'||node_name||'|'||node_type||'|'||coalesce(assignee_type,'-')
                ||'|'||coalesce(approval_mode,'-')||'|'||empty_assignee||'|'||self_approval
                ||'|'||sort_order::text||'|'||timeout_hours::text
         from biz_flow_node where version_id in (select id from biz_flow_def_version where def_id='$D')
       union all
       select 'E|'||edge_key||'|'||from_node_id::text||'|'||to_node_id::text
         from biz_flow_edge where version_id in (select id from biz_flow_def_version where def_id='$D')
       union all
       select 'A|'||node_id::text||'|'||assignee_type||'|'||ref_id::text
         from biz_flow_node_assignee
        where node_id in (select id from biz_flow_node
                           where version_id in (select id from biz_flow_def_version where def_id='$D'))
     ) s"
}

# ============================== S0 基线 ==============================
echo "== S0 基线（无 V0381）"
eq "基线：cost_settle_approve 不存在" "$(q "select count(*) from biz_flow_def where flow_code='cost_settle_approve'")" "0"
eq "基线：流程五表就绪" \
   "$(q "select count(*) from information_schema.tables where table_schema='public' and table_name in ('biz_flow_def','biz_flow_def_version','biz_flow_node','biz_flow_edge','biz_flow_node_assignee')")" "5"
eq "基线：两个财务审批角色在" "$(q "select count(*) from sys_role where id in ('$FM','$FD') and deleted_at is null")" "2"

# ============================== S1 up ==============================
echo "== S1 首次 up"
OUT=$(qf "$UP"); has "up 自检通过" "$OUT" "COST-B3-2 自检通过"
eq "def 1 条 PUBLISHED/is_default/不免审" \
   "$(q "select count(*) from biz_flow_def where id='$D' and status='PUBLISHED' and is_default and not allow_auto_approve")" "1"
eq "version 1 条 v1 PUBLISHED/is_current" \
   "$(q "select count(*) from biz_flow_def_version where id='$V' and version_no=1 and version_status='PUBLISHED' and is_current")" "1"
eq "节点 4 / 边 3 / 审批人 2" \
   "$(q "select (select count(*) from biz_flow_node where version_id='$V')||'/'||(select count(*) from biz_flow_edge where version_id='$V')||'/'||(select count(*) from biz_flow_node_assignee a join biz_flow_node n on n.id=a.node_id where n.version_id='$V')")" "4/3/2"
eq "审批节点：2 个 APPROVAL 且都是 ROLE+ANY+兜底" \
   "$(q "select count(*) from biz_flow_node where version_id='$V' and node_type='APPROVAL' and assignee_type='ROLE' and approval_mode='ANY' and empty_assignee='FALLBACK_ADMIN'")" "2"
eq "节点绑角色：n_fm→FINANCE_MANAGER / n_fd→FINANCE_DIRECTOR" \
   "$(q "select string_agg(n.node_key||'→'||r.role_code, ',' order by n.sort_order) from biz_flow_node_assignee a join biz_flow_node n on n.id=a.node_id join sys_role r on r.id=a.ref_id where n.version_id='$V'")" \
   "n_fm→FINANCE_MANAGER,n_fd→FINANCE_DIRECTOR"
eq "免审直过闸：allow_auto_approve = false" "$(q "select allow_auto_approve::text from biz_flow_def where id='$D'")" "false"
FP1=$(fingerprint)
ok "S1 指纹 $FP1"

# ============================== S2 up 幂等 ==============================
echo "== S2 再跑一次 up（幂等）"
OUT=$(qf "$UP"); has "第二次 up 仍自检通过（顺带证明上一遍的行没漂移）" "$OUT" "COST-B3-2 自检通过"
eq "def 仍 1 条" "$(q "select count(*) from biz_flow_def where flow_code='cost_settle_approve'")" "1"
eq "version 仍 1 条（没发 v2）" "$(q "select count(*) from biz_flow_def_version where def_id='$D'")" "1"
eq "节点仍 4 / 边仍 3" \
   "$(q "select (select count(*) from biz_flow_node where version_id='$V')||'/'||(select count(*) from biz_flow_edge where version_id='$V')")" "4/3"
FP2=$(fingerprint)
eq "S2 指纹 = S1 指纹（重复执行结果不变）" "$FP2" "$FP1"

# ============================== S3 down ×2 ==============================
echo "== S3 down（含幂等）"
OUT=$(qf "$DOWN"); has "down 安全阀通过" "$OUT" "安全阀全部通过"
eq "down 后：def 0 / version 0 / 节点 0 / 边 0 / 审批人 0" \
   "$(q "select (select count(*) from biz_flow_def where id='$D')||'/'||(select count(*) from biz_flow_def_version where id='$V')||'/'||(select count(*) from biz_flow_node where version_id='$V')||'/'||(select count(*) from biz_flow_edge where version_id='$V')||'/'||(select count(*) from biz_flow_node_assignee where node_id in ('20000000-0000-4000-8000-0000000b0392','20000000-0000-4000-8000-0000000b0393'))")" "0/0/0/0/0"
OUT=$(qf "$DOWN"); has "第二次 down 幂等（零异常）" "$OUT" "V0381 down 完成"
hasnt "第二次 down 没有 ERROR" "$OUT" "ERROR"

# ============================== S4 回滚后重放 ==============================
echo "== S4 down 之后重新 up"
OUT=$(qf "$UP"); has "重放自检通过" "$OUT" "COST-B3-2 自检通过"
FP3=$(fingerprint)
eq "S4 指纹 = S1 指纹（回滚后重放逐字节一致）" "$FP3" "$FP1"

# ============================== S5 守卫① ==============================
echo "== S5 守卫①：客户自建同名流程"
qf "$DOWN" >/dev/null
q "insert into biz_flow_def (id, flow_code, flow_name, biz_type, category, status, current_version_no, is_default, allow_auto_approve)
   values ('20000000-0000-4000-8000-0000000b9f01','cost_settle_approve','客户自建结算审批','COST_SETTLE','财务管理','DRAFT',0,false,false)"
OUT=$(qf "$UP")
has "守卫① 提示" "$OUT" "已存在 flow_code=cost_settle_approve 的流程"
has "自检说明跳过（库里那条是客户的）" "$OUT" "由客户自建"
eq "客户流程没被覆盖（名字与状态不变）" \
   "$(q "select flow_name||'/'||status from biz_flow_def where flow_code='cost_settle_approve'")" "客户自建结算审批/DRAFT"
eq "本卡 def id 未插入" "$(q "select count(*) from biz_flow_def where id='$D'")" "0"
eq "本卡版本未插入" "$(q "select count(*) from biz_flow_def_version where id='$V'")" "0"
eq "本卡节点未插入" "$(q "select count(*) from biz_flow_node where version_id='$V'")" "0"
q "delete from biz_flow_def where id='20000000-0000-4000-8000-0000000b9f01'"

# ============================== S6 守卫② ==============================
echo "== S6 守卫②：同 biz_type 已有默认流"
q "insert into biz_flow_def (id, flow_code, flow_name, biz_type, category, status, current_version_no, is_default, allow_auto_approve)
   values ('20000000-0000-4000-8000-0000000b9f02','cust_cost_settle','客户默认结算流','COST_SETTLE','财务管理','DRAFT',0,true,false)"
OUT=$(qf "$UP")
has "守卫② 提示" "$OUT" "已存在 biz_type=COST_SETTLE 的默认流程"
eq "本卡 def 未插入（同 biz_type 已有默认流）" "$(q "select count(*) from biz_flow_def where id='$D'")" "0"
eq "COST_SETTLE 默认流仍只有客户那一条" \
   "$(q "select count(*) from biz_flow_def where biz_type='COST_SETTLE' and is_default and deleted_at is null")" "1"
q "delete from biz_flow_def where id='20000000-0000-4000-8000-0000000b9f02'"

# ============================== S7 守卫③ ==============================
echo "== S7 守卫③：审批角色缺失"
q "update sys_role set deleted_at = now() where id='$FD'"
OUT=$(qf "$UP")
has "守卫③ 提示（角色只找到 1/2）" "$OUT" "审批角色只找到 1/2"
eq "角色缺失时本卡 def 未插入" "$(q "select count(*) from biz_flow_def where id='$D'")" "0"
q "update sys_role set deleted_at = null where id='$FD'"
OUT=$(qf "$UP"); has "角色恢复后 up 成功" "$OUT" "COST-B3-2 自检通过"
eq "恢复后 version v1 就位" "$(q "select count(*) from biz_flow_def_version where id='$V' and version_no=1 and is_current")" "1"

# ============================== S8 守卫④ ==============================
echo "== S8 守卫④：版本递增（def 已有客户 v1，本卡版本尚未种）"
# 场景：def 行已是本卡形态（例如上一次只跑到 §2），但库里已经有一版客户的 v1
#      ⇒ 本卡不能再发 v1（撞 uk_biz_flow_def_version），必须发 v2 且**不抢** is_current。
qf "$DOWN" >/dev/null
q "insert into biz_flow_def (id, flow_code, flow_name, biz_type, category, status, current_version_id, current_version_no, is_default, allow_auto_approve)
   values ('$D','cost_settle_approve','成本结算审批（标准·两级）','COST_SETTLE','财务管理','PUBLISHED','20000000-0000-4000-8000-0000000b9f03',1,true,false)"
q "insert into biz_flow_def_version (id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot, node_count, edge_count, change_note, published_at)
   values ('20000000-0000-4000-8000-0000000b9f03','$D',1,'PUBLISHED',true,repeat('0',64),'{}'::jsonb,0,0,'客户 v1（演练）',now())"
OUT=$(qf "$UP")
hasnt "递增 up 没有 ERROR" "$OUT" "ERROR"
eq "版本数 2（客户 v1 保留 + 本卡 v2 新增）" "$(q "select count(*) from biz_flow_def_version where def_id='$D'")" "2"
eq "版本号 = 1,2（MAX+1，不撞 uk_biz_flow_def_version）" \
   "$(q "select string_agg(version_no::text, ',' order by version_no) from biz_flow_def_version where def_id='$D'")" "1,2"
eq "is_current 仍只有 1 条（不抢客户当前版本）" \
   "$(q "select count(*) from biz_flow_def_version where def_id='$D' and is_current")" "1"
eq "本卡 v2 是 PUBLISHED 且有快照 / 4 节点" \
   "$(q "select count(*) from biz_flow_def_version where def_id='$D' and version_no=2 and version_status='PUBLISHED' and graph_snapshot is not null and node_count=4 and graph_hash='8a4fbd64cf5fb5cfc3b133ef2e4ff6103cd14a9afbabfae2b6f63f5845eb4171'")" "1"
eq "本卡 v2 的节点/边按 version_id 落齐" \
   "$(q "select (select count(*) from biz_flow_node where version_id='$V')||'/'||(select count(*) from biz_flow_edge where version_id='$V')")" "4/3"
eq "客户 v1 一个字没动（hash 仍是演练值）" "$(q "select graph_hash from biz_flow_def_version where id='20000000-0000-4000-8000-0000000b9f03'")" "$(printf '0%.0s' {1..64})"
has "自检说明被客户改过" "$OUT" "已被客户改过"
eq "def 指针仍指向客户 v1（不被抢）" "$(q "select current_version_id::text from biz_flow_def where id='$D'")" "20000000-0000-4000-8000-0000000b9f03"
# 清场：删掉本卡 v2 与客户 v1，回到「无流程」再干净重种一次
q "delete from biz_flow_node_assignee where node_id in (select id from biz_flow_node where version_id='$V')"
q "delete from biz_flow_edge where version_id='$V'"
q "delete from biz_flow_node where version_id='$V'"
q "delete from biz_flow_def_version where def_id='$D'"
q "delete from biz_flow_def where id='$D'"
OUT=$(qf "$UP"); has "清场后重种成功" "$OUT" "COST-B3-2 自检通过"
eq "重种回到出厂形态（单版本 v1 / is_current）" "$(q "select count(*) from biz_flow_def_version where def_id='$D' and version_no=1 and is_current")" "1"

# ============================== S9 down 安全阀 b ==============================
echo "== S9 down 安全阀：结算批次已挂审批实例"
q "insert into biz_cost_period (period_code, batch_no, status, approval_instance_id)
   values ('2099-01','CS-2099-01-0001','DRAFT','20000000-0000-4000-8000-0000000bffff')"
OUT=$(qf "$DOWN")
has "拒绝回滚（错误信息点名结算批次）" "$OUT" "biz_cost_period 已有 1 行挂在审批实例上"
eq "拒绝后 def 仍在（库零变化）" "$(q "select count(*) from biz_flow_def where id='$D'")" "1"
eq "拒绝后节点仍在" "$(q "select count(*) from biz_flow_node where version_id='$V'")" "4"
q "delete from biz_cost_period where batch_no='CS-2099-01-0001'"
OUT=$(qf "$DOWN"); has "清掉结算批次后 down 成功" "$OUT" "V0381 down 完成"

# ============================== S10 down 安全阀 c ==============================
echo "== S10 down 安全阀：本流程下还有别的版本"
qf "$UP" >/dev/null
q "insert into biz_flow_def_version (id, def_id, version_no, version_status, is_current, graph_hash, graph_snapshot, node_count, edge_count, change_note, published_at)
   values ('20000000-0000-4000-8000-0000000b9f09','$D',2,'PUBLISHED',false,repeat('0',64),'{}'::jsonb,0,0,'客户发的新版（演练）',now())"
OUT=$(qf "$DOWN")
has "拒绝回滚（点名额外版本）" "$OUT" "还有 1 个非本卡版本"
eq "拒绝后 def 仍在" "$(q "select count(*) from biz_flow_def where id='$D'")" "1"
q "delete from biz_flow_def_version where id='20000000-0000-4000-8000-0000000b9f09'"

# ============================== S11 收尾 ==============================
echo "== S11 收尾：down 干净、无残留"
OUT=$(qf "$DOWN"); has "down 完成" "$OUT" "V0381 down 完成"
eq "残留行全 0" \
   "$(q "select (select count(*) from biz_flow_def where flow_code='cost_settle_approve')||'/'||(select count(*) from biz_flow_def_version where def_id='$D')||'/'||(select count(*) from biz_flow_node where version_id='$V')||'/'||(select count(*) from biz_flow_edge where version_id='$V')")" "0/0/0/0"
eq "演练临时行已清空（biz_def 只回到基线）" "$(q "select count(*) from biz_flow_def where id::text like '20000000-0000-4000-8000-0000000b9f%'")" "0"
eq "biz_cost_period 演练行已删" "$(q "select count(*) from biz_cost_period where batch_no like 'CS-2099%'")" "0"
eq "角色软删已复原" "$(q "select count(*) from sys_role where deleted_at is null and id in ('$FM','$FD')")" "2"

echo "=== 结果：$FAILS 条断言失败 ==="
[ "$FAILS" -eq 0 ] && exit 0 || exit 1
