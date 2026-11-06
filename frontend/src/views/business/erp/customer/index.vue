<template>
  <div class="customer-page">
    <!-- 筛选行：编码 / 名称 / 状态 / 查询 / 重置 / 刷新 / 新建 -->
    <t-card :bordered="false" class="filter-card">
      <div class="filter-row">
        <t-input
          v-model="filter.code"
          class="filter-row__keyword"
          placeholder="客户编码（模糊）"
          clearable
          data-testid="cus-code-filter"
          @enter="handleSearch"
        >
          <template #prefix-icon><t-icon name="search" /></template>
        </t-input>

        <t-input
          v-model="filter.name"
          class="filter-row__keyword"
          placeholder="客户名称（模糊）"
          clearable
          data-testid="cus-name-filter"
          @enter="handleSearch"
        >
          <template #prefix-icon><t-icon name="search" /></template>
        </t-input>

        <t-select
          v-model="filter.status"
          class="filter-row__select"
          :options="statusOptions"
          placeholder="全部状态"
          clearable
          data-testid="cus-status-filter"
        />

        <t-button theme="primary" data-testid="btn-cus-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-cus-reset" @click="handleReset">重置</t-button>
        <t-button variant="outline" data-testid="btn-cus-refresh" @click="handleRefresh">刷新</t-button>

        <span class="filter-row__spacer" />

        <t-tooltip
          :content="canWrite ? '新建一个客户档案' : '当前账号没有 business:erp:customer:create 权限'"
          placement="top-right"
        >
          <span class="gate-wrap">
            <t-button
              theme="primary"
              data-testid="btn-cus-new"
              :disabled="!canWrite"
              @click="openCreate"
            >
              <template #icon><t-icon name="add" /></template>
              新建客户
            </t-button>
          </span>
        </t-tooltip>
      </div>
    </t-card>

    <!-- 列表 -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">客户档案</span>
          <span class="panel-head__meta" data-testid="cus-meta">共 {{ total }} 条</span>
        </div>
        <span class="panel-head__hint">冻结只影响开新单，历史单据与引用不受影响</span>
      </div>

      <t-alert v-if="loadError" theme="error" :message="loadError" class="list-alert">
        <template #operation>
          <t-button size="small" variant="outline" @click="loadCustomers()">重试</t-button>
        </template>
      </t-alert>

      <t-table
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        :hover="true"
        data-testid="customer-table"
        @page-change="handlePageChange"
      >
        <template #code="{ row }">
          <span class="mono">{{ row.code }}</span>
        </template>

        <template #name="{ row }">
          <span :title="row.name">{{ row.name }}</span>
        </template>

        <template #contact_name="{ row }">
          <span :class="{ muted: !row.contact_name && !row.contact_phone }">{{ contactLabel(row) }}</span>
        </template>

        <template #credit_limit="{ row }">
          <span class="mono" :class="{ muted: Number(row.credit_limit) === 0 }">
            {{ formatCreditLimit(row.credit_limit) }}
          </span>
        </template>

        <template #status="{ row }">
          <t-tag
            size="small"
            :theme="customerStatusTheme(row.status)"
            variant="light"
            data-testid="cus-status-tag"
          >
            {{ customerStatusLabel(row.status) }}
          </t-tag>
        </template>

        <template #op="{ row }">
          <div class="op-row">
            <t-link
              v-privilege="[PERM.update]"
              theme="primary"
              data-testid="btn-cus-edit"
              @click="openEdit(row)"
            >
              编辑
            </t-link>
            <t-link
              v-privilege="[PERM.update]"
              :theme="row.status === 'FROZEN' ? 'success' : 'warning'"
              :data-testid="row.status === 'FROZEN' ? 'btn-cus-activate' : 'btn-cus-freeze'"
              @click="toggleStatus(row)"
            >
              {{ customerToggleLabel(row.status) }}
            </t-link>
            <t-link
              v-privilege="[PERM.delete]"
              theme="danger"
              data-testid="btn-cus-delete"
              @click="confirmRemove(row)"
            >
              删除
            </t-link>
          </div>
        </template>

        <template #empty>
          <div class="panel-empty" data-testid="customer-empty">
            <t-icon class="panel-empty__icon" name="usergroup" />
            <p class="panel-empty__title">{{ isFiltered ? '没有匹配的客户' : '还没有客户档案' }}</p>
            <p class="panel-empty__desc">
              {{
                isFiltered
                  ? '换个编码 / 名称 / 状态试试，或点「重置」看全部'
                  : canWrite
                    ? '点右上角「新建客户」建第一个；信用额度填 0 表示不控额度'
                    : '当前账号没有新建权限，请联系管理员补客户档案'
              }}
            </p>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- 新建 / 编辑弹窗 -->
    <t-dialog
      v-model:visible="dialogVisible"
      :header="isEdit ? `编辑客户 · ${form.code}` : '新建客户'"
      :confirm-btn="{ content: isEdit ? '保存' : '创建', loading: saving }"
      :close-on-overlay-click="false"
      width="720px"
      data-testid="cus-dialog"
      @confirm="handleConfirm"
    >
      <t-alert v-if="submitError" theme="error" :message="submitError" class="dlg-alert" />

      <t-form :data="form" label-width="110px">
        <t-form-item
          label="客户编码"
          name="code"
          :status="formErrors.code ? 'error' : undefined"
          :tips="formErrors.code"
        >
          <t-input
            v-model="form.code"
            :disabled="isEdit"
            :maxlength="CUSTOMER_LIMITS.code"
            placeholder="如 CUS-0002（未删除内唯一）"
            data-testid="cus-code"
          />
          <template v-if="isEdit" #help>
            <span class="dlg-help">编码是客户身份，不支持修改（已软删的编码可以再建）</span>
          </template>
        </t-form-item>

        <t-form-item
          label="客户名称"
          name="name"
          :status="formErrors.name ? 'error' : undefined"
          :tips="formErrors.name"
        >
          <t-input
            v-model="form.name"
            :maxlength="CUSTOMER_LIMITS.name"
            placeholder="营业执照全称 / 客户全称"
            data-testid="cus-name"
          />
        </t-form-item>

        <t-form-item label="联系人" :status="formErrors.contact_name ? 'error' : undefined" :tips="formErrors.contact_name">
          <t-input
            v-model="form.contact_name"
            :maxlength="CUSTOMER_LIMITS.contact_name"
            placeholder="选填，下单时默认带出"
            data-testid="cus-contact-name"
          />
        </t-form-item>

        <t-form-item label="联系电话" :status="formErrors.contact_phone ? 'error' : undefined" :tips="formErrors.contact_phone">
          <t-input
            v-model="form.contact_phone"
            :maxlength="CUSTOMER_LIMITS.contact_phone"
            placeholder="选填"
            data-testid="cus-contact-phone"
          />
        </t-form-item>

        <t-form-item label="税号" :status="formErrors.tax_no ? 'error' : undefined" :tips="formErrors.tax_no">
          <t-input
            v-model="form.tax_no"
            :maxlength="CUSTOMER_LIMITS.tax_no"
            placeholder="纳税人识别号，选填"
            data-testid="cus-tax-no"
          />
        </t-form-item>

        <t-form-item label="地址" :status="formErrors.address ? 'error' : undefined" :tips="formErrors.address">
          <t-input
            v-model="form.address"
            :maxlength="CUSTOMER_LIMITS.address"
            placeholder="选填"
            data-testid="cus-address"
          />
        </t-form-item>

        <t-form-item
          label="信用额度"
          name="credit_limit"
          :status="formErrors.credit_limit ? 'error' : undefined"
          :tips="formErrors.credit_limit"
        >
          <div class="dlg-inline">
            <t-input-number
              v-model="form.credit_limit"
              :min="0"
              :step="1000"
              :decimal-places="2"
              theme="normal"
              data-testid="cus-credit-limit"
            />
            <span class="dlg-help">0 = 不控额度；超额下单由销售单校验拦截</span>
          </div>
        </t-form-item>

        <t-form-item label="状态">
          <t-radio-group v-model="form.status" data-testid="cus-status">
            <t-radio value="ACTIVE">正常</t-radio>
            <t-radio value="FROZEN">冻结</t-radio>
          </t-radio-group>
          <template #help>
            <span class="dlg-help">冻结后不能开新单，历史单据与引用不受影响；列表上也能一键切换</span>
          </template>
        </t-form-item>

        <t-form-item label="备注" :status="formErrors.remark ? 'error' : undefined" :tips="formErrors.remark">
          <t-textarea
            v-model="form.remark"
            :maxlength="CUSTOMER_LIMITS.remark"
            :autosize="{ minRows: 2, maxRows: 4 }"
            placeholder="选填"
            data-testid="cus-remark"
          />
        </t-form-item>
      </t-form>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 客户档案页（销售链 · SO-03）——标准主数据 CRUD：列表 + 新建 / 编辑 + 状态切换 + 软删。
 *
 * # 后端接口（SO-02 已交付，`customer_handler.rs`）
 *
 * | 方法 | 路径 | 权限码 |
 * |------|------|--------|
 * | GET | `/api/business/erp/customer/page?code=&name=&status=&page=&page_size=` | `:list` |
 * | GET | `/api/business/erp/customer/{id}` | `:list` |
 * | POST | `/api/business/erp/customer` | `:create` |
 * | PUT | `/api/business/erp/customer/{id}`（部分更新） | `:update` |
 * | DELETE | `/api/business/erp/customer/{id}`（软删） | `:delete` |
 *
 * # 为什么没有「查看」抽屉
 *
 * 客户只有 9 个业务字段，编辑弹窗已经全展示；再叠一个抽屉只是多一次点击
 * （供应商页有抽屉是因为它有 16 列，列表装不下）。少一个交互面，少一处漂移。
 *
 * # 状态切换
 *
 * `ACTIVE ↔ FROZEN` 走 `PUT` 的部分更新，请求体只有 `{status}`（`buildCustomerStatusPayload`）。
 * 不整表单回写：那会拿列表行（可能已被别人改过）的旧值覆盖别人的修改。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin } from 'tdesign-vue-next';
import { useUserStore } from '@/stores/user';
import {
  createCustomer,
  deleteCustomer,
  fetchCustomerPage,
  updateCustomer,
  type Customer,
  type UpdateCustomerReq,
} from '@/api/business/erp';
import { ApiError } from '@/api/request';
import {
  CUSTOMER_COLUMNS,
  CUSTOMER_LIMITS,
  CUSTOMER_PAGE_SIZE,
  CUSTOMER_STATUS_OPTIONS,
  buildCustomerCreatePayload,
  buildCustomerQuery,
  buildCustomerStatusPayload,
  buildCustomerUpdatePayload,
  contactLabel,
  customerFormFromRow,
  customerStatusLabel,
  customerStatusTheme,
  customerToggleLabel,
  emptyCustomerFilter,
  emptyCustomerForm,
  formatCreditLimit,
  hasCustomerFieldError,
  isCustomerFiltered,
  isCustomerUpdateEmpty,
  nextCustomerStatus,
  validateCustomerForm,
  type CustomerFieldErrors,
  type CustomerFilterState,
  type CustomerFormState,
} from './customer-query';

/** 本页权限码（与 `V0261__sales_customer_permissions.up.sql` 的 auth_code 逐字一致） */
const PERM = {
  /** 列表 / 详情（页面入口） */
  list: 'business:erp:customer:list',
  /** 新建 */
  create: 'business:erp:customer:create',
  /** 编辑 / 状态切换 */
  update: 'business:erp:customer:update',
  /** 软删 */
  delete: 'business:erp:customer:delete',
} as const;

/** 表格列（colKey 与后端出参同名） */
const columns = CUSTOMER_COLUMNS;

/** 状态下拉（`''` = 全部，靠 `clearable`） */
const statusOptions = CUSTOMER_STATUS_OPTIONS.map((item) => ({ ...item }));

/** 新建按钮：无 `:create` 权限时置灰 + tooltip 说明 */
const userStore = useUserStore();
const canWrite = computed(() => userStore.hasPermission(PERM.create));

// ==================== 列表状态 ====================

/** 筛选行 */
const filter = reactive<CustomerFilterState>(emptyCustomerFilter());
/** 当前页数据 */
const rows = ref<Customer[]>([]);
/** 总条数 */
const total = ref(0);
/** 当前页码 */
const current = ref(1);
/** 每页条数 */
const pageSize = CUSTOMER_PAGE_SIZE;
/** 加载中 */
const loading = ref(false);
/** 加载错误 */
const loadError = ref('');

/** 是否筛选态 */
const isFiltered = computed(() => isCustomerFiltered(filter));

/** 分页器（服务端分页） */
const pagination = computed(() => ({
  current: current.value,
  pageSize,
  total: total.value,
  showJumper: true,
  showPageSize: false,
}));

/** 错误文案 */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

/** 拉列表（服务端分页） */
async function loadCustomers(page: number = current.value): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const data = await fetchCustomerPage(buildCustomerQuery(filter, page, pageSize));
    rows.value = data.list ?? [];
    total.value = data.total ?? 0;
    current.value = data.page ?? page;
    // 删掉本页最后一条后，服务端会把 page 夹回合法页码；前端同步显示，避免停在空页
    const lastPage = Math.max(1, Math.ceil(total.value / pageSize));
    if (current.value > lastPage) {
      current.value = lastPage;
      return loadCustomers(lastPage);
    }
  } catch (error) {
    rows.value = [];
    total.value = 0;
    loadError.value = `客户加载失败：${messageOf(error)}`;
  } finally {
    loading.value = false;
  }
}

/** 查询（回第 1 页） */
function handleSearch(): void {
  current.value = 1;
  void loadCustomers(1);
}

/** 重置 */
function handleReset(): void {
  Object.assign(filter, emptyCustomerFilter());
  current.value = 1;
  void loadCustomers(1);
}

/** 刷新 */
function handleRefresh(): void {
  void loadCustomers(current.value);
}

/** 切页 */
function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  current.value = pageInfo.current;
  void loadCustomers(pageInfo.current);
}

// ==================== 新建 / 编辑 ====================

/** 弹窗可见 */
const dialogVisible = ref(false);
/** 编辑对象；`null` = 新建 */
const editing = ref<Customer | null>(null);
/** 表单 */
const form = reactive<CustomerFormState>(emptyCustomerForm());
/** 字段错误 */
const formErrors = ref<CustomerFieldErrors>({});
/** 提交错误（后端原文，如编码重复） */
const submitError = ref('');
/** 提交中 */
const saving = ref(false);

const isEdit = computed(() => editing.value !== null);

/** 打开新建 */
function openCreate(): void {
  editing.value = null;
  Object.assign(form, emptyCustomerForm());
  formErrors.value = {};
  submitError.value = '';
  dialogVisible.value = true;
}

/** 打开编辑（回填） */
function openEdit(row: Customer): void {
  editing.value = row;
  Object.assign(form, customerFormFromRow(row));
  formErrors.value = {};
  submitError.value = '';
  dialogVisible.value = true;
}

/** 提交（新建 / 编辑） */
async function handleConfirm(): Promise<void> {
  const errors = validateCustomerForm(form, isEdit.value);
  formErrors.value = errors;
  if (hasCustomerFieldError(errors)) {
    return;
  }
  saving.value = true;
  submitError.value = '';
  try {
    if (editing.value) {
      const payload = buildCustomerUpdatePayload(form, editing.value);
      if (isCustomerUpdateEmpty(payload)) {
        // 一个字都没改：别发空 PUT，也别弹「已保存」的假成功
        MessagePlugin.info('没有需要保存的修改');
        dialogVisible.value = false;
        return;
      }
      await updateCustomer(editing.value.id, payload);
      MessagePlugin.success('客户已保存');
    } else {
      await createCustomer(buildCustomerCreatePayload(form));
      MessagePlugin.success('客户已创建');
    }
    dialogVisible.value = false;
    await loadCustomers(editing.value ? current.value : 1);
  } catch (error) {
    // 409（编码重复）/ 400（超长、额度为负）都原样显示后端文案：它比前端猜得准
    submitError.value = messageOf(error);
  } finally {
    saving.value = false;
  }
}

// ==================== 状态切换 ====================

/** 冻结 / 解冻（只发 `{status}`） */
async function toggleStatus(row: Customer): Promise<void> {
  const next = nextCustomerStatus(row.status);
  const payload: UpdateCustomerReq = buildCustomerStatusPayload(next);
  try {
    await updateCustomer(row.id, payload);
    MessagePlugin.success(
      next === 'FROZEN' ? '已冻结（不能再开新单，历史单据不受影响）' : '已解冻（可以继续开单）',
    );
    await loadCustomers(current.value);
  } catch (error) {
    MessagePlugin.error(messageOf(error));
  }
}

// ==================== 删除 ====================

/** 删除（软删）；文案写明「同一编码可以再建」，避免用户以为编码永久作废 */
function confirmRemove(row: Customer): void {
  const dialog = DialogPlugin.confirm({
    header: '删除客户',
    theme: 'warning',
    body: `确认删除「${row.name}（${row.code}）」？这是软删除，删除后同一编码可以再次使用；已被销售单引用的历史单据不受影响。`,
    confirmBtn: { content: '删除', theme: 'danger' },
    onConfirm: async () => {
      dialog.hide();
      try {
        await deleteCustomer(row.id);
        MessagePlugin.success('客户已删除');
        await loadCustomers(current.value);
      } catch (error) {
        MessagePlugin.error(messageOf(error));
      }
    },
  });
}

// ==================== 初始化 ====================

onMounted(() => {
  void loadCustomers(1);
});
</script>

<style scoped lang="less">
.customer-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.filter-card {
  flex: 0 0 auto;
}

.filter-row {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}

.filter-row__keyword {
  width: 220px;
}

.filter-row__select {
  width: 150px;
}

.filter-row__spacer {
  flex: 1 1 auto;
}

.list-card {
  flex: 1 1 auto;
  min-width: 0;
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
  gap: 10px;
  min-width: 0;
}

.panel-head__title {
  font-size: 14px;
  font-weight: 600;
  color: var(--td-text-color-primary);
  white-space: nowrap;
}

.panel-head__meta {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
  white-space: nowrap;
}

.panel-head__hint {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.list-alert,
.dlg-alert {
  margin-bottom: 8px;
}

.op-row {
  display: flex;
  align-items: center;
  gap: 12px;
}

.dlg-inline {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}

.dlg-help {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.gate-wrap {
  display: inline-flex;
}

.mono {
  font-family: var(--td-font-family-number, monospace);
}

.muted {
  color: var(--td-text-color-placeholder);
}

.panel-empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 6px;
  padding: 48px 16px;
  text-align: center;
}

.panel-empty__icon {
  font-size: 32px;
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
  max-width: 460px;
  line-height: 1.6;
}
</style>
