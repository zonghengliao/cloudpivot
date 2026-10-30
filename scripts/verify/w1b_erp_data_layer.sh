#!/usr/bin/env bash
# W1b 验收：13 张 ERP 表的 Rust 数据层（model + dto）
#
# 做法（评审可在任意 clone 上复跑，全程不碰 dev / prod 库）：
#   1) 建一次性库 ${DB}（默认 cloudpivot_w1b_verify，`createdb -O cloudpivot`）；
#   2) 用目标仓的 release 二进制起一个隔离实例（独立端口 + 独立 redis db），
#      让它把 migrations 跑到最新（V0001..V0137），看到「数据库迁移完成」即杀掉实例；
#   3) `ERP_DB_URL` 指到该库，跑 `cargo test --lib business::erp`（36 条，含 22 条真库用例）；
#   4) 打两条 EXPLAIN，证明仓库子树走 `idx_biz_warehouse_path_pattern`（~>=~ / ~<~），
#      而参数化 `LIKE $1` 走不了这个索引；
#   5) 复查门禁：新模块里不允许出现 `paginate_with_params`，分页必须走 `scope::paginate*`；
#   6) 收尾：杀掉实例、`dropdb` 删库（`KEEP=1` 可保留现场）。
#
# 用法：
#   REPO=/opt/cloudpivot bash scripts/verify/w1b_erp_data_layer.sh
#   REPO=/root/w1b-clone W1B_PORT=18089 KEEP=1 bash scripts/verify/w1b_erp_data_layer.sh
#
# 环境变量：
#   REPO       仓库根（默认 /opt/cloudpivot）
#   W1B_DB     一次性库名（默认 cloudpivot_w1b_verify）
#   W1B_PORT   隔离实例端口（默认 18089）
#   W1B_BIN    用于跑迁移的二进制（默认 $REPO/backend/target/release/cloudpivot）
#   CARGO_TARGET_DIR  建议给个独立目录，避免和别人的 build 抢锁
#   KEEP=1     保留库与日志（排障用）
set -uo pipefail

REPO="${REPO:-/opt/cloudpivot}"
DB="${W1B_DB:-cloudpivot_w1b_verify}"
PORT="${W1B_PORT:-18089}"
BIN="${W1B_BIN:-$REPO/backend/target/release/cloudpivot}"
LOG_DIR="${W1B_LOGDIR:-/tmp/w1b_verify}"
LOG="$LOG_DIR/instance.log"
PASS=0
FAIL=0

ok() { PASS=$((PASS + 1)); echo "  [PASS] $*"; }
bad() { FAIL=$((FAIL + 1)); echo "  [FAIL] $*"; }
step() { echo; echo "== $*"; }

mkdir -p "$LOG_DIR"
export PATH=/root/.cargo/bin:$PATH

[ -f "$BIN" ] || { echo "找不到二进制 $BIN（先 cargo build --release，或设 W1B_BIN）"; exit 1; }
[ -d "$REPO/backend" ] || { echo "REPO 不是云枢仓库根：$REPO"; exit 1; }

# ---------------------------------------------------------------- 1) 建库
step "1) 建一次性库 $DB"
if su - postgres -c "psql -Atc \"select 1 from pg_database where datname='$DB'\"" | grep -q 1; then
  su - postgres -c "dropdb '$DB'" || exit 1
  ok "旧的 $DB 已删除（每次从空库起跑）"
fi
su - postgres -c "createdb -O cloudpivot '$DB'" || { bad "createdb 失败"; exit 1; }
ok "createdb -O cloudpivot $DB"

# ---------------------------------------------------------------- 2) 迁移
step "2) 起隔离实例跑迁移（127.0.0.1:$PORT，redis db 11）"
set -a
# shellcheck disable=SC1091
. /etc/cloudpivot/cloudpivot-dev.env
set +a
export NIK_DATABASE_NAME="$DB"
export NIK_SERVER_LISTEN="127.0.0.1:$PORT"
export NIK_REDIS_URL="redis://127.0.0.1:6379/11"
cd "$REPO/backend" || exit 1
setsid nohup "$BIN" > "$LOG" 2>&1 < /dev/null &
PID=$!
MIGRATED=0
for _ in $(seq 1 90); do
  if grep -q "数据库迁移完成" "$LOG" 2>/dev/null; then MIGRATED=1; break; fi
  kill -0 "$PID" 2>/dev/null || break
  sleep 1
done
if [ "$MIGRATED" = 1 ]; then
  ok "迁移完成：$(grep -o '数据库迁移完成.*' "$LOG" | tail -1)"
else
  bad "迁移未完成，日志尾部："
  tail -15 "$LOG"
fi
sleep 1
kill "$PID" 2>/dev/null
wait "$PID" 2>/dev/null

# ---------------------------------------------------------------- 3) 表 / 迁移版本
step "3) 13 张表 + 迁移版本"
set -a
# shellcheck disable=SC1091
. /etc/cloudpivot/cloudpivot-test.env
set +a
export PGPASSWORD="$ALUN_DATABASE_PASSWORD"
q() { psql -h 127.0.0.1 -U cloudpivot -d "$DB" -At -F'|' -c "$1" 2>&1; }
TABLES=$(q "select count(*) from information_schema.tables where table_name in
  ('biz_unit','biz_warehouse','biz_location','biz_material','biz_supplier','biz_account',
   'biz_accounting_period','biz_seq','biz_purchase_order','biz_purchase_order_item',
   'biz_inventory','biz_inventory_location','biz_inventory_txn');")
[ "$TABLES" = "13" ] && ok "13 张 biz_* 表齐备" || bad "表数=$TABLES（应为 13）"
VER=$(q "select max(version::int) from schema_migrations where version ~ '^[0-9]+\$';")
[ "$VER" = "137" ] && ok "迁移版本 = 137（V0137 已应用）" || bad "迁移版本=$VER（应为 137）"

# ---------------------------------------------------------------- 4) 真库单测
step "4) cargo test --lib business::erp（真库，36 条）"
export ERP_DB_URL="postgres://${ALUN_DATABASE_USER}:${ALUN_DATABASE_PASSWORD}@${ALUN_DATABASE_HOST}:${ALUN_DATABASE_PORT}/${DB}"
TEST_LOG="$LOG_DIR/erp_tests.log"
(cd "$REPO/backend" && cargo test --lib business::erp -- --test-threads=1 --nocapture) > "$TEST_LOG" 2>&1
RESULT=$(grep -E "^test result:" "$TEST_LOG" | tail -1)
echo "  $RESULT"
if echo "$RESULT" | grep -q "0 failed"; then
  ok "erp 单测全绿：$(grep -cE '^test business::erp' "$TEST_LOG") 条"
else
  bad "erp 单测未全绿（见 $TEST_LOG）"
  grep -E "^(test .*FAILED|failures:)" -A5 "$TEST_LOG" | head -30
fi
if grep -q "SKIP: 未设置 ERP_DB_URL" "$TEST_LOG"; then
  bad "有用例跳过了（ERP_DB_URL 没生效）"
else
  ok "没有用例跳过（真库路径确实跑了）"
fi

# ---------------------------------------------------------------- 5) EXPLAIN
step "5) 仓库子树：~>=~ / ~<~ 走 text_pattern_ops 索引"
PREFIX="/b1b00002-0000-4000-8000-000000000001/"
# 上界必须是「去掉末尾斜杠再补 0」：'/uuid/' + '0' 会把 'b…' 开头的后代排除掉（实测只回 1 行）。
UPPER="${PREFIX%/}0"
PLAN_LIKE=$(q "SET enable_seqscan = off;
  PREPARE a(text) AS SELECT warehouse_code FROM biz_warehouse
    WHERE deleted_at IS NULL AND warehouse_path LIKE \$1;
  EXPLAIN (COSTS OFF) EXECUTE a('$PREFIX%');" 2>&1)
PLAN_RANGE=$(q "SET enable_seqscan = off;
  PREPARE b(text, text) AS SELECT warehouse_code FROM biz_warehouse
    WHERE deleted_at IS NULL AND warehouse_path ~>=~ \$1 AND warehouse_path ~<~ \$2;
  EXPLAIN (COSTS OFF) EXECUTE b('$PREFIX', '$UPPER');" 2>&1)
echo "  --- LIKE \$1（参数化）"
echo "$PLAN_LIKE" | sed 's/^/    /'
echo "  --- ~>=~ / ~<~"
echo "$PLAN_RANGE" | sed 's/^/    /'
echo "$PLAN_RANGE" | grep -q "idx_biz_warehouse_path_pattern" \
  && ok "子树查询命中 idx_biz_warehouse_path_pattern" \
  || bad "子树查询没走 text_pattern_ops 索引"
if echo "$PLAN_LIKE" | grep -q "idx_biz_warehouse_path_pattern"; then
  bad "参数化 LIKE 也走了该索引，结论需重新评估"
else
  ok "参数化 LIKE 用不上该索引（这就是不用 LIKE 的理由）"
fi
SETS_LIKE=$(q "select string_agg(warehouse_code, ',' order by warehouse_path) from biz_warehouse
  where deleted_at is null and warehouse_path like '$PREFIX%';")
SETS_RANGE=$(q "select string_agg(warehouse_code, ',' order by warehouse_path) from biz_warehouse
  where deleted_at is null and warehouse_path ~>=~ '$PREFIX' and warehouse_path ~<~ '$UPPER';")
[ "$SETS_LIKE" = "$SETS_RANGE" ] && ok "两种写法返回同一集合：$SETS_RANGE" || bad "两种写法结果不同（LIKE=$SETS_LIKE RANGE=$SETS_RANGE）"

# ---------------------------------------------------------------- 6) 门禁复查
step "6) 门禁复查（分页入口 / SCOPE 口径）"
ERPDIR="$REPO/backend/src/business/erp"
# 只看真实代码行（剥掉整行注释：`//` / `//!` / `///` / `*`），与 check.sh 闸① 同口径
if grep -rn "paginate_with_params" "$ERPDIR" --include=*.rs | grep -vE ":[0-9]+:[[:space:]]*(//|\*)" | grep -q .; then
  bad "新模块里出现 paginate_with_params（闸① 会红）"
else
  ok "新模块 0 处 paginate_with_params（分页只走 scope::paginate_unscoped）"
fi
PAGE_OF=$(grep -c "page_of(" "$ERPDIR/model.rs")
UNSCOPED=$(grep -c "scope::paginate_unscoped(" "$ERPDIR/model.rs")
[ "$PAGE_OF" -ge 10 ] && ok "13 个列表统一走 page_of 助手（$PAGE_OF 处调用）" \
  || bad "page_of 只出现 $PAGE_OF 处，检查是否有手写分页"
[ "$UNSCOPED" -eq 1 ] && ok "paginate_unscoped 只在 page_of 里出现 1 处（唯一分页入口）" \
  || bad "paginate_unscoped 出现 $UNSCOPED 处（应只在 page_of 里 1 处）"
grep -q "SCOPE-EXEMPT" "$ERPDIR/model.rs" && ok "模块头写明了 SCOPE-EXEMPT 理由" || bad "缺 SCOPE-EXEMPT 说明"
grep -q "pub mod erp;" "$REPO/backend/src/business/mod.rs" && ok "business/mod.rs 已注册 pub mod erp;" || bad "未注册 erp 模块"
ls "$ERPDIR"/{mod,model,dto}.rs > /dev/null 2>&1 && ok "交付 3 个文件：mod.rs / model.rs / dto.rs" || bad "文件缺失"

# ---------------------------------------------------------------- 收尾
step "收尾"
if [ "${KEEP:-0}" = "1" ]; then
  echo "  KEEP=1 → 保留库 $DB 与日志 $LOG_DIR"
else
  su - postgres -c "dropdb --if-exists '$DB'" && echo "  已删库 $DB"
fi

echo
echo "== 汇总：PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
