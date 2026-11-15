-- ============================================================
-- V0171：在途调拨超期告警（LNK-F12 · 在途仓「压货不可见」）
--
-- 依据：
--   backend/src/business/erp/transfer_overdue.rs   （阈值 / 判定 / 扫描任务）
--   backend/src/business/erp/transfer_model.rs     （列表筛选 + 告警台账 CAS）
--   docs/在途调拨超期告警-LNK-F12.md                （口径与验收）
--
-- 迁移号：开工时 `ls migrations/business/` 实测最大号为 V0170（V0161–V0169 空闲），
--         取下一个空号 V0171。
--
-- # 为什么需要这条迁移
--
--   `biz_stock_transfer` 只有 6 态状态机，`IN_TRANSIT` 之后**没有任何入口**把
--   「压了很久的在途单」推给人：发货方点了「发货」、收货方一直没点「收货」，
--   单据永久停在 IN_TRANSIT。这一段库存既不在调出仓也不在调入仓（结算才双写），
--   账实差异只能靠人肉发现。
--
--   本迁移补三样东西：
--     ① 告警台账两列（`overdue_alerted_at` / `overdue_alert_count`）——
--        扫描任务是周期跑的，必须有地方记「上次提醒是什么时候」，
--        否则每小时重复轰炸同一张单（`sys_notice` 的幂等键也挡不住「每次都换键」）；
--     ② 扫描用的部分索引（只覆盖 IN_TRANSIT 且未删除的行）；
--     ③ 两个 `sys_config` 参数（阈值 7 天 / 重提醒间隔 24 小时）。
--
-- # 判定口径（与代码逐字对应）
--
--   超期在途 = `status = 'IN_TRANSIT'`
--              AND `shipped_at IS NOT NULL`
--              AND `shipped_at < now() - make_interval(days => 阈值)`
--
--   * 时间基准是 **`shipped_at`（发货时间）**，不是 `created_at` / `approved_at`：
--     压货的起点是「货离开调出仓」，审批快慢与告警无关；
--   * 阈值取 `sys_config.erp.transfer.in_transit_overdue_days`，缺省 7 天
--     （参数缺失 / 停用 / 非数字 → 回退 7，不因配置问题让告警失效）；
--   * `shipped_at` 为空的 IN_TRANSIT 行不参与判定（库约束 `ck_biz_transfer_state`
--     保证 IN_TRANSIT 必有 shipped_at，这里是防御性写法，`NOT NULL` 之外多一层）。
--
-- # 幂等
--
--   ADD COLUMN IF NOT EXISTS + CREATE INDEX IF NOT EXISTS +
--   INSERT … ON CONFLICT (config_key) WHERE deleted_at IS NULL DO NOTHING。
--   重复执行不报错、不覆盖运维改过的值（ON CONFLICT DO NOTHING 而不是 DO UPDATE）。
-- ============================================================

-- ---------- 1) 告警台账两列 ----------
ALTER TABLE biz_stock_transfer
    ADD COLUMN IF NOT EXISTS overdue_alerted_at timestamptz,
    ADD COLUMN IF NOT EXISTS overdue_alert_count smallint NOT NULL DEFAULT 0;

COMMENT ON COLUMN biz_stock_transfer.overdue_alerted_at IS
    '最近一次「在途超期」告警时间（NULL = 从未提醒过）；重提醒间隔取 sys_config.erp.transfer.in_transit_realert_hours';
COMMENT ON COLUMN biz_stock_transfer.overdue_alert_count IS
    '累计「在途超期」告警次数（只增不减；同时作为 sys_notice 幂等键的序号段）';

-- ---------- 2) 扫描部分索引 ----------
-- 只覆盖「在途、未删除、有发货时间」的行：表里绝大多数是 DRAFT/DONE，
-- 全表索引收益为零；部分索引让「每小时扫一次」的语句稳定走索引顺序扫描。
CREATE INDEX IF NOT EXISTS idx_biz_transfer_in_transit_shipped
    ON biz_stock_transfer (shipped_at)
    WHERE status = 'IN_TRANSIT' AND deleted_at IS NULL;

-- ---------- 3) 两个系统参数 ----------
INSERT INTO sys_config (
    id, config_key, config_value, config_type, description,
    category, config_level, is_modifiable, is_encrypted, is_restart, status,
    sort_order, ref_value
) VALUES
    (
        '2f120000-0000-4000-8000-000000000001',
        'erp.transfer.in_transit_overdue_days',
        '7',
        'NUMBER',
        '调拨单在途超期阈值（天，按 shipped_at 计算）：超过该天数仍未收货即判定为「超期在途」，'
        || '列表可按 in_transit_overdue=true 筛出，并由 transfer_overdue_scan 任务通知相关人',
        'erp', 2, TRUE, FALSE, FALSE, 1,
        10, '1~180'
    ),
    (
        '2f120000-0000-4000-8000-000000000002',
        'erp.transfer.in_transit_realert_hours',
        '24',
        'NUMBER',
        '在途超期重提醒间隔（小时）：同一张调拨单在间隔内不重复通知（0 或非法值按 24 处理）',
        'erp', 2, TRUE, FALSE, FALSE, 1,
        20, '1~720'
    )
ON CONFLICT (config_key) WHERE deleted_at IS NULL DO NOTHING;

-- ---------- 4) 自检（空库首启与有数据重放都必须过） ----------
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
    IF v_cols <> 2 THEN
        RAISE EXCEPTION 'V0171 自检失败：biz_stock_transfer 告警台账列只有 % 列（期望 2）', v_cols;
    END IF;

    SELECT count(*) INTO v_index
      FROM pg_indexes
     WHERE tablename = 'biz_stock_transfer'
       AND indexname = 'idx_biz_transfer_in_transit_shipped';
    IF v_index <> 1 THEN
        RAISE EXCEPTION 'V0171 自检失败：部分索引 idx_biz_transfer_in_transit_shipped 未建立';
    END IF;

    SELECT count(*) INTO v_cfg
      FROM sys_config
     WHERE config_key IN (
               'erp.transfer.in_transit_overdue_days',
               'erp.transfer.in_transit_realert_hours'
           )
       AND deleted_at IS NULL;
    IF v_cfg <> 2 THEN
        RAISE EXCEPTION 'V0171 自检失败：超期告警参数只有 % 条（期望 2）', v_cfg;
    END IF;

    RAISE NOTICE 'V0171 完成：告警台账 2 列 + 部分索引 1 个 + 系统参数 2 条';
END $$;
