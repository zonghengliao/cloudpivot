<template>
  <div class="check-panel">
    <!-- ==================== 顶部摘要（只有调用方给了 summary 才渲染） ==================== -->
    <t-alert
      v-if="summary"
      :theme="summaryTheme"
      :message="summary"
      :data-testid="`${prefix}-close-check-${summaryTheme === 'success' ? 'ok' : 'blocked'}`"
    />

    <!--
      ★ 列表**不能**挂在 `t-alert` 的 `#description` 槽上：TDesign 1.19 的 Alert 只消费
      `default` / `message` 两个槽（见 `node_modules/tdesign-vue-next/es/alert/alert.mjs::renderDescription`），
      `#description` 会被静默丢弃 —— PERIOD-CLOSE 卡实测「列表一个字都没渲染出来」，
      老版本页面的阻塞项列表当年也是这么丢的（只显示一句话摘要）。所以列表自己管样式。

      ★ 为什么过滤「有没有明细」：没有明细的阻塞项（期间未结 / 借贷不平 / 下年未开账）
      调用方通常已经在别处用一句话说清了（年结页的五项 ✅/❌），这里再重复一遍只是噪音。
      有明细的才需要表格。
    -->
    <div v-if="visibleBlockers.length > 0" class="blocker-list">
      <div
        v-for="blocker in visibleBlockers"
        :key="blocker.type"
        class="blocker"
        :data-testid="`${prefix}-blocker-${blocker.type}`"
      >
        <p v-if="showMessages" class="blocker__head">
          <span class="mono blocker__type">{{ blocker.type }}</span>
          <span>{{ blocker.message }}</span>
          <a
            v-if="blocker.link"
            class="blocker__link"
            :href="blocker.link"
            target="_blank"
            rel="noopener"
          >
            去列表处理
          </a>
        </p>

        <t-table
          v-if="blockerHasItems(blocker)"
          row-key="doc_id"
          size="small"
          :data="blocker.items ?? []"
          :columns="columns"
          class="blocker__table"
          :data-testid="`${prefix}-blocker-table-${blocker.type}`"
        >
          <template #doc_no="{ row }">
            <span class="mono">{{ row.doc_no }}</span>
            <span class="blocker__status">{{ row.status_label }}</span>
          </template>
          <template #doc_type="{ row }">{{ blockingDocTypeLabel(row.doc_type) }}</template>
          <template #amount="{ row }">{{ blockingMoneyText(row.amount) }}</template>
          <template #created_by_name="{ row }">{{ row.created_by_name || '—' }}</template>
          <template #owner="{ row }">
            <t-tag
              v-if="canRemindItem(row)"
              size="small"
              variant="light"
              theme="primary"
              :data-testid="`item-owner-${row.doc_id}`"
            >
              {{ ownerText(row) }}
            </t-tag>
            <t-tag
              v-else
              size="small"
              variant="light"
              theme="default"
              :data-testid="`item-owner-${row.doc_id}`"
            >
              待分配
            </t-tag>
          </template>
          <template #waiting="{ row }">{{ waitingText(row.waiting_since, now) }}</template>
          <template #op="{ row }">
            <div class="blocker__op">
              <t-link
                theme="primary"
                :data-testid="`item-goto-${row.doc_id}`"
                @click="$emit('goto', row.route)"
              >
                {{ row.action_label || '去处理' }}
              </t-link>
              <template v-if="remind">
                <t-tooltip
                  v-if="!canRemindItem(row)"
                  content="未指定处理人（先给该岗位授过账/审批权限）"
                >
                  <t-button size="small" variant="text" disabled data-testid="item-remind-disabled">
                    催办
                  </t-button>
                </t-tooltip>
                <t-button
                  v-else-if="reminded.has(row.doc_id)"
                  size="small"
                  variant="text"
                  disabled
                  :data-testid="`item-reminded-${row.doc_id}`"
                >
                  已催（今天）
                </t-button>
                <t-button
                  v-else
                  size="small"
                  variant="text"
                  theme="primary"
                  :loading="reminding"
                  :data-testid="`item-remind-${row.doc_id}`"
                  @click="remindOne(blocker.type, row)"
                >
                  催办
                </t-button>
              </template>
            </div>
          </template>
        </t-table>

        <p
          v-if="overflowNote(blocker)"
          class="blocker__overflow"
          :data-testid="`${prefix}-blocker-overflow`"
        >
          {{ overflowNote(blocker) }}
        </p>
      </div>
    </div>

    <!-- ==================== 全部催办 ==================== -->
    <div v-if="remind && remindableIds.length > 0" class="panel-actions">
      <t-button
        size="small"
        theme="primary"
        variant="outline"
        :disabled="reminding"
        :loading="reminding"
        :data-testid="`${prefix}-remind-all`"
        @click="remindAll"
      >
        全部催办（{{ remindableIds.length }}）
      </t-button>
      <span class="panel-actions__hint">
        催办把待办发到处理人的站内信（有具体人就发给他，只有岗位就发给该岗位全体）；同一张单 24 小时内不重复催。
      </span>
    </div>

    <!-- ==================== 提醒项（不拦结账） ==================== -->
    <div
      v-if="(warnings ?? []).length > 0"
      class="check-warnings"
      :data-testid="`${prefix}-close-warnings`"
    >
      <p class="check-warnings__title">
        另有 {{ (warnings ?? []).length }} 项全库巡检提醒（不拦结账）：
      </p>
      <ul class="warn-list">
        <li v-for="(warning, index) in warnings ?? []" :key="`w-${index}`">
          {{ warning.message }}
          <a
            v-if="warning.link"
            class="blocker__link"
            :href="warning.link"
            target="_blank"
            rel="noopener"
          >
            去看看
          </a>
          <ul v-if="blockerHasItems(warning)" class="warn-list__items">
            <li v-for="item in warning.items ?? []" :key="item.doc_id">
              <span class="mono">{{ item.doc_no }}</span>
              <span class="blocker__status">{{ item.status_label }}</span>
              <a class="blocker__link" :href="item.route" target="_blank" rel="noopener">
                {{ item.action_label || '去看看' }}
              </a>
            </li>
          </ul>
        </li>
      </ul>
    </div>
  </div>
</template>

<script setup lang="ts">
/**
 * 挡路单据**检查清单面板**（PERIOD-CLOSE / YEAR-END 共用）。
 *
 * # 用户原话对上的三件事
 *
 * > 「不光要显示出来有什么问题。还要把要处理的单据列出来？方便跳转处理或者发消息给
 * >   相关人员进行后续处理？列出的列表是不是还写清楚下一步需要谁处理？」
 *
 * | 需求 | 这里的实现 |
 * |------|-----------|
 * | 列出要处理的单据 | `blocker.items` → 表格一行一张（单号 / 类型 / 金额 / 制单人 / 状态） |
 * | 写清该谁处理 | `ownerText()`：`会计(李四)` / `会计` / **待分配**（后端反查不到就留空，不猜） |
 * | 跳转处理 | 每行「去处理」→ `route`（带 id 直达单据；凭证是 `?voucher_no=`，采购/收货是 `?po_id=` / `?in_id=`） |
 * | 发消息催办 | 每行「催办」+ 底部「全部催办」→ 调用方传入的 `remind`（期间：`/period/{id}/remind`；年结：`/year-end/remind`） |
 *
 * # 三个设计约束
 *
 * 1. **不自己发请求**：催办接口的地址/入参由调用方给（`remind` 函数 prop），
 *    这样期间催办与年结催办共用同一个面板，而协议各自演进互不影响；
 * 2. **不自己算收件人**：只传「哪一类 + 哪几张」，收件人由后端按明细的 owner 现算；
 * 3. **不吞结果**：每次催办都 `emit('reminded', result)`，父组件用后端那句 `message` 弹 toast。
 */
import { computed, ref } from 'vue';
import type { PrimaryTableCol } from 'tdesign-vue-next';
import type { PeriodBlockingItem, PeriodRemindResult } from '@/api/business/erp';
import {
  blockingDocTypeLabel,
  blockingMoneyText,
  blockerHasItems,
  canRemindItem,
  ownerText,
  remindableItems,
  waitingText,
  type BlockerRemindFn,
  type BlockingBlocker,
} from './blocker-logic';

const props = withDefaults(
  defineProps<{
    /** 阻断项（有明细的才上表格，没明细的由调用方自己用一句话说清） */
    blockers: BlockingBlocker[];
    /** 提醒项（不拦结账，灰字列表） */
    warnings?: BlockingBlocker[];
    /** 催办函数；不传 = 只读（不渲染任何催办按钮） */
    remind?: BlockerRemindFn;
    /** 顶部一句话摘要；不传 = 不渲染 alert */
    summary?: string;
    /** 摘要主题（`can_close` 时 success，否则 warning） */
    summaryTheme?: 'success' | 'warning' | 'info' | 'error';
    /** data-testid 前缀（`period` / `yearend`） */
    testidPrefix?: string;
    /** 是否在每条阻断项前面重复它的 `message`（调用方已有摘要时传 false） */
    showMessages?: boolean;
  }>(),
  {
    warnings: () => [],
    summary: '',
    summaryTheme: 'warning',
    testidPrefix: 'period',
    showMessages: true,
  },
);

const emit = defineEmits<{
  /** 点「去处理」→ 父组件决定用 router 还是新窗口 */
  (event: 'goto', route: string): void;
  /** 催办完成 → 父组件 toast */
  (event: 'reminded', result: PeriodRemindResult): void;
}>();

const prefix = computed(() => props.testidPrefix);

/** 表格列（同后端 `BlockingItem` 的展示口径） */
const columns: PrimaryTableCol[] = [
  { colKey: 'doc_no', title: '单据号', width: 230 },
  { colKey: 'doc_type', title: '类型', width: 70 },
  { colKey: 'amount', title: '金额', width: 110, align: 'right' },
  { colKey: 'created_by_name', title: '制单人', width: 90 },
  { colKey: 'owner', title: '该谁处理', width: 130 },
  { colKey: 'waiting', title: '已等', width: 80 },
  { colKey: 'op', title: '操作', width: 150, fixed: 'right' },
];

/** 「已等 N 天」的基准时间（面板出现那一刻，不随秒跳） */
const now = ref(new Date());

/** 已催过的单据 id（当天不再重复点） */
const reminded = ref(new Set<string>());
/** 催办进行中（按钮 loading） */
const reminding = ref(false);

/** 只渲染「有明细」的阻断项（没明细的在调用方的摘要里已经说过了） */
const visibleBlockers = computed<BlockingBlocker[]>(() =>
  props.blockers.filter((blocker) => blockerHasItems(blocker)),
);

/** 可催办的明细 id（有处理人 + 今天还没催过） */
const remindableIds = computed<string[]>(() => {
  if (!props.remind) {
    return [];
  }
  const ids: string[] = [];
  for (const blocker of visibleBlockers.value) {
    for (const item of remindableItems(blocker.items ?? [])) {
      if (!reminded.value.has(item.doc_id)) {
        ids.push(item.doc_id);
      }
    }
  }
  return ids;
});

/**
 * 超限提示：条数超过明细条数时说明「只列了前 N 张」。
 *
 * 后端每类最多给 50 条明细、`count` 是全量；两者不等就是被截断了。
 */
function overflowNote(blocker: BlockingBlocker): string {
  const items = blocker.items ?? [];
  const total = blocker.count ?? items.length;
  if (total <= items.length) {
    return '';
  }
  return `共 ${total} 张，仅显示前 ${items.length} 张，其余请先去列表页批量处理`;
}

/** 单张催办 */
async function remindOne(kind: string, item: PeriodBlockingItem): Promise<void> {
  if (reminding.value || !props.remind) {
    return;
  }
  reminding.value = true;
  try {
    const result = await props.remind({ blocker_kind: kind, item_ids: [item.doc_id] });
    // 发出去了、或者 24 小时内已催过 → 这一行今天不用再点了
    if (result.sent > 0 || result.skipped_recent > 0) {
      reminded.value = new Set([...reminded.value, item.doc_id]);
    }
    emit('reminded', result);
  } finally {
    reminding.value = false;
  }
}

/** 一键催所有可催的 */
async function remindAll(): Promise<void> {
  if (reminding.value || !props.remind) {
    return;
  }
  const ids = remindableIds.value;
  if (ids.length === 0) {
    return;
  }
  reminding.value = true;
  try {
    const result = await props.remind({ blocker_kind: null, item_ids: ids });
    if (result.sent > 0 || result.skipped_recent > 0) {
      reminded.value = new Set([...reminded.value, ...ids]);
    }
    emit('reminded', result);
  } finally {
    reminding.value = false;
  }
}

/** 供父组件在重新预检后清空「已催」标记 */
defineExpose({
  reset(): void {
    reminded.value = new Set();
    now.value = new Date();
  },
  remindableCount: remindableIds,
});
</script>

<style scoped lang="less">
.check-panel {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.blocker {
  margin-top: 10px;
}

.blocker__head {
  margin: 0 0 6px;
  font-size: 12px;
  line-height: 1.9;
}

.blocker__type {
  margin-right: 6px;
  color: var(--td-text-color-placeholder);
}

.blocker__link {
  margin-left: 8px;
  color: var(--td-brand-color);
}

.blocker__table {
  font-size: 12px;
}

.blocker__status {
  margin-left: 6px;
  color: var(--td-text-color-placeholder);
}

.blocker__op {
  display: flex;
  align-items: center;
  gap: 6px;
}

.blocker__overflow {
  margin: 4px 0 0;
  font-size: 12px;
  color: var(--td-warning-color);
}

.panel-actions {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-top: 12px;
  flex-wrap: wrap;
}

.panel-actions__hint {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.check-warnings {
  margin-top: 4px;
  padding: 8px 10px;
  border-radius: 4px;
  background-color: var(--td-bg-color-container-hover);
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.check-warnings__title {
  margin: 0;
  font-weight: 500;
}

.warn-list {
  margin: 8px 0 0;
  padding-left: 18px;
  line-height: 1.9;
}

.warn-list__items {
  margin: 4px 0;
  padding-left: 16px;
  list-style: circle;
}
</style>
