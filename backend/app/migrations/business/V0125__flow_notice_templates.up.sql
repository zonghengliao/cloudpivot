-- ======================================================
-- V0125：审批通知 4 个模板（批次 D · D3c）
--
-- 依据：`docs/批次D-工作流审批-技术架构.md` §1.4（`notice::dispatch` + 模板编码）
--       与卡面「4 个通知模板 + 迁移种子」。
--
-- 本迁移**只写种子**，不建表、不改任何 CHECK：
--   * 载体是现成的 `sys_notice_template`（V0044），**不另造通知通道**（卡面红线）；
--   * `sys_change_log.action` 的 CHECK 在分区表上，扩枚举要扫全部历史分区 → 本迁移
--     一个字符都不碰它（D17：只做语义映射，见 `changelog/diff.rs::FlowAction`）。
--
-- 固定 UUID + `ON CONFLICT (id) DO NOTHING`：重跑幂等，且**不覆盖运营改过的文案**
-- （与 V0044 内置模板同一约定）。`ON CONFLICT (template_code) WHERE deleted_at IS NULL`
-- 也兜一层：万一有人先用同名编码手工建了模板，这里不炸。
--
-- 送达强度（`force`）不在库里：它是**调用点的安全语义**，由
-- `flow/notify.rs::FlowEvent::force()` 决定（Todo/Result/Urge = 强制，Cc = 尊重偏好）。
-- 模板表里存不了这个判断，硬塞一列等于给「关掉强制」开一个后台开关 —— 不给。
--
-- 变量口径（8 个键，`flow/notify.rs::template_vars` 恒发）：
--   flowName / instanceNo / nodeName / initiatorName / title / resultText / comment / urgeCount
-- 正文里出现而未在 `variables` 声明的占位符会被 render 规则 5 拦（422 + 60024），
-- 所以每个模板的 `variables` 必须**逐字覆盖**它正文用到的占位符。
-- ======================================================

-- ---------- 1) FLOW_TODO：待你审批（强制） ----------
INSERT INTO sys_notice_template (id, template_code, template_name, notice_type, default_level,
                                 title_template, content_template, content_format, link_template,
                                 channels, variables, status, remark)
VALUES
    ('40000000-0000-0000-0000-0000000000f1'::uuid, 'FLOW_TODO', '审批待办通知',
     'TODO', 'INFO',
     '待你审批：{{flowName}}',
     '「{{flowName}}」有一条待你审批：{{title}}（审批单号 {{instanceNo}}，当前节点「{{nodeName}}」，发起人 {{initiatorName}}）。',
     'TEXT', '/flow',
     'INBOX',
     '[{"name":"flowName","label":"流程名","required":true,"sample":"任务验收"},
       {"name":"instanceNo","label":"审批单号","required":true,"sample":"FLW-202610-000123"},
       {"name":"nodeName","label":"当前节点名","required":true,"sample":"主管审批"},
       {"name":"initiatorName","label":"发起人","required":false,"sample":"武鹏"},
       {"name":"title","label":"单据标题","required":true,"sample":"完成批次 D 架构文档"}]'::jsonb,
     1, '批次 D · D3c：审批人收到待办时由 flow::notify 发送（force=true，跳过 notify.inbox 偏好）'),

-- ---------- 2) FLOW_RESULT：通过 / 驳回（强制） ----------
    ('40000000-0000-0000-0000-0000000000f2'::uuid, 'FLOW_RESULT', '审批结果通知',
     'WORKFLOW', 'WARN',
     '审批{{resultText}}：{{flowName}}',
     '你发起的「{{flowName}}」（审批单号 {{instanceNo}}）在节点「{{nodeName}}」已被{{resultText}}。审批意见：{{comment}}',
     'TEXT', '/flow',
     'INBOX',
     '[{"name":"flowName","label":"流程名","required":true,"sample":"任务验收"},
       {"name":"instanceNo","label":"审批单号","required":true,"sample":"FLW-202610-000123"},
       {"name":"nodeName","label":"当前节点名","required":true,"sample":"主管审批"},
       {"name":"resultText","label":"结果（通过/驳回）","required":true,"sample":"通过"},
       {"name":"comment","label":"审批意见","required":false,"sample":"同意"}]'::jsonb,
     1, '批次 D · D3c：审批结论回推发起人（force=true）'),

-- ---------- 3) FLOW_CC：抄送你（尊重偏好） ----------
    ('40000000-0000-0000-0000-0000000000f3'::uuid, 'FLOW_CC', '审批抄送通知',
     'WORKFLOW', 'INFO',
     '抄送你：{{flowName}}',
     '「{{flowName}}」（审批单号 {{instanceNo}}）已抄送给你，当前节点「{{nodeName}}」，发起人 {{initiatorName}}，单据：{{title}}',
     'TEXT', '/flow',
     'INBOX',
     '[{"name":"flowName","label":"流程名","required":true,"sample":"任务验收"},
       {"name":"instanceNo","label":"审批单号","required":true,"sample":"FLW-202610-000123"},
       {"name":"nodeName","label":"当前节点名","required":true,"sample":"主管审批"},
       {"name":"initiatorName","label":"发起人","required":false,"sample":"武鹏"},
       {"name":"title","label":"单据标题","required":true,"sample":"完成批次 D 架构文档"}]'::jsonb,
     1, '批次 D · D3c：抄送（force=false，尊重 notify.inbox 偏好）'),

-- ---------- 4) FLOW_URGE：催办（强制） ----------
    ('40000000-0000-0000-0000-0000000000f4'::uuid, 'FLOW_URGE', '审批催办通知',
     'TODO', 'WARN',
     '审批催办：{{flowName}}',
     '「{{flowName}}」（审批单号 {{instanceNo}}）的节点「{{nodeName}}」已超时未处理，这是第 {{urgeCount}} 次提醒，请尽快处理。',
     'TEXT', '/flow',
     'INBOX',
     '[{"name":"flowName","label":"流程名","required":true,"sample":"任务验收"},
       {"name":"instanceNo","label":"审批单号","required":true,"sample":"FLW-202610-000123"},
       {"name":"nodeName","label":"当前节点名","required":true,"sample":"主管审批"},
       {"name":"urgeCount","label":"第几次提醒","required":true,"sample":"1"}]'::jsonb,
     1, '批次 D · D3c：催办 / 超时提醒（force=true）')
-- 不带冲突目标：主键冲突（重跑）与 `uk_sys_notice_template_code` 冲突（有人手工建过同名编码）
-- 都按「已存在即跳过」处理，不给部署制造失败面
ON CONFLICT DO NOTHING;

-- ---------- 自检：4 个模板全部在位且启用 ----------
DO $$
DECLARE
    missing text;
BEGIN
    SELECT string_agg(code, ', ')
      INTO missing
      FROM (VALUES ('FLOW_TODO'), ('FLOW_RESULT'), ('FLOW_CC'), ('FLOW_URGE')) AS t(code)
     WHERE NOT EXISTS (
        SELECT 1 FROM sys_notice_template s
         WHERE s.template_code = t.code AND s.status = 1 AND s.deleted_at IS NULL);
    IF missing IS NOT NULL THEN
        RAISE EXCEPTION 'V0125 自检失败：审批通知模板缺失或停用 → %', missing;
    END IF;
END $$;

COMMENT ON TABLE sys_notice_template IS '通知模板：{{变量}} 占位符 + 变量声明，发送时渲染为最终标题/正文（批次 D 追加 FLOW_TODO/FLOW_RESULT/FLOW_CC/FLOW_URGE 四个审批模板）';
