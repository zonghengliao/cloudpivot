#!/usr/bin/env bash
# 第 ⑧ 条（icon 名必须在 TDesign 图标库里存在）的「注入 → 报警 → 复原」验收脚本。
# 只在一次性测试库上跑；每个场景自校验 PASS/FAIL，末尾打印汇总（当前 40 项）。
#
# 用法：DB=cloudpivot_icon8_xxxx bash scripts/verify/icon_invariant_teeth.sh
set -uo pipefail

DB="${DB:?需要 DB=<测试库名>（只能是一次性副本库，别指生产库）}"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRIPT="$REPO/scripts/verify/menu_invariants.sh"
OUT="${OUT:-$(mktemp -d "${TMPDIR:-/tmp}/icon8.XXXXXX")}"
SCRATCH=$(mktemp -d "${TMPDIR:-/tmp}/icon8_scratch.XXXXXX")
mkdir -p "$OUT"
cleanup() { rm -rf "$SCRATCH"; }
trap cleanup EXIT
export PGPASSWORD="${PGPASSWORD:-alun}" PGUSER="${PGUSER:-alun}" PGHOST="${PGHOST:-127.0.0.1}"
export LC_ALL=C LANG=C
PSQL=(psql -X -h "$PGHOST" -U "$PGUSER" -d "$DB" -tA -q -v ON_ERROR_STOP=1)

PASS=0; FAIL=0; declare -a LINES=()
chk() {  # chk <名> <期望值> <实际值>
  local name=$1 want=$2 got=$3
  if [ "$want" = "$got" ]; then PASS=$((PASS+1)); LINES+=("PASS  $name（$got）")
  else FAIL=$((FAIL+1)); LINES+=("FAIL  $name（期望 $want，实际 $got）"); fi
}
grep_out() { grep -q -- "$1" "$2" && echo 1 || echo 0; }

run() {  # run <tag> [env...]  → 输出写 $OUT/<tag>.txt，回显 rc
  local tag=$1; shift
  env "$@" bash "$SCRIPT" "$DB" > "$OUT/$tag.txt" 2>&1
  echo $?
}

echo "测试库: $DB   证据目录: $OUT"

# ── S1 基线：⑧ 应直接绿 ────────────────────────────────────────────────────
rc=$(run s1_baseline)
chk "S1 基线 rc" 0 "$rc"
chk "S1 ⑧ 打印 ✅" 1 "$(grep_out '⑧ icon 名合法: .*✅' "$OUT/s1_baseline.txt")"
chk "S1 打印了图标库来源" 1 "$(grep_out '图标库：.*tdesign-icons' "$OUT/s1_baseline.txt")"
chk "S1 没有跳过字样" 0 "$(grep_out '⏭' "$OUT/s1_baseline.txt")"

# ── S2 注入一个坏 icon（shopping-cart，就是两次事故的那个名字）────────────────
# 先记下这两行的原始 icon，S4 一律按原值复原（别写死 cart/bill，免得复原本身也不准）
# /business/erp/expense 有 2 行（我的报销/费用报销），一律按 id 定位，别用 path 批量改
PUR_ID=$("${PSQL[@]}" -c "select id from sys_menu where path='/business/erp/purchase-order' and deleted_at is null limit 1")
EXP_ID=$("${PSQL[@]}" -c "select id from sys_menu where path='/business/erp/expense' and deleted_at is null order by id limit 1")
ORIG_PUR=$("${PSQL[@]}" -c "select icon from sys_menu where id='$PUR_ID'")
ORIG_EXP=$("${PSQL[@]}" -c "select icon from sys_menu where id='$EXP_ID'")
echo "复原基线: purchase-order/$PUR_ID=$ORIG_PUR  expense/$EXP_ID=$ORIG_EXP"
"${PSQL[@]}" -c "update sys_menu set icon='shopping-cart' where id='$PUR_ID'" >/dev/null
N=$("${PSQL[@]}" -c "select count(*) from sys_menu where deleted_at is null and icon='shopping-cart'")
rc=$(run s2_bad_one)
chk "S2 注入行数" 1 "$N"
chk "S2 rc" 1 "$rc"
chk "S2 ⑧ 计数=1" 1 "$(grep_out '⑧ icon 名合法: *1 *❌' "$OUT/s2_bad_one.txt")"
chk "S2 打印坏名 shopping-cart" 1 "$(grep_out 'shopping-cart' "$OUT/s2_bad_one.txt")"
chk "S2 打印所在 path" 1 "$(grep_out '/business/erp/purchase-order' "$OUT/s2_bad_one.txt")"
chk "S2 结论里点名 ⑧" 1 "$(grep_out '⑧icon名不存在' "$OUT/s2_bad_one.txt")"

# ── S3 再注入第二个坏名（invoice，老库里真实出现过的错名）：两条都要报出来 ────
"${PSQL[@]}" -c "update sys_menu set icon='invoice' where id='$EXP_ID'" >/dev/null
N=$("${PSQL[@]}" -c "select count(*) from sys_menu where deleted_at is null and icon in ('shopping-cart','invoice')")
rc=$(run s3_bad_two)
chk "S3 注入行数" 2 "$N"
chk "S3 rc" 1 "$rc"
chk "S3 ⑧ 计数=2" 1 "$(grep_out '⑧ icon 名合法: *2 *❌' "$OUT/s3_bad_two.txt")"
chk "S3 shopping-cart 的 path 打印" 1 "$(grep_out '/business/erp/purchase-order' "$OUT/s3_bad_two.txt")"
chk "S3 invoice 的 path 打印" 1 "$(grep_out '/business/erp/expense' "$OUT/s3_bad_two.txt")"

# ── S3b icon 里塞换行：两段都是真名字（bill / money），必须被「形状非法」抓到 ────
# 这一条防的是本脚本自己的假绿：若只做「一行一条」的差集，'bill\nmoney' 会被拆成两行且两段都命中
"${PSQL[@]}" -c "update sys_menu set icon='bill' || chr(10) || 'money' where id='$EXP_ID'" >/dev/null
LEN=$("${PSQL[@]}" -c "select length(icon) from sys_menu where id='$EXP_ID'")
rc=$(run s3b_newline)
chk "S3b 注入的是 10 字符带换行的 icon" 10 "$LEN"
chk "S3b rc=1（换行的 icon 不能假绿）" 1 "$rc"
chk "S3b 报「含非法字符」" 1 "$(grep_out 'icon 里含非法字符' "$OUT/s3b_newline.txt")"
chk "S3b 打印可见化的 \\n" 1 "$(grep_out 'bill\\nmoney' "$OUT/s3b_newline.txt")"

# ── S4 复原 → 全绿 ─────────────────────────────────────────────────────────
"${PSQL[@]}" -c "update sys_menu set icon='$ORIG_PUR' where id='$PUR_ID'" >/dev/null
"${PSQL[@]}" -c "update sys_menu set icon='$ORIG_EXP' where id='$EXP_ID'" >/dev/null
N=$("${PSQL[@]}" -c "select count(*) from sys_menu where deleted_at is null and (icon in ('shopping-cart','invoice') or icon ~ E'[\\t\\n]')")
REST_PUR=$("${PSQL[@]}" -c "select icon from sys_menu where id='$PUR_ID'")
REST_EXP=$("${PSQL[@]}" -c "select icon from sys_menu where id='$EXP_ID'")
chk "S4 复原按原值（purchase-order）" "$ORIG_PUR" "$REST_PUR"
chk "S4 复原按原值（expense）" "$ORIG_EXP" "$REST_EXP"
rc=$(run s4_restored)
chk "S4 复原后坏 icon 0 行" 0 "$N"
chk "S4 rc" 0 "$rc"
chk "S4 ⑧ 回到 ✅" 1 "$(grep_out '⑧ icon 名合法: *0 *✅' "$OUT/s4_restored.txt")"

# ── S5 没有前端依赖的机器：跳过 + 打印原因，不报红 ────────────────────────────
# 假仓库根 / 临时清单都放 SCRATCH（$TMPDIR），**不要放 $OUT** —— OUT 可能被指到仓库里的
# 证据目录，拷贝一份 script + icons.ts 进去就是把垃圾提交进 git（踩过）。
FAKE=$(mktemp -d "$SCRATCH/fakeroot.XXXXXX")
mkdir -p "$FAKE/scripts/verify"
cp "$SCRIPT" "$FAKE/scripts/verify/"
rc=$(MENU_REPO_ROOT="$FAKE" bash "$SCRIPT" "$DB" > "$OUT/s5_no_lib.txt" 2>&1; echo $?)
chk "S5 缺库 rc=0（不报红）" 0 "$rc"
chk "S5 打印 ⏭ 跳过" 1 "$(grep_out '⏭  跳过（找不到图标库' "$OUT/s5_no_lib.txt")"
chk "S5 跳过原因带路径" 1 "$(grep_out '试过：.*node_modules' "$OUT/s5_no_lib.txt")"
chk "S5 结论写明有跳过（不静默绿）" 1 "$(grep_out '有跳过.*⑧icon名' "$OUT/s5_no_lib.txt")"

# ── S6 MENU_ICON_LIB_REQUIRED=1：缺库必须 rc=2（不许假绿）───────────────────
rc=$(MENU_REPO_ROOT="$FAKE" MENU_ICON_LIB_REQUIRED=1 bash "$SCRIPT" "$DB" > "$OUT/s6_required.txt" 2>&1; echo $?)
chk "S6 缺库+required rc=2" 2 "$rc"
chk "S6 提示基础设施问题" 1 "$(grep_out '无法校验 icon 名' "$OUT/s6_required.txt")"

# ── S7 MENU_ICON_LIB 显式给清单文件（不给 cart）→ 能报红，证明覆盖生效 ────────
LIST=$(mktemp "$SCRATCH/names.XXXXXX")
printf 'add-circle\napp\nfolder\n' > "$LIST"
rc=$(MENU_ICON_LIB="$LIST" bash "$SCRIPT" "$DB" > "$OUT/s7_custom_list.txt" 2>&1; echo $?)
chk "S7 自定义清单 rc=1" 1 "$rc"
chk "S7 ⑧ 报红" 1 "$(grep_out '⑧ icon 名合法:.*❌' "$OUT/s7_custom_list.txt")"
chk "S7 报出的坏名数 >50" 1 "$([ "$(grep -o '⑧ icon 名合法: *[0-9]*' "$OUT/s7_custom_list.txt" | grep -o '[0-9]*')" -gt 50 ] && echo 1 || echo 0)"
chk "S7 来源显示为清单文件" 1 "$(grep_out "图标库：$LIST" "$OUT/s7_custom_list.txt")"

# ── S8 没有 node_modules 但有仓库内清单 icons.ts → 用它校验，仍然全绿 ─────────
FAKE2=$(mktemp -d "$SCRATCH/fakeroot2.XXXXXX")
mkdir -p "$FAKE2/scripts/verify" "$FAKE2/frontend/src/components/IconSelect"
cp "$SCRIPT" "$FAKE2/scripts/verify/"
cp "$REPO/frontend/src/components/IconSelect/icons.ts" "$FAKE2/frontend/src/components/IconSelect/"
rc=$(MENU_REPO_ROOT="$FAKE2" bash "$FAKE2/scripts/verify/menu_invariants.sh" "$DB" > "$OUT/s8_ts_fallback.txt" 2>&1; echo $?)
chk "S8 用 icons.ts 兜底 rc=0" 0 "$rc"
chk "S8 来源是 icons.ts" 1 "$(grep_out 'IconSelect/icons.ts' "$OUT/s8_ts_fallback.txt")"
chk "S8 解析出 2356 个名字" 1 "$(grep_out '（2356 个名字）' "$OUT/s8_ts_fallback.txt")"

# ── S9 MENU_ICON_LIB 指到 manifest.js（另一种清单格式）→ 一样能校验 ──────────
MAN="$REPO/frontend/node_modules/tdesign-icons-vue-next/esm/manifest.js"
if [ -f "$MAN" ]; then
  rc=$(MENU_ICON_LIB="$MAN" bash "$SCRIPT" "$DB" > "$OUT/s9_manifest.txt" 2>&1; echo $?)
  chk "S9 manifest.js rc=0" 0 "$rc"
  chk "S9 解析出 2356 个名字" 1 "$(grep_out '（2356 个名字）' "$OUT/s9_manifest.txt")"
fi

# ── S10 收尾复核：库里不变量 8 条仍全绿 ─────────────────────────────────────
rc=$(run s10_final)
chk "S10 收尾 rc=0" 0 "$rc"

printf '\n══════ ⑧ 牙齿汇总 ══════\n'
printf '%s\n' "${LINES[@]}"
printf 'PASS=%d FAIL=%d  证据: %s\n' "$PASS" "$FAIL" "$OUT"
[ "$FAIL" -eq 0 ] || exit 1
