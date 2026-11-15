-- ============================================================================
-- V0394__hr_salary_flow.down.sql
--
-- 回滚「薪资单审批流种子」（`hr_salary`）。
--
-- 顺序（受外键与业务事实约束）：
--   ① 安全阀：已有 HR_SALARY 审批实例 / 薪资单已挂实例 / 本流程下还有别的版本
--      → RAISE，拒绝回滚；
--   ② 审批人绑定 → 边 → 节点 → 版本 → 定义（自底向上，外键才不挡）；
--   ③ 幂等：重复执行 0 行、0 异常（第二遍安全阀门与 DELETE 全是 0 命中）。
--
-- 为什么要有安全阀：`biz_flow_instance.version_id` 是指向版本的硬外键（RESTRICT），
-- 直接删版本会报错；更糟的是「库里已有薪资单」时删定义，会让在跑的工资审批变成
-- 指向空气的孤儿单（既看不到进度也没法撤回，AGENTS 约定 11 的死单）。删版本前的口径
-- 与 V0251 / V0332 / V0367 / V0381 / V0393 一致：**有实例就不许回滚**，交给人工处置
-- （先把实例撤回/作废）。
--
-- ★ 财务口径的额外一条：`biz_salary_sheet` 里只要有一张 `POSTED` / `REVERSED` 的工资单，
--   本流程就已经参与过真实出账（凭证已生成）。这时回滚流程定义会让「谁批的这笔工资」
--   无从追溯 —— 故与 **已过账薪资单** 一起拒绝。
-- ============================================================================

DO $$
DECLARE
    v_rows integer;
    v_def  uuid := '20000000-0000-4000-8000-0000000e1401'::uuid;
    v_ver  uuid := '20000000-0000-4000-8000-0000000e1402'::uuid;
BEGIN
    -- ① 安全阀 a：已有薪资审批实例（表可能先被别的迁移回滚掉 → 查得到再判）
    IF to_regclass('public.biz_flow_instance') IS NOT NULL THEN
        EXECUTE 'SELECT count(*) FROM biz_flow_instance WHERE biz_type = ''HR_SALARY'''
           INTO v_rows;
        IF v_rows > 0 THEN
            RAISE EXCEPTION 'V0394 down 中止：已有 % 个 HR_SALARY 审批实例（version_id 是 RESTRICT 外键，'
                            '且删版本会让在跑的审批单变成孤儿）；请先撤回/作废这些实例再回滚', v_rows;
        END IF;
    END IF;

    -- ① 安全阀 b：薪资单已经把审批实例写到单头上 ⇒ 说明链路已被人用过
    IF to_regclass('public.biz_salary_sheet') IS NOT NULL THEN
        EXECUTE 'SELECT count(*) FROM biz_salary_sheet WHERE approval_instance_id IS NOT NULL'
           INTO v_rows;
        IF v_rows > 0 THEN
            RAISE EXCEPTION 'V0394 down 中止：biz_salary_sheet 已有 % 行挂在审批实例上（薪资审批链路已启用）；'
                            '请先清理/归档这些薪资单再回滚', v_rows;
        END IF;
        -- ① 安全阀 c：已过账 / 已红冲的工资单 = 真实出账已发生，流程轨迹是审计证据
        EXECUTE 'SELECT count(*) FROM biz_salary_sheet WHERE status IN (''POSTED'', ''REVERSED'')'
           INTO v_rows;
        IF v_rows > 0 THEN
            RAISE EXCEPTION 'V0394 down 中止：已有 % 张薪资单处于 POSTED / REVERSED（凭证已生成、钱已动）；'
                            '删掉审批流程会让「谁批的」不可追溯，请人工处置后再回滚', v_rows;
        END IF;
    END IF;

    -- ① 安全阀 d：本流程下还有**别的**版本 ⇒ 有人发过新版 / 改过图，回滚会丢版本
    IF to_regclass('public.biz_flow_def_version') IS NOT NULL THEN
        EXECUTE 'SELECT count(*) FROM biz_flow_def_version WHERE def_id = $1 AND id <> $2'
           INTO v_rows USING v_def, v_ver;
        IF v_rows > 0 THEN
            RAISE EXCEPTION 'V0394 down 中止：hr_salary 下还有 % 个非本卡版本（客户发过新版 / 改过图），'
                            '回滚会删掉流程定义却留不下那些版本；请在流程编辑器里先归档处理', v_rows;
        END IF;
    END IF;

    RAISE NOTICE 'V0394 down：安全阀全部通过（无 HR_SALARY 实例 / 无挂实例或已过账的薪资单 / 无额外版本）';
END $$;

-- ② 自底向上删（只碰本卡固定 id：客户自建的同名流程不在删除范围内）
DELETE FROM biz_flow_node_assignee
 WHERE node_id IN (SELECT id FROM biz_flow_node
                    WHERE version_id = '20000000-0000-4000-8000-0000000e1402'::uuid);

DELETE FROM biz_flow_edge
 WHERE version_id = '20000000-0000-4000-8000-0000000e1402'::uuid;

-- def 的 current_version_id 先摘掉，否则删版本行时指针会指向不存在的版本
-- （`ck_biz_flow_def_published_has_version` 只要求 PUBLISHED 时非空，这里先落 DRAFT）
UPDATE biz_flow_def
   SET current_version_id = NULL,
       current_version_no = 0,
       status             = 'DRAFT',
       updated_at         = now()
 WHERE id = '20000000-0000-4000-8000-0000000e1401'::uuid;

DELETE FROM biz_flow_node
 WHERE version_id = '20000000-0000-4000-8000-0000000e1402'::uuid;

DELETE FROM biz_flow_def_version
 WHERE id = '20000000-0000-4000-8000-0000000e1402'::uuid;

DELETE FROM biz_flow_def
 WHERE id = '20000000-0000-4000-8000-0000000e1401'::uuid;

DO $$
DECLARE
    v_rows integer;
BEGIN
    EXECUTE 'SELECT count(*) FROM biz_flow_def WHERE id = $1'
       INTO v_rows USING '20000000-0000-4000-8000-0000000e1401'::uuid;
    IF v_rows <> 0 THEN
        RAISE EXCEPTION 'V0394 down 失败：流程定义 …e1401 仍存在（% 行），回滚不完整', v_rows;
    END IF;
    RAISE NOTICE 'V0394 down 完成：hr_salary 种子已移除（幂等，重复执行零变更）';
END $$;
