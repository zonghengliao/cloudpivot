-- P9 / B1 回滚：删除本脚本补出来的「未分配库位桶」
--
-- ⚠️ 只在「up 之后还没有新业务写入」的干净回滚路径使用。
--    一旦过账/作废又跑过，NULL 桶里已经有新数量，删行会连带丢掉新业务 —— 那种情况
--    请用 00_restore.sh 从 /root/datafix-backup-<时间戳>/ 的 pg_dump 恢复。
DELETE FROM biz_inventory_location
 WHERE location_id IS NULL;
