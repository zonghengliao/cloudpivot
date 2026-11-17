<template>
  <div class="customer-page">
    <!-- 筛选 / 分页 / 行操作 / 导出交给 ListPage（P3-06） -->
    <ListPage
      ref="listRef"
      title="客户档案"
      testid="customer"
      table-testid="customer-table"
      meta-testid="cus-meta"
      btn-prefix="btn-cus"
      :columns="columns"
      :filters="filters"
      :filter-state="filter"
      :initial-page="current"
      :fetcher="fetcher"
      :query-builder="queryBuilder"
      :page-size="CUSTOMER_PAGE_SIZE"
      exportable
      @loaded="onLoaded"
    >
      <template #toolbar>
        <t-tooltip
          :content="canWrite ? '新建一个客户档案' : '当前账号没有 business:erp:customer:create 权限'"
          placement="top-right"
        >
          <span class="gate-wrap">
            <t-button theme="primary" data-testid="btn-cus-new" :disabled="!canWrite" @click="openCreate">
              <template #icon><t-icon name="add" /></template>
              新建客户
            </t-button>
          </span>
        </t-tooltip>
      </template>

      <template #head-extra>
        <span class="panel-head__hint">冻结只影响开新单，历史单据与引用不受影响</span>
      </template>
        <template #code="{ row }">
          <span class="mono">{{ row.code }}</span>
        </template>

        <template #name="{ row }">
          <span :title="row.name">{{ row.name }}</span>
        </template>

        <template #contact_name="{ row }">
          <span :class="{ muted: !row.contact_name && !row.contact_phone }">{{ contactLabel(row) }}</span>
        </template>

        <template #credit_days="{ row }">
          <span class="mono" :class="{ muted: isCashOnDelivery(row.credit_days) }">
            {{ formatCreditDays(row.credit_days) }}
          </span>
        </template>

        <template #credit_limit="{ row }">
          <span class="mono" :class="{ muted: Number(row.credit_limit) === 0 }">
            {{ formatCreditLimit(row.credit_limit) }}
          </span>
        </template>

        <!-- 超限策略（M3-09 / V0437）：BLOCK 是常规口径，WARN / PASS 用警告色提醒「这家会放行」 -->
        <template #credit_policy="{ row }">
          <t-tag
            v-if="isCreditPolicyRelaxed(row.credit_policy)"
            size="small"
            theme="warning"
            variant="light"
          >
            {{ creditPolicyLabel(row.credit_policy) }}
          </t-tag>
          <span v-else>{{ creditPolicyLabel(row.credit_policy) }}</span>
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
          <div class="panel-empty panel-empty--rich" data-testid="customer-empty">
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
    </ListPage>

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
          <!-- CODE-05 交互方式 B：编码框 + 「生成编码」按钮（编辑态都禁用） -->
          <!-- CODE-08：系统参数关闭「允许手工指定编码」后编码栏只读、按钮隐藏 -->
          <div class="dlg-inline dlg-inline--grow">
            <t-input
              v-model="form.code"
              :disabled="manualCodeDisabled(isEdit, allowManualCode)"
              :maxlength="CUSTOMER_LIMITS.code"
              placeholder="留空自动生成，如 CUS-0002"
              class="dlg-inline__grow"
              data-testid="cus-code"
            />
            <t-button
              v-if="allowManualCode"
              variant="outline"
              theme="primary"
              :disabled="isEdit"
              :loading="genCodeLoading"
              data-testid="cus-gen-code"
              @click="handleGenCode"
            >
              生成编码
            </t-button>
          </div>
          <template #help>
            <span class="dlg-help">
              {{
                isEdit
                  ? '编码是客户身份，不支持修改（已软删的编码可以再建）'
                  : manualCodeHelp(allowManualCode, '不点「生成编码」则保存时自动生成；生成后仍可手改（已有编码体系可直接填）')
              }}
            </span>
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

        <t-form-item
          label="超限策略"
          name="credit_policy"
          :tips="`仅对信用额度 > 0 的客户生效：${creditPolicyHint(form.credit_policy)}`"
        >
          <div class="dlg-inline">
            <t-select
              v-model="form.credit_policy"
              :options="CUSTOMER_CREDIT_POLICY_OPTIONS"
              style="width: 140px"
              data-testid="cus-credit-policy"
            />
            <span class="dlg-help">
              {{ creditPolicyHint(form.credit_policy) }}；额度 0（不控额度）时本项不生效
            </span>
          </div>
        </t-form-item>

        <t-form-item
          label="账期(天)"
          name="credit_days"
          :status="formErrors.credit_days ? 'error' : undefined"
          :tips="formErrors.credit_days || '0 = 现结（到期日 = 出库过账日）'"
        >
          <div class="dlg-inline">
            <t-input-number
              v-model="form.credit_days"
              :min="0"
              :step="1"
              :decimal-places="0"
              suffix="天"
              theme="normal"
              data-testid="cus-credit-days"
            />
            <span class="dlg-help">0 = 现结；填 30 则出库过账后 30 天到期（挂应收时算到期日）</span>
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

      <!--
        客户信用（M3-14）：额度 / 占用 / 可用 —— 内嵌在客户详情里。
        只读、现算（`GET /api/business/erp/credit/{id}`），与提交时的信用校验同一口径；
        取不到只降级成一行提示，不挡保存（面板自己处理 loading / error）。
      -->
      <template v-if="isEdit && editing">
        <div class="section-title">
          信用占用
          <span class="section-hint">实时现算，与提交销售单时的校验同一口径</span>
        </div>
        <!--
          `:key` 换客户 → 重建面板（旧快照绝不带进新客户的表头）；
          `:refresh-key` 每次打开 / 保存后 +1 → 同一家客户也重拉（弹窗关闭不销毁内容，实测）
        -->
        <CreditPanel
          :key="editing.id"
          :customer-id="editing.id"
          :refresh-key="creditRefreshKey"
          testid-prefix="cus-credit"
        />
      </template>

      <!--
        ATT-11：营业执照 / 信用资料（建议传，不校验）。
        只有在**编辑已保存的客户**时才出现 —— 新建时还没有 doc id，附件无处可挂；
        保存后重新打开编辑即可上传。
      -->
      <template v-if="isEdit && editing">
        <div class="section-title">附件（资质）</div>
        <p class="dlg-attach-tip">营业执照 / 信用资料建议上传；<b>不传也能保存客户</b>。</p>
        <DocAttachmentPanel
          :doc-type="DOC_KIND.CUSTOMER"
          :doc-id="editing.id"
          :default-role="DEFAULT_ROLE_BY_DOC.CUSTOMER"
          hint="营业执照 / 信用资料（评级报告、授信批复）"
          testid-prefix="customer-attach"
        />
      </template>
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
 * 客户只有 10 个业务字段（含 SO-14 的账期天数），编辑弹窗已经全展示；再叠一个抽屉只是多一次点击
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
  fetchCustomerNextCode,
  fetchCustomerPage,
  updateCustomer,
  type Customer,
  type UpdateCustomerReq,
} from '@/api/business/erp';
import { ApiError } from '@/api/request';
import { DOC_KIND } from '@/api/business/erp/doc-kinds';
import { DEFAULT_ROLE_BY_DOC } from '@/api/business/erp/attachment';
import DocAttachmentPanel from '@/views/business/erp/shared/DocAttachmentPanel.vue';
/**
 * M3-14：客户信用面板（额度 / 占用 / 可用），内嵌在编辑弹窗里。
 * 它自己取数（`GET /api/business/erp/credit/{id}`），本页只负责「保存成功后让它重拉」。
 */
import CreditPanel from './CreditPanel.vue';
/**
 * P3-06：列表壳换成通用 `ListPage`（筛选 / 服务端分页 / 行操作 / 导出）。
 * 只换渲染与分页接线，`buildCustomerQuery` / `isCustomerFiltered` 的口径一行没动。
 */
import ListPage from '@/components/ListPage/index.vue';
import type { FilterDef, ListFetcher, ListQueryBuilder } from '@/components/ListPage/types';
import { loadAllowManualCode, manualCodeDisabled, manualCodeHelp } from '../shared/code-policy';
import {
  CUSTOMER_COLUMNS,
  CUSTOMER_CREDIT_POLICY_OPTIONS,
  CUSTOMER_LIMITS,
  CUSTOMER_PAGE_SIZE,
  CUSTOMER_STATUS_OPTIONS,
  buildCustomerCreatePayload,
  buildCustomerQuery,
  buildCustomerStatusPayload,
  buildCustomerUpdatePayload,
  contactLabel,
  creditPolicyHint,
  creditPolicyLabel,
  customerFormFromRow,
  customerStatusLabel,
  customerStatusTheme,
  customerToggleLabel,
  emptyCustomerFilter,
  emptyCustomerForm,
  formatCreditDays,
  formatCreditLimit,
  hasCustomerFieldError,
  isCashOnDelivery,
  isCreditPolicyRelaxed,
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

// ==================== 列表状态（筛选 / 分页 / 导出交给 ListPage） ====================

/**
 * 筛选行。
 *
 * 传给 `ListPage` 的 `filterState` —— 组件就地读写**同一个对象**，所以
 * `buildCustomerQuery` / `isCustomerFiltered` 这些闭包一行都不用改。
 */
const filter = reactive<CustomerFilterState>(emptyCustomerFilter());
/** 当前页码（新建 / 编辑后回页要用；每拉完一页由 `onLoaded` 同步） */
const current = ref(1);
/** ListPage 实例（保存 / 删完后重拉列表） */
const listRef = ref<InstanceType<typeof ListPage> | null>(null);

/** 是否筛选态（空态文案用） */
const isFiltered = computed(() => isCustomerFiltered(filter));

/** 筛选项：编码 / 名称 / 状态（原页面的两个关键词框 + 状态下拉，改动只换渲染不换口径） */
const filters: FilterDef[] = [
  { key: 'code', type: 'keyword', placeholder: '客户编码（模糊）', width: 200, testid: 'cus-code-filter' },
  { key: 'name', type: 'keyword', placeholder: '客户名称（模糊）', width: 200, testid: 'cus-name-filter' },
  {
    key: 'status',
    type: 'select',
    placeholder: '全部状态',
    width: 150,
    options: statusOptions,
    testid: 'cus-status-filter',
  },
];

/** 拉一页（入参拼装沿用 `buildCustomerQuery`，空值整键缺席等口径由它自己的单测钉住） */
const fetcher: ListFetcher<Customer> = (query) =>
  fetchCustomerPage(query as Parameters<typeof fetchCustomerPage>[0]);

/** 入参拼装 */
const queryBuilder: ListQueryBuilder = (state, page, pageSize) =>
  buildCustomerQuery(state as CustomerFilterState, page, pageSize);

/** 重拉到指定页（不传 = 当前页） */
async function reloadList(page: number = current.value): Promise<void> {
  await listRef.value?.load(page);
}

/** 一页拉完：同步页码（删掉最后一页最后一条时组件已回夹，这里跟着对） */
function onLoaded(payload: { page: number }): void {
  current.value = payload.page;
}

/** 错误 → 人话（ApiError 带后端 msg，别吃掉） */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
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
/**
 * 信用面板的刷新信号（M3-14）：每次打开详情、保存成功后 `+1`，面板跟着重拉。
 *
 * 为什么不能只靠 `customerId` 变化：TDesign 的弹窗关闭**不销毁内容**，同一家客户
 * 再打开时 `customerId` 没变、watch 不触发 —— 刚改完额度会看到旧数（实测踩过）。
 * 也不在父页面缓存信用快照：面板自己拿最新值，省一份会漂的副本。
 */
const creditRefreshKey = ref(0);

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
  // 每次打开详情都重拉信用（别人可能刚收过款 / 改过额度）
  creditRefreshKey.value += 1;
  Object.assign(form, customerFormFromRow(row));
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
 */
const allowManualCode = ref(true);

/**
 * 取下一个可用编码填入编码框（**不占号**，可手改）。
 *
 * 后端 `GET /business/erp/customer/next-code` 只读 `biz_seq`（方案 B）：
 * 点了又取消不浪费流水；并发撞号在保存时由 `409 + 66001` 兜（原文点名编码）。
 */
async function handleGenCode(): Promise<void> {
  if (isEdit.value) {
    return;
  }
  genCodeLoading.value = true;
  try {
    const preview = await fetchCustomerNextCode();
    form.code = preview.code;
    // 之前「超长」之类的提示已不成立，清掉免得红框跟着新号走
    delete formErrors.value.code;
  } catch (error) {
    MessagePlugin.error(`生成编码失败：${messageOf(error)}`);
  } finally {
    genCodeLoading.value = false;
  }
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
      // 额度 / 策略 / 冻结状态可能都变了：让信用面板重拉（同一家客户 watch 不到）
      creditRefreshKey.value += 1;
    } else {
      const created = await createCustomer(buildCustomerCreatePayload(form));
      // 没点「生成编码」直接保存时编码由后端取号 —— 回填表单 + 提示里报出来（验收 ④）
      form.code = created.code;
      MessagePlugin.success(`客户已创建 · 编码 ${created.code}`);
    }
    dialogVisible.value = false;
    await reloadList(editing.value ? current.value : 1);
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
    await reloadList(current.value);
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
        await reloadList(current.value);
      } catch (error) {
        MessagePlugin.error(messageOf(error));
      }
    },
  });
}

// ==================== 初始化 ====================

onMounted(async () => {
  void reloadList(1);
  // CODE-08：编码策略单独取（失败按「允许手填」兜底，不打断列表）
  allowManualCode.value = await loadAllowManualCode();
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
/* ATT-11：编辑弹窗里的附件区（小标题 + 一句建议说明） */
.section-title {
  font-weight: 600;
  font-size: 14px;
  margin: 12px 0 4px;
}

/* M3-14：信用区块标题旁的副说明（小字、不抢标题） */
.section-hint {
  margin-left: 8px;
  font-weight: 400;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.dlg-attach-tip {
  margin: 0 0 6px;
  font-size: 12px;
  color: var(--td-text-color-secondary);
}
</style>
