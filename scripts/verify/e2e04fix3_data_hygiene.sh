#!/usr/bin/env bash
# E2E04-FIX-3 ③④：共享库数据卫生（可幂等重跑）
#
# 两个「别人直插 SQL 留下的夹具」造成的假红源：
#
#   ③ 税率标度残留：`biz_material.tax_rate ∈ (0,1]` 且 `tax_rate_mode IS NULL`。
#      FIX-6（V0327）起全系统统一**百分数**口径（`13` = 13%），LEGACY（不标 mode）
#      的 (0,1] 入参按 tax_rate.rs 的既有规则 ×100 —— 所以这些行在系统眼里**本来就
#      是 13%**，只是库里还存着 0.13 且没标 mode。本脚本把它们显式落成 `13 + PERCENT`，
#      与现行口径一致，避免「同一个 13% 在库里有两套写法」。
#
#   ④ 已记账却零分录的凭证：`status='POSTED'` 且 `biz_voucher_entry` 里一条都没有。
#      「已记账凭证必有分录」是会计不变量（分录才是账）；这种裸头会让后续对账脚本
#      要么假红、要么被逼着写 `voucher_no <> 'xxx'` 的白名单（假绿）。
#
# ⚠️ 安全边界：本脚本**不会**用通配条件删凭证。要删的凭证号必须逐个点名
#    （`--purge-orphan-posted P2C2-V-0001`），且脚本会先把它整行打出来复核。
#    税率归一用固定判据（只会碰到 (0,1] + mode 为空的行），dry-run 先列清单。
#
# 用法：
#   bash scripts/verify/e2e04fix3_data_hygiene.sh --db-url postgres://u:p@h:5432/db
#       只看（dry-run，默认）
#   ... --fix                                  归一税率残留
#   ... --fix --purge-orphan-posted P2C2-V-0001 归一税率 + 点名删掉指定的裸头凭证
#
# 连接串也可以从环境变量 `ERP_DB_URL` 取。
set -uo pipefail

DB_URL="${ERP_DB_URL:-}"
FIX=0
PURGE_NOS=()

usage() {
  sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'
}

while [ $# -gt 0 ]; do
  case "$1" in
    --db-url) DB_URL="${2:-}"; shift 2 ;;
    --fix) FIX=1; shift ;;
    --purge-orphan-posted)
      case "${2:-}" in
        ""|--*) echo "✗ --purge-orphan-posted 需要一个凭证号" >&2; exit 2 ;;
        *) PURGE_NOS+=("$2"); shift 2 ;;
      esac ;;
    -h|--help) usage; exit 0 ;;
    *) echo "✗ 未知参数：$1" >&2; exit 2 ;;
  esac
done

if [ -z "$DB_URL" ]; then
  echo "✗ 缺少数据库连接：传 --db-url 或设 ERP_DB_URL" >&2
  exit 2
fi

# 统一出口：psql 出错立刻停（不静默吞错）
q() { psql "$DB_URL" -v ON_ERROR_STOP=1 -qtA -c "$1"; }
say() { printf '\n%s\n' "$*"; }

FAILED=0

say "=============================================================="
say "E2E04-FIX-3 ③ 税率标度残留（tax_rate ∈ (0,1] 且 tax_rate_mode 为空）"
say "=============================================================="
TAX_ROWS=$(q "SELECT material_code || ' | ' || material_name || ' | tax_rate=' || tax_rate::text
                     || ' | mode=' || coalesce(tax_rate_mode, '<NULL>') || ' | created=' || created_at::date
                FROM biz_material
               WHERE deleted_at IS NULL AND tax_rate > 0 AND tax_rate <= 1 AND tax_rate_mode IS NULL
               ORDER BY material_code;")
TAX_BEFORE=$(q "SELECT count(*) FROM biz_material
                 WHERE deleted_at IS NULL AND tax_rate > 0 AND tax_rate <= 1 AND tax_rate_mode IS NULL;")
echo "残留行数：$TAX_BEFORE"
if [ -n "$TAX_ROWS" ]; then printf '%s\n' "$TAX_ROWS"; fi

if [ "$FIX" = "1" ] && [ "${TAX_BEFORE:-0}" != "0" ]; then
  say "→ --fix：按 LEGACY 既有语义 ×100 并落 tax_rate_mode='PERCENT'"
  UPDATED=$(q "WITH u AS (
                  UPDATE biz_material
                     SET tax_rate = round(tax_rate * 100, 4),
                         tax_rate_mode = 'PERCENT',
                         updated_at = now()
                   WHERE deleted_at IS NULL AND tax_rate > 0 AND tax_rate <= 1
                     AND tax_rate_mode IS NULL
                  RETURNING material_code || ' → ' || tax_rate::text || '/PERCENT' AS line)
                SELECT coalesce(string_agg(line, E'\n'), '') FROM u;")
  if [ -n "$UPDATED" ]; then printf '%s\n' "$UPDATED"; fi
fi

TAX_AFTER=$(q "SELECT count(*) FROM biz_material
                WHERE deleted_at IS NULL AND tax_rate > 0 AND tax_rate <= 1 AND tax_rate_mode IS NULL;")
echo "残留行数（归一后）：$TAX_AFTER"
if [ "${TAX_AFTER:-0}" != "0" ]; then
  echo "✗ ③ 未清零（--fix 才会写库）"
  FAILED=1
else
  echo "✓ ③ 税率标度残留 0 行"
fi

# 同族护栏：发票侧同一判据（当前应为 0；有残留说明又有裸 SQL 直插）
INV_TAX=$(q "SELECT count(*) FROM biz_invoice
              WHERE deleted_at IS NULL AND tax_rate > 0 AND tax_rate <= 1 AND tax_rate_mode IS NULL;")
echo "  附：biz_invoice 同判据残留 = ${INV_TAX:-?} 行（本脚本不改发票：口径同 ③，若 >0 请先查来源）"
if [ "${INV_TAX:-0}" != "0" ]; then FAILED=1; fi

say "=============================================================="
say "E2E04-FIX-3 ④ 已记账（POSTED）却零分录的凭证"
say "=============================================================="
ORPHANS=$(q "SELECT v.voucher_no || ' | ' || v.voucher_type || ' | ' || v.period
                    || ' | ' || v.summary || ' | debit=' || v.total_debit::text
                    || ' | entry_count=' || v.entry_count::text
                    || ' | created=' || v.created_at::date
               FROM biz_voucher v
              WHERE v.deleted_at IS NULL AND v.status = 'POSTED'
                AND NOT EXISTS (SELECT 1 FROM biz_voucher_entry e WHERE e.voucher_id = v.id)
              ORDER BY v.voucher_no;")
ORPHAN_COUNT=$(q "SELECT count(*) FROM biz_voucher v
                   WHERE v.deleted_at IS NULL AND v.status = 'POSTED'
                     AND NOT EXISTS (SELECT 1 FROM biz_voucher_entry e WHERE e.voucher_id = v.id);")
echo "裸头凭证数：$ORPHAN_COUNT"
if [ -n "$ORPHANS" ]; then printf '%s\n' "$ORPHANS"; fi

if [ "$FIX" = "1" ] && [ "${#PURGE_NOS[@]}" -gt 0 ]; then
  say "→ --fix：按点名清单删（只删仍满足「POSTED 且零分录」的行，不级联到任何单据）"
  for no in "${PURGE_NOS[@]}"; do
    matched=$(q "SELECT count(*) FROM biz_voucher v
                  WHERE v.voucher_no = '${no//\'/\'\'}' AND v.deleted_at IS NULL AND v.status = 'POSTED'
                    AND NOT EXISTS (SELECT 1 FROM biz_voucher_entry e WHERE e.voucher_id = v.id);")
    if [ "${matched:-0}" = "0" ]; then
      echo "  · $no：当前不满足「POSTED 且零分录」（已删过 / 状态变了），跳过"
      continue
    fi
    q "DELETE FROM biz_voucher v
        WHERE v.voucher_no = '${no//\'/\'\'}' AND v.deleted_at IS NULL AND v.status = 'POSTED'
          AND NOT EXISTS (SELECT 1 FROM biz_voucher_entry e WHERE e.voucher_id = v.id);" >/dev/null
    echo "  · $no：已删除 ${matched} 行"
  done
fi

ORPHAN_AFTER=$(q "SELECT count(*) FROM biz_voucher v
                   WHERE v.deleted_at IS NULL AND v.status = 'POSTED'
                     AND NOT EXISTS (SELECT 1 FROM biz_voucher_entry e WHERE e.voucher_id = v.id);")
echo "裸头凭证数（清理后）：$ORPHAN_AFTER"
if [ "${ORPHAN_AFTER:-0}" != "0" ]; then
  echo "✗ ④ 未清零（未被点名的裸头会留着 —— 属主不清的数据不在本脚本的通配范围内）"
  FAILED=1
else
  echo "✓ ④ 已记账无分录凭证 0 行"
fi

say "=============================================================="
if [ "$FAILED" = "0" ]; then
  echo "结论：③④ 两项均 0 残留"
else
  echo "结论：仍有残留 —— 看上面清单（dry-run 加 --fix / --purge-orphan-posted <凭证号> 才会写库）"
fi
exit "$FAILED"
