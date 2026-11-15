#!/usr/bin/env bash
# 云枢 PG 慢查询日志（>1s）落地器 + 验收器   卡片 P3-16
#
# 干什么：
#   ① 在 /etc/postgresql/<VER>/<CLUSTER>/postgresql.conf 末尾追加一个「托管块」（带 BEGIN/END 标记，
#      幂等：重复执行只有一块），写入两条参数：
#        log_min_duration_statement = 1000        # 记录执行 >= 1000ms 的语句
#        log_line_prefix            = '%m [%p] %q%u@%d '
#   ② 用 `postgres -C` 预校验配置语法（坏配置不 reload，避免下次重启起不来），再 `pg_ctlcluster reload`
#      —— 这两条参数都是 sighup 级，reload 即生效，不需要 restart（不打断生产连接）。
#   ③ 断言：托管块 / 生效值 / auto.conf 无覆盖 / 前缀口径 / pending_restart=f
#   ④ --probe：真跑一条 `select pg_backend_pid(), pg_sleep(2)`，从 PG 日志里按 pid 捞出慢查询行
#
# 用法：
#   bash scripts/ops/postgres/pg_slowlog.sh                          # 本机落地 + 断言 + 探针
#   bash scripts/ops/postgres/pg_slowlog.sh --host 172.18.10.145     # 远端（ssh root@HOST）落地 + 断言 + 探针
#   bash scripts/ops/postgres/pg_slowlog.sh --host 172.18.10.145 --check    # 只检查，不改配置、不 reload
#   bash scripts/ops/postgres/pg_slowlog.sh --host 172.18.10.145 --probe    # 只跑慢查询探针（要求已落地）
#   bash scripts/ops/postgres/pg_slowlog.sh --self-test             # 临时目录自证：幂等 / 可回滚 / 块在末尾
#   bash scripts/ops/postgres/pg_slowlog.sh --rollback              # 用最近一次备份还原 postgresql.conf 并 reload
#
#   可选：--cluster VER/NAME（多集群时指定）、--threshold-ms 1000、--db-name cloudpivot_test
#
# 退出码：0 全绿 / 1 有断言失败（可直接给 cron / CI 用） / 2 用法错误
#
# 约定（为什么 %q 保留）：
#   卡片原文 log_line_prefix = '%m [%p] %u@%d '；Debian/Ubuntu 默认已是 '%m [%p] %q%u@%d '。
#   %q 只在「非会话进程」（checkpoint / bgwriter / walwriter 等）里抑制 %u@%d 的渲染，
#   会话行（含所有慢查询行）渲染结果与卡片要求逐字节一致；去掉 %q 会让这些后台行变成
#   `... [1234] @ LOG:  checkpoint ...` 的空 user@db 噪声。故保留 %q，由断言④钉死会话行口径。

set -euo pipefail

BEGIN_MARK='# >>> cloudpivot P3-16 pg slowlog (managed by scripts/ops/postgres/pg_slowlog.sh) >>>'
END_MARK='# <<< cloudpivot P3-16 pg slowlog <<<'

MODE="apply"            # apply | check | probe | self-test | rollback
HOST=""
CLUSTER_SPEC=""
THRESHOLD_MS="${THRESHOLD_MS:-1000}"
PROBE_DB="${PROBE_DB:-cloudpivot_test}"
PREFIX='%m [%p] %q%u@%d '
PORT_PREF="${PORT_PREF:-5432}"

usage() { sed -n '2,30p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }
die() { echo "✗ $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    --host)         HOST="${2:?--host 需要参数}"; shift 2 ;;
    --check)        MODE="check"; shift ;;
    --probe)        MODE="probe"; shift ;;
    --rollback)     MODE="rollback"; shift ;;
    --self-test)    MODE="self-test"; shift ;;
    --cluster)      CLUSTER_SPEC="${2:?--cluster 需要参数（形如 18/main）}"; shift 2 ;;
    --threshold-ms) THRESHOLD_MS="${2:?}"; shift 2 ;;
    --db-name)      PROBE_DB="${2:?}"; shift 2 ;;
    -h|--help)      usage; exit 0 ;;
    *) echo "未知参数：$1" >&2; usage >&2; exit 2 ;;
  esac
done

# ---------------------------------------------------------------- 远端自举
# 远端执行走 `ssh root@HOST bash -s -- <args>`，脚本自身作为载荷，不依赖 scp/rsync。
if [ -n "$HOST" ] && [ "${PG_SLOWLOG_REMOTE:-0}" != 1 ]; then
  args=(--threshold-ms "$THRESHOLD_MS" --db-name "$PROBE_DB")
  [ -n "$CLUSTER_SPEC" ] && args+=(--cluster "$CLUSTER_SPEC")
  case "$MODE" in
    check)    args+=(--check) ;;
    probe)    args+=(--probe) ;;
    rollback) args+=(--rollback) ;;
  esac
  echo "==== $HOST ===="
  ssh -o ConnectTimeout=10 -o BatchMode=yes "root@$HOST" \
      "PG_SLOWLOG_REMOTE=1 bash -s -- ${args[*]@Q}" < "${BASH_SOURCE[0]}"
  exit $?
fi

# ---------------------------------------------------------------- 集群探测
PGVER=""; PGCLUSTER=""; PGPORT=""; DATADIR=""; LOGFILE=""; CONF=""

detect_cluster() {
  local line=""
  if [ -n "$CLUSTER_SPEC" ]; then
    PGVER="${CLUSTER_SPEC%%/*}"; PGCLUSTER="${CLUSTER_SPEC##*/}"
    line="$(pg_lsclusters --no-header | awk -v v="$PGVER" -v c="$PGCLUSTER" '$1==v && $2==c{print;exit}')"
  else
    line="$(pg_lsclusters --no-header | awk -v p="$PORT_PREF" '$4=="online" && $3==p{print;exit}')"
    [ -n "$line" ] || line="$(pg_lsclusters --no-header | awk '$4=="online"{print;exit}')"
  fi
  [ -n "$line" ] || die "没找到 online 的 PG 集群（pg_lsclusters 输出为空或全 down）"
  PGVER="$(echo "$line"     | awk '{print $1}')"
  PGCLUSTER="$(echo "$line" | awk '{print $2}')"
  PGPORT="$(echo "$line"    | awk '{print $3}')"
  DATADIR="$(echo "$line"   | awk '{print $6}')"
  LOGFILE="$(echo "$line"   | awk '{print $7}')"
  [ "$LOGFILE" = "-" ] && LOGFILE=""
  CONF="/etc/postgresql/$PGVER/$PGCLUSTER/postgresql.conf"
  [ -r "$CONF" ] || die "读不到配置：$CONF"
}

# ---------------------------------------------------------------- 工具
# 以 postgres 超级用户跑 SQL（不落密码、不依赖 .pgpass）；SQL 走 stdin，免去引号地狱。
pgq() { # $1 = db，其余 = SQL
  local db="$1"; shift
  printf '%s\n' "$*" | su - postgres -s /bin/bash -c "psql -X -q -A -t -p $PGPORT -d $db -f -"
}

gset() { # $1 = 参数名 → setting|unit|source|pending_restart
  pgq postgres "select setting||'|'||coalesce(unit,'')||'|'||source||'|'||pending_restart from pg_settings where name='$1'"
}

gs() { # $1 = 参数名, $2 = 列名（setting/unit/source/pending_restart/context）
  pgq postgres "select coalesce($2::text,'') from pg_settings where name='$1'"
}

strip_block() { # $1 = 文件；删掉托管块（含标记行）与 apply_block 在块前加的那一个空行 → 与落地前逐字节一致
  # 注意：空行本身是 ""，不能用「pend != ""」判断有无缓冲（会把所有空行吃掉），必须用 hp 标志。
  awk -v b="$BEGIN_MARK" -v e="$END_MARK" '
    function flush() { if (hp) { print pend; pend = ""; hp = 0 } }
    $0 == b { pend = ""; hp = 0; skip = 1; next }   # 丢掉紧贴托管块前的那一个空行
    $0 == e { skip = 0; next }
    skip    { next }
    { flush(); if ($0 ~ /^[[:space:]]*$/) { pend = $0; hp = 1 } else { print } }
    END     { flush() }
  ' "$1"
}

want_block() { # 打印托管块正文（不含 SHELL，供写入与自证复用）
  printf '%s\n' "$BEGIN_MARK"
  printf '%s\n' "# 卡片 P3-16：慢查询（>=${THRESHOLD_MS}ms）进 PG 日志。由 scripts/ops/postgres/pg_slowlog.sh 托管，手改会被覆盖。"
  printf '%s\n' "log_min_duration_statement = ${THRESHOLD_MS}		# 记录执行 >= ${THRESHOLD_MS}ms 的语句（-1 = 关闭）"
  printf '%s\n' "log_line_prefix = '${PREFIX}'	# 会话行 = 时间 [pid] user@db；%q 只抑制后台进程的空 user@db"
  printf '%s\n' "$END_MARK"
}

apply_block() { # $1 = conf 路径；幂等（先删旧块再追加到末尾 → 后出现的值覆盖文件里早先的同名行）
  local conf="$1" tmp
  tmp="$(mktemp "${TMPDIR:-/tmp}/pgconf.XXXXXX")"
  strip_block "$conf" > "$tmp"
  printf '\n' >> "$tmp"
  want_block >> "$tmp"
  cat "$tmp" > "$conf"          # 穿过原 inode 写，保住 owner/perm（postgres:postgres 644）
  rm -f "$tmp"
}

validate_conf() { # 用 postgres -C 预校验配置语法：坏配置不 reload，避免下次重启起不来
  local out
  if ! out="$(su - postgres -s /bin/bash -c "/usr/lib/postgresql/$PGVER/bin/postgres -D '$DATADIR' -c config_file='$CONF' -C log_min_duration_statement" 2>&1)"; then
    echo "$out" >&2
    return 1
  fi
  echo "  postgres -C 预校验通过：log_min_duration_statement=$out"
}

reload_cluster() {
  pg_ctlcluster "$PGVER" "$PGCLUSTER" reload
  sleep 1
}

# ---------------------------------------------------------------- 断言骨架
PASS=0; FAIL=0; FAILED=()
ok()   { PASS=$((PASS+1)); printf '  ✓ %s\n' "$*"; }
no()   { FAIL=$((FAIL+1)); FAILED+=("$*"); printf '  ✗ %s\n' "$*"; }
assert_eq()   { if [ "$1" = "$2" ]; then ok "$3（$1）"; else no "$3：期望 [$2] 实得 [$1]"; fi; }
assert_has()  { if printf '%s\n' "$2" | grep -qF -- "$1"; then ok "包含 [$1]"; else no "应包含 [$1]（实得 [$2]）"; fi; }
assert_match(){ if printf '%s\n' "$2" | grep -qE -- "$1"; then ok "匹配 /$1/"; else no "应匹配 /$1/（实得 [$2]）"; fi; }
assert_true() { if [ "$2" = 1 ]; then ok "$1"; else no "$1"; fi; }

check_all() {
  local blk v t u src pr auto
  detect_cluster
  echo "→ 集群 $PGVER/$PGCLUSTER :$PGPORT  conf=$CONF  log=$LOGFILE"

  echo "① postgresql.conf 托管块"
  if grep -qxF -- "$BEGIN_MARK" "$CONF"; then ok "托管块标记存在（$(( $(grep -cxF -- "$BEGIN_MARK" "$CONF") )) 块）"; else no "postgresql.conf 里没有托管块"; fi
  assert_eq "$(grep -cxF -- "$BEGIN_MARK" "$CONF")" "1" "托管块唯一"
  blk="$(awk -v b="$BEGIN_MARK" -v e="$END_MARK" '$0==b{f=1;next} $0==e{f=0;next} f' "$CONF")"
  assert_match "^log_min_duration_statement = ${THRESHOLD_MS}([[:space:]]|$)" "$(printf '%s\n' "$blk" | grep '^log_min_duration_statement' | head -1)"
  assert_has "log_line_prefix = '${PREFIX}'" "$(printf '%s\n' "$blk" | grep '^log_line_prefix' | head -1)"

  echo "② 生效值（pg_settings）"
  assert_eq "$(gs log_min_duration_statement setting)$(gs log_min_duration_statement unit)" "${THRESHOLD_MS}ms" "log_min_duration_statement 生效值"
  assert_eq "$(gs log_min_duration_statement source)" "configuration file" "log_min_duration_statement 来源"
  assert_eq "$(gs log_min_duration_statement pending_restart)" "false" "log_min_duration_statement 无待重启项"
  assert_eq "$(gs log_min_duration_statement context)" "superuser" "log_min_duration_statement 仅超级用户可改（context=superuser）"

  assert_eq "$(gs log_line_prefix setting)" "$PREFIX" "log_line_prefix 生效值"
  assert_eq "$(gs log_line_prefix source)" "configuration file" "log_line_prefix 来源"
  assert_eq "$(gs log_line_prefix pending_restart)" "false" "log_line_prefix 无待重启项"

  echo "③ postgresql.auto.conf 无覆盖"
  auto="/etc/postgresql/$PGVER/$PGCLUSTER/postgresql.auto.conf"
  if [ ! -f "$auto" ]; then ok "auto.conf 不存在（无 ALTER SYSTEM 覆盖）"
  else
    assert_eq "$(grep -cE "^[[:space:]]*(log_min_duration_statement|log_line_prefix)" "$auto" || true)" "0" "auto.conf 未覆盖这两个参数"
  fi

  echo "④ 日志落点"
  if [ -n "$LOGFILE" ] && [ -f "$LOGFILE" ]; then
    ok "PG 日志文件存在：$LOGFILE（$(wc -l < "$LOGFILE") 行）"
    assert_match "stderr" "$(gset log_destination | cut -d'|' -f1)"
  else
    no "拿不到 PG 日志文件（pg_lsclusters 的 Log file 列为空或文件不存在）"
  fi
}

# ---------------------------------------------------------------- 探针
probe_slow_query() {
  local pid line f_before f_after i db
  detect_cluster
  [ -n "$LOGFILE" ] && [ -f "$LOGFILE" ] || die "拿不到 PG 日志文件，无法断言"
  db="$PROBE_DB"
  if [ "$(pgq postgres "select 1 from pg_database where datname='$db'")" != "1" ]; then
    echo "  ! 本机没有库 $db（卡片说的模板库不在这台），改用 postgres 库探针（不碰任何业务库）"
    db=postgres
  fi
  f_before="$(wc -l < "$LOGFILE")"
  echo "→ 跑慢查询：select pg_backend_pid(), pg_sleep(2)  （库 $db）"
  pid="$(pgq "$db" "select pg_backend_pid() as pid, pg_sleep(2);" | cut -d'|' -f1)"
  [ -n "$pid" ] || die "探针没拿到 backend pid"
  echo "  backend pid = $pid"
  line=""
  for i in 1 2 3 4 5 6 7 8 9 10; do
    line="$(grep -E "\[${pid}\] .*duration:" "$LOGFILE" | tail -1 || true)"
    [ -n "$line" ] && break
    sleep 0.5
  done
  f_after="$(wc -l < "$LOGFILE")"
  echo "  日志行数：$f_before → $f_after"
  assert_true "日志里有 pid=$pid 的慢查询行" "$( [ -n "$line" ] && echo 1 || echo 0 )"
  assert_match "\[${pid}\] [a-z_]+@${db} LOG:  duration: 2[0-9]{3}\.[0-9]+ ms  statement: select pg_backend_pid\(\) as pid, pg_sleep\(2\);" "$line"
  assert_true "慢查询阈值确实 >= ${THRESHOLD_MS}ms（duration: 2xxx）" "$(printf '%s' "$line" | grep -qE 'duration: 2[0-9]{3}\.' && echo 1 || echo 0)"

  # 阈值下界反证：400ms 的查询不该进日志（证明是 1000ms 门限在生效，而不是「录所有语句」）
  local pid2 line2
  pid2="$(pgq "$db" "select pg_backend_pid() as pid, pg_sleep(0.4);" | cut -d'|' -f1)"
  sleep 1
  line2="$(grep -E "\[${pid2}\] .*duration:" "$LOGFILE" | tail -1 || true)"
  echo "  backend pid = $pid2（0.4s 查询）→ 日志命中：${line2:-无}"
  assert_true "0.4s 查询未进日志（1000ms 门限生效，不是 log_statement=all）" "$( [ -z "$line2" ] && echo 1 || echo 0 )"

  if [ "$FAIL" -eq 0 ]; then echo; echo "  日志样例："; echo "  $line"; fi
}

# ---------------------------------------------------------------- 回滚
rollback_conf() {
  local b
  detect_cluster
  b="$(ls -1t "$CONF".bak-p316.* 2>/dev/null | head -1 || true)"
  [ -n "$b" ] || die "找不到备份 $CONF.bak-p316.*，无法回滚"
  cp -p "$b" "$CONF"
  echo "→ 已用 $b 还原 $CONF"
  validate_conf && reload_cluster
  echo "→ 生效值：log_min_duration_statement=$(gset log_min_duration_statement | cut -d'|' -f1) log_line_prefix=[$(gset log_line_prefix | cut -d'|' -f1)]"
}

# ---------------------------------------------------------------- 自证
run_self_test() {
  local t; t="$(mktemp -d "${TMPDIR:-/tmp}/pgs-lowlog-selftest.XXXXXX")"
  trap 'rm -rf "$t"' RETURN
  local c="$t/postgresql.conf" orig="$t/orig.conf"
  cat > "$c" <<'EOF'
# 模拟 Debian 交付的 postgresql.conf（片段）
#log_min_duration_statement = -1	# -1 is disabled, 0 logs all statements
log_line_prefix = '%m [%p] %q%u@%d '		# special values:
EOF
  cp "$c" "$orig"
  PREFIX='%m [%p] %q%u@%d ' THRESHOLD_MS=1000
  apply_block "$c"; apply_block "$c"
  assert_eq "$(grep -cxF -- "$BEGIN_MARK" "$c")" "1" "跑两遍幂等：只有 1 块"
  assert_eq "$(grep -c '^log_min_duration_statement' "$c")" "1" "只有 1 条 log_min_duration_statement"
  assert_eq "$(tail -1 "$c")" "$END_MARK" "托管块在文件末尾（后出现者生效）"
  strip_block "$c" > "$t/stripped.conf"
  assert_eq "$(sha256sum < "$t/stripped.conf" | cut -d' ' -f1)" "$(sha256sum < "$orig" | cut -d' ' -f1)" "strip 后与原文逐字节一致（可回滚）"
  assert_match "^log_min_duration_statement = 1000" "$(grep '^log_min_duration_statement' "$c")"
  assert_has "log_line_prefix = '%m [%p] %q%u@%d '" "$(grep '^log_line_prefix' "$c" | tail -1)"
}

# ---------------------------------------------------------------- 主流程
case "$MODE" in
  self-test) echo "== 自证（临时目录，不碰真实配置）=="; run_self_test ;;
  check)
    echo "== 检查（只读）=="
    check_all
    ;;
  probe)
    echo "== 慢查询探针 =="
    probe_slow_query
    ;;
  rollback)
    rollback_conf
    ;;
  apply)
    detect_cluster
    echo "== 落地 =="
    echo "→ 集群 $PGVER/$PGCLUSTER :$PGPORT"
    echo "→ 配置 $CONF"
    backup="$CONF.bak-p316.$(date +%Y%m%d%H%M%S)"
    if ! grep -qxF -- "$BEGIN_MARK" "$CONF"; then
      cp -p "$CONF" "$backup"
      echo "→ 备份 $backup"
    else
      echo "→ 已有托管块（重复执行），本次不新增备份"
    fi
    apply_block "$CONF"
    echo "→ 新配置尾部："
    tail -6 "$CONF" | sed 's/^/    /'
    if [ -f "${backup:-}" ]; then
      FAIL=0; PASS=0; FAILED=()   # 单独计数
      strip_block "$CONF" > /tmp/.pg_slowlog_strip.$$
      assert_eq "$(sha256sum < /tmp/.pg_slowlog_strip.$$ | cut -d' ' -f1)" "$(sha256sum < "$backup" | cut -d' ' -f1)" "除托管块外与备份逐字节一致（可精确回滚）"
      rm -f /tmp/.pg_slowlog_strip.$$
      [ "$FAIL" -eq 0 ] || { echo "✗ 写入后与备份不一致，已中止（未 reload）"; exit 1; }
      PASS=0; FAIL=0; FAILED=()
    fi
    validate_conf
    echo "→ reload（sighup 级参数，不需 restart，不打断连接）"
    reload_cluster
    echo "→ 检查"
    check_all
    echo "→ 探针"
    probe_slow_query
    ;;
esac

echo
echo "== 汇总：通过 $PASS / 失败 $FAIL =="
if [ "$FAIL" -ne 0 ]; then
  printf '  失败项：\n'; printf '    - %s\n' "${FAILED[@]}"
  exit 1
fi
exit 0
