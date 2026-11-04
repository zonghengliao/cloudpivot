-- 卡 t_8ec0a1af 的只读复查口径（与 fixture_hygiene::orphans 同一判据）
-- 用法：psql -h 127.0.0.1 -U alun -d cloudpivot_8ec0a1af -Atc "$(cat residue_check.sql)"

-- ① 本轮修复的目标：PURCHASE_ORDER 状态日志孤儿（必须恒 0）
SELECT count(*) FROM biz_status_log
 WHERE entity_type = 'PURCHASE_ORDER'
   AND entity_id NOT IN (SELECT id FROM biz_purchase_order);

-- ② 对照：PURCHASE_ORDER 状态日志总数（清场有效时也应为 0）
SELECT count(*) FROM biz_status_log WHERE entity_type = 'PURCHASE_ORDER';

-- ③ 对照：采购单行数（清场有效时为 0）
SELECT count(*) FROM biz_purchase_order;

-- ④ 对照：STOCK_IN 状态日志孤儿（本轮之前已修，必须恒 0）
SELECT count(*) FROM biz_status_log
 WHERE entity_type = 'STOCK_IN'
   AND entity_id NOT IN (SELECT id FROM biz_stock_in);

-- ⑤ 库指纹：迁移最大版本 | 迁移条数（干净库口径）
SELECT max(version::int) || '|' || count(*) FROM schema_migrations;
