<template>
  <div class="price-page">
    <!-- ==================== ① 查询条件 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">取价查询</span>
          <span class="panel-head__meta">
            5 级确定性取价：客户协议价 → 客户专属阶梯 → 通用阶梯 → 物料标准售价 → 无价
          </span>
        </div>
      </div>

      <t-form label-align="top" class="form">
        <div class="form-row">
          <t-form-item label="客户（选填）">
            <t-select
              v-model="form.customerId"
              :options="customerOptions"
              placeholder="不选 = 跳过协议价与客户专属阶梯"
              filterable
              clearable
              data-testid="price-customer"
            />
          </t-form-item>
          <t-form-item label="物料" required-mark>
            <t-select
              v-model="form.materialId"
              :options="materialOptions"
              :loading="materialLoading"
              placeholder="输入编码 / 名称搜索"
              filterable
              clearable
              data-testid="price-material"
              @search="searchMaterials"
            />
          </t-form-item>
          <t-form-item label="数量" required-mark>
            <t-input-number
              v-model="form.qty"
              theme="normal"
              :min="0"
              :decimal-places="4"
              :step="1"
              placeholder="订购量（阶梯价按它命中）"
              data-testid="price-qty"
            />
          </t-form-item>
          <t-form-item label="取价日期">
            <t-date-picker
              v-model="form.date"
              value-type="YYYY-MM-DD"
              allow-input
              clearable
              placeholder="留空 = 今天（CST）"
              data-testid="price-date"
            />
          </t-form-item>
          <t-form-item label=" ">
            <t-button
              theme="primary"
              :loading="resolving"
              data-testid="btn-price-resolve"
              @click="resolve"
            >
              取价
            </t-button>
          </t-form-item>
        </div>
      </t-form>

      <t-alert v-if="errorText" theme="error" :message="errorText" data-testid="price-error" />
    </t-card>

    <!-- ==================== ② 结果 ==================== -->
    <t-card :bordered="false" class="result-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">取价结果</span>
          <t-tag
            v-if="result"
            size="small"
            variant="light"
            :theme="result.source === 'NONE' ? 'warning' : 'success'"
            data-testid="price-source-badge"
          >
            {{ priceSourceLabel(result.source) }}
          </t-tag>
        </div>
        <span class="panel-head__tip" data-testid="price-query-desc">{{ queryDesc }}</span>
      </div>

      <div v-if="!result" class="empty" data-testid="price-empty">
        填物料 + 数量后点「取价」。这个页只是**查询**：不写库、不改价、不下单。
      </div>

      <template v-else>
        <t-alert
          v-if="result.source === 'NONE'"
          theme="warning"
          message="五级取价都没有命中：这家客户这个物料在这个数量/日期下没有可用价，销售需手填单价并留依据。"
          data-testid="price-none-alert"
        />

        <div class="result-grid">
          <div class="result-field result-field--primary">
            <span class="result-field__label">不含税单价（填单用这个）</span>
            <span class="result-field__value" data-testid="price-excl">
              {{ result.price_excl ? formatMoney(result.price_excl) : '—' }}
            </span>
            <span class="result-field__hint">
              销售订单行 / 报价单的单价填的是不含税价；含税价还要乘 (1 + 税率)
            </span>
          </div>
          <div class="result-field">
            <span class="result-field__label">命中原值</span>
            <span class="result-field__value" data-testid="price-raw">
              {{ result.price ? formatMoney(result.price) : '—' }}
            </span>
            <span class="result-field__hint">
              {{ result.price ? (result.tax_inclusive ? '含税价' : '不含税价') : '无价' }}
            </span>
          </div>
          <div class="result-field">
            <span class="result-field__label">税率</span>
            <span class="result-field__value" data-testid="price-tax-rate">
              {{ result.tax_rate ? `${trimDecimal(result.tax_rate)}%` : '—' }}
            </span>
            <span class="result-field__hint">来自物料档案的税率（百分数）</span>
          </div>
          <div class="result-field">
            <span class="result-field__label">命中来源</span>
            <span class="result-field__value" data-testid="price-source">
              {{ priceSourceLabel(result.source) }}
            </span>
            <span class="result-field__hint mono">{{ result.source_id || '—' }}</span>
          </div>
        </div>

        <div class="section-title">为什么是这个价</div>
        <table class="level-table" data-testid="price-levels">
          <thead>
            <tr>
              <th class="level-table__idx">#</th>
              <th>层级</th>
              <th>说明</th>
              <th class="level-table__state">本次</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="(level, index) in PRICE_SOURCES" :key="level.value">
              <td class="level-table__idx">{{ index + 1 }}</td>
              <td>{{ level.label }}</td>
              <td class="level-table__hint">{{ level.hint }}</td>
              <td class="level-table__state">
                <t-tag v-if="level.value === result.source" size="small" theme="success" variant="light">
                  命中
                </t-tag>
                <span v-else-if="isBeforeHit(level.value, result.source)" class="muted">未配置 / 不适用</span>
                <span v-else class="muted">未走到</span>
              </td>
            </tr>
          </tbody>
        </table>
      </template>
    </t-card>
  </div>
</template>

<script setup lang="ts">
/**
 * 取价查询页（NEAR-FIX-1 / 卡 t_67dcb9cb）。
 *
 * # 这个页面为什么是「补页面」而不是「删菜单」
 *
 * V0307 建的那行菜单（`…3071`，`auth_code = business:erp:price:resolve`）是
 * **取价接口权限码的唯一承载行**：
 *
 * * `scripts/lint/perm_menu_gate.py`（check.sh 闸⑥）要求每个 `#[nik::permission]`
 *   码在 `sys_menu.auth_code` 都有承载行 —— 删掉这行，门槛变红；
 * * 而且**除 SUPER_ADMIN 外任何人都调不动** `GET /business/erp/price/resolve`
 *   （不报错、不打日志，就是 403）；
 * * 后端接口本身是完整的（SO-33：5 级确定性取价），业务上也是真实诉求（销售报价）；
 * * V0307 头注释原文就写着「等价格页面卡落地后一条 UPDATE 收尾」，并给了
 *   `path = '/business/erp/price'` / `component = 'business/erp/price/index'` 的收尾语句。
 *
 * ⇒ 本页就是那句「价格页面卡」：只读查询，不写库、不改价、不下单。
 *
 * # 一个必须记住的口径
 *
 * **填单只准用 `price_excl`（不含税）**：`price` 对协议价 / 阶梯价是含税价，
 * 拿去填销售订单行会虚高一个税率（120 vs 106.19）。页面把 `price_excl` 放第一格并加粗，
 * 就是为了让销售员不会复制错那一列。
 */
import { computed, onMounted, reactive, ref } from 'vue';

import { fetchCustomerPage, fetchMaterialPage, type Customer, type MaterialRow } from '@/api/business/erp';
import {
  PRICE_SOURCES,
  priceSourceLabel,
  resolvePrice,
  type PriceResolveResult,
  type PriceSource,
} from '@/api/business/erp/price';
import { ApiError } from '@/api/request';

import { formatMoney, todayString } from '../shared/money';

const form = reactive<{
  customerId: string;
  materialId: string;
  qty: number;
  date: string;
}>({
  customerId: '',
  materialId: '',
  qty: 1,
  date: todayString(),
});

const customers = ref<Customer[]>([]);
const materials = ref<MaterialRow[]>([]);
const materialLoading = ref(false);
const resolving = ref(false);
const errorText = ref('');
const result = ref<PriceResolveResult | null>(null);

const customerOptions = computed(() =>
  customers.value.map((item) => ({ label: `${item.name}（${item.code}）`, value: item.id })),
);

const materialOptions = computed(() =>
  materials.value.map((item) => ({
    label: `${item.material_code} · ${item.material_name}${item.spec ? ` / ${item.spec}` : ''}`,
    value: item.id,
  })),
);

/** 结果区右上角那句「查的是什么」—— 让截图能自证（E2E 证据用） */
const queryDesc = computed(() => {
  if (!result.value) {
    return '';
  }
  const material = materials.value.find((item) => item.id === form.materialId);
  const customer = customers.value.find((item) => item.id === form.customerId);
  const parts = [
    material ? `${material.material_code} ${material.material_name}` : '（物料）',
    `数量 ${form.qty}`,
    customer ? `客户 ${customer.name}` : '未指定客户',
    `基准日 ${form.date || todayString()}`,
  ];
  return parts.join(' · ');
});

/** 列表里排在前面的层级说明「它本来该优先，但没命中」 */
function isBeforeHit(level: PriceSource, hit: string): boolean {
  const order = PRICE_SOURCES.map((item) => item.value);
  const levelIndex = order.indexOf(level);
  const hitIndex = order.indexOf(hit as PriceSource);
  if (hitIndex < 0) {
    return false;
  }
  return levelIndex >= 0 && levelIndex < hitIndex;
}

/** `"13.0000"` → `"13"`（税率不需要 4 位小数） */
function trimDecimal(raw: string): string {
  const num = Number(raw);
  if (!Number.isFinite(num)) {
    return raw;
  }
  return String(Number(num.toFixed(4)));
}

function messageOf(error: unknown): string {
  if (error instanceof ApiError) return error.message;
  return error instanceof Error ? error.message : String(error);
}

async function loadCustomers(): Promise<void> {
  customers.value = await fetchCustomerPage({ page: 1, page_size: 200 })
    .then((data) => data.list ?? [])
    .catch(() => [] as Customer[]);
}

async function searchMaterials(keyword: string): Promise<void> {
  materialLoading.value = true;
  try {
    const data = await fetchMaterialPage({ keyword: keyword || undefined, page: 1, page_size: 30 });
    materials.value = data.list ?? [];
  } catch {
    materials.value = [];
  } finally {
    materialLoading.value = false;
  }
}

async function resolve(): Promise<void> {
  errorText.value = '';
  result.value = null;
  if (!form.materialId) {
    errorText.value = '请选择物料（物料必填：取价是按物料算的）';
    return;
  }
  if (!Number.isFinite(form.qty) || form.qty < 0) {
    errorText.value = '数量必须是 ≥ 0 的数字';
    return;
  }
  resolving.value = true;
  try {
    result.value = await resolvePrice({
      customer_id: form.customerId || undefined,
      material_id: form.materialId,
      qty: form.qty,
      date: form.date || undefined,
    });
  } catch (error) {
    errorText.value = messageOf(error);
  } finally {
    resolving.value = false;
  }
}

onMounted(async () => {
  await Promise.all([loadCustomers(), searchMaterials('')]);
});
</script>

<style scoped>
.price-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.tool-card,
.result-card {
  background: var(--td-bg-color-container);
  border-radius: var(--td-radius-medium);
}

.panel-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 10px;
}

.panel-head__main {
  display: flex;
  align-items: baseline;
  gap: 8px;
}

.panel-head__title {
  font-size: 15px;
  font-weight: 600;
}

.panel-head__meta,
.panel-head__tip {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.form-row {
  display: flex;
  gap: 14px;
  flex-wrap: wrap;
  align-items: flex-end;
}

.form-row :deep(.t-form__item) {
  margin-bottom: 0;
  min-width: 180px;
}

.result-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
  gap: 12px;
  margin: 12px 0 4px;
}

.result-field {
  display: flex;
  flex-direction: column;
  gap: 3px;
  padding: 12px;
  border: 1px solid var(--td-component-stroke);
  border-radius: var(--td-radius-medium);
}

.result-field--primary {
  border-color: var(--td-brand-color);
  background: var(--td-brand-color-light);
}

.result-field__label {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.result-field__value {
  font-size: 20px;
  font-weight: 600;
  font-variant-numeric: tabular-nums;
}

.result-field__hint {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.section-title {
  margin: 16px 0 6px;
  font-size: 14px;
  font-weight: 600;
}

.level-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 12px;
}

.level-table th,
.level-table td {
  padding: 6px 8px;
  border-bottom: 1px solid var(--td-component-stroke);
  text-align: left;
}

.level-table__idx {
  width: 36px;
  color: var(--td-text-color-placeholder);
}

.level-table__hint {
  color: var(--td-text-color-secondary);
}

.level-table__state {
  width: 130px;
}

.empty {
  padding: 18px 8px;
  color: var(--td-text-color-secondary);
  font-size: 13px;
}

.mono {
  font-family: var(--td-font-family-mono, monospace);
}

.muted {
  color: var(--td-text-color-placeholder);
}
</style>
