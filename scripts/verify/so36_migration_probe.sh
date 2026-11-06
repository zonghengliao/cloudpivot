#!/usr/bin/env bash
# SO-36 迁移往返探针：在**一个克隆库**上验 V0311 的 up → up(幂等) → down → down(幂等) → up
#
#   runuser -u postgres -- dropdb --if-exists cloudpivot_so36p
#   runuser -u postgres -- createdb -O alun -T cloudpivot_so36 cloudpivot_so36p
#   bash scripts/verify/so36_migration_probe.sh | tee docs/evidence/20260707-so36-commission/migration_probe.txt
#
# 只用 psql 直连（不走 Rust 迁移器）：验的是 SQL 本身幂等 + 自检块能拦住坏数据。
# 「迁移器真的按 kind=versioned / applied_via=executed 记账」由 so36_instance.sh 启动时实测（见 README）。
#
# ⚠ down 之前必须先清掉**负基数明细**：V0311 把 `ck_biz_cmitem_base` 放宽成「≠ 0」，
#   回滚要恢复 `>= 0`，有冲回行时 CHECK 会挡住（这是故意的：宁可回滚失败，也不要
#   把负数静默改成正数）。探针在 ③ 之前先删明细，正是为了走完这条真实路径。
set -uo pipefail
DB=${SO36_PROBE_DB:-cloudpivot_so36p}
URL=${SO36_PROBE_URL:-postgresql://alun:alun@127.0.0.1:5432/$DB}
DIR=/opt/cloudpivot/backend/migrations/business
export PGPASSWORD=alun
PSQL=(psql "$URL" -v ON_ERROR_STOP=1 -q)

say() { printf '\n### %s\n' "$1"; }
run() {
  local file="$1"
  echo "-- psql -f $file"
  if "${PSQL[@]}" -f "$DIR/$file" 2>&1 | grep -E "NOTICE|ERROR|FATAL" | sed 's/^psql:[^ ]* //'; then :; fi
}
state() {
  "${PSQL[@]}" -Atc "
    SELECT 'account_2241_02=' || count(*) FROM biz_account
     WHERE account_code = '2241.02' AND is_leaf = 1 AND deleted_at IS NULL;
    SELECT 'seq_cm=' || count(*) FROM biz_seq WHERE seq_type = 'CM' AND deleted_at IS NULL;
    SELECT 'base_check=' || pg_get_constraintdef(oid) FROM pg_constraint
     WHERE conname = 'ck_biz_cmitem_base';
    SELECT 'idx_cmitem_status=' || count(*) FROM pg_indexes
     WHERE indexname = 'idx_biz_cmitem_settlement_status';
    SELECT 'migration_row=' || kind || '|' || coalesce(applied_via,'?') || '|' || success
      FROM schema_migrations WHERE name = 'commission_settle_setup';"
}

say "库：$DB"
say "① up（第一轮）"
run V0311__commission_settle_setup.up.sql
state

say "② up（第二轮，必须幂等：无 ERROR，值不变）"
run V0311__commission_settle_setup.up.sql
state

say "③ 行为探针：负基数可插、0 被拒"
"${PSQL[@]}" -Atc "
INSERT INTO biz_commission_settlement (id, settlement_no, salesperson_id, period_code)
SELECT 'd0311000-0000-4000-8000-00000000ffff', '__SO36P_SETTLE__', u.id, '2099-02'
  FROM sys_user u WHERE u.deleted_at IS NULL LIMIT 1;
INSERT INTO biz_commission_item (settlement_id, base_amount, rate, commission)
VALUES ('d0311000-0000-4000-8000-00000000ffff', -100, 5, -5);
SELECT 'negative_base_ok=' || base_amount::text FROM biz_commission_item
 WHERE settlement_id = 'd0311000-0000-4000-8000-00000000ffff';" 2>&1 | tail -3
echo "-- 试图插入 base_amount = 0（必须被 ck_biz_cmitem_base 拒绝）"
"${PSQL[@]}" -Atc "
INSERT INTO biz_commission_item (settlement_id, base_amount, rate, commission)
VALUES ('d0311000-0000-4000-8000-00000000ffff', 0, 5, 0);" 2>&1 | grep -E "ERROR|violates" | head -2
# 清场（保留一条负基数行来**证明** down 会被挡住）
"${PSQL[@]}" -Atc "DELETE FROM biz_commission_item WHERE settlement_id = 'd0311000-0000-4000-8000-00000000ffff' AND base_amount > 0;"

say "④ down（有负基数行 → 必须被 ck_biz_cmitem_base 挡住：这条路径是设计，不是 bug）"
run V0311__commission_settle_setup.down.sql
"${PSQL[@]}" -Atc "SELECT 'settlements=' || count(*) FROM biz_commission_settlement;"

say "⑤ 清掉凭证明细与负数行后 down（第二轮，必须成功）"
"${PSQL[@]}" -Atc "DELETE FROM biz_commission_item; DELETE FROM biz_commission_settlement;" >/dev/null
run V0311__commission_settle_setup.down.sql
state

say "⑥ down（第三轮，必须幂等：无 ERROR）"
run V0311__commission_settle_setup.down.sql
state

say "⑦ up（恢复）"
run V0311__commission_settle_setup.up.sql
state
