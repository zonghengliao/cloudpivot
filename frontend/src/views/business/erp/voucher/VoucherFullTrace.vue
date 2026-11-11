<!--
  凭证详情「全链路」标签页（VTRACE-5）：单据 → 审批 → 凭证 → 分录。

  # 为什么是这一屏

  凭证详情原来只能看到「来源：采购入库 / 业务单号：IN-xxx」两个标签，
  审计要回答「这笔账是谁批的、依据哪张单、分了哪些科目」得开五个页面手工串。
  这个组件把四段拼成一条时间轴：**上到下就是业务发生的顺序**。

  # 取数（不在这里造数据）

  - ① 来源单据 / ③ 本凭证 / ④ 分录：父组件传进来的 `voucher`（凭证详情接口）+
    `sourceDoc`（VTRACE-2 算好的可跳结论）；
  - ② 审批记录：本组件自己发一次 `GET /business/flow/instance/by-biz`
    （来源单据的 `biz_id`）。
      * 收货入库单自己常常没有审批（它由采购单审批放行）→ 再取一次上游采购单的审批，
        段落标题会写「审批记录 · 上游采购订单 PO-xxx」，**不把上游审批冒充成本单审批**；
      * 查不到 = 「无审批记录」，请求失败 = 「审批记录加载失败」——两句话严格分开。

  # 为什么不重做溯源

  路由、来源类型、深链档位全在 VTRACE-1/2 已有的模块里；本组件只做「把已有的数据接上」。
-->
<template>
  <div class="full-trace" data-testid="voucher-full-trace">
    <div class="trace-head">
      <span class="trace-head__title">全链路</span>
      <span class="trace-head__hint">从上到下：来源单据 → 审批 → 本凭证 → 分录（四段缺数据时写明原因）</span>
      <t-button
        size="small"
        variant="text"
        :loading="loading"
        data-testid="btn-trace-reload"
        @click="loadApproval()"
      >
        刷新
      </t-button>
    </div>

    <t-loading :loading="loading" size="small">
      <div
        v-for="step in steps"
        :key="step.kind"
        class="trace-step"
        :class="`trace-step--${step.kind}`"
        :data-testid="`trace-step-${step.kind}`"
      >
        <div class="trace-step__rail">
          <span class="trace-step__dot" />
        </div>
        <div class="trace-step__body">
          <div class="trace-step__head">
            <span class="trace-step__title">{{ step.title }}</span>
            <span v-if="step.subtitle" class="trace-step__subtitle">{{ step.subtitle }}</span>
            <t-tag
              v-if="!step.lines.length"
              size="small"
              variant="light"
              theme="default"
              :data-testid="`trace-empty-tag-${step.kind}`"
            >
              无记录
            </t-tag>
            <span v-else class="trace-step__count">{{ step.lines.length }} 条</span>
          </div>

          <p v-if="step.empty" class="trace-step__empty" :data-testid="`trace-empty-${step.kind}`">
            {{ step.empty }}
          </p>

          <div v-else class="trace-lines">
            <div
              v-for="line in step.lines"
              :key="line.key"
              class="trace-line"
              :data-testid="`trace-line-${line.key}`"
            >
              <div class="trace-line__main">
                <t-tag
                  v-if="line.badge"
                  size="small"
                  variant="light"
                  :theme="line.badge.theme"
                  class="trace-line__badge"
                >
                  {{ line.badge.text }}
                </t-tag>

                <t-tooltip v-if="line.tip" :content="line.tip" placement="top-left" :show-arrow="true">
                  <t-link
                    v-if="line.link"
                    theme="primary"
                    class="mono trace-line__title"
                    :data-testid="`trace-link-${line.key}`"
                    @click="emitLink(line)"
                  >
                    {{ line.title }}
                    <t-icon name="jump" />
                  </t-link>
                  <span v-else class="mono trace-line__title">{{ line.title }}</span>
                </t-tooltip>
                <template v-else>
                  <t-link
                    v-if="line.link"
                    theme="primary"
                    class="mono trace-line__title"
                    :data-testid="`trace-link-${line.key}`"
                    @click="emitLink(line)"
                  >
                    {{ line.title }}
                    <t-icon name="jump" />
                  </t-link>
                  <span v-else class="mono trace-line__title">{{ line.title }}</span>
                </template>

                <span v-if="line.amount" class="money trace-line__amount">{{ line.amount }}</span>
              </div>

              <div v-if="line.metas.length" class="trace-line__metas">
                <span v-for="(meta, index) in line.metas" :key="index" class="trace-line__meta">
                  {{ meta }}
                </span>
              </div>
              <p v-if="line.note" class="trace-line__note">{{ line.note }}</p>
            </div>
          </div>

          <p v-if="step.footer" class="trace-step__footer" :data-testid="`trace-footer-${step.kind}`">
            {{ step.footer }}
          </p>
        </div>
      </div>
    </t-loading>
  </div>
</template>

<script setup lang="ts">
/**
 * 全链路标签页。取数只有一处（审批记录），其余全部来自父组件已加载的凭证详情。
 */
import { computed, ref, watch } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';

import type { VoucherDetail } from '@/api/business/erp';
import { fetchStockInDetail } from '@/api/business/erp';
import { fetchFlowInstancesByBiz, type FlowTraceInstance } from '@/api/business/flow-api';

import type { SourceDocDisplay } from './voucher-source-link';
import {
  buildFullTrace,
  TRACE_STEP_TITLES,
  type TraceLine,
} from './voucher-full-trace';

const props = defineProps<{
  /** 凭证详情（含分录） */
  voucher: VoucherDetail;
  /** 来源单据的显示结论（VTRACE-2） */
  sourceDoc: SourceDocDisplay;
  /** 辅助核算 id → 名称（页面已有 `auxOptions` 缓存，注入进来，不在组件里重拉） */
  auxText?: (dim: 'DEPT' | 'SUPPLIER' | 'MATERIAL' | 'WAREHOUSE', id: string) => string;
}>();

const emit = defineEmits<{ (event: 'open', payload: { path: string; query: Record<string, string> }): void }>();

const loading = ref(false);
/** 反查到的审批单（发起时间倒序） */
const instances = ref<FlowTraceInstance[]>([]);
/** 审批段的限定语（`''` = 来源单据自己；否则「上游采购订单 PO-…」） */
const owner = ref('');
/** 加载失败的一句话（**与「无记录」严格区分**） */
const error = ref('');
/** 来源单据自己的日期 / 金额（只有采购入库拿得到真值，见 `sourceMetaOf`） */
const sourceMeta = ref<{ date: string | null; amount: string | null } | null>(null);
/** 已经取过数的来源（避免切换标签页重复打接口） */
const loadedFor = ref('');

/**
 * 上游单据兜底：**这类单据自己不挂审批，审批在它的上游单据上**。
 *
 * | 凭证来源 | 来源单据 | 它自己的审批 | 审批落在哪 |
 * |---|---|---|---|
 * | `STOCK_IN` | 收货入库单 | 通常没有（收货是执行动作） | 采购订单 `PO_APPROVE` |
 *
 * 只登记**核实过**的一条：收货单详情接口确实下发 `source_order_id`（`stockin_dto.rs`）。
 * 别的单据（出库 / 报销 / 折旧…）的审批就挂在它们自己身上（`biz_flow_instance.biz_id` = 该单据 id），
 * 走第 ① 步的直查即可，**不要**在这里凭猜补行（补错了 = 把上游审批冒充成本单审批）。
 */

/** 当前凭证上一次取数的来源指纹（换了凭证 / 换了来源就重新取）。 */
function fingerprintOf(voucher: VoucherDetail): string {
  return [voucher.id, voucher.source_type, voucher.source_id, voucher.source_doc_no, voucher.source_no].join('|');
}

/**
 * 取审批记录。
 *
 * 顺序：来源单据直查 → 空且是采购入库 → 上游采购订单再查一次。
 * 任何一步的**网络/权限错误**都落到 `error`（页面显示「加载失败」），
 * 只有「真的查到 0 条」才落到「无审批记录」。
 */
async function loadApproval(): Promise<void> {
  const voucher = props.voucher;
  loading.value = true;
  error.value = '';
  instances.value = [];
  owner.value = '';
  sourceMeta.value = null;
  try {
    const bizId = voucher.source_id;
    const bizNo = voucher.source_doc_no || voucher.source_no;
    if (!bizId && !bizNo) {
      // 手工凭证 / 未记录来源：没有可反查的业务单据，直接空段（不是失败）。
      loadedFor.value = fingerprintOf(voucher);
      return;
    }
    // 采购入库：顺手取一次收货单详情 —— 它同时给出「来源单据自己的日期 / 金额」
    // 与「上游采购订单 id」，一个请求喂两段，不让用户为一次点击付两次往返。
    const stockIn = voucher.source_type === 'STOCK_IN' && bizId ? await loadStockInDetail(bizId) : null;
    if (stockIn) {
      sourceMeta.value = { date: stockIn.inDate, amount: stockIn.totalWithTax };
    }

    const direct = await fetchFlowInstancesByBiz({ biz_id: bizId, biz_no: bizId ? null : bizNo });
    if (direct.list.length > 0) {
      instances.value = direct.list;
      loadedFor.value = fingerprintOf(voucher);
      return;
    }
    // 来源单据自己没有审批 → 采购入库单再走一步上游（采购订单的审批）。
    const upstream = await loadUpstreamInstances(stockIn);
    if (upstream) {
      instances.value = upstream.instances;
      owner.value = upstream.owner;
    }
    loadedFor.value = fingerprintOf(voucher);
  } catch (reason) {
    error.value = reason instanceof Error ? reason.message : String(reason);
  } finally {
    loading.value = false;
  }
}

/** 收货单详情里本组件用得到的三个字段（取不到就是 `null`，不影响别的段）。 */
interface StockInBrief {
  inDate: string | null;
  totalWithTax: string | null;
  orderId: string | null;
  orderNo: string | null;
}

/**
 * 取收货单详情（采购入库专用）。
 *
 * 失败返回 `null` 而不是抛错：来源段的日期 / 金额退回「记账依据」口径即可，
 * 不该因为收货单接口抽风就让整屏「审批记录加载失败」——两件事。
 */
async function loadStockInDetail(stockInId: string): Promise<StockInBrief | null> {
  try {
    const detail = await fetchStockInDetail(stockInId);
    const header = detail.header;
    return {
      inDate: header.in_date || null,
      totalWithTax: header.total_with_tax === null || header.total_with_tax === undefined ? null : String(header.total_with_tax),
      orderId: header.source_order_id || null,
      orderNo: header.source_order_no || null,
    };
  } catch {
    return null;
  }
}

/**
 * 上游单据的审批（收货入库单 → 采购订单）。
 *
 * 取不到上游（无源直收 / 采购单没挂审批）一律返回 `null`，
 * 由调用方保持「无审批记录」—— **不因为上游查不到就把整段判成失败**。
 */
async function loadUpstreamInstances(
  stockIn: StockInBrief | null,
): Promise<{ instances: FlowTraceInstance[]; owner: string } | null> {
  if (!stockIn?.orderId) {
    return null;
  }
  try {
    const list = await fetchFlowInstancesByBiz({ biz_id: stockIn.orderId });
    if (list.list.length === 0) {
      return null;
    }
    return {
      instances: list.list,
      owner: `上游采购订单 ${stockIn.orderNo ?? ''}`.trim(),
    };
  } catch {
    return null;
  }
}

const steps = computed(() =>
  buildFullTrace({
    source: {
      type: props.voucher.source_type,
      id: props.voucher.source_id,
      no: props.voucher.source_no,
      docNo: props.voucher.source_doc_no,
    },
    sourceDoc: props.sourceDoc,
    sourceMeta: sourceMeta.value,
    approval: { owner: owner.value, instances: instances.value, error: error.value },
    voucher: props.voucher,
    entries: props.voucher.entries,
    auxText: props.auxText,
  }),
);

/** 四段的标题（给界面上做 aria / 调试用；模板里读的是 `step.title`） */
const stepTitles = TRACE_STEP_TITLES;

/** 点来源单据 → 交给父组件跳（父组件负责落盘列表记忆）。 */
function emitLink(line: TraceLine): void {
  if (!line.link) {
    return;
  }
  if (!line.link.path) {
    // 空 path 的 push 会静默失败（停在当前页），必须拦住并说清楚。
    MessagePlugin.warning('该单据暂不支持跳转');
    return;
  }
  emit('open', line.link);
}

// 换了一张凭证（或同一张凭证的来源变了）→ 重新取审批记录。
watch(
  () => fingerprintOf(props.voucher),
  () => {
    if (loadedFor.value !== fingerprintOf(props.voucher)) {
      void loadApproval();
    }
  },
  { immediate: true },
);

defineExpose({ stepTitles, reload: loadApproval });
</script>

<style scoped>
.full-trace {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.trace-head {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 2px 0 8px;
  border-bottom: 1px solid var(--td-component-border);
}

.trace-head__title {
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.trace-head__hint {
  flex: 1;
  min-width: 0;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

/* ── 时间轴：一根竖线 + 每段一个点 ── */
.trace-step {
  display: flex;
  gap: 10px;
  padding: 10px 0 4px;
}

.trace-step__rail {
  position: relative;
  flex: 0 0 14px;
  display: flex;
  justify-content: center;
}

/* 竖线（最后一段不画到尾，避免悬空） */
.trace-step__rail::before {
  content: '';
  position: absolute;
  top: 14px;
  bottom: -14px;
  width: 2px;
  background: var(--td-component-stroke);
}

.trace-step:last-child .trace-step__rail::before {
  display: none;
}

.trace-step__dot {
  width: 10px;
  height: 10px;
  margin-top: 5px;
  border-radius: 50%;
  background: var(--td-brand-color);
  box-shadow: 0 0 0 3px var(--td-brand-color-light);
}

.trace-step--approval .trace-step__dot {
  background: var(--td-warning-color);
  box-shadow: 0 0 0 3px var(--td-warning-color-light);
}

.trace-step--voucher .trace-step__dot {
  background: var(--td-success-color);
  box-shadow: 0 0 0 3px var(--td-success-color-light);
}

.trace-step--entries .trace-step__dot {
  background: var(--td-text-color-placeholder);
  box-shadow: 0 0 0 3px var(--td-bg-color-component);
}

.trace-step__body {
  flex: 1;
  min-width: 0;
}

.trace-step__head {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 6px;
}

.trace-step__title {
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.trace-step__subtitle {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.trace-step__count {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.trace-step__empty {
  margin: 0;
  padding: 8px 10px;
  border-radius: 4px;
  background: var(--td-bg-color-container-hover);
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.trace-lines {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.trace-line {
  padding: 8px 10px;
  border: 1px solid var(--td-component-border);
  border-radius: 4px;
  background: var(--td-bg-color-container);
}

.trace-line__main {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
}

.trace-line__badge {
  margin-right: 6px;
}

.trace-line__title {
  flex: 1;
  min-width: 0;
  font-size: 13px;
  word-break: break-all;
}

.trace-line__amount {
  flex: 0 0 auto;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.trace-line__metas {
  display: flex;
  flex-wrap: wrap;
  gap: 4px 12px;
  margin-top: 4px;
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.trace-line__note {
  margin: 4px 0 0;
  font-size: 12px;
  color: var(--td-text-color-secondary);
  white-space: pre-wrap;
}

.trace-step__footer {
  margin: 8px 0 0;
  font-size: 12px;
  color: var(--td-text-color-secondary);
}
</style>
