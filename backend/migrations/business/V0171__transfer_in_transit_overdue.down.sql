-- ============================================================
-- V0171 down：撤销「在途调拨超期告警」
--
-- # 撤销范围（只有本迁移建的东西）
--
--   ① biz_stock_transfer.overdue_alerted_at / overdue_alert_count 两列
--   ② 部分索引 idx_biz_transfer_in_transit_shipped
--   ③ sys_config 两条参数（erp.transfer.in_transit_overdue_days /
--      erp.transfer.in_transit_realert_hours）
--
-- # 越界检测（比 down 本身更值钱）
--
--   回滚只该撤掉「告警台账」，**绝不**碰 6 态状态机的既有列与约束：
--   `ck_biz_transfer_state`、`shipped_at` / `received_at` 等时间戳列、
--   以及 `transfer_no` / `total_amount` 这些在 V0140 就存在的列。
--   下面 DO 块在删之前逐项断言它们还在 —— 一旦有人把 down 写宽了（例如
--   `DROP TABLE` 或顺手删时间戳列），这里会直接中止整条 down。
--
--   `DROP COLUMN IF EXISTS` + `DROP INDEX IF EXISTS` ⇒ 重复执行不报错。
-- ============================================================

DO $$
DECLARE
    v_missing text;
BEGIN
    -- ① 本迁移之前就存在的列必须还在（抽 4 个有代表性的：状态机的关键位）
    SELECT string_agg(name, ', ') INTO v_missing
      FROM unnest(ARRAY['status', 'shipped_at', 'received_at', 'transit_warehouse_id']) AS name
     WHERE NOT EXISTS (
               SELECT 1 FROM information_schema.columns
                WHERE table_name = 'biz_stock_transfer' AND column_name = name
           );
    IF v_missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0171 down 中止：biz_stock_transfer 缺失既有列（%），'
                        '继续回滚会把状态机打回 V0140 之前', v_missing;
    END IF;

    -- ② 状态机 CHECK 必须还在（drop 列不可能删掉它，这里防「手改 down」）
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'ck_biz_transfer_state'
           AND conrelid = 'public.biz_stock_transfer'::regclass
    ) THEN
        RAISE EXCEPTION 'V0171 down 中止：ck_biz_transfer_state 不存在（状态机已被破坏）';
    END IF;

    -- ③ 有告警台账的行数（只提示，不阻断：回滚一个功能不该被数据挡住）
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
         WHERE table_name = 'biz_stock_transfer' AND column_name = 'overdue_alert_count'
    ) THEN
        EXECUTE 'SELECT count(*) FROM biz_stock_transfer WHERE overdue_alert_count > 0'
           INTO v_missing;
        RAISE NOTICE 'V0171 down：将丢弃 % 张调拨单的超期告警台账（告警本身是通知，不影响单据与库存）',
            v_missing;
    END IF;
END $$;

DROP INDEX IF EXISTS idx_biz_transfer_in_transit_shipped;

ALTER TABLE biz_stock_transfer
    DROP COLUMN IF EXISTS overdue_alert_count,
    DROP COLUMN IF EXISTS overdue_alerted_at;

DELETE FROM sys_config
 WHERE config_key IN (
           'erp.transfer.in_transit_overdue_days',
           'erp.transfer.in_transit_realert_hours'
       );

-- ---------- 收尾自检：本迁移的痕迹必须清零 ----------
DO $$
DECLARE
    v_cols int;
    v_index int;
    v_cfg int;
BEGIN
    SELECT count(*) INTO v_cols
      FROM information_schema.columns
     WHERE table_name = 'biz_stock_transfer'
       AND column_name IN ('overdue_alerted_at', 'overdue_alert_count');
    SELECT count(*) INTO v_index
      FROM pg_indexes
     WHERE tablename = 'biz_stock_transfer'
       AND indexname = 'idx_biz_transfer_in_transit_shipped';
    SELECT count(*) INTO v_cfg
      FROM sys_config
     WHERE config_key IN (
               'erp.transfer.in_transit_overdue_days',
               'erp.transfer.in_transit_realert_hours'
           );

    IF v_cols <> 0 OR v_index <> 0 OR v_cfg <> 0 THEN
        RAISE EXCEPTION 'V0171 down 自检失败：残留 列=% 索引=% 参数=%', v_cols, v_index, v_cfg;
    END IF;
    RAISE NOTICE 'V0171 down 完成：告警台账 / 索引 / 参数 均无残留';
END $$;
