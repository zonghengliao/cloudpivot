-- ============================================================================
-- V0393__hr_leave_flow.down.sql
--
-- 回滚「请假审批流种子」（`hr_leave`）。
--
-- 顺序（受外键与业务事实约束）：
--   ① 安全阀：已有 HR_LEAVE 审批实例 / 请假单已挂实例 / 本流程下还有别的版本
--      → RAISE，拒绝回滚；
--   ② 审批人绑定 → 边 → 节点 → 版本 → 定义（自底向上，外键才不挡）；
--   ③ 幂等：重复执行 0 行、0 异常（第二遍安全阀门与 DELETE 全是 0 命中）。
--
-- 为什么要有安全阀：`biz_flow_instance.version_id` 是指向版本的硬外键（RESTRICT），
-- 直接删版本会报错；更糟的是「会话里已有请假单」时删定义，会让在跑的请假审批变成
-- 指向空气的孤儿单（既看不到进度也没法撤回）。删版本前的口径与 V0251 / V0332 /
-- V0367 / V0381 一致：**有实例就不许回滚**，交给人工处置（先把实例撤回/作废）。
-- ============================================================================

DO $$
DECLARE
    v_rows integer;
    v_def  uuid := '20000000-0000-4000-8000-0000000e0393'::uuid;
    v_ver  uuid := '20000000-0000-4000-8000-0000000e0394'::uuid;
BEGIN
    -- ① 安全阀 a：已有请假审批实例（表可能先被别的迁移回滚掉 → 查得到再判）
    IF to_regclass('public.biz_flow_instance') IS NOT NULL THEN
        EXECUTE 'SELECT count(*) FROM biz_flow_instance WHERE biz_type = ''HR_LEAVE'''
           INTO v_rows;
        IF v_rows > 0 THEN
            RAISE EXCEPTION 'V0393 down 中止：已有 % 个 HR_LEAVE 审批实例（version_id 是 RESTRICT 外键，'
                            '且删版本会让在跑的审批单变成孤儿）；请先撤回/作废这些实例再回滚', v_rows;
        END IF;
    END IF;

    -- ① 安全阀 b：请假单已经把审批实例写到单头上 ⇒ 说明链路已被人用过
    IF to_regclass('public.biz_leave_request') IS NOT NULL THEN
        EXECUTE 'SELECT count(*) FROM biz_leave_request WHERE approval_instance_id IS NOT NULL'
           INTO v_rows;
        IF v_rows > 0 THEN
            RAISE EXCEPTION 'V0393 down 中止：biz_leave_request 已有 % 行挂在审批实例上（请假审批链路已启用）；'
                            '请先清理/归档这些请假单再回滚', v_rows;
        END IF;
    END IF;

    -- ① 安全阀 c：本流程下还有**别的**版本 ⇒ 有人发过新版 / 改过图，回滚会丢版本
    IF to_regclass('public.biz_flow_def_version') IS NOT NULL THEN
        EXECUTE 'SELECT count(*) FROM biz_flow_def_version WHERE def_id = $1 AND id <> $2'
           INTO v_rows USING v_def, v_ver;
        IF v_rows > 0 THEN
            RAISE EXCEPTION 'V0393 down 中止：hr_leave 下还有 % 个非本卡版本（客户发过新版 / 改过图），'
                            '回滚会删掉流程定义却留不下那些版本；请在流程编辑器里先归档处理', v_rows;
        END IF;
    END IF;

    RAISE NOTICE 'V0393 down：安全阀全部通过（无 HR_LEAVE 实例 / 无挂实例的请假单 / 无额外版本）';
END $$;

-- ② 自底向上删（只碰本卡固定 id：客户自建的同名流程不在删除范围内）
DELETE FROM biz_flow_node_assignee
 WHERE node_id IN (SELECT id FROM biz_flow_node
                    WHERE version_id = '20000000-0000-4000-8000-0000000e0394'::uuid);

DELETE FROM biz_flow_edge
 WHERE version_id = '20000000-0000-4000-8000-0000000e0394'::uuid;

-- def 的 current_version_id 先摘掉，否则删版本行时指针会指向不存在的版本
-- （`ck_biz_flow_def_published_has_version` 只要求 PUBLISHED 时非空，这里先落 DRAFT）
UPDATE biz_flow_def
   SET current_version_id = NULL,
       current_version_no = 0,
       status             = 'DRAFT',
       updated_at         = now()
 WHERE id = '20000000-0000-4000-8000-0000000e0393'::uuid;

DELETE FROM biz_flow_node
 WHERE version_id = '20000000-0000-4000-8000-0000000e0394'::uuid;

DELETE FROM biz_flow_def_version
 WHERE id = '20000000-0000-4000-8000-0000000e0394'::uuid;

DELETE FROM biz_flow_def
 WHERE id = '20000000-0000-4000-8000-0000000e0393'::uuid;

DO $$
DECLARE
    v_rows integer;
BEGIN
    EXECUTE 'SELECT count(*) FROM biz_flow_def WHERE id = $1'
       INTO v_rows USING '20000000-0000-4000-8000-0000000e0393'::uuid;
    IF v_rows <> 0 THEN
        RAISE EXCEPTION 'V0393 down 失败：流程定义 …e0393 仍存在（% 行），回滚不完整', v_rows;
    END IF;
    RAISE NOTICE 'V0393 down 完成：hr_leave 种子已移除（幂等，重复执行零变更）';
END $$;
