-- ============================================================================
-- V0395__hr_adjust_resign_flow.down.sql
--
-- 回滚「调薪 / 离职 两套审批流种子」（`hr_salary_adjust` + `hr_employee_resign`）。
--
-- 顺序（受外键与业务事实约束）：
--   ① 安全阀：已有 HR_SALARY_ADJUST / HR_EMPLOYEE_RESIGN 审批实例、
--      离职单已挂实例子、任一流程下还有别的版本 → RAISE，拒绝回滚；
--   ② 自底向上删（审批人 → 边 → 节点 → 版本指针摘除 → 版本 → 定义），两套各自一遍；
--   ③ 幂等：重复执行 0 行、0 异常（第二遍安全阀门与 DELETE 全是 0 命中）。
--
-- 为什么要有安全阀：`biz_flow_instance.version_id` 是指向版本的硬外键（RESTRICT），
-- 直接删版本会报错；更糟的是「库里已有调薪单 / 离职单」时删定义，会让在跑的审批
-- 变成指向空气的孤儿单（既看不到进度也没法撤回，AGENTS 约定 11 的死单）。
-- 口径与 V0251 / V0332 / V0367 / V0381 / V0393 / V0394 一致。
--
-- ★ 这两套流程**没有独立单表**（如实说明，别照抄请假/薪资单的 §① 安全阀 b）：
--   `shared/doc_kind.rs::FlowBizType::doc_kind()` 写明「调薪与离职没有独立的单表
--   （`biz_employee_salary` 是行版本台账，离职只是员工状态的一次流转），审批实例的
--   `biz_id` 都是 `biz_employee.id`」——所以「单据是否挂在实例上」只能由
--   `biz_flow_instance`（安全阀 a）判定；`biz_employee` 上没有 `approval_instance_id` 列，
--   也没有 `biz_employee_resign` 表。写一条 `to_regclass('biz_employee_resign')` 守卫
--   只会永远跳过（假绿）—— 故不做，改成在 NOTICE 里说清判据。
-- ============================================================================

DO $$
DECLARE
    v_rows integer;
BEGIN
    -- ① 安全阀 a：已有两套流程的审批实例（这是「单据是否挂实例」的唯一判据，见文件头）
    IF to_regclass('public.biz_flow_instance') IS NOT NULL THEN
        EXECUTE 'SELECT count(*) FROM biz_flow_instance WHERE biz_type IN (''HR_SALARY_ADJUST'', ''HR_EMPLOYEE_RESIGN'')'
           INTO v_rows;
        IF v_rows > 0 THEN
            RAISE EXCEPTION 'V0395 down 中止：已有 % 个 HR_SALARY_ADJUST / HR_EMPLOYEE_RESIGN 审批实例'
                            '（version_id 是 RESTRICT 外键，删版本会让在跑的审批单变成孤儿）；'
                            '请先撤回/作废这些实例再回滚', v_rows;
        END IF;
    END IF;

    -- ① 安全阀 b：任一流程下还有**别的**版本（客户发过新版 / 改过图）
    IF to_regclass('public.biz_flow_def_version') IS NOT NULL THEN
        EXECUTE 'SELECT count(*) FROM biz_flow_def_version WHERE def_id = $1 AND id <> $2'
           INTO v_rows USING '20000000-0000-4000-8000-0000000e1501'::uuid,
                              '20000000-0000-4000-8000-0000000e1502'::uuid;
        IF v_rows > 0 THEN
            RAISE EXCEPTION 'V0395 down 中止：hr_salary_adjust 下还有 % 个非本卡版本；请在流程编辑器里先归档处理', v_rows;
        END IF;
        EXECUTE 'SELECT count(*) FROM biz_flow_def_version WHERE def_id = $1 AND id <> $2'
           INTO v_rows USING '20000000-0000-4000-8000-0000000e1531'::uuid,
                              '20000000-0000-4000-8000-0000000e1532'::uuid;
        IF v_rows > 0 THEN
            RAISE EXCEPTION 'V0395 down 中止：hr_employee_resign 下还有 % 个非本卡版本；请在流程编辑器里先归档处理', v_rows;
        END IF;
    END IF;

    RAISE NOTICE 'V0395 down：安全阀全部通过（无调薪/离职实例 / 无挂实例的离职单 / 无额外版本）';
END $$;

-- ② 自底向上删（只碰本卡固定 id）
-- ---- ① hr_salary_adjust ----
DELETE FROM biz_flow_node_assignee
 WHERE node_id IN (SELECT id FROM biz_flow_node
                    WHERE version_id = '20000000-0000-4000-8000-0000000e1502'::uuid);

DELETE FROM biz_flow_edge
 WHERE version_id = '20000000-0000-4000-8000-0000000e1502'::uuid;

UPDATE biz_flow_def
   SET current_version_id = NULL,
       current_version_no = 0,
       status             = 'DRAFT',
       updated_at         = now()
 WHERE id = '20000000-0000-4000-8000-0000000e1501'::uuid;

DELETE FROM biz_flow_node
 WHERE version_id = '20000000-0000-4000-8000-0000000e1502'::uuid;

DELETE FROM biz_flow_def_version
 WHERE id = '20000000-0000-4000-8000-0000000e1502'::uuid;

DELETE FROM biz_flow_def
 WHERE id = '20000000-0000-4000-8000-0000000e1501'::uuid;

-- ---- ② hr_employee_resign ----
DELETE FROM biz_flow_node_assignee
 WHERE node_id IN (SELECT id FROM biz_flow_node
                    WHERE version_id = '20000000-0000-4000-8000-0000000e1532'::uuid);

DELETE FROM biz_flow_edge
 WHERE version_id = '20000000-0000-4000-8000-0000000e1532'::uuid;

UPDATE biz_flow_def
   SET current_version_id = NULL,
       current_version_no = 0,
       status             = 'DRAFT',
       updated_at         = now()
 WHERE id = '20000000-0000-4000-8000-0000000e1531'::uuid;

DELETE FROM biz_flow_node
 WHERE version_id = '20000000-0000-4000-8000-0000000e1532'::uuid;

DELETE FROM biz_flow_def_version
 WHERE id = '20000000-0000-4000-8000-0000000e1532'::uuid;

DELETE FROM biz_flow_def
 WHERE id = '20000000-0000-4000-8000-0000000e1531'::uuid;

DO $$
DECLARE
    v_rows integer;
BEGIN
    EXECUTE 'SELECT count(*) FROM biz_flow_def WHERE id IN ($1, $2)'
       INTO v_rows USING '20000000-0000-4000-8000-0000000e1501'::uuid,
                          '20000000-0000-4000-8000-0000000e1531'::uuid;
    IF v_rows <> 0 THEN
        RAISE EXCEPTION 'V0395 down 失败：流程定义仍存在 % 行（…e1501 / …e1531），回滚不完整', v_rows;
    END IF;
    RAISE NOTICE 'V0395 down 完成：hr_salary_adjust + hr_employee_resign 种子已移除（幂等，重复执行零变更）';
END $$;
