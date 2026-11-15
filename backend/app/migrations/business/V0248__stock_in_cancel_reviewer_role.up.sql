-- ============================================================================
-- V0248__stock_in_cancel_reviewer_role.up.sql
--
-- 【t_e63b600f / P1-FIX-1】作废复核岗位没有「人」——审批人来源是 ADMIN（= 全部超管），
-- 发起人本人若也是超管 ⇒ 引擎 `self_approval=FORBID` 在**发起那一刻**就 422（65019），
-- 作废**根本走不通**。
--
-- # 一、现象（真机原文，docs/evidence 里的审计证据）
--   scripts/verify/audit_features_20261104/05_chains_run1.txt ②：
--     [FAIL] 收货单落 CANCELLED  POSTED
--            (HTTP 422 审批人 超级管理员 是发起人本人，本节点不允许自审
--             （self_approval=FORBID）（内控要求：作废复核人不能是发起人本人。请由他人发起…）)
--   作废是**反向记账**（六本账回滚 + 凭证红冲），入口只有这一个 —— 卡住等于「错单只能将错就错」。
--
-- # 二、根因：审批人**来源**配错了，不是内控太严
--   V0174 种这条流程时（P16）本仓还没有业务角色种子，所以 `n_review`：
--     assignee_type = 'ADMIN'           → 运行时展开「持有 SUPER_ADMIN 的所有人」
--     empty_assignee = 'FALLBACK_ADMIN' → 一个超管都没有时再回退给超管（同义反复）
--   生产 `SUPER_ADMIN` = 4 人（admin / laoliu / laoliu_test / henry.liao），**造单的正是
--   admin 与 laoliu**（`biz_stock_in.create_by` 实测 9 张：admin 5 / laoliu 4）⇒ 发起人
--   恰好也在候选里 ⇒ FORBID 立刻命中。这跟内控想拦的东西（本人签自己的字）无关：
--   真正该签字的是**仓管主管**，而仓管主管压根不是审批候选人。
--
-- # 三、修法：审批人来源换成业务角色（与 V0182 的 `stock_in_post.n_wh` 同口径）
--   ① 节点 `n_review`：`assignee_type` ADMIN → **ROLE**，并绑定 `WAREHOUSE_MANAGER`
--      （`biz_flow_node_assignee` 一行）—— 审批人是「仓管主管」这个岗位，不是「超管」这个身份。
--   ② `empty_assignee`：FALLBACK_ADMIN → **ERROR**。
--      这张卡的要求是「不能兜底超管」：角色 0 人时**宁可发起失败**也不许把作废复核悄悄
--      塞给超管（那正是缺陷本身）。ERROR 的报文已由本卡补成可执行指引（见 §五），
--      提示「到 角色管理 → 仓管主管 → 成员 绑人，或把节点审批人改成具体人员」。
--   ③ `self_approval` **保持 FORBID 不动**：作废是反向记账，本人复核等于没有复核。
--      「自审仍被拒」是内控底线，本卡一个字没放宽（验收 ③ 用真机打回：发起人=复核人 → 422）。
--
--   ⚠️ 与 V0182 的差异（刻意）：那条种子里 `n_wh` 用 `FALLBACK_ADMIN`，理由是「角色 0 人时
--   别把单卡死」。这里反着选：作废**没有**「先过账再说」的退路（驳回/撤回都回 POSTED，
--   作废本身不产生正向业务价值），配错就该报错让人去配；而 `FALLBACK_ADMIN` 在单超管库上
--   兜出来的正好是发起人本人 —— 修了等于没修。
--
-- # 四、为什么是「就地修正当前版本」，而不是「发布新版本」
--   · 只动**种子行原样**（版本 id = …e174，且 hash 仍是 V0174 的 78d34c…）；客户重新发布过
--     （hash 变了 / 当前版本换了）的库**整段跳过**，绝不覆盖别人的图；
--   · 生产库里 `biz_flow_instance` 的 STOCK_IN_CANCEL 实例 **0 行**（2026-11-05 实测），
--     不存在「在跑实例按旧图审批」的兼容问题；
--   · 版本号语义（v1 是「开箱即用的默认配置」）不变，客户看「流程定义」不会多出一个 v2。
--     ⇒ 换版本（B 卡 t_1e6a8431 的 5 节点统一去 ADMIN 化）留给那张卡，本卡只修作废这一条。
--
-- # 五、配套（同 commit）
--   · `stockin_handler_cancel.rs`：65019 / 65018 的**出路文案**（点名 WAREHOUSE_MANAGER 与配置路径）；
--   · `flow/assignee.rs`：`empty_assignee=ERROR` 的报文补「节点名 + 配置指引」；
--   · `graph_hash` 由独立 canonical 序列化器（Python，递归键排序 + 紧凑 JSON + sha256）算出，
--     先用 V0174 原快照自证（算得 78d34c… = 迁移里写死的值），再算新快照 = 026766…；
--     落地后由 `flow::maintain`（行 → graph_hash 重算）与单测 `p16_seed_flow_is_self_consistent` 双向核对。
--
-- # 六、回滚
--   V0248__stock_in_cancel_reviewer_role.down.sql：节点回 ADMIN / FALLBACK_ADMIN、删掉
--   WAREHOUSE_MANAGER 绑定、快照与 hash 回 78d34c…（守卫同样是「当前是 026766…」才动）。
-- ============================================================================

DO $$
DECLARE
    c_def      CONSTANT uuid := '20000000-0000-4000-8000-00000000d174';
    c_ver      CONSTANT uuid := '20000000-0000-4000-8000-00000000e174';
    c_node     CONSTANT uuid := '20000000-0000-4000-8000-00000000a175';  -- n_review 作废复核
    c_old_hash CONSTANT text := '78d34c4557630fd8a57c45f901b427649db06ee380baf1868d257813f6e4e56c';
    c_new_hash CONSTANT text := '0267661a1b7d37ebc94a3e8859eecde63173325f57d3949970dcbdd4f8b9b5a0';
    c_role     uuid;
BEGIN
    -- ① 流程不存在（不是本仓口径的库）→ 什么都不做
    IF NOT EXISTS (
        SELECT 1 FROM biz_flow_def
         WHERE id = c_def AND flow_code = 'stock_in_cancel' AND deleted_at IS NULL
    ) THEN
        RAISE NOTICE 'V0248：本库没有 stock_in_cancel 种子流程，跳过';
        RETURN;
    END IF;

    -- ② 已经是「新口径」→ 幂等重跑，别再动
    IF EXISTS (
        SELECT 1 FROM biz_flow_def_version
         WHERE id = c_ver AND btrim(graph_hash) = c_new_hash
    ) THEN
        RAISE NOTICE 'V0248：stock_in_cancel 已是仓管主管复核口径（hash=026766…），跳过';
        RETURN;
    END IF;

    -- ③ 当前版本必须仍是种子 v1、且图还是 V0174 那一张（客户重新发布过就不碰）
    IF NOT EXISTS (
        SELECT 1 FROM biz_flow_def_version
         WHERE id = c_ver AND def_id = c_def AND is_current
           AND version_status = 'PUBLISHED' AND btrim(graph_hash) = c_old_hash
    ) THEN
        RAISE NOTICE 'V0248：stock_in_cancel 的当前版本已不是种子 v1 / 图已被改过，跳过（不覆盖客户配置）';
        RETURN;
    END IF;

    -- ④ 角色必须存在（V0182 种的 20000000-0000-4000-8000-0000000f0004）
    SELECT id INTO c_role FROM sys_role
     WHERE role_code = 'WAREHOUSE_MANAGER' AND deleted_at IS NULL;
    IF c_role IS NULL THEN
        RAISE WARNING 'V0248：WAREHOUSE_MANAGER 角色不存在，跳过（作废复核仍是 ADMIN 口径，请人工配审批岗位）';
        RETURN;
    END IF;

    -- ⑤ 节点：审批人来源 → ROLE；空审批人 → ERROR（不再兜底超管）
    UPDATE biz_flow_node
       SET assignee_type  = 'ROLE',
           empty_assignee = 'ERROR'
     WHERE id = c_node AND version_id = c_ver AND node_key = 'n_review';

    -- ⑥ 审批人绑定：仓管主管（角色 id，跨环境稳定）
    INSERT INTO biz_flow_node_assignee (node_id, assignee_type, ref_id, sort_order)
    VALUES (c_node, 'ROLE', c_role, 0)
    ON CONFLICT (node_id, assignee_type, ref_id) DO NOTHING;

    -- ⑦ 快照 + hash 同步（行与快照必须同口径，否则 flow::maintain 报 RowsEdited 漂移）
    UPDATE biz_flow_def_version
       SET graph_snapshot = '{"edges":[{"condition":null,"from":"n_review","is_default":false,"key":"e2","label":null,"priority":0,"to":"e"},{"condition":null,"from":"s","is_default":false,"key":"e1","label":null,"priority":0,"to":"n_review"}],"nodes":[{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"e","max_remind_times":3,"name":"结束","node_type":"END","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":3,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":480,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":"ROLE","assignees":[{"assignee_type":"ROLE","ref_id":"20000000-0000-4000-8000-0000000f0004","sort_order":0}],"cc_on_approved":false,"config":{},"empty_assignee":"ERROR","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"n_review","max_remind_times":2,"name":"作废复核","node_type":"APPROVAL","reject_veto":true,"remind_before_hours":null,"self_approval":"FORBID","sort_order":2,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":24,"x":240,"y":0},{"allow_robot":false,"approval_mode":null,"approve_ratio":null,"assignee_type":null,"assignees":[],"cc_on_approved":false,"config":{},"empty_assignee":"SKIP","fork_mode":"AND","form_var_key":null,"join_mode":"AND","key":"s","max_remind_times":3,"name":"开始","node_type":"START","reject_veto":true,"remind_before_hours":null,"self_approval":"SKIP","sort_order":1,"superior_levels":1,"timeout_action":"REMIND","timeout_hours":null,"x":0,"y":0}],"schema_version":1}'::jsonb,
           graph_hash     = c_new_hash,
           updated_at     = now()
     WHERE id = c_ver AND btrim(graph_hash) = c_old_hash;

    -- ⑧ 流程描述的「谁复核」也要跟着改（流程定义页直接展示这段文字）
    UPDATE biz_flow_def
       SET description = '收货入库单作废的复核流程：发起作废 → 作废复核（仓管主管 WAREHOUSE_MANAGER，禁止自审） → 结束。'
                         '审批通过才执行六本账（库存/库位账/流水/凭证/应付/序列号）回滚；驳回或撤回则单据回到已过账，账不动。',
           updated_at  = now()
     WHERE id = c_def;

    RAISE NOTICE 'V0248：stock_in_cancel 作废复核已改为 WAREHOUSE_MANAGER（empty_assignee=ERROR），hash 78d34c… → 026766…';
END $$;

-- ---------- 自检（对结果断言；不满足就迁移失败 = 启动失败） ----------
DO $$
DECLARE
    c_ver      CONSTANT uuid := '20000000-0000-4000-8000-00000000e174';
    c_node     CONSTANT uuid := '20000000-0000-4000-8000-00000000a175';
    c_new_hash CONSTANT text := '0267661a1b7d37ebc94a3e8859eecde63173325f57d3949970dcbdd4f8b9b5a0';
    c_old_hash CONSTANT text := '78d34c4557630fd8a57c45f901b427649db06ee380baf1868d257813f6e4e56c';
    v_hash text;
    v_role uuid;
    n int;
    v_snap jsonb;
BEGIN
    SELECT btrim(graph_hash) INTO v_hash FROM biz_flow_def_version WHERE id = c_ver;
    IF v_hash IS NULL THEN
        RAISE NOTICE 'V0248 自检：版本行不在（非本仓口径），跳过';
        RETURN;
    END IF;
    IF v_hash = c_old_hash THEN
        RAISE NOTICE 'V0248 自检：仍是种子 hash（守卫跳过），不做内容断言';
        RETURN;
    END IF;
    IF v_hash <> c_new_hash THEN
        RAISE EXCEPTION 'V0248 自检失败：stock_in_cancel v1 的 hash 既不是种子值也不是本卡的 canonical 值（%）', v_hash;
    END IF;

    -- ① 节点行：ROLE + ERROR + FORBID 三件套
    SELECT count(*) INTO n FROM biz_flow_node
     WHERE id = c_node AND version_id = c_ver AND node_key = 'n_review'
       AND node_type = 'APPROVAL' AND assignee_type = 'ROLE'
       AND empty_assignee = 'ERROR' AND self_approval = 'FORBID';
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0248 自检失败：作废复核节点不是 ROLE + ERROR + FORBID（实际 % 行）', n;
    END IF;

    -- ② 审批人绑定：恰好一个 WAREHOUSE_MANAGER
    SELECT id INTO v_role FROM sys_role WHERE role_code = 'WAREHOUSE_MANAGER' AND deleted_at IS NULL;
    SELECT count(*) INTO n FROM biz_flow_node_assignee
     WHERE node_id = c_node AND assignee_type = 'ROLE' AND ref_id = v_role;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0248 自检失败：作废复核节点没有绑定 WAREHOUSE_MANAGER（实际 % 行）', n;
    END IF;

    -- ③ 快照必须与行同口径（否则 maintain 每日自检会报 RowsEdited 漂移）
    SELECT graph_snapshot INTO v_snap FROM biz_flow_def_version WHERE id = c_ver;
    SELECT count(*) INTO n FROM jsonb_array_elements(v_snap -> 'nodes') AS node
     WHERE node ->> 'key' = 'n_review'
       AND node ->> 'assignee_type' = 'ROLE'
       AND node ->> 'empty_assignee' = 'ERROR'
       AND node ->> 'self_approval' = 'FORBID'
       AND (node -> 'assignees' -> 0 ->> 'ref_id') = v_role::text;
    IF n <> 1 THEN
        RAISE EXCEPTION 'V0248 自检失败：快照里的 n_review 与节点行不同口径（行/快照必须同源）';
    END IF;

    RAISE NOTICE 'V0248 自检通过：作废复核 = WAREHOUSE_MANAGER 角色 + ERROR 兜底 + 禁止自审，快照与 hash 自洽';
END $$;
