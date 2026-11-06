<template>
  <div class="price-page">
    <!-- ==================== 页面头 ==================== -->
    <t-card :bordered="false" class="filter-card">
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

      <t-tabs v-model="activeTab" class="price-tabs" data-testid="price-tabs">
        <!-- ==================== ① 客户协议价 ==================== -->
        <t-tab-panel value="agreed" label="客户协议价">
          <div class="filter-row">
            <t-select
              v-model="agreedFilter.customer_id"
              class="filter-row__customer"
              :options="customerOptions"
              placeholder="全部客户"
              clearable
              filterable
              data-testid="price-filter-customer"
            />
            <t-select
              v-model="agreedFilter.material_id"
              class="filter-row__material"
              :options="materialOptions"
              placeholder="全部物料"
              clearable
              filterable
              data-testid="price-filter-material"
            />
            <t-select
              v-model="agreedFilter.status"
              class="filter-row__status"
              :options="statusOptions"
              placeholder="全部状态"
              clearable
              data-testid="price-filter-status"
            />
            <t-input
              v-model="agreedFilter.keyword"
              class="filter-row__keyword"
              placeholder="客户 / 物料 编码或名称"
              clearable
              data-testid="price-filter-keyword"
              @enter="handleAgreedSearch"
            >
              <template #prefix-icon><t-icon name="search" /></template>
            </t-input>

            <t-button theme="primary" data-testid="btn-price-search" @click="handleAgreedSearch">查询</t-button>
            <t-button variant="outline" data-testid="btn-price-reset" @click="handleAgreedReset">重置</t-button>
            <span class="filter-row__spacer" />
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
          </div>

          <t-alert v-if="agreedError" theme="error" :message="agreedError" class="page-alert" data-testid="price-agreed-error" />

          <t-table
            row-key="id"
            size="small"
            hover
            :data="agreedRows"
            :columns="agreedColumns"
            :loading="agreedLoading"
            :empty="agreedEmptyText"
            data-testid="price-agreed-table"
          >
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
            <template #op="{ row }">
              <div class="op-row">
                <t-link
                  v-privilege="[PERM.agreedUpdate]"
                  theme="primary"
                  data-testid="btn-price-agreed-edit"
                  @click="openAgreedEdit(row as CustomerPriceRow)"
                >
                  编辑
                </t-link>
                <t-link
                  v-privilege="[PERM.agreedDelete]"
                  theme="danger"
                  data-testid="btn-price-agreed-delete"
                  @click="confirmAgreedDelete(row as CustomerPriceRow)"
                >
                  删除
                </t-link>
              </div>
            </template>
          </t-table>

          <div class="pager-row">
            <span class="pager-row__total">共 {{ agreedTotal }} 条</span>
            <t-pagination
              :current="agreedPage"
              :page-size="PRICE_PAGE_SIZE"
              :total="agreedTotal"
              :show-page-size="false"
              show-jumper
              data-testid="price-agreed-pager"
              @current-change="handleAgreedPageChange"
            />
          </div>
        </t-tab-panel>

        <!-- ==================== ② 阶梯价 / 促销价 ==================== -->
        <t-tab-panel value="tier" label="阶梯价 / 促销价">
          <div class="filter-row">
            <t-select
              v-model="tierFilter.material_id"
              class="filter-row__material"
              :options="materialOptions"
              placeholder="全部物料"
              clearable
              filterable
              data-testid="price-tier-filter-material"
            />
            <t-select
              v-model="tierFilter.customer_id"
              class="filter-row__customer"
              :options="customerOptions"
              placeholder="适用客户（通用档位不选）"
              clearable
              filterable
              data-testid="price-tier-filter-customer"
            />
            <t-select
              v-model="tierFilter.tier_type"
              class="filter-row__status"
              :options="tierTypeOptions"
              placeholder="全部类型"
              clearable
              data-testid="price-tier-filter-type"
            />
            <t-select
              v-model="tierFilter.status"
              class="filter-row__status"
              :options="statusOptions"
              placeholder="全部状态"
              clearable
              data-testid="price-tier-filter-status"
            />
            <t-input
              v-model="tierFilter.keyword"
              class="filter-row__keyword"
              placeholder="物料 / 客户 编码或名称"
              clearable
              data-testid="price-tier-filter-keyword"
              @enter="handleTierSearch"
            >
              <template #prefix-icon><t-icon name="search" /></template>
            </t-input>

            <t-button theme="primary" data-testid="btn-price-tier-search" @click="handleTierSearch">查询</t-button>
            <t-button variant="outline" data-testid="btn-price-tier-reset" @click="handleTierReset">重置</t-button>
            <span class="filter-row__spacer" />
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
          </div>

          <t-alert v-if="tierError" theme="error" :message="tierError" class="page-alert" data-testid="price-tier-error" />

          <t-table
            row-key="id"
            size="small"
            hover
            :data="tierRows"
            :columns="tierColumns"
            :loading="tierLoading"
            :empty="tierEmptyText"
            data-testid="price-tier-table"
          >
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
            <template #op="{ row }">
              <div class="op-row">
                <t-link
                  v-privilege="[PERM.tierUpdate]"
                  theme="primary"
                  data-testid="btn-price-tier-edit"
                  @click="openTierEdit(row as PriceTierRow)"
                >
                  编辑
                </t-link>
                <t-link
                  v-privilege="[PERM.tierDelete]"
                  theme="danger"
                  data-testid="btn-price-tier-delete"
                  @click="confirmTierDelete(row as PriceTierRow)"
                >
                  删除
                </t-link>
              </div>
            </template>
          </t-table>

          <div class="pager-row">
            <span class="pager-row__total">共 {{ tierTotal }} 条</span>
            <t-pagination
              :current="tierPage"
              :page-size="PRICE_PAGE_SIZE"
              :total="tierTotal"
              :show-page-size="false"
              show-jumper
              data-testid="price-tier-pager"
              @current-change="handleTierPageChange"
            />
          </div>
        </t-tab-panel>
      </t-tabs>
    </t-card>

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
 * # 权限
 *
 * 三个权限码各自对应一个按钮；`v-privilege` 只隐藏**按钮**，接口侧后端还会再判一次
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
  isAgreedFiltered,
  isTierFiltered,
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

// ==================== ① 协议价列表 ====================

const agreedFilter = reactive<CustomerPriceFilter>(emptyAgreedFilter());
const agreedRows = ref<CustomerPriceRow[]>([]);
const agreedTotal = ref(0);
const agreedPage = ref(1);
const agreedLoading = ref(false);
const agreedError = ref('');

const agreedEmptyText = computed(() => {
  if (agreedLoading.value) {
    return '加载中…';
  }
  if (isAgreedFiltered(agreedFilter)) {
    return '没有符合条件的协议价（换个客户 / 物料试试，或点「重置」）';
  }
  return '还没有协议价 —— 点右上角「新建协议价」给客户单独定价';
});

async function loadAgreed(page: number = agreedPage.value): Promise<void> {
  agreedLoading.value = true;
  agreedError.value = '';
  try {
    const data = await fetchCustomerPricePage({ ...agreedFilter, page, page_size: PRICE_PAGE_SIZE });
    agreedRows.value = data.list ?? [];
    agreedTotal.value = data.total ?? 0;
    agreedPage.value = data.page ?? page;
    const lastPage = Math.max(1, Math.ceil(agreedTotal.value / PRICE_PAGE_SIZE));
    if (agreedPage.value > lastPage) {
      agreedPage.value = lastPage;
      await loadAgreed(lastPage);
    }
  } catch (error) {
    agreedRows.value = [];
    agreedTotal.value = 0;
    agreedError.value = `协议价加载失败：${messageOf(error)}`;
  } finally {
    agreedLoading.value = false;
  }
}

function handleAgreedSearch(): void {
  agreedPage.value = 1;
  void loadAgreed(1);
}

function handleAgreedReset(): void {
  Object.assign(agreedFilter, emptyAgreedFilter());
  agreedPage.value = 1;
  void loadAgreed(1);
}

function handleAgreedPageChange(current: number): void {
  agreedPage.value = current;
  void loadAgreed(current);
}

// ==================== ② 档位列表 ====================

const tierFilter = reactive<PriceTierFilter>(emptyTierFilter());
const tierRows = ref<PriceTierRow[]>([]);
const tierTotal = ref(0);
const tierPage = ref(1);
const tierLoading = ref(false);
const tierError = ref('');

const tierEmptyText = computed(() => {
  if (tierLoading.value) {
    return '加载中…';
  }
  if (isTierFiltered(tierFilter)) {
    return '没有符合条件的档位（换个物料试试，或点「重置」）';
  }
  return '还没有阶梯价 —— 同一物料可以按数量维护多档，区间首尾相接';
});

async function loadTier(page: number = tierPage.value): Promise<void> {
  tierLoading.value = true;
  tierError.value = '';
  try {
    const data = await fetchPriceTierPage({ ...tierFilter, page, page_size: PRICE_PAGE_SIZE });
    tierRows.value = data.list ?? [];
    tierTotal.value = data.total ?? 0;
    tierPage.value = data.page ?? page;
    const lastPage = Math.max(1, Math.ceil(tierTotal.value / PRICE_PAGE_SIZE));
    if (tierPage.value > lastPage) {
      tierPage.value = lastPage;
      await loadTier(lastPage);
    }
  } catch (error) {
    tierRows.value = [];
    tierTotal.value = 0;
    tierError.value = `阶梯价加载失败：${messageOf(error)}`;
  } finally {
    tierLoading.value = false;
  }
}

function handleTierSearch(): void {
  tierPage.value = 1;
  void loadTier(1);
}

function handleTierReset(): void {
  Object.assign(tierFilter, emptyTierFilter());
  tierPage.value = 1;
  void loadTier(1);
}

function handleTierPageChange(current: number): void {
  tierPage.value = current;
  void loadTier(current);
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
    await loadAgreed(agreedEditingId.value ? agreedPage.value : 1);
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
    await loadAgreed(agreedPage.value);
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
    await loadTier(tierEditingId.value ? tierPage.value : 1);
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
    await loadTier(tierPage.value);
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
  void loadOptions();
  // 两个列表都先拉：Tab 切过去时不该再等一次网络（数据量小，服务端分页 20 条）
  void loadAgreed(1);
  void loadTier(1);
});
</script>

<style scoped>
.price-page {
  padding: 0;
}

.filter-card {
  padding: 4px 16px 12px;
}

.page-head {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 12px 0 4px;
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

.price-tabs {
  margin-top: 4px;
}

.filter-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  padding: 8px 0 12px;
}

.filter-row__customer {
  width: 220px;
}

.filter-row__material {
  width: 260px;
}

.filter-row__status {
  width: 150px;
}

.filter-row__keyword {
  width: 220px;
}

.filter-row__spacer {
  flex: 1;
}

.gate-wrap {
  display: inline-flex;
}

.page-alert,
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

.op-row {
  display: flex;
  gap: 12px;
  align-items: center;
}

.pager-row {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 12px 0 4px;
}

.pager-row__total {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.pager-row :deep(.t-pagination) {
  margin-left: auto;
}

.price-form :deep(.t-input-number) {
  width: 100%;
}
</style>
