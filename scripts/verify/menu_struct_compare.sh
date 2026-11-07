#!/usr/bin/env bash
# =============================================================================
# menu_struct_compare.sh — 两个库/基线的菜单结构比对（卡 MENU-03）
#
# 用法：
#   bash scripts/verify/menu_struct_compare.sh <A库> <B库|基线文件> [--strict]
#
#   例：bash scripts/verify/menu_struct_compare.sh cloudpivot_fresh \
#           scripts/verify/baseline/menu_structure_baseline.txt
#
# 口径（与 menu_structure_dump.sh 一致）：
#   每个未删菜单一行 `path|parent_path|menu_type`，只比**导航骨架**。
#   不比 name / title / sort_order / icon / is_visible：那些是业务数据或审美差异，
#   改动它们不会让侧栏"乱"（层级与类型才会）。
#
# 归一（等价规则）：
#   结构里存在**已备案的等价表示**，硬比字符串会永远红、最后没人看。规则放在
#   scripts/verify/baseline/menu_struct_equiv.tsv（带理由、可审计、可整份关掉），
#   比对前对**两侧**同时归一。默认输出「原始差异」与「归一后差异」两个数，
#   --strict 时原始差异必须为 0（用于审计规则本身是否还在被需要）。
#
# 退出码：0 = 归一后 0 差异（严格模式再加原始 0 差异）｜1 = 有差异 ｜2 = 连库/参数失败
# =============================================================================
set -uo pipefail

A="${1:-}"; B="${2:-}"; shift 2 2>/dev/null || true
STRICT=0
for a in "$@"; do
    case "$a" in
        --strict) STRICT=1 ;;
        *) echo "⛔ 未知参数：$a" >&2; exit 2 ;;
    esac
done
[ -n "$A" ] && [ -n "$B" ] || { sed -n '3,8p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2; exit 2; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
DUMP="$SCRIPT_DIR/menu_structure_dump.sh"
[ -f "$DUMP" ] || { echo "⛔ 找不到 menu_structure_dump.sh（应与本脚本同目录）" >&2; exit 2; }
RULES="${MENU_STRUCT_EQUIV:-$SCRIPT_DIR/baseline/menu_struct_equiv.tsv}"

TMPD="$(mktemp -d "${TMPDIR:-/tmp}/menu_cmp_XXXXXX")"
trap 'rm -rf "$TMPD"' EXIT

# ── 1) 取两侧结构 ───────────────────────────────────────────────────────────
dump_side() { # $1=来源（库名或文件）  $2=输出文件  $3=标签
    if [ -f "$1" ]; then
        cp "$1" "$2"
        printf '%s' "文件：$1（md5 $(md5sum "$1" | cut -d' ' -f1)）"
    else
        grep -qE '^[a-z0-9_]+$' <<<"$1" || { echo "⛔ 来源既不是文件也不是合法库名：$1" >&2; exit 2; }
        bash "$DUMP" "$1" "$2" >/dev/null || { echo "⛔ 结构导出失败（库 $1）" >&2; exit 2; }
        printf '%s' "连库现取：$1@${PGHOST:-127.0.0.1}"
    fi
}

SRC_A=$(dump_side "$A" "$TMPD/a.txt") || exit 2
SRC_B=$(dump_side "$B" "$TMPD/b.txt") || exit 2
LC_ALL=C sort "$TMPD/a.txt" -o "$TMPD/a.txt"
LC_ALL=C sort "$TMPD/b.txt" -o "$TMPD/b.txt"

# ── 2) 应用等价规则（两侧同时归一） ─────────────────────────────────────────
# 规则文件格式（TSV，# 开头为注释，空行忽略）：
#   path    -                     <from_path>      <to_path>        <理由>
#   parent  <子节点 path>          <from_parent>    <to_parent>      <理由>
# 语义：把 <from> 归一成 <to>，方向恒为「历史/环境特例 → 迁移链 canonical」。
apply_rules() { # $1=输入  $2=输出
    if [ ! -f "$RULES" ] || ! grep -qE '^[[:space:]]*(path|parent)[[:space:]]' "$RULES"; then
        LC_ALL=C sort "$1" -o "$2"; return 0
    fi
    LC_ALL=C sort "$1" | awk -F'|' -v rules="$RULES" -v cnt="$TMPD/hits.tsv" -v OFS='|' '
        BEGIN {
            n = 0
            while ((getline line < rules) > 0) {
                if (line ~ /^[[:space:]]*#/ || line ~ /^[[:space:]]*$/) continue
                split(line, f, "\t")
                if (f[1] != "path" && f[1] != "parent") continue
                n++; kind[n] = f[1]; subj[n] = f[2]; from[n] = f[3]; to[n] = f[4]; why[n] = f[5]
            }
            close(rules)
        }
        {
            p = $1; par = $2; t = $3
            for (i = 1; i <= n; i++) {
                if (kind[i] == "path") {
                    if (p   == from[i]) { p   = to[i]; hit[i]++ }
                    if (par == from[i]) { par = to[i]; hit[i]++ }
                } else if (kind[i] == "parent") {
                    if (p == subj[i] && par == from[i]) { par = to[i]; hit[i]++ }
                }
            }
            print p, par, t
        }
        END {
            for (i = 1; i <= n; i++)
                printf "%s\t%s\t%s\t%s\t%d\t%s\n", kind[i], subj[i], from[i], to[i], (i in hit ? hit[i] : 0), why[i] > cnt
        }' > "$2"
    if [ ! -s "$2" ] || [ "$(grep -c . "$2")" != "$(grep -c . "$1")" ]; then
        echo "⛔ 等价规则归一失败（awk 输出行数与输入不一致）：$RULES" >&2
        return 2
    fi
}

apply_rules "$TMPD/a.txt" "$TMPD/a_norm.txt" || exit 2
apply_rules "$TMPD/b.txt" "$TMPD/b_norm.txt" || exit 2

# ── 3) 比对 ─────────────────────────────────────────────────────────────────
count_diff() { LC_ALL=C comm -23 "$1" "$2" | grep -c . || true; }

RAW_ONLY_A=$(count_diff "$TMPD/a.txt" "$TMPD/b.txt")
RAW_ONLY_B=$(count_diff "$TMPD/b.txt" "$TMPD/a.txt")
NORM_ONLY_A=$(count_diff "$TMPD/a_norm.txt" "$TMPD/b_norm.txt")
NORM_ONLY_B=$(count_diff "$TMPD/b_norm.txt" "$TMPD/a_norm.txt")
NORM_COMMON=$(LC_ALL=C comm -12 "$TMPD/a_norm.txt" "$TMPD/b_norm.txt" | grep -c . || true)

echo "═══ 菜单结构比对（path|parent_path|menu_type）═══"
echo "A（新库）: $SRC_A  →  $(grep -c . "$TMPD/a.txt") 个节点"
echo "B（对照）: $SRC_B  →  $(grep -c . "$TMPD/b.txt") 个节点"
echo "相同（归一后）: $NORM_COMMON"

if [ -f "$TMPD/hits.tsv" ]; then
    TOTAL_HITS=$(awk -F'\t' '{s += $5} END {print s + 0}' "$TMPD/hits.tsv")
    if [ "$TOTAL_HITS" -gt 0 ]; then
        echo
        echo "已备案的等价规则（$RULES）："
        awk -F'\t' '$5 > 0 {
            if ($1 == "parent") printf "  · parent %s：%s → %s（命中 %d 行）%s\n", $2, $3, $4, $5, ($6 != "" ? "：" $6 : "");
            else                printf "  · path   %s → %s（命中 %d 行）%s\n", $3, $4, $5, ($6 != "" ? "：" $6 : "");
        }' "$TMPD/hits.tsv"
    else
        # 两侧都已是 canonical（例如"新库 vs 迁移链基线"），规则本就无需生效，不算待清理
        echo "（本次比对未触达任何等价规则：两侧均为 canonical 表示）"
    fi
    UNUSED=$(awk -F'\t' '$5 == 0 {printf "%s|%s|%s;", $1, $3, $4}' "$TMPD/hits.tsv")
    if [ -n "$UNUSED" ] && [ "$TOTAL_HITS" -gt 0 ]; then
        echo "  ⚠ 未被命中的规则（可能已收敛，建议清理）：$UNUSED"
    fi
fi

echo
echo "原始差异: 仅 A $RAW_ONLY_A ｜ 仅 B $RAW_ONLY_B"
echo "归一后差异: 仅 A $NORM_ONLY_A ｜ 仅 B $NORM_ONLY_B"

if [ "$RAW_ONLY_A" -ne 0 ] || [ "$RAW_ONLY_B" -ne 0 ]; then
    echo
    echo "原始差异明细（< 仅 A ｜ > 仅 B，最多各 20 条）："
    LC_ALL=C comm -23 "$TMPD/a.txt" "$TMPD/b.txt" | head -20 | sed 's/^/  < /'
    LC_ALL=C comm -13 "$TMPD/a.txt" "$TMPD/b.txt" | head -20 | sed 's/^/  > /'
fi

if [ "$NORM_ONLY_A" -ne 0 ] || [ "$NORM_ONLY_B" -ne 0 ]; then
    echo
    echo "归一后仍存在的差异（真正的结构分叉，< 仅 A ｜ > 仅 B，最多各 20 条）："
    LC_ALL=C comm -23 "$TMPD/a_norm.txt" "$TMPD/b_norm.txt" | head -20 | sed 's/^/  < /'
    LC_ALL=C comm -13 "$TMPD/a_norm.txt" "$TMPD/b_norm.txt" | head -20 | sed 's/^/  > /'
    echo
    echo "❌ 结构不一致：$((NORM_ONLY_A + NORM_ONLY_B)) 处（归一后）"
    echo "   原因只有两种：① 有人手工改过库（应补幂等迁移，别手工改新库）；"
    echo "                ② 迁移链与基线分叉（补迁移或重新生成基线）。两条路都别用 pg_dump 拷库绕过。"
    exit 1
fi

if [ "$STRICT" = "1" ] && { [ "$RAW_ONLY_A" -ne 0 ] || [ "$RAW_ONLY_B" -ne 0 ]; }; then
    echo
    echo "❌ --strict：归一后虽一致，但原始差异 $((RAW_ONLY_A + RAW_ONLY_B)) 处（等价规则还在生效，说明两侧表示仍未统一）"
    exit 1
fi

echo
if [ "$STRICT" = "1" ]; then
    echo "✅ 结构一致（严格模式：原始与归一后均 0 差异）"
else
    echo "✅ 结构一致（归一后 0 差异；原始差异 $((RAW_ONLY_A + RAW_ONLY_B)) 处均为已备案等价表示）"
fi
