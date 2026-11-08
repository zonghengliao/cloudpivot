#!/bin/bash
# CODE-02：145 库侧断言（真落库）+ 清场
#
# 在 145 上以 root 跑（psql 走 su - postgres 本地 peer 认证，不碰口令）：
#   scp 06_db_assert_and_cleanup.sh root@172.18.10.145:/tmp/ && ssh root@172.18.10.145 'bash /tmp/06_db_assert_and_cleanup.sh'
#
# 断言 → 打印 → 再删掉本轮造的测试行（保留 biz_seq.current_no 的推进：
# 主数据编码是档案，建成功就该永久占号，测试也不例外 —— 这也顺手证明了「号只前进不回退」）。
set -uo pipefail

DB=${DB:-cloudpivot_dev}
SQL=/tmp/code02_assert.sql
cat > "$SQL" <<'EOSQL'
\pset footer off
\echo '--- ① 本轮创建的供应商（应按编码排序，contains CODE02）'
SELECT supplier_code, supplier_name, (create_by IS NOT NULL) AS has_operator
  FROM biz_supplier
 WHERE supplier_name LIKE 'CODE02 测试供应商%'
 ORDER BY supplier_code;
\echo '--- ② 本轮创建的客户'
SELECT code, name, status, (create_by IS NOT NULL) AS has_operator
  FROM biz_customer
 WHERE name LIKE 'CODE02 测试客户%'
 ORDER BY code;
\echo '--- ③ 自动编码形态（SUP-####/CUS-#### 正则必须命中；手工编码 GYS-001/KH-001 不命中）'
SELECT supplier_code,
       supplier_code ~ '^SUP-[0-9]{4,}$' AS is_auto_form
  FROM biz_supplier
 WHERE supplier_name LIKE 'CODE02 测试供应商%'
 ORDER BY supplier_code;
SELECT code, code ~ '^CUS-[0-9]{4,}$' AS is_auto_form
  FROM biz_customer
 WHERE name LIKE 'CODE02 测试客户%'
 ORDER BY code;
\echo '--- ④ biz_seq 推进（SUP/CUS 的 current_no 必须 > 0，pad_width 4、不按期间重置）'
SELECT seq_type, prefix, separator, pad_width, current_no, date_pattern, period_key
  FROM biz_seq WHERE seq_type IN ('SUP','CUS') ORDER BY seq_type;
\echo '--- ⑤ 存活行编码无重复（部分唯一索引 uk_biz_supplier_code / uk_biz_customer_code 的显式复核）'
SELECT 'supplier' AS tbl, supplier_code AS code, COUNT(*) AS n
  FROM biz_supplier WHERE deleted_at IS NULL
 GROUP BY supplier_code HAVING COUNT(*) > 1
UNION ALL
SELECT 'customer' AS tbl, code, COUNT(*) AS n
  FROM biz_customer WHERE deleted_at IS NULL
 GROUP BY code HAVING COUNT(*) > 1;
\echo '--- ⑥ 清场：删掉本轮 CODE02 测试行'
DELETE FROM biz_supplier WHERE supplier_name LIKE 'CODE02 测试供应商%';
DELETE FROM biz_customer WHERE name LIKE 'CODE02 测试客户%';
\echo '--- ⑦ 清场后复核（两条 DELETE 的影响行数见上；下面必须都是 0）'
SELECT (SELECT COUNT(*) FROM biz_supplier WHERE supplier_name LIKE 'CODE02 测试供应商%') AS sup_left,
       (SELECT COUNT(*) FROM biz_customer WHERE name LIKE 'CODE02 测试客户%') AS cus_left;
EOSQL

chmod 0644 "$SQL"
su - postgres -c "psql -d $DB -f $SQL"
