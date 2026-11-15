<template>
  <div class="supplier-page">
    <!-- 列表：筛选 / 分页 / 行操作 / 导出 全部交给 ListPage（P3-04 试点） -->
    <ListPage
      ref="listRef"
      title="供应商"
      testid="supplier"
      table-testid="supplier-table"
      meta-testid="sup-meta"
      btn-prefix="btn-sup"
      :columns="columns"
      :filters="filters"
      :fetcher="fetcher"
      :query-builder="buildSupplierQuery"
      :page-size="SUPPLIER_PAGE_SIZE"
      :actions="actions"
      exportable
    >
      <!-- 筛选行右侧：导入 + 新建（无 :create 权限时置灰 + tooltip 说明） -->
      <template #toolbar>
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
      </template>

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

      <template #empty="{ filtered }">
        <div class="panel-empty" data-testid="supplier-empty">
          <t-icon class="panel-empty__icon" name="shop" />
          <p class="panel-empty__title">{{ filtered ? '没有匹配的供应商' : '还没有供应商' }}</p>
          <p class="panel-empty__desc">
            {{
              filtered
                ? '换个关键词 / 状态试试，或点「重置」看全部'
                : canWrite
                  ? '点右上角「新建供应商」建第一个；账期天数会作为采购单的默认账期'
                  : '当前账号没有新建权限，请联系管理员在主数据里补供应商'
            }}
          </p>
        </div>
      </template>
    </ListPage>

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

          <!-- ATT-11：营业执照 / 开户许可 / 资质证书（建议传，不校验） -->
          <div class="section-title">附件（资质）</div>
          <DocAttachmentPanel
            :doc-type="DOC_KIND.SUPPLIER"
            :doc-id="detail.id"
            :default-role="DEFAULT_ROLE_BY_DOC.SUPPLIER"
            hint="营业执照 / 开户许可 / 资质证书；不传也能保存供应商"
            testid-prefix="supplier-attach"
          />
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
          <!-- CODE-08：系统参数关闭「允许手工指定编码」后编码栏只读、按钮隐藏 -->
          <div class="dlg-inline dlg-inline--grow">
            <t-input
              v-model="form.supplier_code"
              :disabled="manualCodeDisabled(isEdit, allowManualCode)"
              :maxlength="40"
              placeholder="留空自动生成，如 SUP-0002"
              class="dlg-inline__grow"
              data-testid="sup-code"
            />
            <t-button
              v-if="allowManualCode"
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
                  : manualCodeHelp(allowManualCode, '不点「生成编码」则保存时自动生成；生成后仍可手改（已有编码体系可直接填）')
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
 * `grep -n 'erp/supplier' backend/app/src/business/erp/handler.rs` 命中 8 处：
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
import ListPage from '@/components/ListPage/index.vue';
import { errorText } from '@/components/ListPage/list-page-pure';
import type {
  FilterDef,
  ListActions,
  ListFetcher,
} from '@/components/ListPage/types';
import { DOC_KIND } from '@/api/business/erp/doc-kinds';
import { DEFAULT_ROLE_BY_DOC } from '@/api/business/erp/attachment';
import DocAttachmentPanel from '@/views/business/erp/shared/DocAttachmentPanel.vue';
import ImportEntryButton from '@/components/ImportEntryButton.vue';
import { loadAllowManualCode, manualCodeDisabled, manualCodeHelp } from '../shared/code-policy';
import {
  SUPPLIER_COLUMNS,
  SUPPLIER_PAGE_SIZE,
  SUPPLIER_STATUS_OPTIONS,
  buildSupplierCreatePayload,
  buildSupplierQuery,
  buildSupplierUpdatePayload,
  contactLabel,
  creditDaysLabel,
  emptySupplierForm,
  formatAmount,
  hasSupplierFieldError,
  supplierFormFromRow,
  supplierStatusLabel,
  supplierStatusTheme,
  textOr,
  validateSupplierForm,
  type SupplierFieldErrors,
  type SupplierFormState,
  type SupplierQuery as SupplierQueryInput,
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

// ==================== 列表（ListPage 声明） ====================

/**
 * 筛选项声明。
 *
 * `buildSupplierQuery` 仍然负责拼入参（trim、空值整键缺席都被单测钉着）——
 * 重构只搬「怎么画」，不换「发什么」。
 */
const filters: FilterDef[] = [
  {
    key: 'keyword',
    type: 'keyword',
    placeholder: '搜索供应商编码 / 名称 / 简称',
    width: 300,
    testid: 'sup-keyword',
  },
  {
    key: 'status',
    type: 'select',
    placeholder: '全部状态',
    width: 150,
    options: statusOptions,
    testid: 'sup-status-filter',
  },
];

/** 拉一页（ListPage 的 fetcher；入参由 `buildSupplierQuery` 拼） */
const fetcher: ListFetcher<Supplier> = (query) => fetchSupplierPage(query as SupplierQueryInput);

/** 行操作（权限码与 `v-privilege` 同语义：没码就不出现） */
const actions: ListActions = {
  row: [
    {
      key: 'view',
      label: '查看',
      testid: 'btn-sup-view',
      handler: (row: Supplier) => openDetail(row),
    },
    {
      key: 'edit',
      label: '编辑',
      testid: 'btn-sup-edit',
      permission: PERM.update,
      handler: (row: Supplier) => openEdit(row),
    },
    {
      key: 'delete',
      label: '删除',
      theme: 'danger',
      testid: 'btn-sup-delete',
      permission: PERM.update,
      handler: (row: Supplier) => confirmRemove(row),
    },
  ],
};

/** ListPage 实例（新建 / 编辑 / 删除后刷新用） */
const listRef = ref<InstanceType<typeof ListPage> | null>(null);

/** 刷新到指定页（不传 = 当前页） */
async function reloadList(page?: number): Promise<void> {
  await listRef.value?.load(page ?? listRef.value.current);
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
    MessagePlugin.warning(`详情没刷新出来，显示的是列表里的数据：${errorText(error)}`);
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
 * 是否允许手工指定编码（CODE-08，系统参数 `erp.master_data.allow_manual_code`）。
 *
 * `false` ⇒ 编码栏只读 + 「生成编码」按钮隐藏 + 提示「编码由系统自动生成」。
 * 初始 `true` 是**乐观**默认：策略接口回来之前不让输入框先灰一下再亮（闪一下更疑惑）。
 */
const allowManualCode = ref(true);

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
    MessagePlugin.error(`生成编码失败：${errorText(error)}`);
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
    await reloadList(editing.value ? undefined : 1);
  } catch (error) {
    submitError.value = errorText(error);
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
        await reloadList();
      } catch (error) {
        MessagePlugin.error(errorText(error));
      }
    },
  });
}

// ==================== 初始化 ====================

onMounted(async () => {
  // 列表由 ListPage 自己在 setup 里拉首屏；这里只补字典（拉不到只是「应付科目」列显示 `—`）
  await loadAccounts();
  // CODE-08：编码策略不影响列表首屏，单独取（失败按「允许手填」兜底，不打断页面）
  allowManualCode.value = await loadAllowManualCode();
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
