<!--
  导入字段说明表（模板中心 / 业务页导入弹窗 / 导入向导共用）。

  # 为什么单独成一个组件

  字段说明必须**与模板 xlsx 的「字段说明」sheet 同源**：数据一律来自后端
  `GET /business/impexp/def/columns`（它由 `ImportColumn` 摊平而来），前端不维护任何
  字段文案副本——否则模板改了页面没改，用户按页面填了报错，是比「没有说明」更坏的结果。

  # 列的含义

  | 列 | 说明 |
  |----|------|
  | 表头 | 模板第 1 行的原文；必填列带 `*`（与 Excel 完全一致，用户照抄不会错） |
  | 必填 | 是 / 否 |
  | 类型 | 文本 / 整数 / 小数 / 日期 / 枚举 / 字典 / 布尔 |
  | 长度/范围 | 如「≤40 个字符」「0 ~ 999999」 |
  | 可选值 | 枚举逐项列出；字典列给字典编码 |
  | 说明 | 与模板「说明」列同一段文案 |
  | 示例 | 模板示例行用的值 |

  # 事件

  无（纯展示）。加载状态、错误文案由父组件负责。
-->
<template>
  <div class="import-field-table" :data-testid="testid">
    <t-table
      row-key="key"
      :data="columns"
      :columns="tableColumns"
      :loading="loading"
      :max-height="maxHeight"
      size="small"
      :stripe="true"
      :hover="true"
    >
      <template #template_header="{ row }">
        <span class="import-field-table__header">{{ row.template_header }}</span>
        <t-tag v-if="row.unique_key" theme="warning" variant="light" size="small" class="import-field-table__tag">
          唯一
        </t-tag>
      </template>
      <template #required="{ row }">
        <t-tag v-if="row.required" theme="danger" variant="light" size="small">必填</t-tag>
        <span v-else class="import-field-table__muted">选填</span>
      </template>
      <template #rule_text="{ row }">
        <span class="import-field-table__mono">{{ row.rule_text || '—' }}</span>
      </template>
      <template #option_text="{ row }">
        <span v-if="row.option_text" class="import-field-table__option">{{ row.option_text }}</span>
        <span v-else class="import-field-table__muted">—</span>
      </template>
      <template #remark="{ row }">
        <span>{{ row.remark || '—' }}</span>
        <t-tooltip v-if="row.fk_label" :content="`须先在「${row.fk_label}」中维护，否则整行报 FK_NOT_FOUND`">
          <t-tag theme="primary" variant="light" size="small" class="import-field-table__tag">
            引用 {{ row.fk_label }}
          </t-tag>
        </t-tooltip>
      </template>
      <template #example="{ row }">
        <span class="import-field-table__mono">{{ row.example || '—' }}</span>
      </template>
      <template #empty>
        <div class="import-field-table__empty">暂无字段说明</div>
      </template>
    </t-table>
  </div>
</template>

<script setup lang="ts">
import type { PrimaryTableCol } from 'tdesign-vue-next';
import type { ImportColumnRes } from '@/api/business/impexp';

withDefaults(
  defineProps<{
    /** 列清单（后端 `def/columns` 原样返回，顺序即模板列顺序） */
    columns: ImportColumnRes[];
    /** 表格加载中 */
    loading?: boolean;
    /** 表体最大高度（px） */
    maxHeight?: number;
    /** data-testid 前缀（e2e 用） */
    testid?: string;
  }>(),
  {
    loading: false,
    maxHeight: 480,
    testid: 'import-field-table',
  },
);

const tableColumns: PrimaryTableCol[] = [
  { colKey: 'template_header', title: '表头（模板第 1 行）', width: 180 },
  { colKey: 'required', title: '必填', width: 78 },
  { colKey: 'type_label', title: '类型', width: 78 },
  { colKey: 'rule_text', title: '长度/范围', width: 150 },
  { colKey: 'option_text', title: '可选值', width: 190 },
  { colKey: 'remark', title: '说明', minWidth: 220 },
  { colKey: 'example', title: '示例', width: 150, ellipsis: true },
];
</script>

<style lang="less" scoped>
.import-field-table {
  &__header {
    font-weight: 600;
    font-family: var(--app-font-mono, monospace);
  }

  &__mono {
    font-family: var(--app-font-mono, monospace);
    font-size: 12px;
  }

  &__option {
    font-size: 12px;
    color: var(--td-text-color-primary);
  }

  &__muted {
    color: var(--td-text-color-placeholder);
    font-size: 12px;
  }

  &__tag {
    margin-left: 6px;
  }

  &__empty {
    padding: 16px 0;
    color: var(--td-text-color-placeholder);
    font-size: 12px;
    text-align: center;
  }
}
</style>
