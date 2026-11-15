-- ============================================================================
-- V0365：`biz_status_log` 加 `action`（业务动作码）—— E2E05-FIX-3 ①（P4）
--
-- 卡：t_e99f0719（E2E05-FIX-3【P4×3】状态日志 from=to / 静默忽略字段 / 作废处置 change_date）
--
-- # 为什么是 V0365（跳过 0363 / 0364）
--
--   本迁移起草时 `V0363__add_server_status_menu`（HOME-3，前端在途卡）已占用 0363，
--   而迁移版本号**跨目录全局唯一**（`alun-db` loader：版本重复直接启动失败，
--   不是「后一条覆盖前一条」）。0364 留作并发在途卡的缓冲，故本卡取 0365。
--   跳号不影响执行顺序（版本按数值升序），只是给并行开发留出空位。
--
-- # 一、现场（E2E-05 演练 Q5）
--
--   部门转移 / 使用人变更**不改变状态**（仍是 IN_USE），但 ARCH-6 §5.6.1/§5.6.2 要求
--   它们也写 `biz_status_log`（`state_machine::log_event`，`from_status = to_status = 当前状态`）。
--   表里没有任何字段说明「这条日志是业务动作而不是状态流转」⇒ 任何按
--   `from_status → to_status` 渲染时间线的消费者只能看到「IN_USE → IN_USE」，
--   看起来像脏数据，实际是业务动作留痕。前端要区分只能去猜 `reason` 的前缀。
--
-- # 二、修法（卡面「方案 A」：数据层就分清）
--
--   加一列 `action varchar(32)`：
--     · NULL      = 真正的状态流转（`transition` 写的，from ≠ to，或建单 NULL → 状态）；
--     · 'TRANSFER' = 部门转移（`entity_type='FIXED_ASSET'`）；
--     · 'CUSTODIAN' = 使用人变更。
--   词表与 `biz_fixed_asset_change.change_type` 的 `TRANSFER` / `CUSTODIAN` 同字面，
--   前端 / 对账脚本可以两边直接对拍（`asset_dto::CHANGE_TRANSFER` / `CHANGE_CUSTODIAN`）。
--
--   为什么可空而不 NOT NULL + 默认值：绝大多数行是状态流转，给它们填一个假动作码
--   比留 NULL 更容易被误读（NULL 的语义是「这是状态流转，不是业务动作」）。
--
-- # 三、幂等
--
--   · `ADD COLUMN IF NOT EXISTS`；
--   · 不动存量行（历史 from=to 行没有动作码可还原 —— 猜错了比留空更糟；
--     前端对 from_status = to_status 的行已经做了「不渲染状态对」的兜底，见
--     `frontend/src/views/business/erp/asset/asset-logic.ts::changeSummary`）；
--   · 末尾自检：列必须存在且为 varchar(32)。
-- ============================================================================

ALTER TABLE biz_status_log
    ADD COLUMN IF NOT EXISTS action varchar(32);

COMMENT ON COLUMN biz_status_log.action IS
    '业务动作码（状态未变的事件）：TRANSFER=部门转移 / CUSTODIAN=使用人变更；'
    'NULL = 真正的状态流转（由 state_machine::transition 写）';

-- 按「某实体的业务动作」查：存量为空，索引留给巡检 / 对账用（部分索引，只索非空行）
CREATE INDEX IF NOT EXISTS idx_status_log_action
    ON biz_status_log (entity_type, action)
    WHERE action IS NOT NULL;

DO $mig$
DECLARE
    col_type text;
BEGIN
    SELECT format_type(a.atttypid, a.atttypmod)
      INTO col_type
      FROM pg_attribute a
      JOIN pg_class c ON c.oid = a.attrelid
      JOIN pg_namespace n ON n.oid = c.relnamespace
     WHERE n.nspname = current_schema()
       AND c.relname = 'biz_status_log'
       AND a.attname = 'action'
       AND a.attnum > 0
       AND NOT a.attisdropped;

    IF col_type IS NULL THEN
        RAISE EXCEPTION 'V0365 自检失败：biz_status_log.action 列不存在';
    END IF;
    IF col_type <> 'character varying(32)' THEN
        RAISE EXCEPTION 'V0365 自检失败：biz_status_log.action 类型是 %（期望 character varying(32)）', col_type;
    END IF;
    RAISE NOTICE 'V0365 OK：biz_status_log.action 已就位（%），NULL = 状态流转', col_type;
END
$mig$;
