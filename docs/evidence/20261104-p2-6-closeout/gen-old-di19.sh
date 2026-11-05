#!/usr/bin/env bash
# 生成 7f0fe5c^（改前）里 di.c_di19 的 SELECT 全文（不带 CREATE），用于在生产上只读「重放旧口径」
set -uo pipefail
REPO=${REPO:-/opt/cloudpivot}
OUT=${1:-/tmp/d4-out/old-di19.sql}
git -C "$REPO" show '7f0fe5c^:docs/architecture/snapshot-drift/10_checks.sql' \
  | awk '/^CREATE OR REPLACE VIEW di\.c_di19 AS/{f=1;next} f&&/^COMMENT ON VIEW di\.c_di19/{exit} f' \
  | sed -e '/^[[:space:]]*$/d' > "$OUT"
echo "生成: $OUT（$(wc -l < "$OUT") 行）"
cat "$OUT"
