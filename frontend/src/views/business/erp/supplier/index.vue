<template>
  <div class="supplier-page">
    <!-- 筛选行：关键词 / 状态 / 查询 / 重置 / 刷新 / 新建 -->
    <t-card :bordered="false" class="filter-card">
      <div class="filter-row">
        <t-input
          v-model="filter.keyword"
          class="filter-row__keyword"
          placeholder="搜索供应商编码 / 名称 / 简称"
          clearable
          data-testid="sup-keyword"
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
          data-testid="sup-status-filter"
        />

        <t-button theme="primary" data-testid="btn-sup-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-sup-reset" @click="handleReset">重置</t-button>
        <t-button variant="outline" data-testid="btn-sup-refresh" @click="handleRefresh">刷新</t-button>

        <span class="filter-row__spacer" />

        <ImportEntryButton
          biz-type="business.erp.supplier"
          biz-label="供应商"
          testid="btn-import-supplier"
        />

        <t-tooltip
          :content="canWrite ? '新建一个供应商' : '当前账号没有 business:erp:supplier:create 权限'"
          placement="top-right"
        >
          <span class="gate-wrap">
            <t-button
              theme="primary"
              data-testid="btn-sup-new"
              :disabled="!canWrite"
              @click="openCreate"
            >
              <template #icon><t-icon name="add" /></template>
              新建供应商
            </t-button>
          </span>
        </t-tooltip>
      </div>
    </t-card>

    <!-- 列表 -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">供应商</span>
          <span class="panel-head__meta" data-testid="sup-meta">共 {{ total }} 条</span>
        </div>
      </div>

      <t-alert v-if="loadError" theme="error" :message="loadError" class="list-alert">
        <template #operation>
          <t-button size="small" variant="outline" @click="loadSuppliers()">重试</t-button>
        </template>
      </t-alert>

      <t-table
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        :hover="true"
        data-testid="supplier-table"
        @page-change="handlePageChange"
      >
        <template #contact_person="{ row }">
          <span :class="{ muted: !row.contact_person && !row.contact_phone }">{{ contactLabel(row) }}</span>
        </template>

        <template #tax_no="{ row }">
          <span :class="['mono', { muted: !row.tax_no }]">{{ textOr(row.tax_no) }}</span>
        </template>

        <template #credit_days="{ row }">
          <span class="mono">{{ creditDaysLabel(row.credit_days) }}</span>
        </template>

        <template #status="{ row }">
          <t-tag size="small" :theme="supplierStatusTheme(row.status)" variant="light" data-testid="sup-status-tag">
            {{ supplierStatusLabel(row.status) }}
          </t-tag>
        </template>

        <template #op="{ row }">
          <div class="op-row">
            <t-link theme="primary" data-testid="btn-sup-view" @click="openDetail(row)">查看</t-link>
            <t-link v-privilege="[PERM.update]" theme="primary" data-testid="btn-sup-edit" @click="openEdit(row)">
              编辑
            </t-link>
            <t-link v-privilege="[PERM.update]" theme="danger" data-testid="btn-sup-delete" @click="confirmRemove(row)">
              删除
            </t-link>
          </div>
        </template>

        <template #empty>
          <div class="panel-empty" data-testid="supplier-empty">
            <t-icon class="panel-empty__icon" name="shop" />
            <p class="panel-empty__title">{{ isFiltered ? '没有匹配的供应商' : '还没有供应商' }}</p>
            <p class="panel-empty__desc">
              {{
                isFiltered
                  ? '换个关键词 / 状态试试，或点「重置」看全部'
                  : canWrite
                    ? '点右上角「新建供应商」建第一个；账期天数会作为采购单的默认账期'
                    : '当前账号没有新建权限，请联系管理员在主数据里补供应商'
              }}
            </p>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- 详情抽屉 -->
    <t-drawer v-model:visible="detailVisible" size="640px" :footer="false" :header="false" data-testid="sup-drawer">
      <div v-if="detail" class="drawer" data-testid="sup-detail">
        <div class="drawer-head">
          <div class="drawer-head__main">
            <span class="drawer-head__title mono">{{ detail.supplier_code }}</span>
            <t-tag size="small" :theme="supplierStatusTheme(detail.status)">{{ supplierStatusLabel(detail.status) }}</t-tag>
          </div>
          <div class="drawer-head__side">
            <t-button size="small" variant="outline" data-testid="btn-sup-drawer-close" @click="detailVisible = false">
              关闭
            </t-button>
          </div>
        </div>

        <div class="drawer-body">
          <div class="field-grid">
            <div class="field field--wide">
              <span class="field__label">供应商名称</span>
              <span class="field__value">{{ detail.supplier_name }}</span>
            </div>
            <div class="field">
              <span class="field__label">简称</span>
              <span class="field__value">{{ textOr(detail.short_name) }}</span>
            </div>
            <div class="field">
              <span class="field__label">税号</span>
              <span class="field__value mono">{{ textOr(detail.tax_no) }}</span>
            </div>
            <div class="field">
              <span class="field__label">联系人</span>
              <span class="field__value">{{ textOr(detail.contact_person) }}</span>
            </div>
            <div class="field">
              <span class="field__label">联系电话</span>
              <span class="field__value mono">{{ textOr(detail.contact_phone) }}</span>
            </div>
            <div class="field">
              <span class="field__label">邮箱</span>
              <span class="field__value">{{ textOr(detail.email) }}</span>
            </div>
            <div class="field">
              <span class="field__label">账期</span>
              <span class="field__value">{{ creditDaysLabel(detail.credit_days) }}</span>
            </div>
            <div class="field">
              <span class="field__label">信用额度</span>
              <span class="field__value mono">{{ formatAmount(detail.credit_limit) }}</span>
            </div>
            <div class="field">
              <span class="field__label">应付科目</span>
              <span class="field__value">{{ accountNameOf(detail.payable_account_id) }}</span>
            </div>
            <div class="field field--wide">
              <span class="field__label">地址</span>
              <span class="field__value">{{ textOr(detail.address) }}</span>
            </div>
            <div class="field">
              <span class="field__label">开票抬头</span>
              <span class="field__value">{{ textOr(detail.invoice_title) }}</span>
            </div>
            <div class="field">
              <span class="field__label">开户行</span>
              <span class="field__value">{{ textOr(detail.invoice_bank_name) }}</span>
            </div>
            <div class="field">
              <span class="field__label">银行账号</span>
              <span class="field__value mono">{{ textOr(detail.invoice_bank_account) }}</span>
            </div>
            <div class="field">
              <span class="field__label">更新时间</span>
              <span class="field__value mono">{{ detail.updated_at }}</span>
            </div>
            <div class="field field--wide">
              <span class="field__label">备注</span>
              <span class="field__value">{{ textOr(detail.remark) }}</span>
            </div>
          </div>
        </div>
      </div>
    </t-drawer>

    <!-- 新建 / 编辑弹窗 -->
    <t-dialog
      v-model:visible="dialogVisible"
      :header="isEdit ? '编辑供应商' : '新建供应商'"
      :confirm-btn="{ content: isEdit ? '保存' : '创建', loading: saving }"
      :close-on-overlay-click="false"
      width="720px"
      data-testid="sup-dialog"
      @confirm="handleConfirm"
    >
      <t-alert v-if="submitError" theme="error" :message="submitError" class="dlg-alert" />

      <t-form :data="form" label-width="110px">
        <t-form-item
          label="供应商编码"
          name="supplier_code"
          :status="formErrors.supplier_code ? 'error' : undefined"
          :tips="formErrors.supplier_code"
        >
          <!-- CODE-05 交互方式 B：编码框 + 「生成编码」按钮（编辑态都禁用） -->
          <div class="dlg-inline dlg-inline--grow">
            <t-input
              v-model="form.supplier_code"
              :disabled="isEdit"
              :maxlength="40"
              placeholder="留空自动生成，如 SUP-0002"
              class="dlg-inline__grow"
              data-testid="sup-code"
            />
            <t-button
              variant="outline"
              theme="primary"
              :disabled="isEdit"
              :loading="genCodeLoading"
              data-testid="sup-gen-code"
              @click="handleGenCode"
            >
              生成编码
            </t-button>
          </div>
          <template #help>
            <span class="dlg-help">
              {{
                isEdit
                  ? '编码是供应商身份，不支持修改'
                  : '不点「生成编码」则保存时自动生成；生成后仍可手改（已有编码体系可直接填）'
              }}
            </span>
          </template>
        </t-form-item>

        <t-form-item
          label="供应商名称"
          name="supplier_name"
          :status="formErrors.supplier_name ? 'error' : undefined"
          :tips="formErrors.supplier_name"
        >
          <t-input v-model="form.supplier_name" :maxlength="200" placeholder="营业执照全称" data-testid="sup-name" />
        </t-form-item>

        <t-form-item label="简称">
          <t-input v-model="form.short_name" :maxlength="100" placeholder="选填，列表里更短" data-testid="sup-short-name" />
        </t-form-item>

        <t-form-item label="联系人">
          <t-input v-model="form.contact_person" :maxlength="50" placeholder="选填" data-testid="sup-contact" />
        </t-form-item>

        <t-form-item label="联系电话">
          <t-input v-model="form.contact_phone" :maxlength="30" placeholder="选填" data-testid="sup-phone" />
        </t-form-item>

        <t-form-item label="邮箱" name="email" :status="formErrors.email ? 'error' : undefined" :tips="formErrors.email">
          <t-input v-model="form.email" :maxlength="120" placeholder="选填" data-testid="sup-email" />
        </t-form-item>

        <t-form-item label="税号">
          <t-input v-model="form.tax_no" :maxlength="50" placeholder="纳税人识别号，选填" data-testid="sup-tax" />
        </t-form-item>

        <t-form-item label="地址">
          <t-input v-model="form.address" :maxlength="255" placeholder="选填" data-testid="sup-address" />
        </t-form-item>

        <t-form-item
          label="账期天数"
          name="credit_days"
          :status="formErrors.credit_days ? 'error' : undefined"
          :tips="formErrors.credit_days"
        >
          <div class="dlg-inline">
            <t-input-number v-model="form.credit_days" :min="0" :step="1" theme="normal" data-testid="sup-credit-days" />
            <span class="dlg-help">0 = 现结；采购单默认带这个账期</span>
          </div>
        </t-form-item>

        <t-form-item
          label="信用额度"
          name="credit_limit"
          :status="formErrors.credit_limit ? 'error' : undefined"
          :tips="formErrors.credit_limit"
        >
          <t-input-number
            v-model="form.credit_limit"
            :min="0"
            :step="1000"
            :decimal-places="2"
            theme="normal"
            data-testid="sup-credit-limit"
          />
        </t-form-item>

        <t-form-item label="应付科目">
          <t-select
            v-model="form.payable_account_id"
            :options="accountOptions"
            :loading="accountLoading"
            placeholder="选填，默认应付账款科目"
            filterable
            clearable
            data-testid="sup-payable-account"
          />
        </t-form-item>

        <t-form-item label="开票抬头">
          <t-input v-model="form.invoice_title" :maxlength="200" placeholder="选填" data-testid="sup-invoice-title" />
        </t-form-item>

        <t-form-item label="开户行">
          <t-input v-model="form.invoice_bank_name" :maxlength="200" placeholder="选填" data-testid="sup-bank-name" />
        </t-form-item>

        <t-form-item label="银行账号">
          <t-input v-model="form.invoice_bank_account" :maxlength="64" placeholder="选填" data-testid="sup-bank-account" />
        </t-form-item>

        <t-form-item label="启用">
          <t-switch v-model="enabledSwitch" data-testid="sup-status-switch" />
        </t-form-item>

        <t-form-item label="备注">
          <t-input v-model="form.remark" :maxlength="2000" placeholder="选填" data-testid="sup-remark" />
        </t-form-item>
      </t-form>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 供应商页（P4-A）——标准主数据 CRUD：列表 + 详情抽屉 + 新建 / 编辑 + 软删。
 *
 * # 后端接口（开工前按硬红线 grep 过）
 *
 * `grep -n 'erp/supplier' backend/src/business/erp/handler.rs` 命中 8 处：
 * `GET /business/erp/supplier/page`、`GET|PUT|DELETE /business/erp/supplier/{id}`、`POST /business/erp/supplier`
 * —— 读写齐全，所以做完整 CRUD。`\d biz_supplier` 核过列名，见 `supplier-query.ts` 头注释。
 *
 * # 权限码（V0141 已种）
 *
 * `business:erp:supplier:list` / `:create` / `:update`（删除也走 `:update`）。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin } from 'tdesign-vue-next';
import { useUserStore } from '@/stores/user';
import {
  createSupplier,
  deleteSupplier,
  fetchAccountOptions,
  fetchSupplierDetail,
  fetchSupplierNextCode,
  fetchSupplierPage,
  updateSupplier,
  type AccountRow,
  type Supplier,
} from '@/api/business/erp';
import { ApiError } from '@/api/request';
import ImportEntryButton from '@/components/ImportEntryButton.vue';
import {
  SUPPLIER_COLUMNS,
  SUPPLIER_PAGE_SIZE,
  SUPPLIER_STATUS_OPTIONS,
  buildSupplierCreatePayload,
  buildSupplierQuery,
  buildSupplierUpdatePayload,
  contactLabel,
  creditDaysLabel,
  emptySupplierFilter,
  emptySupplierForm,
  formatAmount,
  hasSupplierFieldError,
  isSupplierFiltered,
  supplierFormFromRow,
  supplierStatusLabel,
  supplierStatusTheme,
  textOr,
  validateSupplierForm,
  type SupplierFieldErrors,
  type SupplierFilterState,
  type SupplierFormState,
} from './supplier-query';

/** 本页权限码（与 `V0141__erp_permissions.up.sql` 的 auth_code 逐字一致） */
const PERM = {
  /** 列表（页面入口） */
  list: 'business:erp:supplier:list',
  /** 新建 */
  create: 'business:erp:supplier:create',
  /** 编辑 / 删除 */
  update: 'business:erp:supplier:update',
} as const;

/** 表格列（colKey 与后端出参同名） */
const columns = SUPPLIER_COLUMNS;

/** 状态下拉 */
const statusOptions = SUPPLIER_STATUS_OPTIONS.map((item) => ({ ...item }));

/** 新建按钮：无 `:create` 权限时置灰 + tooltip 说明（卡面 ④） */
const userStore = useUserStore();
const canWrite = computed(() => userStore.hasPermission(PERM.create));

// ==================== 列表状态 ====================

/** 筛选行 */
const filter = reactive<SupplierFilterState>(emptySupplierFilter());
/** 当前页数据 */
const rows = ref<Supplier[]>([]);
/** 总条数 */
const total = ref(0);
/** 当前页码 */
const current = ref(1);
/** 每页条数 */
const pageSize = SUPPLIER_PAGE_SIZE;
/** 加载中 */
const loading = ref(false);
/** 加载错误 */
const loadError = ref('');

/** 是否筛选态 */
const isFiltered = computed(() => isSupplierFiltered(filter));

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
async function loadSuppliers(page: number = current.value): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const data = await fetchSupplierPage(buildSupplierQuery(filter, page, pageSize));
    rows.value = data.list ?? [];
    total.value = data.total ?? 0;
    current.value = data.page ?? page;
    const lastPage = Math.max(1, Math.ceil(total.value / pageSize));
    if (current.value > lastPage) {
      current.value = lastPage;
      return loadSuppliers(lastPage);
    }
  } catch (error) {
    rows.value = [];
    total.value = 0;
    loadError.value = `供应商加载失败：${messageOf(error)}`;
  } finally {
    loading.value = false;
  }
}

/** 查询（回第 1 页） */
function handleSearch(): void {
  current.value = 1;
  void loadSuppliers(1);
}

/** 重置 */
function handleReset(): void {
  Object.assign(filter, emptySupplierFilter());
  current.value = 1;
  void loadSuppliers(1);
}

/** 刷新 */
function handleRefresh(): void {
  void loadSuppliers(current.value);
}

/** 切页 */
function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  current.value = pageInfo.current;
  void loadSuppliers(pageInfo.current);
}

// ==================== 应付科目（译名 + 下拉） ====================

/** 会计科目（应付科目下拉与详情译名） */
const accounts = ref<AccountRow[]>([]);
/** 科目加载中 */
const accountLoading = ref(false);

/** 应付科目下拉 */
const accountOptions = computed(() =>
  accounts.value.map((item) => ({ label: `${item.account_code} ${item.account_name}`, value: item.id })),
);

/** `payable_account_id` → 「编码 名称」；查不到给 `—`，别显示 uuid */
function accountNameOf(id: string | null): string {
  if (!id) {
    return '—';
  }
  const found = accounts.value.find((item) => item.id === id);
  return found ? `${found.account_code} ${found.account_name}` : '—';
}

/** 拉科目字典（失败不挡列表） */
async function loadAccounts(): Promise<void> {
  accountLoading.value = true;
  try {
    accounts.value = await fetchAccountOptions();
  } catch {
    accounts.value = [];
  } finally {
    accountLoading.value = false;
  }
}

// ==================== 详情抽屉 ====================

/** 抽屉可见 */
const detailVisible = ref(false);
/** 详情 */
const detail = ref<Supplier | null>(null);

/** 打开详情：先垫列表行，再用详情接口覆盖（能发现别人刚改过的值） */
async function openDetail(row: Supplier): Promise<void> {
  detail.value = row;
  detailVisible.value = true;
  try {
    detail.value = await fetchSupplierDetail(row.id);
  } catch (error) {
    MessagePlugin.warning(`详情没刷新出来，显示的是列表里的数据：${messageOf(error)}`);
  }
}

// ==================== 新建 / 编辑 ====================

/** 弹窗可见 */
const dialogVisible = ref(false);
/** 编辑对象；`null` = 新建 */
const editing = ref<Supplier | null>(null);
/** 表单 */
const form = reactive<SupplierFormState>(emptySupplierForm());
/** 表单错误 */
const formErrors = ref<SupplierFieldErrors>({});
/** 提交错误 */
const submitError = ref('');
/** 提交中 */
const saving = ref(false);

const isEdit = computed(() => editing.value !== null);

/** 启用开关（`status` 1/0 ↔ 布尔） */
const enabledSwitch = computed({
  get: () => form.status === 1,
  set: (value: boolean) => {
    form.status = value ? 1 : 0;
  },
});

/** 打开新建 */
function openCreate(): void {
  editing.value = null;
  Object.assign(form, emptySupplierForm());
  formErrors.value = {};
  submitError.value = '';
  dialogVisible.value = true;
}

/** 打开编辑（回填） */
function openEdit(row: Supplier): void {
  editing.value = row;
  Object.assign(form, supplierFormFromRow(row));
  formErrors.value = {};
  submitError.value = '';
  dialogVisible.value = true;
}

// ==================== 生成编码（CODE-05 交互方式 B） ====================

/** 「生成编码」按钮加载中 */
const genCodeLoading = ref(false);

/**
 * 取下一个可用编码填入编码框（**不占号**，可手改）。
 *
 * 后端 `GET /business/erp/supplier/next-code` 只读 `biz_seq`（方案 B）：
 * - 点了又取消不会浪费流水；
 * - 并发下两人可能看到同一个号 ⇒ 保存时被抢走的人是 `409 + 66001`，
 *   错误面板会把后端原文（点名编码）弹出来，点一次「生成编码」换个号即可。
 */
async function handleGenCode(): Promise<void> {
  if (isEdit.value) {
    return;
  }
  genCodeLoading.value = true;
  try {
    const preview = await fetchSupplierNextCode();
    form.supplier_code = preview.code;
    // 之前「超长」之类的提示已不成立，清掉免得红框跟着新号走
    delete formErrors.value.supplier_code;
  } catch (error) {
    MessagePlugin.error(`生成编码失败：${messageOf(error)}`);
  } finally {
    genCodeLoading.value = false;
  }
}

/** 提交（新建 / 编辑） */
async function handleConfirm(): Promise<void> {
  const errors = validateSupplierForm(form, isEdit.value);
  formErrors.value = errors;
  if (hasSupplierFieldError(errors)) {
    return;
  }
  saving.value = true;
  submitError.value = '';
  try {
    if (editing.value) {
      await updateSupplier(editing.value.id, buildSupplierUpdatePayload(form, editing.value));
      MessagePlugin.success('供应商已保存');
    } else {
      const created = await createSupplier(buildSupplierCreatePayload(form));
      // 没点「生成编码」直接保存时编码是后端取的号 —— 回填到表单并在提示里报出来，
      // 用户当场就能看到自己这条到底叫 SUP-000几（卡片验收 ④）。
      form.supplier_code = created.supplier_code;
      MessagePlugin.success(`供应商已创建 · 编码 ${created.supplier_code}`);
    }
    dialogVisible.value = false;
    await loadSuppliers(editing.value ? current.value : 1);
  } catch (error) {
    submitError.value = messageOf(error);
  } finally {
    saving.value = false;
  }
}

// ==================== 删除 ====================

/** 删除（软删）；被采购单引用时后端 409，原文弹给用户 */
function confirmRemove(row: Supplier): void {
  const dialog = DialogPlugin.confirm({
    header: '删除供应商',
    theme: 'warning',
    body: `确认删除「${row.supplier_name}（${row.supplier_code}）」？删除后采购单不能再选它；已被引用时后端会拒绝。`,
    confirmBtn: { content: '删除', theme: 'danger' },
    onConfirm: async () => {
      dialog.hide();
      try {
        await deleteSupplier(row.id);
        MessagePlugin.success('供应商已删除');
        await loadSuppliers(current.value);
      } catch (error) {
        MessagePlugin.error(messageOf(error));
      }
    },
  });
}

// ==================== 初始化 ====================

onMounted(async () => {
  // 科目字典与列表并行：字典拉不到只是「应付科目」列显示 `—`
  await Promise.all([loadAccounts(), loadSuppliers(1)]);
});
</script>

<style scoped lang="less">
.supplier-page {
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
  width: 300px;
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

/* CODE-05：「编码框 + 生成编码」一行铺满（输入框吃掉剩余宽度，按钮不缩） */
.dlg-inline--grow {
  width: 100%;
  flex-wrap: nowrap;
}

.dlg-inline__grow {
  flex: 1 1 auto;
  min-width: 0;
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

/* ==================== 详情抽屉 ==================== */

.drawer {
  display: flex;
  flex-direction: column;
  gap: 16px;
  padding: 4px 4px 24px;
}

.drawer-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding-bottom: 12px;
  border-bottom: 1px solid var(--td-component-stroke);
}

.drawer-head__main {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
}

.drawer-head__title {
  font-size: 16px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.field-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 14px 20px;
}

.field {
  display: flex;
  flex-direction: column;
  gap: 4px;
  min-width: 0;
}

.field--wide {
  grid-column: 1 / -1;
}

.field__label {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.field__value {
  font-size: 13px;
  color: var(--td-text-color-primary);
  word-break: break-all;
}
</style>
