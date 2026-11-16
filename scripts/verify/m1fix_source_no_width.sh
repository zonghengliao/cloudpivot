#!/usr/bin/env bash
# =============================================================================
# m1fix_source_no_width.sh —— V0419（biz_voucher.source_no VARCHAR(32) → 64）迁移验收
#
# 卡：t_112c3502（外币手工凭证 source_no 超 varchar(32) → 建单 500）
#
# 做什么（全部在**克隆库**上，脚本自己 DROP）：
#   ① 克隆模板库 → 修前现场：source_no = VARCHAR(32)，33 字单号必被 22001 拦
#   ② 跑 up.sql 两遍：放宽 + 自检一（两列 ≥64）+ 自检二（33/57 可写、65 被拦、探针零残留）
#   ③ 跑 down.sql：有 >32 字行时**拒绝**；清掉长行后能收窄；source_doc_no 不被误伤
#   ④ 再跑 up.sql 还原（证明同一文件可重放）
#
# 用法：
#   bash scripts/verify/m1fix_source_no_width.sh              # 99 上跑，连 145:5432
#   PGPASSWORD=... bash scripts/verify/m1fix_source_no_width.sh
# =============================================================================
set -uo pipefail

PG_HOST="${PG_HOST:-172.18.10.145}"
export PGPASSWORD="${PGPASSWORD:?需要 PG 口令（145 的 cloudpivot 用户）}"
PG_USER="${PG_USER:-cloudpivot}"
TEMPLATE="${TEMPLATE:-cloudpivot_test}"
DB="${DB:-cp_t112c3502_t1}"
MIG_DIR="${MIG_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../backend/app/migrations/business" && pwd)}"
UP="$MIG_DIR/V0419__voucher_source_no_width.up.sql"
DOWN="$MIG_DIR/V0419__voucher_source_no_width.down.sql"
KEEP="${KEEP:-0}"

PASS=0; FAIL=0
ok()  { PASS=$((PASS+1)); echo "  PASS  $*"; }
bad() { FAIL=$((FAIL+1)); echo "  FAIL  $*"; }
adm() { psql -h "$PG_HOST" -U "$PG_USER" -d postgres -tAc "$1"; }
q()   { psql -h "$PG_HOST" -U "$PG_USER" -d "$DB" -tAc "$1"; }
runf(){ psql -v ON_ERROR_STOP=1 -h "$PG_HOST" -U "$PG_USER" -d "$DB" -q -f "$1" 2>&1; }
width(){ q "select character_maximum_length from information_schema.columns
            where table_name='biz_voucher' and column_name='$1'"; }

cleanup() { [[ "$KEEP" == "1" ]] || adm "drop database if exists $DB" >/dev/null 2>&1; }
trap cleanup EXIT

echo "== 0 克隆 $TEMPLATE → $DB"
adm "drop database if exists $DB" >/dev/null
adm "create database $DB template $TEMPLATE" >/dev/null || { echo "FATAL 建库失败"; exit 2; }
ok "库已克隆（$(q 'select count(*) from schema_migrations') 条迁移记录）"

echo "== 1 修前现场"
[[ "$(width source_no)" == "32" ]] && ok "source_no = VARCHAR(32)（缺陷可复现）" || bad "source_no=$(width source_no)"
before="$(psql -v ON_ERROR_STOP=1 -h "$PG_HOST" -U "$PG_USER" -d "$DB" -q -c "
  insert into biz_voucher (voucher_word, voucher_no, period, voucher_date, voucher_type, source_type,
                           source_no, source_doc_no, summary, total_debit, total_credit, entry_count, status)
  values ('记','BEFORE0419','1999-01',DATE '1999-01-01','MANUAL','MANUAL',
          '手工凭证 1000.0000 USD（本位币 7200.0000）','x','__BEFORE__',1,1,2,'DRAFT')" 2>&1)"
echo "$before" | grep -q "value too long for type character varying(32)" \
  && ok "33 字 INSERT 被 22001 拦（就是 500 的根因）" || bad "修前竟能写：$before"

echo "== 2 up.sql 第 1 遍"
out="$(runf "$UP")"
echo "$out" | grep -q "V0419 自检二通过" && ok "自检一/二通过" || { bad "up 自检未通过"; echo "$out" | sed 's/^/      /'; }
[[ "$(width source_no)" == "64" ]] && ok "source_no → VARCHAR(64)" || bad "source_no=$(width source_no)"
[[ "$(width source_doc_no)" == "64" ]] && ok "source_doc_no 仍 64（V0357 原值，未动）" || bad "source_doc_no=$(width source_doc_no)"
[[ "$(q "select count(*) from biz_voucher where voucher_no like 'V0419%'")" == "0" ]] && ok "迁移探针零残留" || bad "探针残留"

echo "== 3 up.sql 第 2 遍（幂等）"
out2="$(runf "$UP")"
echo "$out2" | grep -q "已是 VARCHAR(64)，跳过" && ok "重复执行走跳过分支" || bad "重复执行不幂等"

echo "== 4 down.sql 数据护栏"
q "insert into biz_voucher (voucher_word, voucher_no, period, voucher_date, voucher_type, source_type,
     source_no, source_doc_no, summary, total_debit, total_credit, entry_count, status)
   values ('记','LONG0419','1999-01',DATE '1999-01-01','MANUAL','MANUAL',
     '手工凭证 1000.0000 USD（本位币 7200.0000）','x','__LONG__',1,1,2,'DRAFT')" >/dev/null
dout="$(runf "$DOWN")"
echo "$dout" | grep -q "拒绝执行" && ok "有 33 字行时 down 被拒（不静默截断业务单号）" || bad "down 竟然收窄成功"
[[ "$(width source_no)" == "64" ]] && ok "被拒后宽度仍 64（事务回滚干净）" || bad "宽度变 $(width source_no)"

echo "== 5 清长行后 down 能收窄，且不误伤 source_doc_no"
q "delete from biz_voucher where voucher_no='LONG0419'" >/dev/null
dout2="$(runf "$DOWN")"
echo "$dout2" | grep -q "V0419 down 完成" && ok "down 自检通过" || bad "down 未完成"
[[ "$(width source_no)" == "32" ]] && ok "source_no 回到 32" || bad "source_no=$(width source_no)"
[[ "$(width source_doc_no)" == "64" ]] && ok "source_doc_no 未被误伤" || bad "source_doc_no=$(width source_doc_no)"

echo "== 6 再跑 up.sql（还原，证明可重放）"
out3="$(runf "$UP")"
echo "$out3" | grep -q "V0419 自检二通过" && ok "32 → 64 重新放宽成功" || bad "重新放宽失败"
[[ "$(width source_no)" == "64" ]] && ok "库停在迁移后状态" || bad "宽度=$(width source_no)"

echo
echo "== 汇总：PASS=$PASS FAIL=$FAIL"
[[ "$FAIL" == "0" ]] || exit 1
