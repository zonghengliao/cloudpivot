#!/usr/bin/env bash
# 云枢日志切割落地器 + 验收器
#
# 干什么：
#   ① 把 scripts/ops/logrotate/cloudpivot 装到 /etc/logrotate.d/cloudpivot
#      （按天切割 / 保留 30 天 / 压缩，卡片 P3-15 原文）
#   ② 把 journald-cloudpivot-retention.conf 装到
#      /etc/systemd/journald.conf.d/cloudpivot-retention.conf
#      （应用日志走 journald，logrotate 切不到，30 天保留只能在这里落地）
#   ③ 跑断言：文件一致 / logrotate -d 无错 / 归档 .gz 存在且完整 / 归档数 ≤31 / 定时器在跑 / journald 生效
#
# 用法：
#   bash scripts/ops/logrotate/install.sh                 # 本机安装 + 断言
#   bash scripts/ops/logrotate/install.sh --host 172.18.10.145
#   bash scripts/ops/logrotate/install.sh --host 172.18.10.144 --check   # 只检查不改动
#   bash scripts/ops/logrotate/install.sh --host 172.18.10.145 --rotate  # 强制切一次（验收用）
#   bash scripts/ops/logrotate/install.sh --no-journald   # 只做 logrotate，不碰 journald
#   bash scripts/ops/logrotate/install.sh --self-test     # 临时目录自证 rotate 30 / notifempty / missingok
#   bash scripts/ops/logrotate/install.sh --fix-conflicts # 修 /etc/logrotate.d 里的重复 glob（仅当两份逐字节相同时才移走）
#
# 退出码：0 全绿 / 1 有断言失败（可直接给 cron / CI 用）
#
# 说明：远端执行走 `ssh root@HOST bash -s`，载荷里内嵌 base64 配置文件，
#       不依赖 scp/rsync，也不在线拼接 heredoc，配置内容与仓库逐字节一致（断言②校验 sha256）。

set -euo pipefail

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONF_SRC="$SELF_DIR/cloudpivot"
JOURNALD_SRC="$SELF_DIR/journald-cloudpivot-retention.conf"

TARGET_CONF="/etc/logrotate.d/cloudpivot"
JOURNALD_DIR="/etc/systemd/journald.conf.d"
JOURNALD_TARGET="$JOURNALD_DIR/cloudpivot-retention.conf"
LOG_DIR="/var/log/cloudpivot"
RETAIN_DAYS=30

HOST=""
MODE="install"          # install | check | rotate | self-test
WITH_JOURNALD=1
FIX_CONFLICTS=0

usage() { sed -n '2,25p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

while [ $# -gt 0 ]; do
  case "$1" in
    --host)         HOST="${2:?--host 需要参数}"; shift 2 ;;
    --check)        MODE="check"; shift ;;
    --rotate)       MODE="rotate"; shift ;;
    --no-journald)  WITH_JOURNALD=0; shift ;;
    --fix-conflicts) FIX_CONFLICTS=1; shift ;;
    --self-test)    MODE="self-test"; shift ;;
    -h|--help)      usage; exit 0 ;;
    *) echo "未知参数：$1" >&2; usage >&2; exit 2 ;;
  esac
done

[ -r "$CONF_SRC" ]     || { echo "缺配置文件：$CONF_SRC" >&2; exit 2; }
[ "$WITH_JOURNALD" = 1 ] && { [ -r "$JOURNALD_SRC" ] || { echo "缺配置：$JOURNALD_SRC" >&2; exit 2; }; }

CONF_B64="$(base64 -w0 < "$CONF_SRC")"
JRNL_B64="$(base64 -w0 < "$JOURNALD_SRC" 2>/dev/null || true)"
CONF_SHA="$(sha256sum "$CONF_SRC" | awk '{print $1}')"

# ---------------------------------------------------------------------------
# --self-test：用临时目录 + 独立 state 文件，把仓库里这份 conf 的语义跑一遍
#   （不读也不写 /var/log/cloudpivot，可在任何机器上跑，含无 root 的 CI）
# ---------------------------------------------------------------------------
run_self_test() {
  local t; t="$(mktemp -d "${TMPDIR:-/tmp}/lr-selftest.XXXXXX")"
  local pass=0 fail=0
  local ok bad
  ok()  { printf 'PASS %s\n' "$1"; pass=$((pass+1)); }
  bad() { printf 'FAIL %s — %s\n' "$1" "$2"; fail=$((fail+1)); }

  echo "================ 语义自证 · 临时目录 $t（不碰线上日志） ================"
  mkdir -p "$t/logs"
  seq 1 100 > "$t/logs/app.log"                       # 有内容的基文件
  : > "$t/logs/empty.log"                              # 空基文件 → notifempty 应跳过

  # 用仓库里那份 conf 原样跑，只替换日志路径（保证测的就是线上语义）
  sed "s#^/var/log/cloudpivot/\*\.log {#$t/logs/*.log {#" "$CONF_SRC" > "$t/test.conf"
  if grep -q "^$t/logs/\*\.log {" "$t/test.conf"; then ok "自证 conf 由仓库源派生（路径已替换）"
  else bad "自证 conf 由仓库源派生" "替换失败，见 $t/test.conf"; fi

  local out rc
  out="$(logrotate -f -s "$t/state" "$t/test.conf" 2>&1)"; rc=$?
  if [ $rc -eq 0 ]; then ok "logrotate -f 在临时目录执行成功（rc=0）"
  else bad "logrotate -f 在临时目录执行成功" "rc=$rc out=$(printf '%s' "$out" | tr '\n' ' ')"; fi

  # ① copytruncate：基文件被清空但 inode 还在
  if [ -f "$t/logs/app.log" ] && [ ! -s "$t/logs/app.log" ]; then ok "① copytruncate：app.log 仍在且已清零"
  else bad "① copytruncate：app.log 仍在且已清零" "$(stat -c '%s bytes' "$t/logs/app.log" 2>/dev/null || echo 缺失)"; fi

  # ② compress：新归档是 .1.gz，且内容完整
  if [ -f "$t/logs/app.log.1.gz" ]; then
    local lines; lines="$(gzip -dc "$t/logs/app.log.1.gz" 2>/dev/null | wc -l)"
    if [ "$lines" -eq 100 ]; then ok "② compress：app.log.1.gz 可解压且内容完整（100 行）"
    else bad "② compress：内容完整" "解压得 $lines 行，期望 100"; fi
  else bad "② compress：app.log.1.gz 存在" "不存在"; fi

  # ④ notifempty：空文件不切
  if [ ! -f "$t/logs/empty.log.1.gz" ] && [ -f "$t/logs/empty.log" ]; then ok "④ notifempty：empty.log 未被切（无 empty.log.1.gz）"
  else bad "④ notifempty：empty.log 未被切" "出现了 empty.log.1.gz"; fi
  # ③ 连切 32 天，验证「保留 30 天」的真实语义（不是数已有文件，而是跑出来）
  #    每天：往 app.log 追加 marker-N → logrotate -f → 期望第 N 天的内容出现在 .1.gz
  local d rotfail=0
  for d in $(seq 1 32); do
    printf 'marker-%s\n' "$d" >> "$t/logs/app.log"
    logrotate -f -s "$t/state" "$t/test.conf" >/dev/null 2>&1 || rotfail=1
  done
  [ "$rotfail" -eq 0 ] && ok "③ 连切 32 次全部 rc=0（天天切也不炸）" \
    || bad "③ 连切 32 次全部 rc=0" "中途有非零退出"

  local cnt; cnt="$(compgen -G "$t/logs/app.log.[0-9]*.gz" | wc -l)"
  if [ "$cnt" -eq 30 ]; then ok "③b rotate 30：连切 32 天后恰好保留 30 份归档"
  else bad "③b rotate 30：连切 32 天后恰好保留 30 份归档" "实际 $cnt 份"; fi
  if [ -f "$t/logs/app.log.30.gz" ] && [ ! -f "$t/logs/app.log.31.gz" ]; then
    ok "③c 编号上限：.30.gz 在、.31.gz 不存在（不会越滚越多）"
  else bad "③c 编号上限" ".30=$( [ -f "$t/logs/app.log.30.gz" ] && echo y || echo n) .31=$( [ -f "$t/logs/app.log.31.gz" ] && echo y || echo n)"; fi

  # 内容对位：.1 必须是最后一天，.30 必须是最早保留的一天，头两天已被丢弃
  local c1 c30
  c1="$(gzip -dc "$t/logs/app.log.1.gz" 2>/dev/null | grep -o 'marker-[0-9]*' | tail -1 || true)"
  c30="$(gzip -dc "$t/logs/app.log.30.gz" 2>/dev/null | grep -o 'marker-[0-9]*' | tail -1 || true)"
  if [ "$c1" = "marker-32" ]; then ok "③d .1.gz = 最新一天（marker-32）"
  else bad "③d .1.gz = 最新一天（marker-32）" "实际 '$c1'"; fi
  if [ "$c30" = "marker-3" ]; then ok "③e .30.gz = 最老保留日（marker-3），marker-1/marker-2 已随第 31/32 天滚出"
  else bad "③e .30.gz = 最老保留日（marker-3）" "实际 '$c30'"; fi


  # ⑤ missingok：路径下没有匹配文件时不报错退出
  sed "s#^$t/logs/\*\.log {#$t/nonexistent/*.log {#" "$t/test.conf" > "$t/missing.conf"
  out="$(logrotate -f -s "$t/state2" "$t/missing.conf" 2>&1)"; rc=$?
  if [ $rc -eq 0 ]; then ok "⑤ missingok：路径无匹配时 rc=0（不会让每日 cron 报错）"
  else bad "⑤ missingok：路径无匹配时 rc=0" "rc=$rc out=$(printf '%s' "$out" | tr '\n' ' ')"; fi

  # ⑥ 同样的基文件再切一次不应炸（幂等：日志被清空后靠 notifempty 自然跳过）
  out="$(logrotate -f -s "$t/state" "$t/test.conf" 2>&1)"; rc=$?
  if [ $rc -eq 0 ]; then ok "⑥ 重复执行 rc=0（清空后的基文件被 notifempty 跳过，不会产生空归档）"
  else bad "⑥ 重复执行 rc=0" "rc=$rc out=$(printf '%s' "$out" | tr '\n' ' ')"; fi

  echo "---- 小计"
  printf 'PASS=%d  FAIL=%d  （临时目录 %s 保留供复核）\n' "$pass" "$fail" "$t"
  [ "$fail" -eq 0 ] && return 0 || return 1
}

if [ "$MODE" = self-test ]; then
  set +e; run_self_test; rc=$?; set -e
  exit "$rc"
fi

# ---------------------------------------------------------------------------
# 载荷：在目标机上执行的完整脚本（本机 = bash -s，远端 = ssh host bash -s）
# ---------------------------------------------------------------------------
build_payload() {
cat <<PAYLOAD
set -uo pipefail
MODE='$MODE'
WITH_JOURNALD='$WITH_JOURNALD'
FIX_CONFLICTS='$FIX_CONFLICTS'
TARGET_CONF='$TARGET_CONF'
JOURNALD_DIR='$JOURNALD_DIR'
JOURNALD_TARGET='$JOURNALD_TARGET'
LOG_DIR='$LOG_DIR'
RETAIN_DAYS='$RETAIN_DAYS'
CONF_SHA='$CONF_SHA'
CONF_B64='$CONF_B64'
JRNL_B64='$JRNL_B64'

pass=0; fail=0; skip=0
ok()   { printf 'PASS %s\n' "\$1"; pass=\$((pass+1)); }
bad()  { printf 'FAIL %s — %s\n' "\$1" "\$2"; fail=\$((fail+1)); }
skp()  { printf 'SKIP %s — %s\n' "\$1" "\$2"; skip=\$((skip+1)); }
hr()   { printf -- '---- %s\n' "\$1"; }

cleanup() { rm -f "\$TARGET_CONF.tmp" "\$JOURNALD_TARGET.tmp" "\${ROTATE_LIST:-}" 2>/dev/null; }
trap cleanup EXIT

# ---------------------------------------------------------------------------
# 安装（MODE=install / rotate；check 只读不动）
# ---------------------------------------------------------------------------
if [ "\$MODE" != check ]; then
  hr '安装 logrotate 片段'
  mkdir -p /etc/logrotate.d
  old_sha=""; [ -f "\$TARGET_CONF" ] && old_sha="\$(sha256sum "\$TARGET_CONF" | awk '{print \$1}')"
  printf '%s' "\$CONF_B64" | base64 -d > "\$TARGET_CONF.tmp"
  chown root:root "\$TARGET_CONF.tmp"; chmod 644 "\$TARGET_CONF.tmp"
  mv -f "\$TARGET_CONF.tmp" "\$TARGET_CONF"
  new_sha="\$(sha256sum "\$TARGET_CONF" | awk '{print \$1}')"
  if [ "\$old_sha" = "\$new_sha" ]; then echo "  \$TARGET_CONF 已是最新（sha256 未变）"
  else echo "  \$TARGET_CONF 已写入（旧=\${old_sha:0:12} 新=\${new_sha:0:12}）"; fi

  if [ "\$WITH_JOURNALD" = 1 ]; then
    hr '安装 journald 保留口径'
    mkdir -p "\$JOURNALD_DIR"
    old_j=""; [ -f "\$JOURNALD_TARGET" ] && old_j="\$(sha256sum "\$JOURNALD_TARGET" | awk '{print \$1}')"
    printf '%s' "\$JRNL_B64" | base64 -d > "\$JOURNALD_TARGET.tmp"
    chown root:root "\$JOURNALD_TARGET.tmp"; chmod 644 "\$JOURNALD_TARGET.tmp"
    mv -f "\$JOURNALD_TARGET.tmp" "\$JOURNALD_TARGET"
    new_j="\$(sha256sum "\$JOURNALD_TARGET" | awk '{print \$1}')"
    if [ "\$old_j" = "\$new_j" ]; then
      echo "  \$JOURNALD_TARGET 已是最新，不重启 journald"
    else
      echo "  \$JOURNALD_TARGET 已写入（旧=\${old_j:0:12} 新=\${new_j:0:12}）→ 重启 systemd-journald"
      systemctl restart systemd-journald
      sleep 1
      systemctl is-active --quiet systemd-journald && echo "  systemd-journald: active（重启后存活）" \
        || echo "  !! systemd-journald 未存活，请立即排查"
    fi
  fi

  # 片段冲突：/etc/logrotate.d 是按目录整体解析的，两个包提供同一 glob 会让整份配置 rc=1，
  # 定时器天天红（cloud-init / cloud-init-base 就是这种，2026-11-12 起每天失败）。
  # 修法：给重复的那份加 .disabled 后缀 —— logrotate 忽略该后缀（99 上已是这个形态，实测 rc=0）。
  # 只在「两份文件逐字节相同」时才动手，否则拒绝并打印 diff —— 绝不猜。
  if [ "\$FIX_CONFLICTS" = 1 ]; then
    hr '修 logrotate 片段冲突（重复 glob）'
    for pair in cloud-init:cloud-init-base; do
      canon="\${pair%%:*}"; dup="\${pair##*:}"
      a="/etc/logrotate.d/\$canon"; b="/etc/logrotate.d/\$dup"
      if [ ! -f "\$b" ]; then echo "  \$b 不存在或已禁用（无需处理）"; continue; fi
      if [ -f "\$a" ] && cmp -s "\$a" "\$b"; then
        mv "\$b" "\$b.disabled"
        echo "  \$b 与 \$a 逐字节相同 → 重命名为 \$b.disabled（logrotate 忽略该后缀；原件内容不变，改回来只需去掉后缀）"
      else
        printf '  !! %s 与 %s 内容不同，未改动（需人工判断）：%s\n' "\$b" "\$a" "\$(diff -q "\$a" "\$b" 2>&1)"
      fi
    done
  fi
fi

# ---------------------------------------------------------------------------
# 强制切一次（MODE=rotate）
# ---------------------------------------------------------------------------
ROTATE_LIST="\$(mktemp)"
if [ "\$MODE" = rotate ]; then
  hr '强制切割一次（logrotate -f）'
  # 记录旋转前「非空」的基文件，旋转后逐个核对
  for f in "\$LOG_DIR"/*.log; do
    [ -e "\$f" ] || continue
    [ -s "\$f" ] || continue
    printf '%s\n' "\$f" >> "\$ROTATE_LIST"
  done
  echo "  待切基文件 \$(wc -l < "\$ROTATE_LIST") 个"
  out="\$(logrotate -f "\$TARGET_CONF" 2>&1)"; rc=\$?
  if [ \$rc -eq 0 ]; then ok "logrotate -f 执行成功（rc=0）"
  else bad "logrotate -f 执行成功（rc=0）" "rc=\$rc output=\$(printf '%s' "\$out" | tr '\n' ' ')"; fi

  n_expect=\$(wc -l < "\$ROTATE_LIST")
  n_archived=0; n_trunc=0
  while IFS= read -r f; do
    [ -n "\$f" ] || continue
    if compgen -G "\${f}.[0-9]*.gz" >/dev/null; then n_archived=\$((n_archived+1)); fi
    [ -s "\$f" ] || n_trunc=\$((n_trunc+1))
  done < "\$ROTATE_LIST"
  if [ "\$n_archived" -eq "\$n_expect" ]; then ok "旋转前非空的 \$n_expect 个基文件全部产出 .gz 归档"
  else bad "旋转前非空的基文件全部产出 .gz 归档" "期望 \$n_expect 实际 \$n_archived"; fi
  if [ "\$n_trunc" -eq "\$n_expect" ]; then ok "copytruncate 生效：\$n_expect 个基文件已清空（字节数 0）"
  else bad "copytruncate 生效（基文件清空）" "期望 \$n_expect 实际 \$n_trunc"; fi
fi

# ---------------------------------------------------------------------------
# 断言（check / rotate 共用）
# ---------------------------------------------------------------------------
hr '断言'

# ① 文件存在 / 权限 / 属主
if [ -f "\$TARGET_CONF" ]; then
  perms="\$(stat -c '%a %U:%G' "\$TARGET_CONF")"
  if [ "\$perms" = "644 root:root" ]; then ok "① \$TARGET_CONF 存在且 644 root:root"
  else bad "① \$TARGET_CONF 存在且 644 root:root" "实际 \$perms"; fi
else bad "① \$TARGET_CONF 存在且 644 root:root" "文件不存在"; fi

# ② 与仓库源逐字节一致
if [ -f "\$TARGET_CONF" ]; then
  got="\$(sha256sum "\$TARGET_CONF" | awk '{print \$1}')"
  if [ "\$got" = "\$CONF_SHA" ]; then ok "② 线上片段与仓库源一致（sha256 \${CONF_SHA:0:12}…）"
  else bad "② 线上片段与仓库源一致" "线上=\${got:0:12}… 仓库=\${CONF_SHA:0:12}…"; fi
else bad "② 线上片段与仓库源一致" "文件不存在，无法比对"; fi

# ③ logrotate -d 解析无错
if [ -f "\$TARGET_CONF" ]; then
  dout="\$(logrotate -d "\$TARGET_CONF" 2>&1)"; drc=\$?
  errs="\$(printf '%s\n' "\$dout" | grep -ciE '(^|[^a-z])error:' || true)"
  if [ \$drc -eq 0 ] && [ "\$errs" -eq 0 ]; then
    ok "③ logrotate -d 解析通过（rc=0，error 行 0）"
  else
    bad "③ logrotate -d 解析通过" "rc=\$drc error行=\$errs 样例=\$(printf '%s' "\$dout" | grep -iE 'error:' | head -2 | tr '\n' ' ')"
  fi
  nwarn="\$(printf '%s\n' "\$dout" | grep -ci 'warning' || true)"
  [ "\$nwarn" -gt 0 ] && echo "  （提示：logrotate -d 有 \$nwarn 条 warning，不影响退出码）"
else bad "③ logrotate -d 解析通过" "文件不存在"; fi

# ③b 整份配置 dry-run：定时器每天跑的是 /etc/logrotate.conf（含 /etc/logrotate.d/*），
#    只验自己的片段会漏掉「别人的片段坏了 → 整个 run rc=1 → 天天红」。
fc_out="\$(logrotate -d /etc/logrotate.conf 2>&1)"; fc_rc=\$?
fc_err="\$(printf '%s\n' "\$fc_out" | grep -iE 'error:' || true)"
if [ \$fc_rc -eq 0 ] && [ -z "\$fc_err" ]; then
  ok "③b 整份配置 logrotate -d rc=0 无 error（定时器每天跑的正是这份）"
else
  bad "③b 整份配置 logrotate -d 无错" "rc=\$fc_rc \$(printf '%s' "\$fc_err" | head -3 | tr '\n' ' ')"
  printf '%s\n' "\$fc_err" | head -5 | sed 's/^/    /'
fi

# ④ 按天切割的触发者：logrotate.timer（systemd）或 cron.daily
timer_state="\$(systemctl is-active logrotate.timer 2>/dev/null || echo unknown)"
timer_en="\$(systemctl is-enabled logrotate.timer 2>/dev/null || echo unknown)"
if [ "\$timer_state" = active ]; then
  ok "④ logrotate.timer active（enabled=\$timer_en，OnCalendar=daily → 每天真会跑）"
  last="\$(systemctl show -p LastTriggerUSec --value logrotate.timer 2>/dev/null || true)"
  [ -n "\$last" ] && echo "  上次触发：\$last"
else
  # 没有 systemd timer 时退回 cron.daily
  if [ -x /etc/cron.daily/logrotate ]; then ok "④ cron.daily/logrotate 可执行（timer 未启用，走 cron）"
  else bad "④ 按天触发者存在" "logrotate.timer=\$timer_state 且无 /etc/cron.daily/logrotate"; fi
fi

# ⑤ 归档存在且 gzip 完整
shopt -s nullglob
archives=( "\$LOG_DIR"/*.log.[0-9]*.gz )
shopt -u nullglob
if [ "\${#archives[@]}" -eq 0 ]; then
  if [ "\$MODE" = check ]; then
    skp "⑤ .gz 归档存在且可解压" "当前 0 个归档（尚未切过；跑 --rotate 生成）"
  else
    bad "⑤ .gz 归档存在且可解压" "0 个归档"
  fi
else
  broken=0
  for a in "\${archives[@]}"; do gzip -t "\$a" 2>/dev/null || { broken=\$((broken+1)); echo "   损坏：\$a"; }; done
  if [ "\$broken" -eq 0 ]; then ok "⑤ \${#archives[@]} 个 .gz 归档全部 gzip -t 通过"
  else bad "⑤ .gz 归档完整性" "\$broken/\${#archives[@]} 个损坏"; fi
fi

# ⑥ 归档数不超保留期（每个基名 ≤ RETAIN_DAYS+1，容忍 .1..30 之外的一两个边界）
shopt -s nullglob
over=0
for f in "\$LOG_DIR"/*.log; do
  b="\$(basename "\$f")"
  # 用 compgen 数归档：不能写 ls -1 "\$f".[0-9]*.gz —— nullglob 下无匹配会让 ls 列出当前目录
  cnt=\$(compgen -G "\$f.[0-9]*.gz" | wc -l)
  [ "\$cnt" -gt \$((RETAIN_DAYS+1)) ] && { echo "   超限：\$b 有 \$cnt 个归档"; over=\$((over+1)); }
done
shopt -u nullglob
if [ "\$over" -eq 0 ]; then ok "⑥ 每个基名归档数 ≤ \$((RETAIN_DAYS+1))（rotate \$RETAIN_DAYS 生效上限）"
else bad "⑥ 归档数 ≤ \$((RETAIN_DAYS+1))" "\$over 个基名超限"; fi

# ⑦ journald 保留口径
if [ "\$WITH_JOURNALD" = 1 ]; then
  if [ -f "\$JOURNALD_TARGET" ]; then
    eff="\$(systemd-analyze cat-config systemd/journald.conf 2>/dev/null | grep -E '^MaxRetentionSec=' | tail -1 || true)"
    eff_f="\$(systemd-analyze cat-config systemd/journald.conf 2>/dev/null | grep -E '^MaxFileSec=' | tail -1 || true)"
    if [ "\$eff" = 'MaxRetentionSec=30day' ] && [ "\$eff_f" = 'MaxFileSec=1day' ]; then
      ok "⑦ journald 生效：\$eff_f / \$eff（按天切分 + 保留 30 天）"
    else
      bad "⑦ journald 生效 MaxFileSec=1day / MaxRetentionSec=30day" "实际 '\$eff_f' '\$eff'"
    fi
    js="\$(systemctl is-active systemd-journald 2>/dev/null || echo unknown)"
    [ "\$js" = active ] && ok "⑦b systemd-journald active" || bad "⑦b systemd-journald active" "实际 \$js"
  else
    bad "⑦ journald 保留口径已安装" "\$JOURNALD_TARGET 不存在"
  fi
fi

hr '小计'
printf 'PASS=%d  FAIL=%d  SKIP=%d\n' "\$pass" "\$fail" "\$skip"
if [ "\$fail" -eq 0 ]; then echo 'RESULT=ok'; exit 0; else echo 'RESULT=fail'; exit 1; fi
PAYLOAD
}

# ---------------------------------------------------------------------------
# 执行
# ---------------------------------------------------------------------------
label="${HOST:-本机($(hostname))}"
echo "================ 云枢日志切割 · $label · MODE=$MODE · journald=$WITH_JOURNALD ================"

payload="$(build_payload)"
set +e
if [ -z "$HOST" ]; then
  printf '%s' "$payload" | bash -s
  rc=$?
else
  printf '%s' "$payload" | ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 -o ServerAliveInterval=5 "root@$HOST" 'bash -s'
  rc=$?
fi
set -e

echo "================ $label 结束：rc=$rc ================"
exit "$rc"
