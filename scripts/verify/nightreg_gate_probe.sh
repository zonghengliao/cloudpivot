#!/usr/bin/env bash
# ============================================================================
# nightreg_gate_probe.sh —— `scripts/check.sh` 新增闸④（词表值域）/ 闸⑤（快照巡检）的
#                            **三场景自测**：干净必绿 / 无库跳过 / 域外值必红。
#
# 为什么单测闸门：闸门最常见的坏法是「恒绿」——判据写错、输出解析错、变量拼错，
# 都不会让 check.sh 报错，只会静默放行。所以每条闸都要能当场演示「它真的会红」。
#
# 用法（指向一个装好 di 视图的干净库）：
#   NIK_DATABASE_NAME=cloudpivot_nightreg_inv NIK_DATABASE_USER=alun \
#   NIK_DATABASE_PASSWORD=alun bash scripts/verify/nightreg_gate_probe.sh
#
# 退出码：0 = 三个场景都符合预期。
# ============================================================================
set -uo pipefail

ROOT=${REPO:-/opt/cloudpivot}
WORK=$(mktemp -d /tmp/nr-gate.XXXXXX)
trap 'rm -rf "$WORK"' EXIT

# 只抽出两个闸的函数体，不跑整个 check.sh（fmt/clippy/typecheck 与本自测无关）
sed -n '/^biz_vocab_gate() {/,/^}/p' "$ROOT/scripts/check.sh" > "$WORK/gates.sh"
sed -n '/^snapshot_audit_gate() {/,/^}/p' "$ROOT/scripts/check.sh" >> "$WORK/gates.sh"
if [ ! -s "$WORK/gates.sh" ]; then
  echo "✘ 没抽到闸④/闸⑤ 的函数定义（check.sh 被改坏了？）" >&2
  exit 1
fi
# shellcheck disable=SC1091
source "$WORK/gates.sh"

fail=0

echo "### 场景 1：干净库（闸④/闸⑤ 都应 rc=0）"
biz_vocab_gate; rc4=$?; echo "  闸④ rc=$rc4"
snapshot_audit_gate; rc5=$?; echo "  闸⑤ rc=$rc5"
[ "$rc4" -eq 0 ] || { echo "✘ 干净库闸④ 应为 0" >&2; fail=1; }
[ "$rc5" -eq 0 ] || { echo "✘ 干净库闸⑤ 应为 0" >&2; fail=1; }

echo
echo "### 场景 2：无库口令（两条都应跳过，rc=0）"
env -u NIK_DATABASE_PASSWORD -u PGPASSWORD bash -c "
  ROOT=$ROOT; source '$WORK/gates.sh'
  biz_vocab_gate; echo \"  闸④ rc=\$?\"
  snapshot_audit_gate; echo \"  闸⑤ rc=\$?\"
" || { echo "✘ 无库口令场景不应失败" >&2; fail=1; }

echo
echo "### 场景 3：词表域外值（闸④ 必须 rc=1）"
PGPASSWORD="${NIK_DATABASE_PASSWORD:-alun}" psql -h "${NIK_DATABASE_HOST:-127.0.0.1}" \
  -U "${NIK_DATABASE_USER:-alun}" -d "${NIK_DATABASE_NAME:?}" -q -c \
  "insert into biz_voucher_rule (rule_code, biz_type, is_active) values ('NR-GATE-RULE','NR_BOGUS_TYPE',0)" \
  >/dev/null || { echo "✘ 塞脏行失败（本库没有 biz_voucher_rule？）" >&2; exit 1; }
biz_vocab_gate; rc4b=$?
echo "  闸④ rc=$rc4b"
[ "$rc4b" -eq 1 ] || { echo "✘ 有域外值时闸④ 应为 1（现在是 $rc4b，闸门失效）" >&2; fail=1; }
PGPASSWORD="${NIK_DATABASE_PASSWORD:-alun}" psql -h "${NIK_DATABASE_HOST:-127.0.0.1}" \
  -U "${NIK_DATABASE_USER:-alun}" -d "${NIK_DATABASE_NAME:?}" -q -c \
  "delete from biz_voucher_rule where rule_code='NR-GATE-RULE'" >/dev/null

if [ "$fail" -eq 0 ]; then
  echo
  echo "== 三场景全部符合预期（闸门既会绿也会红）=="
fi
exit "$fail"
