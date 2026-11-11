<!--
  单据详情页的「查看凭证 (N)」按钮 —— VTRACE-4。

  # 为什么是一个组件，而不是六个页面各写一段

  六张单据（采购订单 / 销售订单 / 收货入库 / 出库单 / 费用报销 / 折旧）的差异**只在
  「该拿哪些 (source_type, source_id) 去查」**，那部分已经抽到 `voucher-trace.ts` 的六个
  `xxxSources()` 里；剩下的「查 / 藏 / 弹 / 跳」必须只有一份实现，否则
  「无凭证时不显示」「单张直跳」「失败不报错」这些细节迟早各有各的写法。

  # 四个刻意的降级（都是为了「宁可没有入口，不给点了打不开的假链接」）

  1. **没有 `business:erp:voucher:list`** ⇒ 一个请求都不发，按钮不出现（刷 403 没有意义）；
  2. **查询失败**（网络 / 500 / 非 UUID）⇒ 当没有凭证 ⇒ 按钮不出现，页面不红；
  3. **0 张** ⇒ `v-if` 直接不渲染（不是灰按钮 —— 灰按钮会让用户以为是权限或状态问题）；
  4. **来源单据还没生成**（采购单未收货 / 销售单未发货 / 出库单未过账）⇒ `sources` 为空
     ⇒ 同样不显示，语义正确：这单确实还没有凭证。

  # 单张 vs 多张

  - 1 张：直接 `router.push('/business/erp/voucher?voucher_no=…')`，与应付页
    `openVoucherDoc` 同一口径（凭证页 `openByQuery` 预填筛选并清掉 query），少一次点击；
  - 多张：弹窗列出全部（凭证号 / 日期 / 期间 / 摘要 / 金额 / 状态 / 来源），
    点任意一行同样跳到凭证页。红字凭证单独打标签 —— 红冲后同来源会出现 2 张。

  # 用法

  ```vue
  <VoucherTraceButton :sources="voucherSources" testid="stockin-detail-vouchers" />
  ```

  `voucherSources` 用本域 `xxxSources()` 算（见 `voucher-trace.ts` 的表）。
-->
<template>
  <t-button
    v-if="rows.length > 0"
    class="voucher-trace-btn"
    size="small"
    variant="outline"
    theme="primary"
    :title="buttonTip"
    :data-testid="testid"
    @click="onClick"
  >
    {{ buttonText }}
  </t-button>

  <!--
    `attach="body"` 是必须的：本组件被放在单据**详情抽屉**里，而 `t-dialog` 默认原地渲染
    ⇒ 弹窗会被裁在 720px 宽的抽屉体内（实测截图里列全被挤烂）。挂到 body 才铺满视口。
  -->
  <t-dialog
    v-model:visible="listVisible"
    attach="body"
    :header="dialogHeader"
    width="1000px"
    :footer="false"
    data-testid="voucher-trace-dialog"
  >
    <div class="vt" :data-testid="`${testid}-list`">
      <div class="vt__row vt__row--head">
        <span class="vt__no">凭证号</span>
        <span class="vt__date">日期 / 期间</span>
        <span class="vt__summary">摘要</span>
        <span class="vt__amount">金额</span>
        <span class="vt__status">状态</span>
        <span class="vt__source">来源</span>
      </div>
      <div
        v-for="row in rows"
        :key="row.id"
        class="vt__row vt__row--body"
        role="button"
        tabindex="0"
        :data-testid="`voucher-trace-row-${row.voucher_no}`"
        @click="openVoucher(row)"
        @keydown.enter.prevent="openVoucher(row)"
      >
        <span class="vt__no mono">
          {{ row.voucher_word }} {{ row.voucher_no }}
          <t-tag v-if="row.is_red" size="small" theme="danger" variant="light">红字</t-tag>
        </span>
        <span class="vt__date">
          <span class="mono">{{ dash(row.voucher_date) }}</span>
          <span class="muted">{{ dash(row.period) }}</span>
        </span>
        <span class="vt__summary" :title="row.summary">{{ dash(row.summary) }}</span>
        <span class="vt__amount money">{{ amountText(row) }}</span>
        <span class="vt__status">
          <t-tag size="small" variant="light" :theme="voucherStatusTheme(row.status)">{{ row.status_label }}</t-tag>
        </span>
        <span class="vt__source">{{ sourceTypeLabel(row.source_type) }}</span>
      </div>
    </div>
  </t-dialog>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useRouter } from 'vue-router';
import { MessagePlugin } from 'tdesign-vue-next';

import { fetchVouchersBySource, type VoucherRow } from '@/api/business/erp';
import { voucherStatusTheme } from '@/views/business/erp/voucher/voucher-logic';
import { dash } from '@/views/business/erp/shared/money';
import { useUserStore } from '@/stores/user';
import {
  amountText,
  mergeVoucherRows,
  normalizeSources,
  sourceTypeLabel,
  sourcesKey,
  voucherDocPathFor,
  voucherTraceButtonText,
  type VoucherSourceRef,
} from './voucher-trace';

const props = withDefaults(
  defineProps<{
    /** 该去反查凭证的来源（空 = 本单还没有凭证 = 不渲染，见文件头第 4 条） */
    sources?: VoucherSourceRef[];
    /** `data-testid`（各页面不同，便于 e2e 定位） */
    testid?: string;
  }>(),
  { sources: () => [], testid: 'doc-vouchers' },
);

/** 凭证列表权限（与凭证页路由 `meta.permission` 同一个码；没有它连请求都不发）。 */
const VOUCHER_LIST_PERM = 'business:erp:voucher:list';

const userStore = useUserStore();
const router = useRouter();

const canList = computed(() => userStore.hasPermission(VOUCHER_LIST_PERM));

/** 反查结果（已按凭证 id 去重、按后端同一套键排序） */
const rows = ref<VoucherRow[]>([]);
const listVisible = ref(false);

/** 请求令牌：详情抽屉连续切换单据时，只认最后一次的结果（否则会闪出上一单的凭证）。 */
let requestToken = 0;

/**
 * 参数指纹：`sources` 每次 computed 都会新建数组，直接 watch 引用会无限触发；
 * 用「类型+id 的排序串」当键，内容真变了才重查。
 */
const key = computed(() => sourcesKey(props.sources ?? []));

async function load(): Promise<void> {
  const list = normalizeSources(props.sources ?? []);
  const token = ++requestToken;
  if (list.length === 0 || !canList.value) {
    rows.value = [];
    return;
  }
  try {
    const groups = await Promise.all(list.map((source) => fetchVouchersBySource(source.type, source.id)));
    if (token !== requestToken) {
      return;
    }
    rows.value = mergeVoucherRows(groups);
  } catch {
    // 查询失败 = 当没有凭证（按钮不出现）。这里刻意不弹错误：
    // 详情页主体是单据，凭证只是附加信息，403/500 不该让用户以为单据坏了。
    if (token === requestToken) {
      rows.value = [];
    }
  }
}

watch(key, () => void load(), { immediate: true });

const buttonText = computed(() => voucherTraceButtonText(rows.value.length));
const buttonTip = computed(() =>
  rows.value.length === 1 ? '直接打开这张凭证' : `本单产生了 ${rows.value.length} 张凭证，点击查看清单`,
);
const dialogHeader = computed(() => `本单产生的凭证（${rows.value.length} 张）`);

/** 点按钮：单张直跳，多张弹清单。 */
function onClick(): void {
  if (rows.value.length === 1) {
    openVoucher(rows.value[0]);
    return;
  }
  listVisible.value = true;
}

/** 跳凭证页（`?voucher_no=` 深链，由凭证页 `openByQuery` 预填筛选）。 */
function openVoucher(row: VoucherRow): void {
  const path = voucherDocPathFor(row.voucher_no);
  if (!path) {
    void MessagePlugin.warning('这张凭证没有凭证号，无法定位');
    return;
  }
  listVisible.value = false;
  void router.push(path);
}

/** 空值统一显示成破折号（复用 `shared/money.ts`，与凭证列表同一套占位符）。 */
</script>

<style scoped>
.voucher-trace-btn {
  /* 工具条里与其它小按钮同高，不多占一行 */
  flex: none;
}

.vt {
  display: flex;
  flex-direction: column;
  max-height: 60vh;
  overflow-y: auto;
}

.vt__row {
  display: grid;
  /* 摘要列用 `minmax(0, 1fr)`：不加 0 下限时 min-content 会撑爆整行（实测出横向滚动条） */
  grid-template-columns: 200px 110px minmax(0, 1fr) 112px 88px 124px;
  gap: 8px;
  align-items: center;
  padding: 8px 12px;
  font-size: 13px;
  border-bottom: 1px solid var(--td-component-stroke, #e7e7e7);
}

/* 网格子项默认 `min-width:auto`，长内容会把列撑开 —— 统一压到 0，交给各自的 ellipsis */
.vt__row > * {
  min-width: 0;
}

.vt__row--head {
  position: sticky;
  top: 0;
  z-index: 1;
  color: var(--td-text-color-secondary, #666);
  background: var(--td-bg-color-container, #fff);
  font-weight: 500;
}

.vt__row--body {
  cursor: pointer;
}

.vt__row--body:hover,
.vt__row--body:focus-visible {
  background: var(--td-bg-color-container-hover, #f3f3f3);
  outline: none;
}

.vt__no {
  display: flex;
  gap: 4px;
  align-items: center;
}

.vt__date {
  display: flex;
  flex-direction: column;
  line-height: 18px;
}

.vt__summary {
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
}

.vt__source {
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
}

.vt__amount {
  text-align: right;
}

.mono {
  font-family: var(--td-font-family-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
}

.muted {
  color: var(--td-text-color-placeholder, #999);
}
</style>
