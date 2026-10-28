#!/usr/bin/env bash
# =============================================================================
# 双版本往返一致性校验（verify-roundtrip.sh）
#
# 做什么：
#   以「改名后的仓库（云枢版）」为基准，验证
#      反向改名 → 正向改名  ==  原树（逐字节一致，除 .git）
#   即证明两个版本之间**只存在命名差异**，不存在其它任何漂移。
#
# 顺带验证：
#   * 反向结果里不得残留 cloudpivot / CloudPivot / 云枢（script 自身会做，这里再独立 grep 一遍）
#   * 幂等：反向跑两次、正向跑两次，第二次都应 0 改动
#
# 用法：
#   bash 01-rename/verify-roundtrip.sh                     # 基准 = 当前 git 仓库 HEAD（云枢版）
#   bash 01-rename/verify-roundtrip.sh --base /opt/cloudpivot
#   bash 01-rename/verify-roundtrip.sh --base /root/work/cailu-version --cailu-base
#   bash 01-rename/verify-roundtrip.sh --work /tmp/rt --keep   # 保留中间产物便于排查
#
#   --cailu-base  基准树是「原名版」：则先正向再反向（默认是先反向再正向）
#
# 退出码：0 全部通过；1 有一项失败（会打印失败项）
# =============================================================================
set -uo pipefail

BASE=""
WORK=""
KEEP=0
CAILU_BASE=0
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RENAME="$HERE/rename.sh"

while [ $# -gt 0 ]; do
  case "$1" in
    --base)  BASE="${2:?}"; shift 2 ;;
    --work)  WORK="${2:?}"; shift 2 ;;
    --keep)  KEEP=1; shift ;;
    --cailu-base) CAILU_BASE=1; shift ;;
    -h|--help) sed -n '2,24p' "$0"; exit 0 ;;
    *) echo "未知参数: $1" >&2; exit 2 ;;
  esac
done

if [ -z "$BASE" ]; then
  BASE="$(git rev-parse --show-toplevel 2>/dev/null)" || {
    echo "✘ 不在 git 仓库里，请用 --base 指定基准树" >&2; exit 2; }
fi
BASE="$(cd "$BASE" && pwd)"
[ -x "$RENAME" ] || { echo "✘ 找不到 rename.sh: $RENAME" >&2; exit 2; }

if [ -z "$WORK" ]; then
  WORK="$(mktemp -d "${TMPDIR:-/tmp}/roundtrip.XXXXXX")"
else
  mkdir -p "$WORK"
fi

# 方向编排：默认「基准=云枢版」→ 先反向再做正向；--cailu-base 反过来
if [ "$CAILU_BASE" -eq 1 ]; then
  D1=""; D2="--reverse"
  N1="正向改名（cailu → cloudpivot）"; N2="再反向改名（cloudpivot → cailu）"
  RESIDUE_RE='cloudpivot|CloudPivot|云枢'; RESIDUE_DESC="改名后（云枢版）无 cailu/菜芦 残留（migration-kit 除外）"
  RESIDUE_RE2='cailu|Cailu|菜芦'
  NOTE_BASE="基准树是原名版（--cailu-base）"
else
  D1="--reverse"; D2=""
  N1="反向改名（cloudpivot → cailu）"; N2="再正向改名（cailu → cloudpivot）"
  RESIDUE_RE='cloudpivot|CloudPivot|云枢'; RESIDUE_DESC="反向结果无 cloudpivot/云枢 残留（migration-kit 除外）"
  RESIDUE_RE2='cloudpivot|CloudPivot|云枢'
  NOTE_BASE="基准树是云枢版"
fi
P1="$WORK/step1"     # 第一方向产物
P2="$WORK/step2"     # 第二方向产物（应 == BASE）
BASE_CLEAN="$WORK/base-clean"  # 基准树的「受版本管理文件」快照（避免把 target/node_modules 等运行态算进 diff）
mkdir -p "$P1" "$P2" "$BASE_CLEAN"

# 只复制「纳入版本管理的文件」，避开 node_modules / target / 运行时目录
copy_tree() {  # $1=src  $2=dst
  if git -C "$1" rev-parse --git-dir >/dev/null 2>&1; then
    git -C "$1" ls-files -z | tar -C "$1" --null -T - -cf - 2>/dev/null | tar -x -C "$2"
  else
    tar -C "$1" --exclude=.git --exclude=node_modules --exclude=target --exclude=dist \
        --exclude=.rename-backup -cf - . | tar -x -C "$2"
  fi
}

FAIL=0
ok()   { echo "  ✅ $*"; }
bad()  { echo "  ❌ $*"; FAIL=1; }

echo "基准树: $BASE  （$NOTE_BASE）"
echo "工作区: $WORK"
echo "---------------------------------------------"

copy_tree "$BASE" "$P1"

echo "[1/4] $N1"
OUT_REV="$WORK/step1.log"
bash "$RENAME" $D1 --root "$P1" --no-backup >"$OUT_REV" 2>&1
RC_REV=$?
if [ $RC_REV -eq 0 ]; then ok "执行成功（退出码 0）"; else bad "执行失败（退出码 $RC_REV），见 $OUT_REV"; fi
if [ "$CAILU_BASE" -eq 1 ]; then
  # 第一步产物应是云枢版：抽查 crate 名
  if grep -q '^name = "cloudpivot"' "$P1/backend/Cargo.toml" 2>/dev/null; then
    ok "第一步产物已是云枢版（backend crate = cloudpivot）"
  else
    bad "第一步产物不是云枢版（backend/Cargo.toml crate 名不对）"
  fi
else
  LEFT=$(grep -rlE 'cloudpivot|CloudPivot|云枢' --exclude-dir=migration-kit \
          --exclude-dir=node_modules --exclude-dir=target --exclude-dir=dist \
          --exclude=双版本说明.md "$P1" 2>/dev/null | wc -l)
  if [ "$LEFT" -eq 0 ]; then ok "反向产物无 cloudpivot/云枢 残留（migration-kit 除外）"
  else bad "反向产物仍有 $LEFT 个文件残留新标识（见 grep）"; fi
fi

echo "[2/4] $N2"
copy_tree "$P1" "$P2" 2>/dev/null || { tar -C "$P1" -cf - . | tar -x -C "$P2"; }
OUT_FWD="$WORK/step2.log"
bash "$RENAME" $D2 --root "$P2" --no-backup >"$OUT_FWD" 2>&1
RC_FWD=$?
if [ $RC_FWD -eq 0 ]; then ok "执行成功（退出码 0）"; else bad "执行失败（退出码 $RC_FWD），见 $OUT_FWD"; fi

echo "[3/4] 逐字节比对 往返结果 vs 基准树（只比受版本管理的文件）"
copy_tree "$BASE" "$BASE_CLEAN" 2>/dev/null || { tar -C "$BASE" --exclude=.git --exclude=node_modules \
  --exclude=target --exclude=dist -cf - . | tar -x -C "$BASE_CLEAN"; }
DIFFOUT="$WORK/diff.txt"
if diff -r --exclude=.git "$P2" "$BASE_CLEAN" >"$DIFFOUT" 2>&1; then
  ok "diff -r 无任何差异 —— 两版只差命名（逐字节一致）"
else
  bad "diff -r 有差异（$(wc -l <"$DIFFOUT") 行），见 $DIFFOUT"
  head -40 "$DIFFOUT" | sed 's/^/      /'
fi

echo "[4/4] 幂等性（两个方向各再跑一次，应 0 改动）"
bash "$RENAME" $D1 --root "$P1" --no-backup >"$WORK/step1b.log" 2>&1
R2=$(grep -o '内容改动文件: [0-9]*' "$WORK/step1b.log" | grep -o '[0-9]*$')
bash "$RENAME" $D2 --root "$P2" --no-backup >"$WORK/step2b.log" 2>&1
F2=$(grep -o '内容改动文件: [0-9]*' "$WORK/step2b.log" | grep -o '[0-9]*$')
[ "${R2:-x}" = "0" ] && ok "第一方向幂等（第二次 0 改动）" || bad "第一方向不幂等（第二次改动 ${R2:-?} 个文件）"
[ "${F2:-x}" = "0" ] && ok "第二方向幂等（第二次 0 改动）" || bad "第二方向不幂等（第二次改动 ${F2:-?} 个文件）"

echo "---------------------------------------------"
if [ $FAIL -eq 0 ]; then
  echo "✅ 往返一致性校验全部通过（改名映射表严格互逆）"
  [ $KEEP -eq 0 ] && rm -rf "$WORK" 2>/dev/null
  exit 0
else
  echo "❌ 往返一致性校验失败 —— 说明映射表有遗漏/不对称，必须补齐"
  echo "   中间产物保留在: $WORK"
  exit 1
fi
