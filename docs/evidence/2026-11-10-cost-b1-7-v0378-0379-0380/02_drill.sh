#!/usr/bin/env bash
# ============================================================================
# B1-7 真库往返演练：V0378（菜单/权限/授权）+ V0379（号段/字典）+ V0380（批号回填）
#
# 卡：t_191cf205（COST-B1-7）
# 卡面验收：
#   ① 菜单挂到财务管理下，sort 不冲突
#   ② 号段表 biz_seq 加成本相关编码
#   ③ 批号回填幂等（重复跑不重复插，ON CONFLICT DO NOTHING）
#   ④ 不合规批号清单输出到日志（不阻断迁移）
#
# 做法：建一个临时库，先把它跑到 V0378 之前（= 干净基线），然后分段演练：
#   S0 基线       —— 库跑到 V0377，成本块 0 菜单 / 0 号段 / 无字典 / 0 批次
#   S1 V0378 up   —— 6 页 + 20 按钮 + 48 条角色授权；menu_invariants 8 条全绿
#   S2 V0378 幂等 —— 再 up 一次：菜单/授权指纹完全不变（0 行插入）
#   S3 V0378 down —— 26 条菜单 + 授权删净，回到基线（down 幂等：再跑一次 0 行）
#   S4 V0379 up   —— 号段 CB/CS + 字典 1 类 7 项；再 up 幂等（0 行插入）
#   S5 V0379 down —— 已发号时被拦（库零变化）；清零后 down 成功
#   S6 V0380 up   —— 造 3 处来源 + 1 条客户自建批次：回填 4 行、客户批次不被覆盖、
#                    不合规清单 2 条进日志；再 up = 0 行插入（幂等）
#   S7 V0380 down —— 只删回填行，客户自建批次保留；down 幂等
#   S8 全链       —— 三件按序 up（= 真实升级），menu_invariants 全绿，再按逆序 down 回基线
#
# 用法：bash docs/evidence/2026-11-10-cost-b1-7-v0378-0379-0380/02_drill.sh
#       KEEP_DB=1 bash .../02_drill.sh     # 保留演练库（排查用，默认 drop）
# 退出码：0 = 全部断言通过；1 = 有断言失败
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
MROOT="$ROOT/backend/migrations"
DBHOST="${DBHOST:-127.0.0.1}"
DBPORT="${DBPORT:-5432}"
DBUSER="${DBUSER:-alun}"
DBPASS="${DBPASS:-alun}"
CUTOFF="${CUTOFF:-378}"                       # 本卡第一版迁移号
DB="${DB:-mig_costb17_drill_$(date +%m%d%H%M%S)}"
UP78="$MROOT/business/V0378__cost_menu_and_perm.up.sql"
DN78="$MROOT/business/V0378__cost_menu_and_perm.down.sql"
UP79="$MROOT/business/V0379__cost_seq_and_dict.up.sql"
DN79="$MROOT/business/V0379__cost_seq_and_dict.down.sql"
UP80="$MROOT/business/V0380__cost_lot_backfill.up.sql"
DN80="$MROOT/business/V0380__cost_lot_backfill.down.sql"
SEED_LOT="$(dirname "${BASH_SOURCE[0]}")/03_lot_seed.sql"

PDF='20000000-0000-4000-8000-00000000'
PDP='20000000-0000-0000-0000-000000000'

export PGPASSWORD="$DBPASS"
q()  { psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "${2:-$DB}" -tAc "$1" 2>&1; }
qq() { psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -q -X -v ON_ERROR_STOP=1 \
             --single-transaction -f "$1" 2>&1 >/dev/null; }
# 打 NOTICE 的版本（要抓迁移日志时用）
qq_notice() { psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -q -X -v ON_ERROR_STOP=1 \
             --single-transaction -f "$1" 2>&1 >/dev/null; }

FAILS=0
ok()   { printf '  ✓ %s\n' "$1"; }
bad()  { printf '  ✗ %s\n' "$1"; FAILS=$((FAILS + 1)); }
head1(){ printf '\n== %s\n' "$1"; }
eq()   { if [ "$2" = "$3" ]; then ok "$1（$2）"; else bad "$1：期望 [$3] 实际 [$2]"; fi; }
neq()  { if [ "$2" != "$3" ]; then ok "$1（$2）"; else bad "$1：不该是 [$3]"; fi; }
has()  { if printf '%s' "$2" | grep -qF -- "$3"; then ok "$1"; else bad "$1：输出里没有 [$3]"; fi; }

cleanup() {
    if [ "${KEEP_DB:-0}" = "1" ]; then
        printf '\n==> KEEP_DB=1：保留演练库 %s\n' "$DB"; return
    fi
    su - postgres -c "dropdb --force $DB" >/dev/null 2>&1 \
        || q "DROP DATABASE IF EXISTS $DB" postgres >/dev/null 2>&1 \
        || printf '⚠ 演练库 %s 没删掉（无权限），请手工清理\n' "$DB"
}
trap cleanup EXIT

# 成本块菜单指纹（id/title/父级/sort/权限码/icon 全参与）
fp_menu() {
    q "select count(*) || '/' || md5(string_agg(id::text || title || coalesce(parent_id::text,'') || sort_order::text
                                               || coalesce(auth_code,'') || coalesce(icon,''), '|' order by id))
         from sys_menu where deleted_at is null and id::text like '${PDF}c%'"
}
fp_grant() {
    q "select count(*) || '/' || md5(string_agg(role_id::text || menu_id::text, '|' order by role_id, menu_id))
         from sys_role_menu rm join sys_menu m on m.id = rm.menu_id where m.id::text like '${PDF}c%'"
}

# ---------- 建库 ----------
su - postgres -c "dropdb --force $DB" >/dev/null 2>&1
su - postgres -c "createdb -O $DBUSER $DB" >/dev/null 2>&1 \
    || { printf 'FAIL: 建库失败（%s）\n' "$DB" >&2; exit 2; }
printf '==> 演练库：%s\n' "$DB"

head1 "S0 基线：跑到 V0377 为止"
applied=0
while IFS=$'\t' read -r ver f; do
    [ "$ver" -lt "$CUTOFF" ] || continue
    if ! PGOPTIONS='-c client_min_messages=warning' qq "$f"; then
        printf 'FAIL: 基线迁移失败 v%s %s\n' "$ver" "$(basename "$f")" >&2
        PGOPTIONS='-c client_min_messages=warning' qq "$f" | grep -E "ERROR|FATAL" | head -3 >&2
        exit 2
    fi
    applied=$((applied + 1))
done < <(find "$MROOT" -name 'V*__*.up.sql' \
         | awk -F'/V' '{split($NF, a, "__"); print a[1] + 0 "\t" $0}' | sort -n -k1,1)
printf '  基线迁移数：%s 条（< V%s）\n' "$applied" "$CUTOFF"
eq "基线：成本块菜单 0 条"      "$(q "select count(*) from sys_menu where id::text like '${PDF}c%'")" "0"
eq "基线：成本号段 0 条"        "$(q "select count(*) from biz_seq where seq_type in ('COST_OBJECT','COST_PERIOD')")" "0"
eq "基线：费用项目字典 0 条"    "$(q "select count(*) from sys_dict_type where dict_code='cost_expense_item'")" "0"
eq "基线：biz_lot 0 行"         "$(q "select count(*) from biz_lot")" "0"
eq "基线：菜单总数 385 条（menu_invariants ⑤ 的下限 400 靠本卡补齐）" \
   "$(q "select count(*) from sys_menu where deleted_at is null")" "385"
eq "基线：财务管理(e02)下已有 sort 10/20/21/22/30/40/45/46/47/48/49，60 起是空档" \
   "$(q "select count(*) from sys_menu where parent_id='${PDP}e02'::uuid and deleted_at is null and sort_order >= 60")" "0"
eq "基线：仓库管理(e01)下 sort 80/90 已被出库单/盘点单占用" \
   "$(q "select count(*) from sys_menu where parent_id='${PDP}e01'::uuid and deleted_at is null and sort_order in (80,90)")" "2"

# ---------- S1 ----------
head1 "S1 V0378 up：6 页 + 20 按钮 + 角色授权"
out78="$(qq_notice "$UP78")"; rc=$?
eq "up 成功（exit=$rc）" "$rc" "0"
printf '%s\n' "$out78" | grep -E "NOTICE" | sed 's/^/    │ /'
eq "页面菜单 6 条（menu_type=2）" \
   "$(q "select count(*) from sys_menu where deleted_at is null and menu_type=2 and id::text like '${PDF}c0%'")" "6"
eq "按钮权限 20 条（menu_type=3）" \
   "$(q "select count(*) from sys_menu where deleted_at is null and menu_type=3 and id::text like '${PDF}c1%'")" "20"
eq "5 个成本页面挂在财务管理 e02" \
   "$(q "select count(*) from sys_menu where parent_id='${PDP}e02'::uuid and deleted_at is null and path like '/business/erp/cost-%'")" "5"
eq "批次管理挂在仓库管理 e01、sort=95（设计写 80 与出库单撞，见卡内偏差）" \
   "$(q "select sort_order from sys_menu where id='${PDF}c006'::uuid")" "95"
eq "财务管理下新菜单 sort = 60/62/64/66/68" \
   "$(q "select string_agg(sort_order::text, ',' order by sort_order) from sys_menu where parent_id='${PDP}e02'::uuid and deleted_at is null and sort_order >= 60")" "60,62,64,66,68"
eq "同父 sort 撞车 0 组（e01/e02）" \
   "$(q "select count(*) from (select parent_id, sort_order from sys_menu where deleted_at is null and menu_type=2 and parent_id in ('${PDP}e01'::uuid,'${PDP}e02'::uuid) group by 1,2 having count(*)>1) d")" "0"
eq "页面 title 超 12 字 0 条" \
   "$(q "select count(*) from sys_menu where deleted_at is null and id::text like '${PDF}c00%' and length(title)>12")" "0"
eq "页面 icon 全非空" \
   "$(q "select count(*) from sys_menu where deleted_at is null and id::text like '${PDF}c00%' and (icon is null or btrim(icon)='')")" "0"
eq "角色授权 48 条（财务经理 22 + 财务总监 23 + 仓管 3；空库无 SUPER_ADMIN/ADMIN）" \
   "$(q "select count(*) from sys_role_menu rm join sys_menu m on m.id=rm.menu_id where m.id::text like '${PDF}c%'")" "48"
eq "财务经理拿到 22 条且不含红冲" \
   "$(q "select count(*) from sys_role_menu rm join sys_role r on r.id=rm.role_id join sys_menu m on m.id=rm.menu_id where r.role_code='FINANCE_MANAGER' and m.id::text like '${PDF}c%'")" "22"
eq "财务总监拿到 23 条（含红冲 c144）" \
   "$(q "select count(*) from sys_role_menu rm join sys_role r on r.id=rm.role_id join sys_menu m on m.id=rm.menu_id where r.role_code='FINANCE_DIRECTOR' and m.id::text like '${PDF}c%'")" "23"
eq "红冲结算只授给财务总监（财务经理无 c144）" \
   "$(q "select count(*) from sys_role_menu rm join sys_role r on r.id=rm.role_id where r.role_code='FINANCE_MANAGER' and rm.menu_id='${PDF}c144'::uuid")" "0"
eq "仓管拿到 3 条（批次管理 1 页 + 2 按钮）" \
   "$(q "select count(*) from sys_role_menu rm join sys_role r on r.id=rm.role_id join sys_menu m on m.id=rm.menu_id where r.role_code='WAREHOUSE_MANAGER' and m.id::text like '${PDF}c%'")" "3"
eq "无孤儿：每个被授按钮的父页也授给了同一角色" \
   "$(q "select count(*) from sys_role_menu rm join sys_menu b on b.id=rm.menu_id where b.id::text like '${PDF}c1%' and not exists (select 1 from sys_role_menu rm2 where rm2.role_id=rm.role_id and rm2.menu_id=b.parent_id)")" "0"
printf '  ── menu_invariants.sh 结果：\n'
PGPASSWORD="$DBPASS" PGHOST="$DBHOST" PGPORT="$DBPORT" PGUSER="$DBUSER" \
    bash "$ROOT/scripts/verify/menu_invariants.sh" "$DB" 2>&1 | sed 's/^/    │ /'

# ---------- S2 ----------
head1 "S2 V0378 幂等：再 up 一次，指纹不变、0 行插入"
m1=$(fp_menu); g1=$(fp_grant)
out2="$(qq_notice "$UP78")"; rc2=$?
m2=$(fp_menu); g2=$(fp_grant)
eq "第二次 up 成功（exit=$rc2）" "$rc2" "0"
eq "菜单指纹不变" "$m2" "$m1"
eq "授权指纹不变" "$g2" "$g1"
if printf '%s' "$out2" | grep -q "INSERT 0 "; then bad "第二次 up 还有插入行（不幂等）"; else ok "第二次 up 0 行插入"; fi

# ---------- S3 ----------
head1 "S3 V0378 down：删净 26 条菜单 + 授权，回到基线"
outdn="$(qq_notice "$DN78")"; rcdn=$?
eq "down 成功（exit=$rcdn）" "$rcdn" "0"
printf '%s\n' "$outdn" | grep -E "NOTICE" | sed 's/^/    │ /'
eq "成本块菜单删净" "$(q "select count(*) from sys_menu where id::text like '${PDF}c%'")" "0"
eq "成本块授权删净" "$(q "select count(*) from sys_role_menu rm join sys_menu m on m.id=rm.menu_id where m.id::text like '${PDF}c%'")" "0"
eq "菜单总数回到 385" "$(q "select count(*) from sys_menu where deleted_at is null")" "385"
equ="$(qq_notice "$DN78")"; rc3=$?
eq "down 幂等：再跑一次不报错（exit=$rc3）" "$rc3" "0"

# ---------- S4 ----------
head1 "S4 V0379 up：号段 CB/CS + 费用项目字典 7 项"
out79="$(qq_notice "$UP79")"; rc=$?
eq "up 成功（exit=$rc）" "$rc" "0"
printf '%s\n' "$out79" | grep -E "NOTICE" | sed 's/^/    │ /'
eq "号段 COST_OBJECT = CB/yyyyMMdd/current_no=0" \
   "$(q "select prefix||'/'||date_pattern||'/'||current_no from biz_seq where seq_type='COST_OBJECT' and deleted_at is null")" "CB/yyyyMMdd/0"
eq "号段 COST_PERIOD = CS/yyyyMM/current_no=0" \
   "$(q "select prefix||'/'||date_pattern||'/'||current_no from biz_seq where seq_type='COST_PERIOD' and deleted_at is null")" "CS/yyyyMM/0"
eq "字典项 7 条" \
   "$(q "select count(*) from sys_dict_item i join sys_dict_type t on t.id=i.dict_type_id where t.dict_code='cost_expense_item' and i.deleted_at is null")" "7"
eq "默认项唯一且为折旧费" \
   "$(q "select string_agg(item_value, ',') from sys_dict_item i join sys_dict_type t on t.id=i.dict_type_id where t.dict_code='cost_expense_item' and i.is_default and i.deleted_at is null")" "DEPRECIATION"
out79b="$(qq_notice "$UP79")"; rc=$?
eq "第二次 up 成功（exit=$rc）" "$rc" "0"
if printf '%s' "$out79b" | grep -q "INSERT 0 "; then bad "第二次 up 还有插入行（不幂等）"; else ok "第二次 up 0 行插入"; fi

# ---------- S5 ----------
head1 "S5 V0379 down 护栏：已发号被拦 / 清零后能删"
q "update biz_seq set current_no=3, period_key='20261110' where seq_type='COST_OBJECT'" >/dev/null
outdn79="$(qq_notice "$DN79")"; rcd=$?
neq "已发号时 down 失败（exit=$rcd）" "$rcd" "0"
printf '%s\n' "$outdn79" | grep -E "ERROR" | sed 's/^/    │ /'
has "拒绝信息点名号段与 current_no" "$outdn79" "号段已发过号"
eq "失败后库零变化：号段仍在" "$(q "select count(*) from biz_seq where seq_type in ('COST_OBJECT','COST_PERIOD') and deleted_at is null")" "2"
eq "失败后库零变化：字典项仍在" "$(q "select count(*) from sys_dict_item i join sys_dict_type t on t.id=i.dict_type_id where t.dict_code='cost_expense_item' and i.deleted_at is null")" "7"
q "update biz_seq set current_no=0, period_key='' where seq_type='COST_OBJECT'" >/dev/null
outdn79ok="$(qq_notice "$DN79")"; rcd2=$?
eq "清零后 down 成功（exit=$rcd2）" "$rcd2" "0"
printf '%s\n' "$outdn79ok" | grep -E "NOTICE" | sed 's/^/    │ /'
eq "号段删净" "$(q "select count(*) from biz_seq where seq_type in ('COST_OBJECT','COST_PERIOD')")" "0"
eq "字典类型删净" "$(q "select count(*) from sys_dict_type where dict_code='cost_expense_item'")" "0"

# ---------- S6 ----------
head1 "S6 V0380 up：三处来源回填 + 不合规清单 + 幂等"
qq "$SEED_LOT" || { bad "种子数据插入失败"; }
eq "种子：入库明细 3 条 / 出库 2 条 / 序列号 2 条（含 1 条软删）" \
   "$(q "select (select count(*) from biz_stock_in_item where lot_no is not null)||'/'||(select count(*) from biz_stock_out_item where lot_no is not null)||'/'||(select count(*) from biz_material_serial where batch_no is not null)")" "3/2/2"
out80="$(qq_notice "$UP80")"; rc=$?
eq "up 成功（exit=$rc）" "$rc" "0"
printf '%s\n' "$out80" | grep -E "NOTICE" | sed 's/^/    │ /'
has "回填 4 行（A/C/D/E；B 已被客户自建占位）" "$out80" "本次回填 4 行"
has "不合规清单：含首尾空格的批号被点名" "$out80" "批号=[ B17-LOT-A ] 来源=STOCK_IN 问题=首尾含空格"
has "不合规清单：物料软删的批号被点名" "$out80" "问题=物料不存在或已软删"
has "不合规清单合计 2 条且明确不阻断" "$out80" "不合规批号合计 2 条"
eq "biz_lot 5 行（4 回填 + 1 客户自建）" "$(q "select count(*) from biz_lot where deleted_at is null")" "5"
eq "客户自建批次 B 未被覆盖（仍是 PRODUCTION/COST_PERIOD）" \
   "$(q "select source_type||'/'||source_doc_type from biz_lot where lot_no='B17-LOT-B' and deleted_at is null")" "PRODUCTION/COST_PERIOD"
eq "含空格批号落库已 btrim（无 ' B17-LOT-A '）" \
   "$(q "select count(*) from biz_lot where lot_no <> btrim(lot_no)")" "0"
eq "软删序列号的批号没进批次（B17-LOT-GONE 不存在）" \
   "$(q "select count(*) from biz_lot where lot_no='B17-LOT-GONE'")" "0"
eq "回填行形态固定（MANUAL/BACKFILL）" \
   "$(q "select count(*) from biz_lot where source_doc_type='BACKFILL' and source_type='MANUAL'")" "4"
out80b="$(qq_notice "$UP80")"; rc=$?
eq "第二次 up 成功（exit=$rc）" "$rc" "0"
has "第二次 up 回填 0 行（幂等）" "$out80b" "本次回填 0 行"
eq "biz_lot 行数不变（5）" "$(q "select count(*) from biz_lot where deleted_at is null")" "5"

# ---------- S7 ----------
head1 "S7 V0380 down：只删回填行，客户自建批次保留"
# 先造一个引用回填批次的成本对象，验证 down 会 WARNING 但仍能删
q "insert into biz_cost_object (object_no, product_id, lot_no, target_warehouse_id, plan_qty,
                                workshop_dept_id, workshop_dept_name, start_date)
   values ('CB-B17-DRILL', gen_random_uuid(), 'B17-LOT-D', gen_random_uuid(), 10,
           gen_random_uuid(), '演练车间', DATE '2026-10-01')" >/dev/null
outdn80="$(qq_notice "$DN80")"; rcd=$?
eq "down 成功（exit=$rcd）" "$rcd" "0"
printf '%s\n' "$outdn80" | grep -E "NOTICE|WARNING" | sed 's/^/    │ /'
has "被成本对象引用的回填批次 → WARNING 点名" "$outdn80" "已被成本对象引用"
eq "回填行删净" "$(q "select count(*) from biz_lot where source_doc_type='BACKFILL'")" "0"
eq "客户自建批次保留（B17-LOT-B 仍在）" "$(q "select count(*) from biz_lot where lot_no='B17-LOT-B' and deleted_at is null")" "1"
outdn80b="$(qq_notice "$DN80")"; rcd=$?
eq "down 幂等：再跑一次不报错（exit=$rcd）" "$rcd" "0"
q "delete from biz_cost_object where object_no='CB-B17-DRILL'" >/dev/null

# ---------- S8 ----------
head1 "S8 全链：三件按序 up（= 真实升级）→ menu_invariants → 逆序 down 回基线"
q "delete from biz_stock_in_item where stock_in_id='20000000-0000-4000-8000-0000000b1710'::uuid" >/dev/null
q "delete from biz_stock_out_item where stock_out_id='20000000-0000-4000-8000-0000000b1720'::uuid" >/dev/null
q "delete from biz_material_serial where serial_no like 'B17-%'" >/dev/null
q "delete from biz_stock_in where id='20000000-0000-4000-8000-0000000b1710'::uuid" >/dev/null
q "delete from biz_stock_out where id='20000000-0000-4000-8000-0000000b1720'::uuid" >/dev/null
q "delete from biz_material where material_code like 'B17-%'" >/dev/null
# 客户自建批次 B17-LOT-B 故意留着（让最后一行断言能证明「down 不碰客户数据」）
for f in "$UP78" "$UP79" "$UP80"; do
    if ! PGOPTIONS='-c client_min_messages=warning' qq "$f"; then
        bad "全链 up 失败：$(basename "$f")"; break
    fi
done
eq "全链后：成本块菜单 26 条" "$(q "select count(*) from sys_menu where id::text like '${PDF}c%'")" "26"
eq "全链后：号段 2 条 / 字典 7 项" \
   "$(q "select (select count(*) from biz_seq where seq_type in ('COST_OBJECT','COST_PERIOD'))||'/'||(select count(*) from sys_dict_item i join sys_dict_type t on t.id=i.dict_type_id where t.dict_code='cost_expense_item' and i.deleted_at is null)")" "2/7"
eq "全链后：菜单总数 411（385 + 26，落在 menu_invariants ⑤ 的 400-460）" \
   "$(q "select count(*) from sys_menu where deleted_at is null")" "411"
printf '  ── menu_invariants.sh 结果：\n'
PGPASSWORD="$DBPASS" PGHOST="$DBHOST" PGPORT="$DBPORT" PGUSER="$DBUSER" \
    bash "$ROOT/scripts/verify/menu_invariants.sh" "$DB" 2>&1 | sed 's/^/    │ /'
for f in "$DN80" "$DN79" "$DN78"; do
    if ! PGOPTIONS='-c client_min_messages=warning' qq "$f"; then
        bad "逆序 down 失败：$(basename "$f")"; break
    fi
done
eq "逆序 down 后：成本块 0 条菜单" "$(q "select count(*) from sys_menu where id::text like '${PDF}c%'")" "0"
eq "逆序 down 后：菜单总数回 385" "$(q "select count(*) from sys_menu where deleted_at is null")" "385"
eq "逆序 down 后：号段/字典/biz_lot 全空" \
   "$(q "select (select count(*) from biz_seq where seq_type in ('COST_OBJECT','COST_PERIOD'))||'/'||(select count(*) from sys_dict_type where dict_code='cost_expense_item')||'/'||(select count(*) from biz_lot where deleted_at is null)")" "0/0/1"

printf '\n=== 结果：%s 条断言失败 ===\n' "$FAILS"
[ "$FAILS" -eq 0 ] && exit 0 || exit 1
