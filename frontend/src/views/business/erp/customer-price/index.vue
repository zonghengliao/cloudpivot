<template>
  <div class="price-page">
    <!-- ==================== 页面头（取价链口径，两个 Tab 共用） ==================== -->
    <div class="page-head">
      <div class="page-head__title">
        <span>价格管理</span>
        <t-tag size="small" variant="light" theme="primary">协议价 / 阶梯价 / 促销价</t-tag>
      </div>
      <span class="page-head__tip">
        取价优先级：客户协议价 → 客户专属阶梯 → 通用阶梯 → 物料标准售价 → 未找到（手填）。
        同一物料可维护多档，档位区间是<b>左闭右开</b>（10 到 100 指 [10, 100)）。
      </span>
    </div>

    <t-tabs v-model="activeTab" class="page-tabs" data-testid="price-tabs">
      <t-tab-panel value="agreed" label="客户协议价" />
      <t-tab-panel value="tier" label="阶梯价 / 促销价" />
    </t-tabs>

    <!-- ==================== ① 客户协议价（服务端分页，tab 内 v-if 只挂当前 tab） ==================== -->
    <ListPage
      v-if="activeTab === 'agreed'"
      ref="agreedListRef"
      title="客户协议价"
      meta-unit="条"
      testid="price-agreed"
      table-testid="price-agreed-table"
      btn-prefix="btn-price"
      :columns="agreedColumns"
      :filters="agreedFilters"
      :filter-state="agreedFilter"
      :fetcher="agreedFetcher"
      :query-builder="agreedQueryBuilder"
      :page-size="PRICE_PAGE_SIZE"
      :actions="agreedActions"
      @loaded="onAgreedLoaded"
    >
      <template #toolbar>
        <t-tooltip
          :content="
            canAgreedCreate
              ? '给某个客户单独定价（同一客户 + 物料 + 起订量 + 生效日 只能一条）'
              : '当前账号没有 business:erp:cust-price:create 权限'
          "
          placement="top-right"
        >
          <span class="gate-wrap">
            <t-button
              theme="primary"
              data-testid="btn-price-agreed-new"
              :disabled="!canAgreedCreate"
              @click="openAgreedCreate"
            >
              <template #icon><t-icon name="add" /></template>
              新建协议价
            </t-button>
          </span>
        </t-tooltip>
      </template>

      <template #customer="{ row }">
        <span>{{ customerText(row) }}</span>
      </template>
      <template #material="{ row }">
        <span>{{ materialText(row) }}</span>
      </template>
      <template #price="{ row }">
        <span class="mono price-cell" data-testid="price-agreed-price">{{ priceText(row.price) }}</span>
      </template>
      <template #min_qty="{ row }">
        <span class="mono">{{ decimalText(row.min_qty) }}</span>
      </template>
      <template #validity="{ row }">
        <span class="mono validity-cell">{{ validityText(row.effective_from, row.effective_to) }}</span>
      </template>
      <template #status="{ row }">
        <t-tag size="small" variant="light" :theme="statusTheme(row.status)">{{ statusLabel(row.status) }}</t-tag>
      </template>

      <template #empty="{ filtered }">
        <div class="panel-empty" data-testid="price-agreed-empty">
          <t-icon class="panel-empty__icon" name="price" />
          <p class="panel-empty__title">
            {{ filtered ? '没有符合条件的协议价' : '还没有协议价' }}
          </p>
          <p class="panel-empty__desc">
            {{
              filtered
                ? '换个客户 / 物料试试，或点「重置」'
                : '点右上角「新建协议价」给客户单独定价'
            }}
          </p>
        </div>
      </template>
    </ListPage>

    <!-- ==================== ② 阶梯价 / 促销价（服务端分页） ==================== -->
    <ListPage
      v-else
      ref="tierListRef"
      title="阶梯价 / 促销价"
      meta-unit="条"
      testid="price-tier"
      table-testid="price-tier-table"
      btn-prefix="btn-price-tier"
      :columns="tierColumns"
      :filters="tierFilters"
      :filter-state="tierFilter"
      :fetcher="tierFetcher"
      :query-builder="tierQueryBuilder"
      :page-size="PRICE_PAGE_SIZE"
      :actions="tierActions"
      @loaded="onTierLoaded"
    >
      <template #toolbar>
        <t-tooltip
          :content="
            canTierCreate
              ? '同一物料可维护多档：区间左闭右开、首尾相接即形成阶梯'
              : '当前账号没有 business:erp:price-tier:create 权限'
          "
          placement="top-right"
        >
          <span class="gate-wrap">
            <t-button
              theme="primary"
              data-testid="btn-price-tier-new"
              :disabled="!canTierCreate"
              @click="openTierCreate"
            >
              <template #icon><t-icon name="add" /></template>
              新建档位
            </t-button>
          </span>
        </t-tooltip>
      </template>

      <template #material="{ row }">
        <span>{{ materialText(row) }}</span>
      </template>
      <template #scope="{ row }">
        <t-tag v-if="!row.customer_id" size="small" variant="light" theme="default">通用</t-tag>
        <span v-else>{{ customerText(row) }}</span>
      </template>
      <template #tier_type="{ row }">
        <t-tag size="small" variant="light" :theme="tierTypeTheme(row.tier_type)">
          {{ tierTypeLabel(row.tier_type) }}
        </t-tag>
      </template>
      <template #range="{ row }">
        <span class="mono" data-testid="price-tier-range">{{ tierRangeText(row) }}</span>
      </template>
      <template #price="{ row }">
        <span class="mono price-cell">{{ priceText(row.price) }}</span>
      </template>
      <template #validity="{ row }">
        <span class="mono validity-cell">{{ validityText(row.effective_from, row.effective_to) }}</span>
      </template>
      <template #status="{ row }">
        <t-tag size="small" variant="light" :theme="statusTheme(row.status)">{{ statusLabel(row.status) }}</t-tag>
      </template>

      <template #empty="{ filtered }">
        <div class="panel-empty" data-testid="price-tier-empty">
          <t-icon class="panel-empty__icon" name="price" />
          <p class="panel-empty__title">
            {{ filtered ? '没有符合条件的档位' : '还没有阶梯价' }}
          </p>
          <p class="panel-empty__desc">
            {{
              filtered
                ? '换个物料试试，或点「重置」'
                : '同一物料可以按数量维护多档，区间首尾相接'
            }}
          </p>
        </div>
      </template>
    </ListPage>

    <!-- ==================== ③ 协议价新建 / 编辑 ==================== -->
    <t-dialog
      v-model:visible="agreedFormVisible"
      :header="agreedEditingId ? '编辑客户协议价' : '新建客户协议价'"
      width="620px"
      :confirm-btn="{ content: agreedEditingId ? '保存' : '创建', loading: agreedSubmitting }"
      data-testid="price-agreed-dialog"
      @confirm="submitAgreedForm"
    >
      <t-form label-width="104px" class="price-form">
        <t-form-item label="客户" required-mark>
          <t-select
            v-model="agreedForm.customer_id"
            :options="customerOptions"
            filterable
            placeholder="选择客户（协议价给某个客户单独定价）"
            data-testid="price-form-customer"
          />
        </t-form-item>
        <t-form-item label="物料" required-mark>
          <t-select
            v-model="agreedForm.material_id"
            :options="materialOptions"
            filterable
            placeholder="物料编码 / 名称"
            data-testid="price-form-material"
          />
        </t-form-item>
        <t-form-item label="协议单价" required-mark>
          <t-input-number
            v-model="agreedForm.price"
            theme="normal"
            :decimal-places="4"
            :min="0"
            align="right"
            placeholder="含税单价，必须 > 0"
            data-testid="price-form-price"
          />
        </t-form-item>
        <t-form-item label="起订量">
          <t-input-number
            v-model="agreedForm.min_qty"
            theme="normal"
            :decimal-places="4"
            :min="0"
            align="right"
            placeholder="0 = 不限制"
            data-testid="price-form-min-qty"
          />
        </t-form-item>
        <t-form-item label="生效日" required-mark>
          <t-date-picker
            v-model="agreedForm.effective_from"
            value-type="YYYY-MM-DD"
            format="YYYY-MM-DD"
            data-testid="price-form-from"
          />
        </t-form-item>
        <t-form-item label="失效日">
          <t-date-picker
            v-model="agreedForm.effective_to"
            value-type="YYYY-MM-DD"
            format="YYYY-MM-DD"
            placeholder="留空 = 长期有效"
            clearable
            data-testid="price-form-to"
          />
        </t-form-item>
        <t-form-item label="状态">
          <t-radio-group v-model="agreedForm.status" data-testid="price-form-status">
            <t-radio value="ACTIVE">启用（参与取价）</t-radio>
            <t-radio value="INACTIVE">停用（保留历史，不参与取价）</t-radio>
          </t-radio-group>
        </t-form-item>
        <t-form-item label="备注">
          <t-input v-model="agreedForm.remark" placeholder="选填，例如合同号" data-testid="price-form-remark" />
        </t-form-item>
      </t-form>

      <t-alert v-if="agreedFormError" theme="error" :message="agreedFormError" class="form-alert" data-testid="price-agreed-form-error" />
    </t-dialog>

    <!-- ==================== ④ 档位新建 / 编辑 ==================== -->
    <t-dialog
      v-model:visible="tierFormVisible"
      :header="tierEditingId ? '编辑阶梯 / 促销档位' : '新建阶梯 / 促销档位'"
      width="620px"
      :confirm-btn="{ content: tierEditingId ? '保存' : '创建', loading: tierSubmitting }"
      data-testid="price-tier-dialog"
      @confirm="submitTierForm"
    >
      <t-form label-width="104px" class="price-form">
        <t-form-item label="物料" required-mark>
          <t-select
            v-model="tierForm.material_id"
            :options="materialOptions"
            filterable
            placeholder="物料编码 / 名称"
            data-testid="price-tier-form-material"
          />
        </t-form-item>
        <t-form-item label="适用客户">
          <t-select
            v-model="tierForm.customer_id"
            :options="[{ label: '通用（所有客户）', value: '' }, ...customerOptions]"
            filterable
            placeholder="留空 = 通用档位"
            clearable
            data-testid="price-tier-form-customer"
          />
        </t-form-item>
        <t-form-item label="档位类型" required-mark>
          <t-radio-group v-model="tierForm.tier_type" data-testid="price-tier-form-type">
            <t-radio value="TIER">阶梯价（按量降价）</t-radio>
            <t-radio value="PROMO">促销价（优先于阶梯命中）</t-radio>
          </t-radio-group>
        </t-form-item>
        <t-form-item label="区间下界" required-mark>
          <t-input-number
            v-model="tierForm.min_qty"
            theme="normal"
            :decimal-places="4"
            :min="0"
            align="right"
            placeholder="含本值，例如 10"
            data-testid="price-tier-form-min"
          />
        </t-form-item>
        <t-form-item label="区间上界">
          <t-input-number
            v-model="tierForm.max_qty"
            theme="normal"
            :decimal-places="4"
            :min="0"
            align="right"
            placeholder="不含本值；留空 = 无上限"
            data-testid="price-tier-form-max"
          />
        </t-form-item>
        <t-form-item label="档位单价" required-mark>
          <t-input-number
            v-model="tierForm.price"
            theme="normal"
            :decimal-places="4"
            :min="0"
            align="right"
            placeholder="含税单价，必须 > 0"
            data-testid="price-tier-form-price"
          />
        </t-form-item>
        <t-form-item label="生效日" required-mark>
          <t-date-picker
            v-model="tierForm.effective_from"
            value-type="YYYY-MM-DD"
            format="YYYY-MM-DD"
            data-testid="price-tier-form-from"
          />
        </t-form-item>
        <t-form-item label="失效日">
          <t-date-picker
            v-model="tierForm.effective_to"
            value-type="YYYY-MM-DD"
            format="YYYY-MM-DD"
            placeholder="留空 = 长期有效"
            clearable
            data-testid="price-tier-form-to"
          />
        </t-form-item>
        <t-form-item label="状态">
          <t-radio-group v-model="tierForm.status" data-testid="price-tier-form-status">
            <t-radio value="ACTIVE">启用（参与取价）</t-radio>
            <t-radio value="INACTIVE">停用</t-radio>
          </t-radio-group>
        </t-form-item>
        <t-form-item label="备注">
          <t-input v-model="tierForm.remark" placeholder="选填，例如促销活动名" data-testid="price-tier-form-remark" />
        </t-form-item>
      </t-form>

      <t-alert v-if="tierFormError" theme="error" :message="tierFormError" class="form-alert" data-testid="price-tier-form-error" />
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 价格管理页（销售链 · SO-34 / ARCH-7 §15.3.2）
 *
 * 两个 Tab，对应两张表（取价链的 ①②③ 级）：
 *
 * | Tab | 表 | 取价位置 | 权限码前缀 |
 * |-----|----|----------|-----------|
 * | 客户协议价 | `biz_customer_price` | ① `AGREED` | `business:erp:cust-price:*` |
 * | 阶梯价 / 促销价 | `biz_price_tier` | ② `TIER_CUSTOMER` / ③ `TIER_COMMON` | `business:erp:price-tier:*` |
 *
 * # 口径（写错了业务会静默用错价）
 *
 * * 档位区间**左闭右开**：`min_qty <= qty < max_qty`，`max_qty` 空 = 无上限；
 * * 有效期两端都**含**，`effective_to` 空 = 长期有效；
 * * 唯一键：协议价 `(客户, 物料, 起订量, 生效日)`；档位 `(物料, 客户含通用, 类型, 起订量, 生效日)`；
 * * 停用（`INACTIVE`）不删行：取价链只看 `ACTIVE`，历史价永远可追溯。
 *
 * # 两个 Tab 各一张 ListPage（P3-12 收口）
 *
 * 两个 Tab 都是**服务端分页**，所以各挂一个 ListPage、tab 内容用 `v-if` 只挂当前 tab
 * （与 `storage/backend` 同款）：切过去才拉，两个 tab 的筛选 / 页码 / 空态互不干扰。
 *
 * # 权限
 *
 * 三个权限码各自对应一个按钮；`actions.row[].permission` 只隐藏**按钮**，接口侧后端还会再判一次
 * （前端隐藏不是安全边界）。
 *
 * # 数据
 *
 * 列表一律**服务端分页**（`page` / `page_size`），不在前端切片；客户 / 物料下拉
 * 一次拉 200 条（与销售订单页同口径），超出时让用户去主数据页查。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin } from 'tdesign-vue-next';
import { useUserStore } from '@/stores/user';
import { ApiError } from '@/api/request';
import ListPage from '@/components/ListPage/index.vue';
import type { FilterDef, ListActions, ListFetcher, ListQueryBuilder } from '@/components/ListPage/types';
import {
  createCustomerPrice,
  createPriceTier,
  deleteCustomerPrice,
  deletePriceTier,
  fetchCustomerPricePage,
  fetchPriceTierPage,
  updateCustomerPrice,
  updatePriceTier,
  type CustomerPriceFilter,
  type CustomerPriceRow,
  type PriceTierFilter,
  type PriceTierRow,
} from '@/api/business/erp/customer-price';
import { fetchCustomerPage, fetchMaterialPage, type Customer, type MaterialRow } from '@/api/business/erp';
import {
  AGREED_COLUMNS,
  PRICE_PAGE_SIZE,
  STATUS_OPTIONS,
  TIER_COLUMNS,
  TIER_TYPE_OPTIONS,
  agreedFormFrom,
  buildAgreedCreatePayload,
  buildAgreedUpdatePayload,
  buildTierCreatePayload,
  buildTierUpdatePayload,
  customerText,
  decimalText,
  emptyAgreedFilter,
  emptyAgreedForm,
  emptyTierFilter,
  emptyTierForm,
  materialText,
  priceText,
  statusLabel,
  statusTheme,
  tierFormFrom,
  tierRangeText,
  tierTypeLabel,
  tierTypeTheme,
  validityText,
  validateAgreedForm,
  validateTierForm,
  type AgreedFormDraft,
  type TierFormDraft,
} from './customer-price-query';

/** 本页权限码（与 `V0308__customer_price_menu.up.sql` 的 auth_code 逐字一致） */
const PERM = {
  agreedList: 'business:erp:cust-price:list',
  agreedCreate: 'business:erp:cust-price:create',
  agreedUpdate: 'business:erp:cust-price:update',
  agreedDelete: 'business:erp:cust-price:delete',
  tierList: 'business:erp:price-tier:list',
  tierCreate: 'business:erp:price-tier:create',
  tierUpdate: 'business:erp:price-tier:update',
  tierDelete: 'business:erp:price-tier:delete',
} as const;

const userStore = useUserStore();
const canAgreedCreate = computed(() => userStore.hasPermission(PERM.agreedCreate));
const canTierCreate = computed(() => userStore.hasPermission(PERM.tierCreate));

const agreedColumns = AGREED_COLUMNS;
const tierColumns = TIER_COLUMNS;
const statusOptions = STATUS_OPTIONS;
const tierTypeOptions = TIER_TYPE_OPTIONS;

/** 当前 Tab（默认协议价：取价链最高优先级，也是最常维护的一张） */
const activeTab = ref<'agreed' | 'tier'>('agreed');

/** 北京时间今天（与后端 `today_cst()` 同口径） */
function todayCst(): string {
  return new Date(Date.now() + 8 * 60 * 60 * 1000).toISOString().slice(0, 10);
}

/** 错误 → 人话（ApiError 带后端 msg，别吃掉） */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

// ==================== 主数据下拉 ====================

const customers = ref<Customer[]>([]);
const materials = ref<MaterialRow[]>([]);

const customerOptions = computed(() =>
  customers.value.map((item) => ({ label: `${item.code} ${item.name}`, value: item.id })),
);
const materialOptions = computed(() =>
  materials.value.map((item) => {
    const spec = item.spec ? ` / ${item.spec}` : '';
    return { label: `${item.material_code} ${item.material_name}${spec}`, value: item.id };
  }),
);

async function loadOptions(): Promise<void> {
  // 两个下拉互不依赖，一起发；单边失败不该让整页失败（价格列表本身仍可用）
  const [customerResult, materialResult] = await Promise.allSettled([
    fetchCustomerPage({ page: 1, page_size: 200 }),
    fetchMaterialPage({ page: 1, page_size: 200 }),
  ]);
  if (customerResult.status === 'fulfilled') {
    customers.value = customerResult.value.list ?? [];
  }
  if (materialResult.status === 'fulfilled') {
    materials.value = materialResult.value.list ?? [];
  }
}

// ==================== ① 协议价列表（交给 ListPage） ====================

const agreedFilter = reactive<CustomerPriceFilter>(emptyAgreedFilter());
/** 当前页（保存 / 删除后回页用；每拉完一页由 `onAgreedLoaded` 同步） */
const agreedCurrent = ref(1);
/** ListPage 实例（新建 / 编辑 / 删除后重拉） */
const agreedListRef = ref<InstanceType<typeof ListPage> | null>(null);

/** 协议价筛选项（客户 / 物料下拉是异步拉的，所以用 computed） */
const agreedFilters = computed<FilterDef[]>(() => [
  {
    key: 'customer_id',
    type: 'select',
    placeholder: '全部客户',
    width: 220,
    options: customerOptions.value,
    filterable: true,
    testid: 'price-filter-customer',
  },
  {
    key: 'material_id',
    type: 'select',
    placeholder: '全部物料',
    width: 260,
    options: materialOptions.value,
    filterable: true,
    testid: 'price-filter-material',
  },
  {
    key: 'status',
    type: 'select',
    placeholder: '全部状态',
    width: 150,
    options: statusOptions,
    testid: 'price-filter-status',
  },
  {
    key: 'keyword',
    type: 'keyword',
    placeholder: '客户 / 物料 编码或名称',
    width: 220,
    testid: 'price-filter-keyword',
  },
]);

const agreedFetcher: ListFetcher<CustomerPriceRow> = (query) =>
  fetchCustomerPricePage(query as Parameters<typeof fetchCustomerPricePage>[0]);

const agreedQueryBuilder: ListQueryBuilder = (state, page, size) => ({
  ...(state as CustomerPriceFilter),
  page,
  page_size: size,
});

/** 行操作：只有「编辑 / 删除」，权限码一挂就行 */
const agreedActions: ListActions = {
  row: [
    {
      key: 'edit',
      label: '编辑',
      permission: PERM.agreedUpdate,
      testid: 'btn-price-agreed-edit',
      handler: (row: CustomerPriceRow) => openAgreedEdit(row),
    },
    {
      key: 'delete',
      label: '删除',
      theme: 'danger',
      permission: PERM.agreedDelete,
      testid: 'btn-price-agreed-delete',
      handler: (row: CustomerPriceRow) => confirmAgreedDelete(row),
    },
  ],
};

/** 一页拉完：同步页码 */
function onAgreedLoaded(payload: { page: number }): void {
  agreedCurrent.value = payload.page;
}

/** 重拉协议价列表（不传 = 当前页） */
async function reloadAgreed(page: number = agreedCurrent.value): Promise<void> {
  await agreedListRef.value?.load(page);
}

// ==================== ② 档位列表（交给 ListPage） ====================

const tierFilter = reactive<PriceTierFilter>(emptyTierFilter());
const tierCurrent = ref(1);
const tierListRef = ref<InstanceType<typeof ListPage> | null>(null);

const tierFilters = computed<FilterDef[]>(() => [
  {
    key: 'material_id',
    type: 'select',
    placeholder: '全部物料',
    width: 260,
    options: materialOptions.value,
    filterable: true,
    testid: 'price-tier-filter-material',
  },
  {
    key: 'customer_id',
    type: 'select',
    placeholder: '适用客户（通用档位不选）',
    width: 220,
    options: customerOptions.value,
    filterable: true,
    testid: 'price-tier-filter-customer',
  },
  {
    key: 'tier_type',
    type: 'select',
    placeholder: '全部类型',
    width: 150,
    options: tierTypeOptions,
    testid: 'price-tier-filter-type',
  },
  {
    key: 'status',
    type: 'select',
    placeholder: '全部状态',
    width: 150,
    options: statusOptions,
    testid: 'price-tier-filter-status',
  },
  {
    key: 'keyword',
    type: 'keyword',
    placeholder: '物料 / 客户 编码或名称',
    width: 220,
    testid: 'price-tier-filter-keyword',
  },
]);

const tierFetcher: ListFetcher<PriceTierRow> = (query) =>
  fetchPriceTierPage(query as Parameters<typeof fetchPriceTierPage>[0]);

const tierQueryBuilder: ListQueryBuilder = (state, page, size) => ({
  ...(state as PriceTierFilter),
  page,
  page_size: size,
});

const tierActions: ListActions = {
  row: [
    {
      key: 'edit',
      label: '编辑',
      permission: PERM.tierUpdate,
      testid: 'btn-price-tier-edit',
      handler: (row: PriceTierRow) => openTierEdit(row),
    },
    {
      key: 'delete',
      label: '删除',
      theme: 'danger',
      permission: PERM.tierDelete,
      testid: 'btn-price-tier-delete',
      handler: (row: PriceTierRow) => confirmTierDelete(row),
    },
  ],
};

/** 一页拉完：同步页码 */
function onTierLoaded(payload: { page: number }): void {
  tierCurrent.value = payload.page;
}

/** 重拉档位列表（不传 = 当前页） */
async function reloadTier(page: number = tierCurrent.value): Promise<void> {
  await tierListRef.value?.load(page);
}

// ==================== ③ 协议价新建 / 编辑 ====================

const agreedFormVisible = ref(false);
const agreedEditingId = ref<string | null>(null);
const agreedForm = reactive<AgreedFormDraft>(emptyAgreedForm(todayCst()));
const agreedFormError = ref('');
const agreedSubmitting = ref(false);

function openAgreedCreate(): void {
  agreedEditingId.value = null;
  agreedFormError.value = '';
  Object.assign(agreedForm, emptyAgreedForm(todayCst()));
  // 列表正在按某客户 / 物料筛，新建时就预填上 —— 少点两次
  if (agreedFilter.customer_id) {
    agreedForm.customer_id = agreedFilter.customer_id;
  }
  if (agreedFilter.material_id) {
    agreedForm.material_id = agreedFilter.material_id;
  }
  agreedFormVisible.value = true;
}

function openAgreedEdit(row: CustomerPriceRow): void {
  agreedEditingId.value = row.id;
  agreedFormError.value = '';
  Object.assign(agreedForm, agreedFormFrom(row));
  agreedFormVisible.value = true;
}

async function submitAgreedForm(): Promise<void> {
  const error = validateAgreedForm(agreedForm);
  if (error) {
    agreedFormError.value = error;
    return;
  }
  agreedFormError.value = '';
  agreedSubmitting.value = true;
  try {
    if (agreedEditingId.value) {
      const updated = await updateCustomerPrice(agreedEditingId.value, buildAgreedUpdatePayload(agreedForm));
      MessagePlugin.success(`已保存 ${materialText(updated)} 的协议价 ${priceText(updated.price)}`);
    } else {
      const created = await createCustomerPrice(buildAgreedCreatePayload(agreedForm));
      MessagePlugin.success(`已创建 ${customerText(created)} × ${materialText(created)} 协议价 ${priceText(created.price)}`);
    }
    agreedFormVisible.value = false;
    await reloadAgreed(agreedEditingId.value ? agreedCurrent.value : 1);
  } catch (submitError) {
    // 后端原文（唯一键冲突 / 价格必须 > 0 / 物料不存在）直接摆进弹窗，别只说「保存失败」
    agreedFormError.value = messageOf(submitError);
  } finally {
    agreedSubmitting.value = false;
  }
}

async function confirmAgreedDelete(row: CustomerPriceRow): Promise<void> {
  const confirmed = await confirmDialog(
    `删除 ${customerText(row)} × ${materialText(row)} 的协议价 ${priceText(row.price)}？`,
    '软删（行仍在库，价格历史可追溯；同一组键可以重新录入）',
  );
  if (!confirmed) {
    return;
  }
  try {
    await deleteCustomerPrice(row.id);
    MessagePlugin.success('已删除该协议价');
    await reloadAgreed();
  } catch (error) {
    MessagePlugin.error(`删除失败：${messageOf(error)}`);
  }
}

// ==================== ④ 档位新建 / 编辑 ====================

const tierFormVisible = ref(false);
const tierEditingId = ref<string | null>(null);
const tierForm = reactive<TierFormDraft>(emptyTierForm(todayCst()));
const tierFormError = ref('');
const tierSubmitting = ref(false);

function openTierCreate(): void {
  tierEditingId.value = null;
  tierFormError.value = '';
  Object.assign(tierForm, emptyTierForm(todayCst()));
  if (tierFilter.material_id) {
    tierForm.material_id = tierFilter.material_id;
  }
  if (tierFilter.customer_id) {
    tierForm.customer_id = tierFilter.customer_id;
  }
  tierFormVisible.value = true;
}

function openTierEdit(row: PriceTierRow): void {
  tierEditingId.value = row.id;
  tierFormError.value = '';
  Object.assign(tierForm, tierFormFrom(row));
  tierFormVisible.value = true;
}

async function submitTierForm(): Promise<void> {
  const error = validateTierForm(tierForm);
  if (error) {
    tierFormError.value = error;
    return;
  }
  tierFormError.value = '';
  tierSubmitting.value = true;
  try {
    if (tierEditingId.value) {
      const updated = await updatePriceTier(tierEditingId.value, buildTierUpdatePayload(tierForm));
      MessagePlugin.success(`已保存档位 ${tierRangeText(updated)} → ${priceText(updated.price)}`);
    } else {
      const created = await createPriceTier(buildTierCreatePayload(tierForm));
      MessagePlugin.success(`已创建档位 ${tierRangeText(created)} → ${priceText(created.price)}`);
    }
    tierFormVisible.value = false;
    await reloadTier(tierEditingId.value ? tierCurrent.value : 1);
  } catch (submitError) {
    tierFormError.value = messageOf(submitError);
  } finally {
    tierSubmitting.value = false;
  }
}

async function confirmTierDelete(row: PriceTierRow): Promise<void> {
  const confirmed = await confirmDialog(
    `删除 ${materialText(row)} 的 ${tierTypeLabel(row.tier_type)} 档位 ${tierRangeText(row)}？`,
    '软删（行仍在库，价格历史可追溯；同一组键可以重新录入）',
  );
  if (!confirmed) {
    return;
  }
  try {
    await deletePriceTier(row.id);
    MessagePlugin.success('已删除该档位');
    await reloadTier();
  } catch (error) {
    MessagePlugin.error(`删除失败：${messageOf(error)}`);
  }
}

/**
 * 删除确认（`DialogPlugin.confirm`：与本仓 `payable` / `receipt` 页同一写法，
 * 不用原生 `confirm` —— 原生弹窗会阻塞自动化与截图验证）。
 */
function confirmDialog(title: string, tip: string): Promise<boolean> {
  return new Promise<boolean>((resolve) => {
    const dialog = DialogPlugin.confirm({
      header: '确认删除',
      body: `${title}\n${tip}`,
      theme: 'warning',
      confirmBtn: '删除',
      cancelBtn: '取消',
      onConfirm: () => {
        dialog.destroy();
        resolve(true);
      },
      onClose: () => {
        dialog.destroy();
        resolve(false);
      },
      onCancel: () => {
        dialog.destroy();
        resolve(false);
      },
    });
  });
}

onMounted(() => {
  // 两个列表由各自的 ListPage 在 setup 里拉首屏（tab 内 v-if，切过去才拉）；这里只补主数据下拉
  void loadOptions();
});
</script>

<style scoped lang="less">
.price-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.page-head {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.page-head__title {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 16px;
  font-weight: 600;
}

.page-head__tip {
  font-size: 12px;
  color: var(--td-text-color-secondary);
  line-height: 1.6;
}

.page-tabs {
  margin-top: 4px;
}

.gate-wrap {
  display: inline-flex;
}

.form-alert {
  margin-bottom: 12px;
}

.mono {
  font-family: var(--td-font-family-mono, 'SFMono-Regular', Consolas, monospace);
}

.price-cell {
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.validity-cell {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.panel-empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 4px;
  padding: 24px 0;
}

.panel-empty__icon {
  font-size: 28px;
  color: var(--td-text-color-placeholder);
}

.panel-empty__title {
  margin: 0;
  font-size: 14px;
  color: var(--td-text-color-secondary);
}

.panel-empty__desc {
  margin: 0;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.price-form :deep(.t-input-number) {
  width: 100%;
}
</style>
