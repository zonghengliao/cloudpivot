<template>
  <div class="unit-page">
    <!-- 筛选行：关键词 / 状态 / 查询 / 重置 / 刷新 / 新建 -->
    <t-card :bordered="false" class="filter-card">
      <div class="filter-row">
        <t-input
          v-model="filter.keyword"
          class="filter-row__keyword"
          placeholder="搜索单位编码 / 名称"
          clearable
          data-testid="unit-keyword"
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
          data-testid="unit-status-filter"
        />

        <t-button theme="primary" data-testid="btn-unit-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-unit-reset" @click="handleReset">重置</t-button>
        <t-button variant="outline" data-testid="btn-unit-refresh" @click="handleRefresh">刷新</t-button>

        <span class="filter-row__spacer" />

        <t-tooltip :content="canWrite ? '新建一个计量单位' : '当前账号没有 business:erp:unit:create 权限'" placement="top-right">
          <span class="gate-wrap">
            <t-button
              theme="primary"
              data-testid="btn-unit-new"
              :disabled="!canWrite"
              @click="openCreate"
            >
              <template #icon><t-icon name="add" /></template>
              新建单位
            </t-button>
          </span>
        </t-tooltip>
      </div>
    </t-card>

    <!-- 列表 -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">计量单位</span>
          <span class="panel-head__meta" data-testid="unit-meta">
            共 {{ total }} 条<template v-if="baseCountOnPage > 0"> · 本页 {{ baseCountOnPage }} 个基础单位</template>
          </span>
        </div>
      </div>

      <t-alert v-if="loadError" theme="error" :message="loadError" class="list-alert">
        <template #operation>
          <t-button size="small" variant="outline" @click="loadUnits()">重试</t-button>
        </template>
      </t-alert>

      <t-table
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        :hover="true"
        data-testid="unit-table"
        @page-change="handlePageChange"
      >
        <template #is_system="{ row }">
          <t-tag
            size="small"
            :theme="unitSourceTheme(row.is_system)"
            :variant="row.is_system ? 'light' : 'outline'"
            data-testid="unit-source-tag"
          >
            {{ unitSourceLabel(row.is_system) }}
          </t-tag>
        </template>

        <template #is_base="{ row }">
          <t-tag size="small" :theme="unitBaseTheme(row.is_base)" :variant="row.is_base ? 'light' : 'outline'">
            {{ unitBaseLabel(row.is_base) }}
          </t-tag>
        </template>

        <template #rate="{ row }">
          <span class="mono">{{ rateLabel(row, unitBaseNameOf(row, unitDict)) }}</span>
        </template>

        <template #precision="{ row }">
          <span class="mono muted" data-testid="unit-precision">{{ precisionLabel(row.precision) }}</span>
        </template>

        <template #sort_order="{ row }">
          <span class="mono muted">{{ row.sort_order }}</span>
        </template>

        <template #status="{ row }">
          <t-tag size="small" :theme="unitStatusTheme(row.status)" variant="light" data-testid="unit-status-tag">
            {{ unitStatusLabel(row.status) }}
          </t-tag>
        </template>

        <template #op="{ row }">
          <div class="op-row">
            <t-link theme="primary" data-testid="btn-unit-view" @click="openDetail(row)">查看</t-link>
            <t-link
              v-privilege="[PERM.update]"
              theme="primary"
              data-testid="btn-unit-edit"
              @click="openEdit(row)"
            >
              编辑
            </t-link>
            <t-link
              v-privilege="[PERM.update]"
              v-if="canRemoveUnit(row)"
              theme="danger"
              data-testid="btn-unit-delete"
              @click="confirmRemove(row)"
            >
              删除
            </t-link>
            <t-tooltip v-else content="系统预置单位不能删除，只能停用（编辑 → 关闭「启用」）" placement="top-right">
              <span class="muted" data-testid="unit-delete-locked">系统预置</span>
            </t-tooltip>
          </div>
        </template>

        <template #empty>
          <div class="panel-empty" data-testid="unit-empty">
            <t-icon class="panel-empty__icon" name="measurement" />
            <p class="panel-empty__title">{{ isFiltered ? '没有匹配的计量单位' : '还没有计量单位' }}</p>
            <p class="panel-empty__desc">
              {{
                isFiltered
                  ? '换个关键词 / 状态试试，或点「重置」看全部'
                  : canWrite
                    ? '点右上角「新建单位」建第一个；基础单位（个 / 千克）先建，折算单位（箱 / 打）要指向它'
                    : '当前账号没有新建权限，请联系管理员在主数据里补单位'
              }}
            </p>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- 详情抽屉 -->
    <t-drawer v-model:visible="detailVisible" size="560px" :footer="false" :header="false" data-testid="unit-drawer">
      <div v-if="detail" class="drawer" data-testid="unit-detail">
        <div class="drawer-head">
          <div class="drawer-head__main">
            <span class="drawer-head__title mono">{{ detail.unit_code }}</span>
            <t-tag size="small" :theme="unitStatusTheme(detail.status)">{{ unitStatusLabel(detail.status) }}</t-tag>
            <t-tag size="small" variant="outline" :theme="unitBaseTheme(detail.is_base)">
              {{ unitBaseLabel(detail.is_base) }}
            </t-tag>
          </div>
          <div class="drawer-head__side">
            <t-button size="small" variant="outline" data-testid="btn-unit-drawer-close" @click="detailVisible = false">
              关闭
            </t-button>
          </div>
        </div>

        <div class="drawer-body">
          <div class="field-grid">
            <div class="field">
              <span class="field__label">单位编码</span>
              <span class="field__value mono">{{ detail.unit_code }}</span>
            </div>
            <div class="field">
              <span class="field__label">单位名称</span>
              <span class="field__value">{{ detail.unit_name }}</span>
            </div>
            <div class="field">
              <span class="field__label">换算关系</span>
              <span class="field__value mono">{{ rateLabel(detail, unitBaseNameOf(detail, unitDict)) }}</span>
            </div>
            <div class="field">
              <span class="field__label">折算基准单位</span>
              <span class="field__value">{{ unitBaseNameOf(detail, unitDict) || '—' }}</span>
            </div>
            <div class="field">
              <span class="field__label">排序值</span>
              <span class="field__value mono">{{ detail.sort_order }}</span>
            </div>
            <div class="field">
              <span class="field__label">状态</span>
              <span class="field__value">{{ unitStatusLabel(detail.status) }}</span>
            </div>
            <div class="field field--wide">
              <span class="field__label">备注</span>
              <span class="field__value">{{ detail.remark || '—' }}</span>
            </div>
            <div class="field field--wide">
              <span class="field__label">更新时间</span>
              <span class="field__value mono">{{ detail.updated_at }}</span>
            </div>
          </div>
        </div>
      </div>
    </t-drawer>

    <!-- 新建 / 编辑弹窗 -->
    <t-dialog
      v-model:visible="dialogVisible"
      :header="isEdit ? '编辑计量单位' : '新建计量单位'"
      :confirm-btn="{ content: isEdit ? '保存' : '创建', loading: saving }"
      :close-on-overlay-click="false"
      width="620px"
      data-testid="unit-dialog"
      @confirm="handleConfirm"
    >
      <t-alert v-if="submitError" theme="error" :message="submitError" class="dlg-alert" />

      <t-form :data="form" label-width="110px">
        <t-form-item
          label="单位编码"
          name="unit_code"
          :status="formErrors.unit_code ? 'error' : undefined"
          :tips="formErrors.unit_code"
        >
          <t-input
            v-model="form.unit_code"
            :disabled="isEdit"
            :maxlength="20"
            placeholder="如 PCS / KG / BOX（未删除内唯一）"
            data-testid="unit-code"
          />
          <template v-if="isEdit" #help>
            <span class="dlg-help">编码是单位身份，不支持修改</span>
          </template>
        </t-form-item>

        <t-form-item
          label="单位名称"
          name="unit_name"
          :status="formErrors.unit_name ? 'error' : undefined"
          :tips="formErrors.unit_name"
        >
          <t-input v-model="form.unit_name" :maxlength="50" placeholder="如 个 / 千克 / 箱" data-testid="unit-name" />
        </t-form-item>

        <t-form-item label="基础单位">
          <div class="dlg-inline">
            <t-switch v-model="form.is_base" data-testid="unit-is-base" />
            <span class="dlg-help" data-testid="unit-is-base-help">
              {{ form.is_base ? '基础单位：折算率恒为 1，没有折算基准' : '折算单位：必须指向一个基础单位并给折算率' }}
            </span>
          </div>
        </t-form-item>

        <t-form-item
          label="折算基准"
          name="base_unit_id"
          :status="formErrors.base_unit_id ? 'error' : undefined"
          :tips="formErrors.base_unit_id"
        >
          <t-select
            v-model="form.base_unit_id"
            :options="baseOptionList"
            :disabled="form.is_base"
            :placeholder="form.is_base ? '基础单位不需要折算基准' : '选一个基础单位'"
            clearable
            data-testid="unit-base-unit"
          />
        </t-form-item>

        <t-form-item label="折算率" name="rate" :status="formErrors.rate ? 'error' : undefined" :tips="formErrors.rate">
          <div class="dlg-inline">
            <t-input-number
              v-model="form.rate"
              :disabled="form.is_base"
              :min="0"
              :step="1"
              :decimal-places="6"
              theme="normal"
              data-testid="unit-rate"
            />
            <span class="dlg-help">{{ form.is_base ? '基础单位恒等 1' : '1 个本单位 = N 个基准单位' }}</span>
          </div>
        </t-form-item>

        <t-form-item label="排序">
          <t-input-number v-model="form.sort_order" :min="0" :step="1" theme="normal" data-testid="unit-sort" />
        </t-form-item>

        <t-form-item
          label="小数位数"
          name="precision"
          :status="formErrors.precision ? 'error' : undefined"
          :tips="formErrors.precision"
        >
          <div class="dlg-inline">
            <t-input-number
              v-model="form.precision"
              :min="0"
              :max="6"
              :step="1"
              theme="normal"
              data-testid="unit-precision-input"
            />
            <span class="dlg-help">个数 0 位；重量 / 长度 / 面积体积 3 位</span>
          </div>
        </t-form-item>

        <t-form-item label="启用">
          <t-switch v-model="enabledSwitch" data-testid="unit-status-switch" />
        </t-form-item>

        <t-form-item label="备注">
          <t-input v-model="form.remark" :maxlength="2000" placeholder="选填" data-testid="unit-remark" />
        </t-form-item>
      </t-form>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 计量单位页（P4-A）——标准主数据 CRUD：列表 + 详情抽屉 + 新建 / 编辑 + 软删。
 *
 * # 后端接口（开工前已按硬红线核对）
 *
 * `grep -n 'erp/unit' backend/src/business/erp/handler.rs` 命中 8 处：
 * `GET /business/erp/unit/page`、`GET|PUT|DELETE /business/erp/unit/{id}`、`POST /business/erp/unit`
 * —— **读写接口齐全**，所以本页做完整 CRUD（不是只读页）。
 * 表结构 `\d biz_unit` 核过：没有「小数位数」列，见 `unit-query.ts` 头注释。
 *
 * # 权限码（V0141 已种）
 *
 * `business:erp:unit:list` / `:create` / `:update`（**删除也用 `:update`**，后端 `unit_delete`
 * 的 `#[nik::permission]` 就是这么声明的）。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin } from 'tdesign-vue-next';
import { useUserStore } from '@/stores/user';
import {
  createUnit,
  deleteUnit,
  fetchUnitDetail,
  fetchUnitOptions,
  fetchUnitPage,
  updateUnit,
  type Unit,
  type UnitRow,
} from '@/api/business/erp';
import { ApiError } from '@/api/request';
import {
  UNIT_COLUMNS,
  UNIT_PAGE_SIZE,
  UNIT_STATUS_OPTIONS,
  baseUnitOptions,
  buildUnitCreatePayload,
  buildUnitQuery,
  buildUnitUpdatePayload,
  canRemoveUnit,
  emptyUnitFilter,
  emptyUnitForm,
  hasUnitFieldError,
  isUnitFiltered,
  precisionLabel,
  rateLabel,
  unitBaseLabel,
  unitBaseNameOf,
  unitBaseTheme,
  unitFormFromRow,
  unitSourceLabel,
  unitSourceTheme,
  unitStatusLabel,
  unitStatusTheme,
  validateUnitForm,
  type UnitFieldErrors,
  type UnitFilterState,
  type UnitFormState,
} from './unit-query';

/** 本页权限码（与 `backend/migrations/business/V0141__erp_permissions.up.sql` 的 auth_code 逐字一致） */
const PERM = {
  /** 列表（页面入口） */
  list: 'business:erp:unit:list',
  /** 新建 */
  create: 'business:erp:unit:create',
  /** 编辑 / 删除 */
  update: 'business:erp:unit:update',
} as const;

/** 表格列（colKey 与后端出参同名，插槽按 colKey 命中；`op` 是唯一非后端列） */
const columns = UNIT_COLUMNS;

/** 状态下拉（只读常量转可写数组） */
const statusOptions = UNIT_STATUS_OPTIONS.map((item) => ({ ...item }));

/**
 * 当前账号是否有新建权限。
 *
 * 卡面 ④ 要求「无写权限 → 按钮置灰 + tooltip 说明」，所以这里**不用** `v-privilege`
 * （那个指令是 `display:none`，工具提示永远看不到），改成 `hasPermission` + `:disabled`。
 * 行内的编辑 / 删除仍走 `v-privilege`：行内操作没有权限时直接不出现更干净。
 */
const userStore = useUserStore();
const canWrite = computed(() => userStore.hasPermission(PERM.create));

// ==================== 列表状态 ====================

/** 筛选行 */
const filter = reactive<UnitFilterState>(emptyUnitFilter());
/** 当前页数据 */
const rows = ref<Unit[]>([]);
/** 总条数 */
const total = ref(0);
/** 当前页码 */
const current = ref(1);
/** 每页条数 */
const pageSize = UNIT_PAGE_SIZE;
/** 加载中 */
const loading = ref(false);
/** 加载错误 */
const loadError = ref('');
/** 单位字典（把 `base_unit_id` 译成名字 + 「折算基准」下拉候选） */
const unitDict = ref<UnitRow[]>([]);

/** 是否筛选态 */
const isFiltered = computed(() => isUnitFiltered(filter));

/** 本页基础单位数（面板副标题用） */
const baseCountOnPage = computed(() => rows.value.filter((row) => row.is_base).length);

/** 表格分页器（服务端分页：只喂 current / pageSize / total） */
const pagination = computed(() => ({
  current: current.value,
  pageSize,
  total: total.value,
  showJumper: true,
  showPageSize: false,
}));

/** 「折算基准」下拉候选（只列基础单位，剔除自己） */
const baseOptionList = computed(() => baseUnitOptions(unitDict.value, editing.value?.id ?? ''));

/** 错误文案（后端 4xx 的 msg 比「请求失败」有用得多） */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

/**
 * 拉列表（服务端分页）。
 *
 * @param page 目标页码（默认当前页；筛选变化时传 1）
 */
async function loadUnits(page: number = current.value): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const data = await fetchUnitPage(buildUnitQuery(filter, page, pageSize));
    rows.value = data.list ?? [];
    total.value = data.total ?? 0;
    current.value = data.page ?? page;
    // 页码夹紧：筛到只剩 1 页时别停在不存在的第 3 页
    const lastPage = Math.max(1, Math.ceil(total.value / pageSize));
    if (current.value > lastPage) {
      current.value = lastPage;
      return loadUnits(lastPage);
    }
  } catch (error) {
    rows.value = [];
    total.value = 0;
    loadError.value = `计量单位加载失败：${messageOf(error)}`;
  } finally {
    loading.value = false;
  }
}

/** 拉单位字典（译名 + 下拉；失败不挡列表） */
async function loadDict(): Promise<void> {
  try {
    unitDict.value = await fetchUnitOptions();
  } catch {
    unitDict.value = [];
  }
}

/** 查询（回第 1 页） */
function handleSearch(): void {
  current.value = 1;
  void loadUnits(1);
}

/** 重置筛选 */
function handleReset(): void {
  Object.assign(filter, emptyUnitFilter());
  current.value = 1;
  void loadUnits(1);
}

/** 刷新（保持当前筛选与页码） */
function handleRefresh(): void {
  void loadDict();
  void loadUnits(current.value);
}

/** 切页 */
function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  current.value = pageInfo.current;
  void loadUnits(pageInfo.current);
}

// ==================== 详情抽屉 ====================

/** 抽屉可见 */
const detailVisible = ref(false);
/** 详情数据 */
const detail = ref<Unit | null>(null);

/**
 * 打开详情：先拿列表行垫上（点击即有内容，不闪空白），再用详情接口覆盖。
 *
 * `GET /erp/unit/{id}` 只多给了 `created_at` / `updated_at`，但走一次真接口能发现
 * 「行已过期 / 已被别人停用」——主数据页的值班场景就是看别人刚改了什么。
 */
async function openDetail(row: Unit): Promise<void> {
  detail.value = row;
  detailVisible.value = true;
  try {
    detail.value = await fetchUnitDetail(row.id);
  } catch (error) {
    MessagePlugin.warning(`详情没刷新出来，显示的是列表里的数据：${messageOf(error)}`);
  }
}

// ==================== 新建 / 编辑 ====================

/** 弹窗可见 */
const dialogVisible = ref(false);
/** 编辑对象；`null` = 新建 */
const editing = ref<Unit | null>(null);
/** 表单 */
const form = reactive<UnitFormState>(emptyUnitForm());
/** 表单错误 */
const formErrors = ref<UnitFieldErrors>({});
/** 提交错误 */
const submitError = ref('');
/** 提交中 */
const saving = ref(false);

const isEdit = computed(() => editing.value !== null);

/** 启用开关（`status` 1/0 与布尔互转） */
const enabledSwitch = computed({
  get: () => form.status === 1,
  set: (value: boolean) => {
    form.status = value ? 1 : 0;
  },
});

/** 打开新建 */
function openCreate(): void {
  editing.value = null;
  Object.assign(form, emptyUnitForm());
  formErrors.value = {};
  submitError.value = '';
  dialogVisible.value = true;
}

/** 打开编辑（回填） */
function openEdit(row: Unit): void {
  editing.value = row;
  Object.assign(form, unitFormFromRow(row));
  formErrors.value = {};
  submitError.value = '';
  dialogVisible.value = true;
}

/** 提交（新建 / 编辑） */
async function handleConfirm(): Promise<void> {
  const errors = validateUnitForm(form, isEdit.value);
  formErrors.value = errors;
  if (hasUnitFieldError(errors)) {
    return;
  }
  // 基础单位：折算基准与折算率都归位（后端会拦「基础单位不能有基准」「rate 必须 1」）
  if (form.is_base) {
    form.base_unit_id = '';
    form.rate = 1;
  }
  saving.value = true;
  submitError.value = '';
  try {
    if (editing.value) {
      await updateUnit(editing.value.id, buildUnitUpdatePayload(form, editing.value));
      MessagePlugin.success('计量单位已保存');
    } else {
      await createUnit(buildUnitCreatePayload(form));
      MessagePlugin.success('计量单位已创建');
    }
    dialogVisible.value = false;
    await Promise.all([loadDict(), loadUnits(editing.value ? current.value : 1)]);
  } catch (error) {
    submitError.value = messageOf(error);
  } finally {
    saving.value = false;
  }
}

// ==================== 删除 ====================

/**
 * 删除（软删）。被物料引用时后端 409：「还有 N 个物料在用该单位，不能删除」——
 * 原文弹给用户，别改写成「删除失败」。
 */
function confirmRemove(row: Unit): void {
  const dialog = DialogPlugin.confirm({
    header: '删除计量单位',
    theme: 'warning',
    body: `确认删除「${row.unit_name}（${row.unit_code}）」？删除后物料不能再选它；已被物料引用时后端会拒绝。`,
    confirmBtn: { content: '删除', theme: 'danger' },
    onConfirm: async () => {
      dialog.hide();
      try {
        await deleteUnit(row.id);
        MessagePlugin.success('计量单位已删除');
        await Promise.all([loadDict(), loadUnits(current.value)]);
      } catch (error) {
        MessagePlugin.error(messageOf(error));
      }
    },
  });
}

// ==================== 初始化 ====================

onMounted(async () => {
  // 字典与列表并行：字典拉不到不影响列表（只是「折算基准」译名与下拉是空的）
  await Promise.all([loadDict(), loadUnits(1)]);
});
</script>

<style scoped lang="less">
.unit-page {
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
  width: 260px;
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

.dlg-help {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

/* 禁用的按钮外面包一层 span，否则 t-tooltip 在 disabled 元素上不触发 */
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
