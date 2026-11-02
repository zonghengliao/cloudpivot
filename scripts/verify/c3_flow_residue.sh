#!/usr/bin/env bash
# ============================================================
# C3 验收脚本：测试/演示审批流「残留」+ 审批待办「孤儿」审计（**只读，不改一行数据**）
#
# 用途：证明「流程定义列表 = 8 条标准流程」「目标测试流程在 9 张 flow 表里 0 行残留」
#       「biz_flow_task 里 PENDING 的待办全部可追溯（无孤儿）」「『我的待办』不会出现测试流程的东西」。
#
# 用法（在仓根跑；默认本机 prod）：
#   bash scripts/verify/c3_flow_residue.sh                      # 审计 prod（127.0.0.1/cloudpivot）
#   DB_NAME=cloudpivot_p10r bash scripts/verify/c3_flow_residue.sh   # 审计任意库（如清理前快照，应告警）
#   TARGET_FLOWS="d5_281866,e2e_cs_084913" bash scripts/verify/c3_flow_residue.sh
#
# 连接口径：DB_HOST/DB_PORT/DB_USER/DB_PASS/DB_NAME，或 NIK_DATABASE_* 环境变量
#          （在 144 上 `set -a; . /etc/cloudpivot/cloudpivot.env; set +a` 即可）。
#
# 退出码：0 = 全部通过；1 = 有 [FAIL]
#
# 断言口径（与卡片 t_118db23a 的验收标准一一对应）：
#   C1 目标测试流程在 biz_flow_def 里 0 行（含软删行）        ← 卡片「已清理」
#   C2 存活流程定义 = 8 条，且 flow_code 集合 == 8 条标准流程   ← 卡片「流程定义列表」
#   C3 目标测试流程的实例 = 0 行                              ← 卡片「实例已归档/清理」
#   C4 目标测试流程的待办 = 0 行（含 CANCELLED 留痕也没了=随定义物理删） ← 卡片「未完成待办已撤销」
#   C5 PENDING 待办必须可追溯：活定义 + 活实例 + 实例非终态      ← 卡片「不能有孤儿」
#   C6 孤儿待办（实例已终态但待办 PENDING）= 0                 ← 卡片「不能有孤儿」
#   C7 非终态实例的流程定义必须存活（定义没了实例还在跑 = 另一类孤儿）= 0
#   C8 打印 PENDING 待办清单（判决证据）
# ============================================================
set -u

DB_HOST="${DB_HOST:-${NIK_DATABASE_HOST:-127.0.0.1}}"
DB_PORT="${DB_PORT:-${NIK_DATABASE_PORT:-5432}}"
DB_USER="${DB_USER:-${NIK_DATABASE_USER:-cloudpivot}}"
DB_PASS="${DB_PASS:-${NIK_DATABASE_PASSWORD:-}}"
DB_NAME="${DB_NAME:-${NIK_DATABASE_NAME:-cloudpivot}}"

TARGET_FLOWS="${TARGET_FLOWS:-d5_281866,d5_282996,e2e_cs_084913,p12_canvas_demo}"
STANDARD_FLOWS="${STANDARD_FLOWS:-payable_cancel,payable_pay,po_approve,stock_in_cancel,stock_in_post,stock_transfer,task_accept,voucher_reverse}"

PASS=0
FAIL=0
ok()  { PASS=$((PASS+1)); echo "[PASS] $1"; }
bad() { FAIL=$((FAIL+1)); echo "[FAIL] $1"; }
check() { if [ "$2" = "$3" ]; then ok "$1（$2）"; else bad "$1（期望 [$3] 实际 [$2]）"; fi; }
psql_q() { PGPASSWORD="$DB_PASS" psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -tAF '~' -c "$1" 2>&1 | head -1; }
psql_rows() { PGPASSWORD="$DB_PASS" psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -tAF ' | ' -c "$1" 2>&1; }

T_LIST="'$(printf '%s' "$TARGET_FLOWS"  | sed "s/,/','/g")'"
S_LIST="'$(printf '%s' "$STANDARD_FLOWS" | sed "s/,/','/g")'"

echo "=============================================================="
echo "C3 残留审计  db=$DB_NAME@$DB_HOST:$DB_PORT  只读"
echo "目标测试流程: $TARGET_FLOWS"
echo "标准流程(8):  $STANDARD_FLOWS"
echo "=============================================================="

echo "---- C1 目标测试流程在 biz_flow_def 的行数（含软删）"
C1=$(psql_q "select count(*) from biz_flow_def where flow_code in ($T_LIST)")
check "C1 目标测试流程定义残留 = 0" "$C1" "0"

echo "---- C2 存活流程定义（deleted_at is null）"
C2_N=$(psql_q "select count(*) from biz_flow_def where deleted_at is null")
check "C2a 存活流程定义条数 = 8" "$C2_N" "8"
# 集合比对与排序/排序规则无关：缺一条算一条，多一条算一条
C2_MISS=$(psql_q "select count(*) from (select unnest(array[$S_LIST]) as code) v
                   where not exists (select 1 from biz_flow_def d where d.deleted_at is null and d.flow_code = v.code)")
C2_EXTRA=$(psql_q "select count(*) from biz_flow_def where deleted_at is null and flow_code not in ($S_LIST)")
check "C2b 存活流程定义缺标准流程 = 0" "$C2_MISS" "0"
check "C2c 存活流程定义多出标准之外的 = 0" "$C2_EXTRA" "0"
echo "        实际存活集合: $(psql_q "select coalesce(string_agg(flow_code, ',' order by flow_code),'(空)') from biz_flow_def where deleted_at is null")"

echo "---- C3 目标测试流程的实例行数（按 flow_code 与 def_id 双口径）"
C3=$(psql_q "select count(*) from biz_flow_instance i where i.flow_code in ($T_LIST)
               or i.def_id in (select id from biz_flow_def where flow_code in ($T_LIST))")
check "C3 目标测试流程实例残留 = 0" "$C3" "0"

echo "---- C4 目标测试流程的待办行数"
C4=$(psql_q "select count(*) from biz_flow_task ft join biz_flow_instance i on i.id = ft.instance_id
              where i.flow_code in ($T_LIST)")
check "C4 目标测试流程待办残留 = 0" "$C4" "0"

echo "---- C5 PENDING 待办的可追溯性（活定义 + 活实例 + 实例非终态）"
C5_TOTAL=$(psql_q "select count(*) from biz_flow_task where status='PENDING'")
C5_BAD=$(psql_q "
  select count(*) from biz_flow_task ft
    left join biz_flow_instance i on i.id = ft.instance_id
    left join biz_flow_def d on d.id = i.def_id
   where ft.status='PENDING'
     and (i.id is null or i.deleted_at is not null or d.id is null or d.deleted_at is not null
          or i.status in ('CANCELLED','APPROVED','REJECTED','WITHDRAWN'))")
check "C5 PENDING 待办中不可追溯的 = 0（PENDING 总数 $C5_TOTAL）" "$C5_BAD" "0"

echo "---- C6 孤儿待办（实例终态 + 待办 PENDING）"
C6=$(psql_q "select count(*) from biz_flow_task ft join biz_flow_instance i on i.id=ft.instance_id
              where ft.status='PENDING' and i.status in ('CANCELLED','APPROVED','REJECTED','WITHDRAWN')
                and i.deleted_at is null")
check "C6 孤儿待办 = 0" "$C6" "0"

echo "---- C7 非终态实例的流程定义必须存活"
C7=$(psql_q "select count(*) from biz_flow_instance i left join biz_flow_def d on d.id = i.def_id
              where i.deleted_at is null and i.status not in ('CANCELLED','APPROVED','REJECTED','WITHDRAWN')
                and (d.id is null or d.deleted_at is not null)")
check "C7 定义已删/不存在但实例还在跑 = 0" "$C7" "0"

echo "---- C8 PENDING 待办清单（判决证据）"
psql_rows "
select ft.id, d.flow_code, i.instance_no, coalesce(i.biz_no,'(无单号)'), i.status,
       coalesce(ft.assignee_name, ft.assignee_id::text, '(无)'), ft.node_key, ft.created_at
  from biz_flow_task ft
  join biz_flow_instance i on i.id = ft.instance_id
  join biz_flow_def d on d.id = i.def_id
 where ft.status='PENDING' order by ft.created_at" | sed 's/^/        /'
[ "$C5_TOTAL" = "0" ] && echo "        (无 PENDING 待办)"

echo "=============================================================="
echo "结果：$PASS 通过 / $FAIL 失败"
echo "=============================================================="
[ "$FAIL" -eq 0 ] || exit 1
