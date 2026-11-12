#!/usr/bin/env bash
# ============================================================================
# HR-40 迁移 V0397 的「幂等 + down 可回滚」实测（演练库同样建在 145）
#
#   V0397 改的是**索引谓词**（uk_biz_sal_sheet_period → 排除 REVERSED），
#   这类迁移最怕两件事：
#     ① 重复执行时炸（先删后建写反了顺序 / 没写 IF EXISTS）；
#     ② 回滚脚本建不回严格唯一键，或者在有数据时硬建索引报 PG 原文。
#   本脚本按「up → up（重复）→ down → up → down（重复）」走一圈，每步查
#   pg_indexes.indexdef 断言谓词，最后 DROP 演练库。
#
# 用法：
#   export PGPASSWORD=$(ssh root@172.18.10.145 \
#     'grep "^NIK_DATABASE_PASSWORD" /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2-')
#   bash 04_migration_idempotent.sh
# ============================================================================
set -uo pipefail

ROOT=${ROOT:-/opt/cloudpivot}
MROOT=$ROOT/backend/migrations
UP=$MROOT/business/V0397__hr_salary_period_after_reverse.up.sql
DOWN=$MROOT/business/V0397__hr_salary_period_after_reverse.down.sql
DBHOST=${DBHOST:-172.18.10.145}
DBUSER=${DBUSER:-cloudpivot}
DB=${DB:-hr40_mig_drill}

if [ -z "${PGPASSWORD:-}" ]; then
    echo "FAIL: 未设置 PGPASSWORD"; exit 2
fi
export PGPASSWORD

q()   { psql -h "$DBHOST" -U "$DBUSER" -d "$DB" -tAc "$1" 2>&1; }
adm() { psql -h "$DBHOST" -U "$DBUSER" -d postgres -tAc "$1" 2>&1; }
qf()  { psql -h "$DBHOST" -U "$DBUSER" -d "$DB" -q -X -v ON_ERROR_STOP=1 --single-transaction -f "$1" 2>&1; }

FAILS=0
ok()  { printf '  ✓ %s\n' "$1"; }
bad() { printf '  ✗ %s\n' "$1"; FAILS=$((FAILS+1)); }
eq()  { if [ "$2" = "$3" ]; then ok "$1（$2）"; else bad "$1：期望 [$3] 实际 [$2]"; fi; }
def() { q "SELECT indexdef FROM pg_indexes WHERE indexname='uk_biz_sal_sheet_period'"; }
has_reversed() { case "$(def)" in *REVERSED*) echo t ;; *) echo f ;; esac; }
has_softdel() { case "$(def)" in *deleted_at*) echo t ;; *) echo f ;; esac; }

cleanup(){ adm "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1; }
trap cleanup EXIT

echo "==> 建演练库 $DB（先 DROP 残留）"
cleanup
adm "CREATE DATABASE $DB" >/dev/null 2>&1 || { echo "FAIL: 建库失败"; exit 2; }

echo "==> 回放全部迁移（V0397 首次执行 = up 第 1 次）"
n=0; failed=0
while IFS=$' \t' read -r ver f; do
    if ! qf "$f" >/dev/null 2>/tmp/hr40_mig_err.log; then
        echo "FAIL: 迁移 v$ver $(basename "$f")"; grep -E "ERROR|FATAL" /tmp/hr40_mig_err.log | head -3
        failed=1; break
    fi
    n=$((n+1))
done < <(ls "$MROOT"/*/*.up.sql | sed -E 's#.*/V([0-9]{4,})__.*#\1 &#' | sort -n)
[ "$failed" = 1 ] && exit 2
echo "    迁移 $n 条完成"
eq "首跑后谓词排除 REVERSED" "$(has_reversed)" "t"

echo "==> up 第 2 次（幂等：先 DROP IF EXISTS 再 CREATE IF NOT EXISTS）"
if qf "$UP" >/dev/null 2>/tmp/hr40_mig_up2.log; then ok "up 重复执行不报错"; else bad "up 重复执行失败"; grep ERROR /tmp/hr40_mig_up2.log | head -3; fi
eq "重复执行后谓词仍是排除 REVERSED" "$(has_reversed)" "t"
eq "重复执行后软删条件仍在" "$(has_softdel)" "t"

echo "==> down（还原成严格唯一键）"
if qf "$DOWN" >/dev/null 2>/tmp/hr40_mig_down.log; then ok "down 执行成功"; else bad "down 执行失败"; grep ERROR /tmp/hr40_mig_down.log | head -3; fi
eq "down 后谓词不再排除 REVERSED" "$(has_reversed)" "f"
eq "down 后仍是唯一索引" \
   "$(q "SELECT count(*) FROM pg_indexes WHERE indexname='uk_biz_sal_sheet_period'")" "1"

echo "==> down 再执行一次（幂等）"
if qf "$DOWN" >/dev/null 2>/tmp/hr40_mig_down2.log; then ok "down 重复执行不报错"; else bad "down 重复执行失败"; grep ERROR /tmp/hr40_mig_down2.log | head -3; fi

echo "==> down 的数据护栏：制造「同期间 1 张有效 + 1 张红冲」后必须 RAISE（不是硬建索引报 PG 原文）"
# 先回到放宽形态（否则严格唯一键连第二张都插不进去，测不到护栏）
qf "$UP" >/dev/null 2>&1
q "INSERT INTO biz_salary_sheet (org_id, sheet_no, period_code, period_start, period_end)
   VALUES ('00000000-0000-4000-8000-0000000f0398'::uuid, 'SELFCHECK-0397-A', '2099-02', DATE '2099-02-01', DATE '2099-02-28')" >/dev/null
q "INSERT INTO biz_salary_sheet (org_id, sheet_no, period_code, period_start, period_end, status, reverse_reason)
   VALUES ('00000000-0000-4000-8000-0000000f0398'::uuid, 'SELFCHECK-0397-B', '2099-02', DATE '2099-02-01', DATE '2099-02-28', 'REVERSED', 'V0397 护栏自检')" >/dev/null
eq "同期间已造出 2 张未删除单（1 有效 + 1 红冲）" \
   "$(q "SELECT count(*) FROM biz_salary_sheet WHERE org_id = '00000000-0000-4000-8000-0000000f0398'::uuid")" "2"
if qf "$DOWN" >/tmp/hr40_mig_guard.log 2>&1; then
    bad "有重复单时 down 竟然成功了（护栏失效）"
else
    if grep -q "拒绝回滚" /tmp/hr40_mig_guard.log; then ok "down 在有重复单时拒绝回滚（报文可操作）"; else bad "down 失败了但不是护栏报文"; head -3 /tmp/hr40_mig_guard.log; fi
fi
q "DELETE FROM biz_salary_sheet WHERE org_id = '00000000-0000-4000-8000-0000000f0398'::uuid" >/dev/null
eq "探针行已清干净" \
   "$(q "SELECT count(*) FROM biz_salary_sheet WHERE org_id = '00000000-0000-4000-8000-0000000f0398'::uuid")" "0"

echo "==> up 再跑一次（回到本卡目标形态）"
if qf "$UP" >/dev/null 2>/tmp/hr40_mig_up3.log; then ok "up 再次执行成功"; else bad "up 再次执行失败"; grep ERROR /tmp/hr40_mig_up3.log | head -3; fi
eq "谓词回到排除 REVERSED" "$(has_reversed)" "t"

echo "==> 清理"
cleanup
eq "演练库已 DROP" "$(adm "SELECT count(*) FROM pg_database WHERE datname='$DB'")" "0"

echo
if [ "$FAILS" = 0 ]; then echo "ALL PASS（0 失败）"; exit 0; else echo "FAILED: $FAILS 项"; exit 1; fi
