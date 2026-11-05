-- ============================================================
-- V0254：跨单据联动的 **3 条预置业务规则**（P1-FIX-5B）
--
-- 依据：`backend/src/business/flow/linkage/`（spec 白名单 / engine 引擎 / actions 动作实现）
--       + `docs/evidence/20261105-p1fix5b-linkage-presets/`
--
-- # 为什么需要这张迁移
--
--   评审实测：`biz_flow_linkage_rule` **0 行** ⇒ 引擎虽然写好了，但从来没真正跑过
--   （收货过账永远走旧路径、任务通知永远由旧代码发）。空表是 V0205 刻意的初始态
--   （「配置前行为逐字不变」），但**只有空表**就等于这个能力不存在。
--   本迁移把 3 条**真实业务规则**种进去，让「配置驱动」这条路在真实动作上有据可查。
--
-- # 3 条规则（都从现有硬编码流程里挑，都是「已经存在的手工/隐式联动」）
--
--   | # | 规则名 | 触发点 | 动作 | 事务归属 | 目标单据 |
--   |---|--------|--------|------|----------|----------|
--   | 1 | 收货过账 → 生成凭证（联动接管） | STOCK_IN/POSTED | GENERATE_VOUCHER | IN_TX / CRITICAL | erp_voucher |
--   | 2 | 收货过账 → 通知制单人       | STOCK_IN/POSTED | SEND_NOTICE       | AFTER_COMMIT    | sys_notice |
--   | 3 | 任务开工 → 通知负责人与创建人 | TASK/DOING      | SEND_NOTICE       | AFTER_COMMIT    | sys_notice |
--
--   ① 与原有硬编码**同源**：`GENERATE_VOUCHER` 走的是 `voucher::create_by_rule`
--      （收货过账 ⑦ 的旧路径也是它），配置接管后仍然是同一个函数、同一份取号。
--      参数 `biz_type` 取 `PURCHASE_IN`——这是**凭证映射的业务类型**
--      （`voucher::BIZ_TYPE_PURCHASE_IN`，`biz_voucher_rule.rule_code = 'PURCHASE_IN_DEFAULT'`），
--      不是单据状态机里的实体名 `STOCK_IN`，两者别混。
--   ② 是**新增**的联动：收货过账以前不发任何通知（只有审批流程节点会发），
--      这条规则把「你的收货单已过账」发给制单人；正文/收件人由触发点算
--      （`stockin_post_linkage_ctx`），配置只声明「接管 + 发给谁」。
--   ③ 与旧逻辑同触发点、同收件人，但**发送权从代码搬到配置**：任务状态通知以前硬编码在
--      `task::handler::notify_status_change` 里（改一句文案要发版），接管后由规则决定
--      「发不发、发给谁、覆盖哪些字段」，运维可以停用/改参数而不改代码。
--      故意不覆盖任何字段（`recipient` 缺省 = LEGACY），所以接管前后用户看到的通知
--      逐字相同 —— 预置规则的第一步是「等价接管」，不是「顺手改业务口径」。
--      触发点选 **DOING**（开工）而不是 REVIEW：`→ REVIEW` 在配了 `task_accept`
--      验收流程的环境里会先起审批实例（通知由流程发，见 `task_bridge::prepare_accept`），
--      直接通知会被流程接管，规则永远等不到；`TODO → DOING` 是纯状态流转，稳定可触发。
--
-- # 为什么都写 `mode = TAKEOVER`
--
--   `SHADOW` 只做影子对比、真账仍由旧代码落 —— 那是上线灰度用的中间态。
--   预置规则必须是**真生效**的：单测 `presets_are_takeover_not_shadow` 钉住这一点
--   （有人把它改成 SHADOW 而不改测试，测试立刻红：规则会静默失效）。
--
-- # 幂等
--
--   `ON CONFLICT (trigger_entity, trigger_status, action_code) DO UPDATE`：
--   重复执行只把 3 行刷回预置值（`deleted_at = NULL` 复活），不会生出第二份。
--   **不写 BEGIN/COMMIT**（迁移器自己在事务里执行）。
--
-- # 与配置页的关系
--
--   种完之后在「流程配置 → 跨单据联动」里能看到这 3 条，可停用 / 改参数 / 删除
--   （`linkage::model::{upsert_rule, delete_rule}`，超管 + 二次确认 + 写
--   `biz_status_log(entity_type='FLOW_LINKAGE_CFG')` 审计）。本迁移不写审计行：
--   迁移是「部署时的初始数据」，不是「某个人改了配置」。
-- ============================================================

INSERT INTO biz_flow_linkage_rule
    (id, name, trigger_entity, trigger_status, action_code, action_params,
     enabled, seq, created_at, updated_at, create_by, update_by, deleted_at)
VALUES
    -- ① 收货过账 → 生成凭证：接管 ⑦ 步的凭证生成（专用路由 `engine::stockin_voucher`）
    (gen_random_uuid(),
     '收货过账 → 生成凭证（联动接管）',
     'STOCK_IN', 'POSTED', 'GENERATE_VOUCHER',
     '{"biz_type": "PURCHASE_IN", "mode": "TAKEOVER"}'::jsonb,
     true, 10, now(), now(), NULL, NULL, NULL),
    -- ② 收货过账 → 通知制单人：以前过账后不发通知，这是新增的联动（通用引擎 `engine::run`）
    (gen_random_uuid(),
     '收货过账 → 通知制单人',
     'STOCK_IN', 'POSTED', 'SEND_NOTICE',
     '{"mode": "TAKEOVER", "recipient": "CREATOR"}'::jsonb,
     true, 20, now(), now(), NULL, NULL, NULL),
    -- ③ 任务开工 → 通知负责人与创建人：接管任务通知（专用路由 `engine::status_notice`）
    (gen_random_uuid(),
     '任务开工 → 通知负责人与创建人',
     'TASK', 'DOING', 'SEND_NOTICE',
     '{"mode": "TAKEOVER"}'::jsonb,
     true, 10, now(), now(), NULL, NULL, NULL)
ON CONFLICT (trigger_entity, trigger_status, action_code) DO UPDATE
   SET name          = EXCLUDED.name,
       action_params = EXCLUDED.action_params,
       enabled       = EXCLUDED.enabled,
       seq           = EXCLUDED.seq,
       updated_at    = EXCLUDED.updated_at,
       update_by     = NULL,
       deleted_at    = NULL;

-- ------------------------------------------------------------
-- 自检（迁移期断言，失败即整笔回滚）
-- ------------------------------------------------------------
DO $$
DECLARE
    n_rows   integer;
    n_bad    integer;
    n_take   integer;
BEGIN
    -- 3 条规则必须在（按触发点 + 动作码精确定位，不看名字）
    SELECT count(*) INTO n_rows FROM biz_flow_linkage_rule
     WHERE deleted_at IS NULL
       AND (trigger_entity, trigger_status, action_code) IN (
           ('STOCK_IN', 'POSTED', 'GENERATE_VOUCHER'),
           ('STOCK_IN', 'POSTED', 'SEND_NOTICE'),
           ('TASK',     'DOING', 'SEND_NOTICE')
       );
    IF n_rows <> 3 THEN
        RAISE EXCEPTION 'V0254 自检失败：预置规则应为 3 条，实际 %', n_rows;
    END IF;

    -- 三条都必须启用（停用是运维动作，不该由迁移造成）
    SELECT count(*) INTO n_bad FROM biz_flow_linkage_rule
     WHERE deleted_at IS NULL
       AND (trigger_entity, trigger_status, action_code) IN (
           ('STOCK_IN', 'POSTED', 'GENERATE_VOUCHER'),
           ('STOCK_IN', 'POSTED', 'SEND_NOTICE'),
           ('TASK',     'DOING', 'SEND_NOTICE')
       )
       AND enabled = false;
    IF n_bad <> 0 THEN
        RAISE EXCEPTION 'V0254 自检失败：预置规则里有 % 条是停用状态', n_bad;
    END IF;

    -- 预置 = 真生效：mode 必须是 TAKEOVER（SHADOW 只做影子对比，规则等于没生效）
    SELECT count(*) INTO n_take FROM biz_flow_linkage_rule
     WHERE deleted_at IS NULL
       AND (trigger_entity, trigger_status, action_code) IN (
           ('STOCK_IN', 'POSTED', 'GENERATE_VOUCHER'),
           ('STOCK_IN', 'POSTED', 'SEND_NOTICE'),
           ('TASK',     'DOING', 'SEND_NOTICE')
       )
       AND action_params ->> 'mode' = 'TAKEOVER';
    IF n_take <> 3 THEN
        RAISE EXCEPTION 'V0254 自检失败：预置规则里 mode=TAKEOVER 的应为 3 条，实际 %', n_take;
    END IF;

    RAISE NOTICE 'V0254 完成：跨单据联动预置 3 条规则（收货过账→生成凭证 / 收货过账→通知制单人 / 任务开工(TODO→DOING)→通知负责人与创建人），全部 enabled + TAKEOVER';
END $$;
