#!/bin/bash
# CODE-03 库侧断言：取号规则 / 落库编码 / 号段推进 / 无重复
# 用法：DB=cloudpivot_code03run bash 02_db_assert.sh
set -uo pipefail
DB=${DB:-cloudpivot_code03run}
HERE=$(cd "$(dirname "$0")" && pwd)
RAW="$HERE/../raw"
mkdir -p "$RAW"
OUT="$RAW/02_db_assert.txt"
PSQL=(psql -h 127.0.0.1 -U alun -d "$DB" -v ON_ERROR_STOP=1)

export PGPASSWORD=${PGPASSWORD:-alun}

{
  echo "=== ① 取号规则（V0337 种入的六条主数据号段，MAT 4 位 / WH 2 位）==="
  "${PSQL[@]}" -c "SELECT seq_type, prefix, separator, pad_width, step, date_pattern, period_key, current_no, status
                     FROM biz_seq WHERE seq_type IN ('MAT','WH') ORDER BY seq_type"
  echo "=== ② 本次 curl 落库的物料（MAT-0001..0003 自动 + YB-001-02 手工）==="
  "${PSQL[@]}" -c "SELECT material_code, material_name, created_at FROM biz_material
                    WHERE material_name LIKE 'CODE03%' ORDER BY material_code"
  echo "=== ③ 本次 curl 落库的仓库（WH-01..03 自动 + CK-001 手工）==="
  "${PSQL[@]}" -c "SELECT warehouse_code, warehouse_name, level, warehouse_path FROM biz_warehouse
                    WHERE warehouse_name LIKE 'CODE03%' ORDER BY warehouse_code"
  echo "=== ④ 号段与落库一致：MAT current_no = 3 / WH current_no = 3（3 次自动取号）==="
  "${PSQL[@]}" -tAc "SELECT 'MAT current_no=' || current_no FROM biz_seq WHERE seq_type='MAT'"
  "${PSQL[@]}" -tAc "SELECT 'WH  current_no=' || current_no FROM biz_seq WHERE seq_type='WH'"
  echo "=== ⑤ 无重复编码（部分唯一索引 uk_biz_*_code 未触发冲突残留）==="
  "${PSQL[@]}" -tAc "SELECT 'material dup groups=' || count(*) FROM (
                         SELECT material_code FROM biz_material WHERE deleted_at IS NULL
                          GROUP BY material_code HAVING count(*) > 1) t"
  "${PSQL[@]}" -tAc "SELECT 'warehouse dup groups=' || count(*) FROM (
                         SELECT warehouse_code FROM biz_warehouse WHERE deleted_at IS NULL
                          GROUP BY warehouse_code HAVING count(*) > 1) t"
} 2>&1 | tee "$OUT"
