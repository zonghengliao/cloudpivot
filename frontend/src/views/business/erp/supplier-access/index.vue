<template>
  <div class="sa-page">
    <!-- 没有取数码：页面能进（有页面显形码）但列表接口会 403 —— 说清缺什么，不摆一个点不动的按钮 -->
    <t-alert
      v-if="!canList"
      theme="warning"
      class="sa-alert"
      data-testid="sa-no-perm"
      :message="`当前账号没有准入申请单的取数权限（${PERM.list}），请让管理员在角色授权里勾上「查看列表」那一行`"
    />

    <ListPage
      v-else
      ref="listRef"
      title="供应商准入"
      testid="sa-list"
      table-testid="sa-list-table"
      meta-unit="张"
      :columns="columns"
      :filters="filters"
      :filter-state="filter"
      :fetcher="fetcher"
      :query-builder="queryBuilder"
      :page-size="PAGE_SIZE"
      show-page-size
    >
      <template #toolbar>
        <t-tooltip
          :content="
            canCreate
              ? '发起一张准入 / 黑名单 / 解除申请：审批通过后供应商状态才会变（拉黑 → 停用，准入 / 解除 → 启用）'
              : `当前账号没有 ${PERM.create} 权限`
          "
          placement="top-right"
        >
          <span class="gate-wrap">
            <t-button theme="primary" :disabled="!canCreate" data-testid="sa-new" @click="openCreate">
              <template #icon><t-icon name="add" /></template>
              新建申请
            </t-button>
          </span>
        </t-tooltip>
      </template>

      <template #access_no="{ row }">
        <span class="mono">{{ row.access_no }}</span>
      </template>

      <template #supplier="{ row }">
        <div class="cell-stack">
          <span class="cell-strong">{{ row.supplier_name }}</span>
          <span class="cell-sub mono">{{ row.supplier_code }}</span>
        </div>
      </template>

      <template #access_type="{ row }">
        <div class="cell-stack">
          <t-tag :theme="typeTheme(row.access_type)" variant="light" size="small">
            {{ row.access_type_label || accessTypeLabel(row.access_type) }}
          </t-tag>
          <span class="cell-sub">通过后：{{ targetStateLabel(row) }}</span>
        </div>
      </template>

      <template #status="{ row }">
        <div class="cell-stack">
          <t-tag :theme="statusTheme(row.status)" variant="light" size="small">
            {{ row.status_label || accessStatusLabel(row.status) }}
          </t-tag>
          <span class="cell-sub">{{ decidedText(row) }}</span>
        </div>
      </template>

      <template #reason="{ row }">
        <span class="cell-reason" :class="{ 'is-missing': !row.reason }">
          {{ row.reason?.trim() ? row.reason : '（未填理由，提交前必须补 ≥5 字）' }}
        </span>
      </template>

      <template #apply="{ row }">
        <div class="cell-stack">
          <span>{{ applicantText(row) }}</span>
          <span class="cell-sub mono">{{ timeText(row.created_at) }}</span>
        </div>
      </template>

      <template #op="{ row }">
        <t-space size="small" break-line>
          <t-link theme="primary" data-testid="sa-row-detail" @click="openDetail(row)">详情</t-link>
          <!-- 写入口逐条 gate：只有草稿能改 / 提 / 删（判据来自后端 editable / submittable，不自己推状态机） -->
          <t-tooltip v-if="canUpdate" :content="row.editable ? '改草稿的理由 / 等级 / 备注（类型与供应商不可改）' : '只有草稿能编辑'" placement="top">
            <span class="gate-wrap">
              <t-link
                theme="primary"
                data-testid="sa-row-edit"
                :disabled="!canEditRow(row)"
                @click="openEdit(row)"
              >
                编辑
              </t-link>
            </span>
          </t-tooltip>
          <t-tooltip v-if="canSubmit" :content="canSubmitRow(row) ? '提交后走 supplier_access 审批流' : submitBlockedTip(row)" placement="top">
            <span class="gate-wrap">
              <t-link
                theme="primary"
                data-testid="sa-row-submit"
                :disabled="!canSubmitRow(row)"
                @click="openSubmit(row)"
              >
                提交
              </t-link>
            </span>
          </t-tooltip>
          <t-tooltip v-if="canUpdate" :content="row.editable ? '软删这张草稿（不可恢复）' : '只有草稿能作废'" placement="top">
            <span class="gate-wrap">
              <t-link
                theme="danger"
                data-testid="sa-row-delete"
                :disabled="!canEditRow(row)"
                @click="handleDelete(row)"
              >
                作废
              </t-link>
            </span>
          </t-tooltip>
        </t-space>
      </template>

      <template #footer>
        <div class="sa-note" data-testid="sa-list-note">
          三种动作走同一张申请单、同一个审批流：<span class="mono">准入</span>（首次进合格名录）、
          <span class="mono">黑名单</span>（停用且不可下单，采购经理 → 财务经理两级）、
          <span class="mono">解除黑名单</span>（恢复启用）。<b>提交与审批通过都不直接改主数据</b> ——
          只有结论为「通过」时才写供应商状态（唯一入口在后端）。驳回 / 撤回会打回草稿，
          供应商状态一个字节不动；同一家供应商同时只允许一张未结申请。
        </div>
      </template>

      <template #empty>
        <div class="cell-sub">{{ emptyListText(filter) }}</div>
      </template>
    </ListPage>

    <!-- ==================== 新建 / 编辑（编辑只有草稿能进） ==================== -->
    <t-dialog
      v-model:visible="formVisible"
      :header="formHeader"
      width="720px"
      :confirm-btn="{ content: '保存草稿', loading: saving }"
      :close-on-overlay-click="false"
      data-testid="sa-form-dialog"
      @confirm="handleSave"
    >
      <t-form label-width="96px" class="sa-form">
        <t-form-item label="供应商" required-mark>
          <!-- 编辑草稿时供应商锁定：后端 `UpdateAccessReq` 根本不接 supplier_id（类型与供应商不可改） -->
          <t-select
            v-model="form.supplier_id"
            :options="supplierOptions"
            :loading="supplierLoading"
            :disabled="Boolean(form.id)"
            placeholder="输入编码 / 名称搜索（含已停用的供应商）"
            filterable
            clearable
            data-testid="sa-form-supplier"
            @search="searchSuppliers"
          />
        </t-form-item>

        <t-form-item label="申请类型" required-mark>
          <t-radio-group
            :value="form.access_type"
            :disabled="Boolean(form.id)"
            data-testid="sa-form-type"
            @change="handleTypeChange"
          >
            <t-radio v-for="item in TYPE_OPTIONS" :key="item.value" :value="item.value">
              {{ item.label }}
            </t-radio>
          </t-radio-group>
          <div class="sa-form-hint" data-testid="sa-form-type-hint">{{ typeHint(form.access_type) }}</div>
        </t-form-item>

        <t-form-item v-if="form.access_type === 'BLACKLIST'" label="评估等级">
          <t-select
            v-model="form.grade_snapshot"
            :options="GRADE_OPTIONS"
            placeholder="拉黑依据（取最近一期评估等级）"
            clearable
            data-testid="sa-form-grade"
          />
          <div class="sa-form-hint">等级快照只对黑名单有意义；准入 / 解除单传它会被后端 400。</div>
        </t-form-item>

        <t-form-item label="申请理由">
          <t-textarea
            v-model="form.reason"
            :autosize="{ minRows: 3, maxRows: 6 }"
            :placeholder="`谁、为什么、依据什么（草稿可空；提交前至少 ${MIN_REASON_CHARS} 个字）`"
            data-testid="sa-form-reason"
          />
          <div class="sa-form-hint" data-testid="sa-form-reason-hint">
            当前 {{ reasonLength(form.reason) }} 个字{{ reasonError(form.reason) ? '（未达提交要求）' : '（够提交）' }}
          </div>
        </t-form-item>

        <t-form-item label="备注">
          <t-textarea
            v-model="form.remark"
            :autosize="{ minRows: 2, maxRows: 4 }"
            placeholder="补充说明 / 依据附件编号（审批人会看）"
            data-testid="sa-form-remark"
          />
        </t-form-item>
      </t-form>
    </t-dialog>

    <!-- ==================== 提交审批 ==================== -->
    <t-dialog
      v-model:visible="submitVisible"
      header="提交审批"
      width="560px"
      :confirm-btn="{ content: '提交', loading: submitting }"
      :close-on-overlay-click="false"
      data-testid="sa-submit-dialog"
      @confirm="handleSubmit"
    >
      <div class="sa-submit-body">
        <div class="cell-sub" data-testid="sa-submit-target">
          申请单 <span class="mono">{{ submitTarget?.access_no }}</span> ·
          {{ submitTarget?.access_type_label || accessTypeLabel(submitTarget?.access_type ?? '') }} ·
          {{ submitTarget?.supplier_name }}
        </div>
        <t-textarea
          v-model="submitReason"
          :autosize="{ minRows: 3, maxRows: 6 }"
          :placeholder="`申请理由（至少 ${MIN_REASON_CHARS} 个字；留空则用草稿里已填的）`"
          data-testid="sa-submit-reason"
        />
        <t-alert
          v-if="submitTarget && submitTarget.access_type === 'BLACKLIST'"
          theme="warning"
          :message="'拉黑会让这家供应商立刻不可下单（影响在手订单与应付），所以要加一道财务经理复核'"
        />
      </div>
    </t-dialog>

    <!-- ==================== 详情抽屉 ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      :header="detailTitle(detail)"
      size="760px"
      :footer="false"
      data-testid="sa-detail-drawer"
    >
      <t-loading :loading="detailLoading" size="small">
        <t-alert v-if="detailError" theme="error" :message="detailError" data-testid="sa-detail-error" />

        <template v-if="detail">
          <div class="sa-detail-head">
            <t-tag :theme="typeTheme(detail.access_type)" variant="light">
              {{ detail.access_type_label || accessTypeLabel(detail.access_type) }}
            </t-tag>
            <t-tag :theme="statusTheme(detail.status)" variant="light">
              {{ detail.status_label || accessStatusLabel(detail.status) }}
            </t-tag>
            <t-space size="small" class="sa-detail-ops">
              <t-link
                v-if="canUpdate && detail.editable"
                theme="primary"
                data-testid="sa-detail-edit"
                @click="openEdit(detail)"
              >
                编辑草稿
              </t-link>
              <t-link
                v-if="canSubmit && detail.submittable"
                theme="primary"
                data-testid="sa-detail-submit"
                @click="openSubmit(detail)"
              >
                提交审批
              </t-link>
              <t-link
                v-if="detail.approval_instance_id"
                theme="default"
                data-testid="sa-detail-instance"
                @click="openApprovalInstance(detail.approval_instance_id)"
              >
                审批详情
              </t-link>
            </t-space>
          </div>

          <t-descriptions :column="2" size="small" bordered class="sa-detail-desc">
            <t-descriptions-item label="申请单号">
              <span class="mono">{{ detail.access_no }}</span>
            </t-descriptions-item>
            <t-descriptions-item label="供应商">
              <div class="cell-stack">
                <span>{{ detail.supplier_name }}</span>
                <span class="cell-sub mono">{{ detail.supplier_code }}</span>
              </div>
            </t-descriptions-item>
            <t-descriptions-item label="供应商现值">
              <t-tag :theme="detail.supplier_status === 1 ? 'success' : 'danger'" variant="outline" size="small">
                {{ detail.supplier_status_label || supplierStateLabel(detail.supplier_status) }}
              </t-tag>
            </t-descriptions-item>
            <t-descriptions-item label="通过后">
              {{ targetStateLabel(detail) }}
            </t-descriptions-item>
            <t-descriptions-item label="评估等级">
              {{ detail.grade_snapshot || '—' }}
            </t-descriptions-item>
            <t-descriptions-item label="申请人">
              {{ applicantText(detail) }}
            </t-descriptions-item>
            <t-descriptions-item label="申请时间">
              <span class="mono">{{ timeText(detail.created_at) }}</span>
            </t-descriptions-item>
            <t-descriptions-item label="结论">
              {{ decidedText(detail) }}
            </t-descriptions-item>
            <t-descriptions-item label="申请理由" :span="2">
              <div class="cell-pre" :class="{ 'is-missing': !detail.reason }">
                {{ detail.reason?.trim() ? detail.reason : '（未填理由：提交审批前必须补 ≥5 字）' }}
              </div>
            </t-descriptions-item>
            <t-descriptions-item label="备注" :span="2">
              <div class="cell-pre">{{ detail.remark?.trim() ? detail.remark : '—' }}</div>
            </t-descriptions-item>
          </t-descriptions>

          <!-- 审批进度：只有挂到实例的单据才画（草稿 / 打回的单据不摆空标题） -->
          <template v-if="hasApprovalProgress(approvalProgress)">
            <div class="section-title">{{ APPROVAL_PROGRESS_TITLE }}</div>
            <FlowProgress :nodes="approvalProgress.nodes" :tasks="approvalProgress.tasks" />
          </template>
        </template>
      </t-loading>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
/**
 * 供应商准入 / 黑名单页面（M4-14 的后端 + 本卡的前端）。
 *
 * # 三条口径（照抄后端，别在页面里重新发明）
 *
 * | 谁 | 判什么 |
 * |----|--------|
 * | 后端 `to_row_res` | `editable`（草稿可改可删）、`submittable`（草稿 + 理由 ≥5 字）、`target_supplier_status`（通过后变成什么） |
 * | 本页 | 只根据上面三个字段决定**按钮显隐 / 置灰**，不自己写状态机；越态请求由后端 409 报文兜 |
 * | 路由 `meta.permission` | 页面显形码 `business:erp:supplier-access:page`（后端零引用，本文件不拿它判任何东西） |
 *
 * # 一个必须记住的业务事实
 *
 * **提交与审批通过都不改主数据**：只有审批结论为「通过」时才写 `biz_supplier.status`
 * （唯一入口 `supplier_access_flow::settle`）。所以本页任何按钮都不会让供应商「立刻」
 * 被拉黑 —— 拉黑生效点在审批通过那一刻。
 */
import { computed, reactive, ref } from 'vue';
import { useRouter } from 'vue-router';
import { DialogPlugin, MessagePlugin } from 'tdesign-vue-next';
import ListPage from '@/components/ListPage/index.vue';
import type { ColumnDef, FilterDef, ListFetcher, ListPageData } from '@/components/ListPage/types';
import { ApiError } from '@/api/request';
import { useUserStore } from '@/stores/user';
import {
  fetchSupplierPage,
  type Supplier,
} from '@/api/business/erp';
import {
  createAccess,
  deleteAccess,
  fetchAccessDetail,
  fetchAccessPage,
  submitAccess,
  updateAccess,
  type AccessQuery,
  type AccessRow,
  type AccessType,
  type AccessGrade,
} from '@/api/business/erp/supplier-access';
import { getFlowInstance } from '@/api/business/flow-api';
import FlowProgress from '@/views/business/flow/components/FlowProgress.vue';
import {
  APPROVAL_PROGRESS_TITLE,
  EMPTY_APPROVAL_PROGRESS,
  hasApprovalProgress,
  loadApprovalProgress,
  type ApprovalProgress,
} from '@/views/business/erp/shared/approval-progress';
import {
  GRADE_OPTIONS,
  MIN_REASON_CHARS,
  PAGE_SIZE,
  STATUS_OPTIONS,
  TYPE_OPTIONS,
  accessStatusLabel,
  accessTypeLabel,
  applicantText,
  buildAccessQuery,
  canEditRow,
  canSubmitRow,
  decidedText,
  detailTitle,
  emptyAccessFilter,
  emptyListText,
  keywordWarning,
  reasonError,
  reasonLength,
  statusTheme,
  submitBlockedTip,
  supplierStateLabel,
  targetStateLabel,
  timeText,
  typeTheme,
  typeHint,
  type AccessFilterState,
} from './supplier-access-logic';

/** 本页用到的权限码（与 `V0455` 的 `sys_menu.auth_code` 逐字一致） */
const PERM = {
  /** 页面显形（**只在路由 meta 用**，本文件不拿它判断任何东西） */
  page: 'business:erp:supplier-access:page',
  /** 列表 + 详情（按钮行 …4485） */
  list: 'business:erp:supplier-access:list',
  /** 新建（…4486） */
  create: 'business:erp:supplier-access:create',
  /** 改草稿 + 作废（一个码管两个动作，…4487） */
  update: 'business:erp:supplier-access:update',
  /** 提交审批（…4488） */
  submit: 'business:erp:supplier-access:submit',
} as const;

const router = useRouter();
const userStore = useUserStore();

const canList = computed(() => userStore.hasPermission(PERM.list));
const canCreate = computed(() => userStore.hasPermission(PERM.create));
const canUpdate = computed(() => userStore.hasPermission(PERM.update));
const canSubmit = computed(() => userStore.hasPermission(PERM.submit));

const listRef = ref<InstanceType<typeof ListPage> | null>(null);

/** 错误文案归一（后端 400 / 409 的 message 就是给人看的，原样展示） */
function errorText(error: unknown, fallback: string): string {
  if (error instanceof ApiError) {
    return error.message || fallback;
  }
  if (error instanceof Error) {
    return error.message || fallback;
  }
  return fallback;
}

// ==================== 列表 ====================

const filter = ref<AccessFilterState>(emptyAccessFilter());

const filters: FilterDef[] = [
  {
    key: 'keyword',
    type: 'keyword',
    placeholder: '单号 / 供应商编码 / 供应商名 / 理由',
    width: 320,
    testid: 'sa-filter-keyword',
  },
  { key: 'status', type: 'select', options: STATUS_OPTIONS, placeholder: '全部状态', width: 140, testid: 'sa-filter-status' },
  {
    key: 'access_type',
    type: 'select',
    options: TYPE_OPTIONS.map((item) => ({ value: item.value, label: item.label })),
    placeholder: '全部类型',
    width: 150,
    testid: 'sa-filter-type',
  },
];

const queryBuilder = (state: AccessFilterState, page: number, pageSize: number) =>
  buildAccessQuery(state, page, pageSize);

const columns: ColumnDef[] = [
  { colKey: 'access_no', title: '申请单号', width: 170, fixed: 'left' },
  { colKey: 'supplier', title: '供应商', minWidth: 180 },
  { colKey: 'access_type', title: '类型 / 通过后', width: 150 },
  { colKey: 'status', title: '状态 / 结论', width: 170 },
  { colKey: 'reason', title: '申请理由', minWidth: 220 },
  { colKey: 'apply', title: '申请人 / 时间', width: 150 },
  { colKey: 'op', title: '操作', width: 180, fixed: 'right' },
];

const fetcher: ListFetcher<AccessRow> = async (query): Promise<ListPageData<AccessRow>> => {
  const warn = keywordWarning(filter.value.keyword);
  if (warn) {
    // 后端会 400；先给一条本地提示，再把请求照发 —— 让用户看到后端的原文案
    MessagePlugin.warning(warn);
  }
  try {
    return await fetchAccessPage(query as AccessQuery);
  } catch (error) {
    throw new Error(errorText(error, '取准入申请单列表失败'));
  }
};

async function reloadList(): Promise<void> {
  await listRef.value?.load(listRef.value.current);
}

// ==================== 供应商下拉（懒加载 + 远端搜索） ====================

/**
 * 供应商候选。
 *
 * **不能用 `fetchSupplierOptions`**（那个写死 `status: 1`）：解除黑名单要选的供应商
 * 恰恰是停用的（`status = 0`），过滤掉启用态就永远选不到它。这里走
 * `fetchSupplierPage({ keyword })`，不带 status → 全部都在，并在标签上标出「停用」。
 *
 * 依赖 `business:erp:supplier:list`（主数据列表码）：没有它时下拉为空 + 一条提示，
 * 不静默变成「选不了供应商还以为没数据」。
 */
const supplierOptions = ref<{ label: string; value: string }[]>([]);
const supplierLoading = ref(false);
let supplierLoaded = false;

async function loadSuppliers(keyword = ''): Promise<void> {
  supplierLoading.value = true;
  try {
    const page = await fetchSupplierPage({ keyword: keyword.trim() || undefined, page: 1, page_size: 50 });
    supplierOptions.value = (page.list ?? []).map((item: Supplier) => ({
      label: `${item.supplier_code} ${item.supplier_name}${item.status === 1 ? '' : '（停用）'}`,
      value: item.id,
    }));
    supplierLoaded = true;
  } catch (error) {
    if (!supplierLoaded) {
      MessagePlugin.warning(errorText(error, '供应商下拉加载失败：可能缺 business:erp:supplier:list 权限'));
    }
  } finally {
    supplierLoading.value = false;
  }
}

function searchSuppliers(keyword: string): void {
  void loadSuppliers(keyword);
}

/** 编辑草稿回填时，把当前供应商塞进候选（否则远端搜索结果里没有它，选择框会显示空白） */
function ensureSupplierOption(row: Pick<AccessRow, 'supplier_id' | 'supplier_code' | 'supplier_name'>): void {
  if (supplierOptions.value.some((item) => item.value === row.supplier_id)) {
    return;
  }
  supplierOptions.value = [
    { label: `${row.supplier_code} ${row.supplier_name}`, value: row.supplier_id },
    ...supplierOptions.value,
  ];
}

// ==================== 新建 / 编辑 ====================

/** 表单状态 */
type FormState = {
  /** 编辑时的申请单 id（空 = 新建） */
  id: string;
  /** 申请单号（编辑时展示用） */
  access_no: string;
  /** 供应商 id */
  supplier_id: string;
  /** 申请类型 */
  access_type: AccessType;
  /** 评估等级快照（只有黑名单可填） */
  grade_snapshot: AccessGrade | '';
  /** 申请理由 */
  reason: string;
  /** 备注 */
  remark: string;
};

function emptyForm(): FormState {
  return { id: '', access_no: '', supplier_id: '', access_type: 'ADMIT', grade_snapshot: '', reason: '', remark: '' };
}

const formVisible = ref(false);
const saving = ref(false);
const form = reactive<FormState>(emptyForm());

const formHeader = computed(() =>
  form.id ? `编辑申请单 ${form.access_no}（只有草稿能改：类型与供应商已锁定）` : '新建准入 / 黑名单申请',
);

function handleTypeChange(value: unknown): void {
  form.access_type = (value as AccessType) ?? 'ADMIT';
  // 类型切走黑名单 → 等级快照必须清空（后端 `ck_biz_sa_grade_only_blacklist`，传了就是 400）
  if (form.access_type !== 'BLACKLIST') {
    form.grade_snapshot = '';
  }
}

function openCreate(): void {
  Object.assign(form, emptyForm());
  if (!supplierLoaded) {
    void loadSuppliers();
  }
  formVisible.value = true;
}

/** 编辑：类型与供应商不可改（后端 `UpdateAccessReq` 只接 reason / grade_snapshot / remark） */
function openEdit(row: AccessRow): void {
  Object.assign(form, {
    id: row.id,
    access_no: row.access_no,
    supplier_id: row.supplier_id,
    access_type: row.access_type,
    grade_snapshot: row.grade_snapshot ?? '',
    reason: row.reason ?? '',
    remark: row.remark ?? '',
  });
  ensureSupplierOption(row);
  if (!supplierLoaded) {
    void loadSuppliers();
  }
  formVisible.value = true;
}

/** 本地校验：只拦「后端一定会 400」的（供应商 / 类型必填；理由可以留空存草稿） */
function validateForm(state: FormState): string {
  if (!state.supplier_id) {
    return '请选择供应商（输入编码或名称搜索，含已停用的供应商）';
  }
  if (!state.access_type) {
    return '请选择申请类型（准入 / 黑名单 / 解除黑名单）';
  }
  if (state.grade_snapshot && state.access_type !== 'BLACKLIST') {
    return '评估等级快照只对黑名单有意义：准入 / 解除单不要填';
  }
  return '';
}

async function handleSave(): Promise<void> {
  const error = validateForm(form);
  if (error) {
    MessagePlugin.warning(error);
    return;
  }
  saving.value = true;
  try {
    const reason = form.reason.trim();
    const remark = form.remark.trim();
    if (form.id) {
      const saved = await updateAccess(form.id, {
        reason: reason || null,
        grade_snapshot: form.grade_snapshot || null,
        remark: remark || null,
      });
      MessagePlugin.success(`已保存草稿 ${saved.access_no}`);
    } else {
      const saved = await createAccess({
        supplier_id: form.supplier_id,
        access_type: form.access_type,
        reason: reason || null,
        grade_snapshot: form.grade_snapshot || null,
        remark: remark || null,
      });
      MessagePlugin.success(`已新建申请单 ${saved.access_no}（草稿）`);
    }
    formVisible.value = false;
    await reloadList();
  } catch (err) {
    MessagePlugin.error(errorText(err, '保存申请单失败'));
  } finally {
    saving.value = false;
  }
}

// ==================== 提交审批 ====================

const submitVisible = ref(false);
const submitting = ref(false);
const submitReason = ref('');
const submitTarget = ref<AccessRow | null>(null);

function openSubmit(row: AccessRow): void {
  submitTarget.value = row;
  submitReason.value = row.reason ?? '';
  submitVisible.value = true;
}

async function handleSubmit(): Promise<void> {
  const target = submitTarget.value;
  if (!target) {
    return;
  }
  // 理由是「请求值优先、否则草稿值」——所以只有两处都空 / 都太短才拦
  const effective = submitReason.value.trim() || (target.reason ?? '');
  const error = reasonError(effective);
  if (error) {
    MessagePlugin.warning(error);
    return;
  }
  submitting.value = true;
  try {
    const saved = await submitAccess(target.id, { reason: submitReason.value.trim() || undefined });
    MessagePlugin.success(`${saved.access_no} 已提交审批（采购经理${saved.access_type === 'BLACKLIST' ? ' → 财务经理复核' : ''}）`);
    submitVisible.value = false;
    await reloadList();
    if (detailVisible.value) {
      await loadDetail(saved.id);
    }
  } catch (err) {
    MessagePlugin.error(errorText(err, '提交审批失败'));
  } finally {
    submitting.value = false;
  }
}

// ==================== 作废（软删草稿） ====================

function handleDelete(row: AccessRow): void {
  const dialog = DialogPlugin.confirm({
    header: '作废申请单',
    theme: 'danger',
    body: `确认作废 ${row.access_no}（${row.supplier_name}）？只有草稿能作废，作废后不可恢复；审批中的单要终止请走审批中心撤回。`,
    confirmBtn: '作废',
    onConfirm: async () => {
      try {
        await deleteAccess(row.id);
        MessagePlugin.success(`${row.access_no} 已作废`);
        dialog.hide();
        if (detailVisible.value) {
          detailVisible.value = false;
        }
        await reloadList();
      } catch (error) {
        MessagePlugin.error(errorText(error, '作废失败'));
      }
    },
  });
}

// ==================== 详情抽屉 ====================

const detailVisible = ref(false);
const detailLoading = ref(false);
const detailError = ref('');
const detail = ref<AccessRow | null>(null);
const approvalProgress = ref<ApprovalProgress>({ ...EMPTY_APPROVAL_PROGRESS });

async function loadDetail(id: string): Promise<void> {
  detailLoading.value = true;
  detailError.value = '';
  approvalProgress.value = { ...EMPTY_APPROVAL_PROGRESS };
  try {
    const row = await fetchAccessDetail(id);
    detail.value = row;
    // 审批进度拉不到不报错（`loadApprovalProgress` 内部吃掉异常，退化成只有「审批详情」入口）
    approvalProgress.value = await loadApprovalProgress(row.approval_instance_id, getFlowInstance);
  } catch (error) {
    detail.value = null;
    detailError.value = errorText(error, '加载申请单详情失败');
  } finally {
    detailLoading.value = false;
  }
}

function openDetail(row: AccessRow): void {
  detail.value = row;
  detailVisible.value = true;
  void loadDetail(row.id);
}

/** 跳到审批实例页（路由是 `/flow/instance/:id`，要实例 id 不是单号） */
function openApprovalInstance(instanceId: string | null): void {
  if (!instanceId) {
    return;
  }
  void router.push({ path: `/flow/instance/${instanceId}` });
}
</script>

<style scoped>
.sa-page {
  padding: 16px;
}

.sa-alert {
  margin-bottom: 12px;
}

.cell-stack {
  display: flex;
  flex-direction: column;
  gap: 2px;
  line-height: 1.35;
}

.cell-strong {
  font-weight: 500;
}

.cell-sub {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

/* 理由：列表里最多两行，多的省略（详情抽屉里给全文） */
.cell-reason {
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
  line-height: 1.45;
}

/* 缺理由：灰斜体，与「有理由」在视觉上分开 */
.is-missing {
  color: var(--td-text-color-placeholder);
  font-style: italic;
}

.cell-pre {
  white-space: pre-wrap;
  word-break: break-word;
  line-height: 1.55;
}

.mono {
  font-family: var(--td-font-family-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
  font-variant-numeric: tabular-nums;
}

/* 被 tooltip 包住的按钮/链接需要一个能接鼠标事件的盒子 */
.gate-wrap {
  display: inline-block;
}

.sa-note {
  color: var(--td-text-color-secondary);
  font-size: 12px;
  line-height: 1.6;
  padding-top: 8px;
}

.sa-form-hint {
  color: var(--td-text-color-secondary);
  font-size: 12px;
  line-height: 1.5;
  padding-top: 4px;
}

.sa-submit-body {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.sa-detail-head {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 12px;
}

.sa-detail-ops {
  margin-left: auto;
}

.sa-detail-desc {
  margin-bottom: 8px;
}

.section-title {
  font-size: 14px;
  font-weight: 600;
  margin: 16px 0 8px;
}
</style>
