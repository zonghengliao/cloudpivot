-- V0136__flow_expression_assignee.down.sql
-- 回滚：先确认没有节点还在用 EXPRESSION（有则中止并给出数量），再还原枚举与列
--
-- ⚠ 与设计文档 §7.3 的差异：去掉文件自带的 BEGIN; … COMMIT; ——
--    迁移器自己在事务里执行本文件，文件内 COMMIT 会提前提交掉它的事务。
DO $$
DECLARE n BIGINT;
BEGIN
    SELECT count(*) INTO n FROM biz_flow_node WHERE assignee_type = 'EXPRESSION';
    IF n > 0 THEN
        RAISE EXCEPTION 'V0136 回滚中止：还有 % 个节点使用 EXPRESSION，请先改成静态来源（或用 UPDATE 清理草稿）', n;
    END IF;
END $$;

ALTER TABLE biz_flow_node DROP CONSTRAINT IF EXISTS ck_biz_flow_node_expression;
ALTER TABLE biz_flow_node DROP CONSTRAINT IF EXISTS biz_flow_node_assignee_type_check;
ALTER TABLE biz_flow_node
    ADD CONSTRAINT biz_flow_node_assignee_type_check
    CHECK (assignee_type IN ('USER','ROLE','INITIATOR','INITIATOR_SUPERIOR','DEPT_LEADER',
                             'TASK_OWNER','TASK_CREATOR','TASK_ASSIGNER','FORM_VAR','ADMIN'));
ALTER TABLE biz_flow_task DROP COLUMN IF EXISTS assignee_rule_id;
