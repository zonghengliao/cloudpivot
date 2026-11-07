-- ============================================================
-- V0323：固定资产（FIXED_ASSET）**状态变更联动的 3 条预置规则**（FIX-3 · 问题 1）
--
-- 依据：`backend/src/business/erp/asset_linkage.rs`（三处 hook）
--       + `backend/src/business/flow/linkage/{spec,engine,actions}.rs`
--       + `docs/evidence/20261107-p004-fixed-asset-chain/README.md`（坑 #1）
--
-- # 为什么需要这张迁移
--
--   P0-04 资产链演练实测：`spec::TRIGGER_SPECS` 把 FIXED_ASSET 标成 `wired = true`，
--   代码三处 hook（停用 / 启用、处置、作废处置）也真的在跑，但**迁移全集跑完后
--   `biz_flow_linkage_rule` 里 FIXED_ASSET 规则 0 条** ⇒ 停用 / 启用 / 处置一条通知都
--   不发，且**没有任何报错**（典型「看起来配好了，实际不工作」）。
--
--   这与 V0254 的结论同源：`biz_flow_linkage_rule` 的**空表**是 V0205 刻意的初始态
--   （「配置前行为逐字不变」），但只有空表就等于「资产通知」这个能力不存在 ——
--   没有人会想到「资产状态变更要先去配一条联动规则才会发通知」。
--
-- # 3 条规则（一个触发点一条，覆盖资产的全部真实状态推进）
--
--   | # | 规则名 | 触发点 | 动作 | 事务归属 | 目标单据 |
--   |---|--------|--------|------|----------|----------|
--   | 1 | 资产停用 → 通知制单人 | FIXED_ASSET/IDLE     | SEND_NOTICE | AFTER_COMMIT | sys_notice |
--   | 2 | 资产启用 / 作废处置 → 通知制单人 | FIXED_ASSET/IN_USE | SEND_NOTICE | AFTER_COMMIT | sys_notice |
--   | 3 | 资产处置 → 通知制单人 | FIXED_ASSET/DISPOSED | SEND_NOTICE | AFTER_COMMIT | sys_notice |
--
--   触发状态 = 变更**之后**的目标态（引擎按 `ctx.status` 匹配 `trigger_status`）：
--
--   * `IN_USE → IDLE`（停用，`asset_handler::asset_status_change`）⇒ 命中 #1；
--   * `IDLE → IN_USE`（启用）与 `DISPOSED → IN_USE`（**作废处置**）⇒ 命中 #2
--     （同一个目标态的两种真实入口，一条规则覆盖两者；文案由触发点算，见下）；
--   * `IN_USE|IDLE → DISPOSED`（处置，`asset_bridge::execute` 第 ⑤ 步）⇒ 命中 #3。
--
--   建卡（`NULL → IN_USE`）**不是**状态流转（写的是 `log_event`），不接触发点：
--   挂上就是「导入 N 张卡 = 扇出 N 条通知」。
--
-- # 参数为什么是 `{"mode": "TAKEOVER"}`（**必须 TAKEOVER，不能 SHADOW**）
--
--   `SHADOW` 只做「影子对比」、不发通知（`linkage::engine` 里非 TAKEOVER 直接跳过投递）。
--   资产通知**没有旧代码在发**（`asset_linkage::AssetStatusChange::ctx` 只把草稿放进
--   `LinkageContext.notice`，真正投递完全靠引擎）⇒ 写成 SHADOW 等于规则配了也一条不发。
--
--   不收件人键 = `recipient` 缺省 `LEGACY` = 沿用触发点算出来的收件人
--   （`asset_linkage` 取「制单人，制单人缺失时退操作人」），这与「配置不覆盖 = 零重复真相」
--   的口径一致（V0254 的任务通知同样不写 recipient）。标题 / 正文同理不覆盖：
--   `asset_linkage` 已经拼好「资产「FA-…」状态已变更为「已处置」」并带详情页链接。
--
-- # 幂等（生产上可能已经手工配过规则）
--
--   幂等写法：按唯一键 `(trigger_entity, trigger_status, action_code)` 做 `DO NOTHING`
--   （见下方 INSERT 尾行）：
--   同一触发点上**已经存在的行**（运维手工配的、被停用的、被软删的）一律**原样保留**
--   —— 那是运维的决定，迁移不该覆盖。因此本迁移的自检只断言
--   「3 个触发点各有一行」（插入成功 / 早就存在都行），不强制 `enabled` / `TAKEOVER`：
--   在**全新库**上这 3 行由本迁移插入，必然 `enabled + TAKEOVER`，验收见迁移末尾 NOTICE。
--   不写 BEGIN/COMMIT（迁移器自己在事务里执行）。
--
-- # 与配置页的关系
--
--   种完之后「流程配置 → 跨单据联动」里能看到这 3 条，可停用 / 改参数 / 删除
--   （`linkage::model::{upsert_rule, delete_rule}`，超管 + 二次确认 + 写
--   `biz_status_log(entity_type='FLOW_LINKAGE_CFG')` 审计）。本迁移不写审计行：
--   迁移是「部署时的初始数据」，不是「某个人改了配置」。
-- ============================================================

INSERT INTO biz_flow_linkage_rule
    (id, name, trigger_entity, trigger_status, action_code, action_params,
     enabled, seq, created_at, updated_at, create_by, update_by, deleted_at)
VALUES
    -- ① 停用（IN_USE → IDLE）：以前不发任何通知，这是新增的联动
    (gen_random_uuid(),
     '资产停用 → 通知制单人',
     'FIXED_ASSET', 'IDLE', 'SEND_NOTICE',
     '{"mode": "TAKEOVER"}'::jsonb,
     true, 10, now(), now(), NULL, NULL, NULL),
    -- ② 启用（IDLE → IN_USE）与作废处置（DISPOSED → IN_USE）：同一目标态两个真实入口
    (gen_random_uuid(),
     '资产启用 / 作废处置 → 通知制单人',
     'FIXED_ASSET', 'IN_USE', 'SEND_NOTICE',
     '{"mode": "TAKEOVER"}'::jsonb,
     true, 10, now(), now(), NULL, NULL, NULL),
    -- ③ 处置（IN_USE|IDLE → DISPOSED）：最关心的「资产被处置了」
    (gen_random_uuid(),
     '资产处置 → 通知制单人',
     'FIXED_ASSET', 'DISPOSED', 'SEND_NOTICE',
     '{"mode": "TAKEOVER"}'::jsonb,
     true, 10, now(), now(), NULL, NULL, NULL)
ON CONFLICT (trigger_entity, trigger_status, action_code) DO NOTHING;

-- ------------------------------------------------------------
-- 自检（迁移期断言，失败即整笔回滚）
-- ------------------------------------------------------------
DO $$
DECLARE
    n_keys    integer;
    n_alive   integer;
    n_take    integer;
    n_bad_act integer;
BEGIN
    -- 3 个触发点必须各有一行（唯一约束保证 ≤1）：插入成功 / 早已存在都满足
    SELECT count(DISTINCT trigger_status) INTO n_keys FROM biz_flow_linkage_rule
     WHERE trigger_entity = 'FIXED_ASSET'
       AND action_code = 'SEND_NOTICE'
       AND trigger_status IN ('IDLE', 'IN_USE', 'DISPOSED');
    IF n_keys <> 3 THEN
        RAISE EXCEPTION 'V0323 自检失败：FIXED_ASSET 的 SEND_NOTICE 规则应覆盖 3 个状态，实际 %', n_keys;
    END IF;

    -- 资产只允许 SEND_NOTICE / CREATE_TASK 这类「提交后」动作（凭证由 asset_bridge 专用路径出）：
    -- 这个触发点上出现 GENERATE_VOUCHER 就是重复落账的种子，必须当场红
    SELECT count(*) INTO n_bad_act FROM biz_flow_linkage_rule
     WHERE trigger_entity = 'FIXED_ASSET' AND deleted_at IS NULL
       AND action_code IN ('GENERATE_VOUCHER', 'REVERSE_VOUCHER');
    IF n_bad_act <> 0 THEN
        RAISE EXCEPTION 'V0323 自检失败：FIXED_ASSET 上有 % 条凭证类规则（处置凭证由 asset_bridge 专用路径落账，通用引擎再跑一次就是重复落账）', n_bad_act;
    END IF;

    SELECT count(*) INTO n_alive FROM biz_flow_linkage_rule
     WHERE trigger_entity = 'FIXED_ASSET' AND action_code = 'SEND_NOTICE'
       AND trigger_status IN ('IDLE', 'IN_USE', 'DISPOSED') AND deleted_at IS NULL;
    SELECT count(*) INTO n_take FROM biz_flow_linkage_rule
     WHERE trigger_entity = 'FIXED_ASSET' AND action_code = 'SEND_NOTICE'
       AND trigger_status IN ('IDLE', 'IN_USE', 'DISPOSED') AND deleted_at IS NULL
       AND enabled = true AND action_params ->> 'mode' = 'TAKEOVER';

    RAISE NOTICE 'V0323 完成：FIXED_ASSET 预置 3 条 SEND_NOTICE 联动规则（停用 IDLE / 启用·作废处置 IN_USE / 处置 DISPOSED）；当前存活 % 条、其中 enabled+TAKEOVER % 条（已有手工配置时按运维的来，不覆盖）', n_alive, n_take;
END $$;
