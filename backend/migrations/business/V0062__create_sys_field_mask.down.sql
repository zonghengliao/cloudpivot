-- V0062 回滚：删除 `sys_field_mask` 整表。
--
-- 数据影响：所有脱敏规则（含全局默认规则）丢失。**注意顺序风险**：若已按本批 B6 上线
-- 「导出 fail-closed」，回滚本表会让「敏感列没有生效规则」的导出一律 422
-- （FIELD_MASK_RULE_MISSING）——这是刻意的保守行为，但回滚方必须知情：
-- 回滚本迁移前应先回滚使用它的代码（B6）。
--
-- 索引 uk_sys_field_mask_rule（唯一，NULLS NOT DISTINCT）与 idx_sys_field_mask_table
-- 随表一起消失，不需要单独 DROP INDEX。

DROP TABLE IF EXISTS sys_field_mask;
