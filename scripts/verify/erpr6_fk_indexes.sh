#!/usr/bin/env bash
# ============================================================
# ERP-R6 验收：12 个外键索引（**在生产机 144 上跑**，读 /etc/cloudpivot/cloudpivot.env）
#
#   bash scripts/verify/erpr6_fk_indexes.sh
#
# 只做**只读**动作：查 pg_indexes / pg_constraint / EXPLAIN，不写业务数据。
#
# 断言：
#   ① V0186 迁移记录 success
#   ② 12 个索引逐个存在，且「部分索引谓词」与列的 NOT NULL 属性一致
#      （可空列必须带 WHERE col IS NOT NULL；NOT NULL 列不许带，否则索引永远用不上）
#   ③ 卡面验收复查：biz_% 里「外键列上无索引」= 0 行
#   ④ 12 列逐个 EXPLAIN（enable_seqscan=off）必须命中各自索引
#   ⑤ 反连接 / join 等值也必须命中（外键索引的真实用途）
#   ⑥ 索引体积报告（部分索引省空间的效果，仅供参考不计分）
#
# 为什么 EXPLAIN 要关 seqscan：生产表最大 947 行，规划器在这么小的表上**理性地**
# 选 Seq Scan（代价更低）。关掉 seqscan 才能验证「索引存在且可被采用」——
# 这是本卡要证明的东西；数据量起来后规划器自己会切回 Index Scan。
# ============================================================
set -uo pipefail
# 生产默认读 /etc/cloudpivot/cloudpivot.env；干跑（隔离库/副本库）可用 ENV_FILE 覆盖
ENV_FILE=${ENV_FILE:-/etc/cloudpivot/cloudpivot.env}
if [ -f "$ENV_FILE" ]; then
    set -a
    . "$ENV_FILE"
    set +a
fi
PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); echo "[PASS] $*"; }
bad() { FAIL=$((FAIL + 1)); echo "[FAIL] $*"; }
expect() { if [ "$2" = "$3" ]; then ok "$1（$3）"; else bad "$1（期望 $2，实际 $3）"; fi; }

PSQLA() { PGPASSWORD=$NIK_DATABASE_PASSWORD psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -Atc "$1" 2>&1; }
# EXPLAIN（关 seqscan），只输出「命中/未命中 索引名」
PLAN() {
    PGPASSWORD=$NIK_DATABASE_PASSWORD psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -Atc \
        "set enable_seqscan=off; set enable_bitmapscan=off; explain (costs off) $1" 2>&1 | tr '\n' ' '
}

echo "== 库 $NIK_DATABASE_NAME @ 127.0.0.1"

# ---------- ① 迁移记录 ----------
expect "V0186 迁移记录 success" "t" "$(PSQLA "select success from schema_migrations where name='fk_indexes'")"

# ---------- ② 12 个索引 + 谓词 ----------
# 格式：索引名|表|列|是否部分索引(1/0)
TARGETS="
idx_biz_flow_instance_prev_instance|biz_flow_instance|prev_instance_id|1
idx_biz_flow_instance_version_id|biz_flow_instance|version_id|0
idx_biz_flow_instance_node_node_id|biz_flow_instance_node|node_id|1
idx_biz_flow_task_transferred_from|biz_flow_task|transferred_from|1
idx_biz_stock_transfer_transit_warehouse_id|biz_stock_transfer|transit_warehouse_id|1
idx_biz_stock_transfer_item_from_location_id|biz_stock_transfer_item|from_location_id|1
idx_biz_stock_transfer_item_to_location_id|biz_stock_transfer_item|to_location_id|1
idx_biz_task_comment_parent_comment_id|biz_task_comment|parent_comment_id|1
idx_biz_task_comment_user_id|biz_task_comment|user_id|0
idx_biz_task_template_owner_id|biz_task_template|owner_id|1
idx_biz_task_template_project_id|biz_task_template|project_id|1
idx_biz_task_worklog_user_id|biz_task_worklog|user_id|0
"
n_idx=0
while IFS='|' read -r idx tbl col partial; do
    [ -z "${idx:-}" ] && continue
    n_idx=$((n_idx + 1))
    def=$(PSQLA "select coalesce(indexdef,'') from pg_indexes where schemaname='public' and indexname='$idx'")
    if [ -z "$def" ]; then bad "索引缺失 $idx"; continue; fi
    # 列名与表名必须对得上（防止「索引建在别的列上」这种假绿）
    echo "$def" | grep -q "ON public.$tbl USING btree ($col)" \
        && ok "$idx 建在 $tbl($col)" || bad "$idx 定义不符：$def"
    # NULL 与否决定能不能做部分索引谓词
    nullable=$(PSQLA "select case when a.attnotnull then '0' else '1' end from pg_attribute a where a.attrelid='public.$tbl'::regclass and a.attname='$col'")
    if [ "$nullable" = "1" ]; then
        echo "$def" | grep -q "WHERE ($col IS NOT NULL)" \
            && ok "$idx 部分索引谓词正确（$col 可空）" || bad "$idx 缺 WHERE $col IS NOT NULL：$def"
        expect "$idx 应为部分索引（与卡面 $partial 一致）" "1" "$partial"
    else
        echo "$def" | grep -q " WHERE " \
            && bad "$idx 建在 NOT NULL 列上却带谓词（会漏用）：$def" || ok "$idx 全表索引（$col NOT NULL）"
    fi
done <<< "$TARGETS"
expect "目标索引个数" "12" "$n_idx"

# ---------- ③ 复查：0 个无索引外键 ----------
left=$(PSQLA "select count(*) from (
  select c.conrelid::regclass::text as tbl, a.attname as col
  from pg_constraint c
  join pg_attribute a on a.attrelid = c.conrelid and a.attnum = c.conkey[1]
  where c.contype='f'
    and not exists (select 1 from pg_index i where i.indrelid=c.conrelid and a.attnum=any(i.indkey))
    and c.conrelid::regclass::text like 'biz_%') t")
expect "复查：无索引业务外键（卡面验收=0）" "0" "$left"

# ---------- ④ 12 列 EXPLAIN 命中 ----------
while IFS='|' read -r idx tbl col partial; do
    [ -z "${idx:-}" ] && continue
    plan=$(PLAN "select * from $tbl where $col = '00000000-0000-0000-0000-000000000001'")
    echo "$plan" | grep -q "$idx" && ok "EXPLAIN 命中 $idx" || bad "EXPLAIN 未命中 $idx → $plan"
done <<< "$TARGETS"

# ---------- ⑤ 反连接 / join 等值（外键索引的真实用途） ----------
plan=$(PLAN "select p.id from biz_flow_instance p where not exists (select 1 from biz_flow_instance c where c.prev_instance_id = p.id)")
echo "$plan" | grep -q 'idx_biz_flow_instance_prev_instance' \
    && ok "反连接（not exists）命中 prev_instance 索引" || bad "反连接未命中 → $plan"
plan=$(PLAN "select t.id from biz_flow_task t join biz_flow_instance_node n on n.node_id = t.id")
echo "$plan" | grep -q 'idx_biz_flow_instance_node_node_id' \
    && ok "join 等值（var=var）命中 node_id 索引" || bad "join 等值未命中 → $plan"
plan=$(PLAN "select i.id from biz_stock_transfer_item i join biz_flow_task t on t.transferred_from = i.id")
echo "$plan" | grep -q 'idx_biz_flow_task_transferred_from' \
    && ok "join 等值命中 transferred_from 索引" || bad "join 等值 transferred_from 未命中 → $plan"

# ---------- ⑥ 报告（不计分） ----------
echo "== 索引体积 / 表行数（部分索引省空间的效果） =="
PSQLA "select 'idx '||i.indexname||' → '||pg_size_pretty(pg_relation_size(i.indexname::regclass))||'（表 '||c.relname||' 行数 '||(xpath('/row/c/text()', query_to_xml('select count(*) as c from '||c.relname, false, true, '')))[1]::text||'）'
from pg_indexes i join pg_class c on c.relname=i.tablename
where i.schemaname='public' and i.indexname like 'idx_biz_%'
  and i.indexname in ('idx_biz_flow_instance_prev_instance','idx_biz_flow_instance_version_id',
      'idx_biz_flow_instance_node_node_id','idx_biz_flow_task_transferred_from',
      'idx_biz_stock_transfer_transit_warehouse_id','idx_biz_stock_transfer_item_from_location_id',
      'idx_biz_stock_transfer_item_to_location_id','idx_biz_task_comment_parent_comment_id',
      'idx_biz_task_comment_user_id','idx_biz_task_template_owner_id',
      'idx_biz_task_template_project_id','idx_biz_task_worklog_user_id')
order by 1"

echo "== 12 个索引总体积 =="
PSQLA "select pg_size_pretty(sum(pg_relation_size(indexname::regclass))) from pg_indexes
 where schemaname='public' and indexname in ('idx_biz_flow_instance_prev_instance','idx_biz_flow_instance_version_id','idx_biz_flow_instance_node_node_id','idx_biz_flow_task_transferred_from','idx_biz_stock_transfer_transit_warehouse_id','idx_biz_stock_transfer_item_from_location_id','idx_biz_stock_transfer_item_to_location_id','idx_biz_task_comment_parent_comment_id','idx_biz_task_comment_user_id','idx_biz_task_template_owner_id','idx_biz_task_template_project_id','idx_biz_task_worklog_user_id')"

echo
echo "== 汇总：PASS=$PASS FAIL=$FAIL =="
[ "$FAIL" -eq 0 ] || exit 1
