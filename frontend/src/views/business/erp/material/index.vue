<template>
  <div class="material-page">
    <!-- 筛选行：关键词 / 分类 / 一物一码 / 查询 / 重置 / 新建 -->
    <t-card :bordered="false" class="filter-card">
      <div class="filter-row">
        <t-input
          v-model="filter.keyword"
          class="filter-row__keyword"
          placeholder="搜索物料编码 / 名称 / 规格"
          clearable
          data-testid="mat-keyword"
          @enter="handleSearch"
        >
          <template #prefix-icon><t-icon name="search" /></template>
        </t-input>

        <t-select
          v-model="filter.material_type"
          class="filter-row__select"
          :options="typeOptions"
          placeholder="全部分类"
          clearable
          data-testid="mat-type"
        />

        <t-select
          v-model="filter.serial_required"
          class="filter-row__select filter-row__select--wide"
          :options="serialOptions"
          data-testid="mat-serial"
        />

        <t-button theme="primary" data-testid="btn-search" @click="handleSearch">查询</t-button>
        <t-button variant="outline" data-testid="btn-reset" @click="handleReset">重置</t-button>

        <span class="filter-row__spacer" />

        <ImportEntryButton
          biz-type="business.erp.material"
          biz-label="物料"
          testid="btn-import-material"
        />

        <t-button
          v-privilege="[PERM.materialCreate]"
          theme="primary"
          data-testid="btn-new-material"
          @click="openCreate"
        >
          <template #icon><t-icon name="add" /></template>
          新建物料
        </t-button>
      </div>
    </t-card>

    <!-- 列表 -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">物料列表</span>
          <span class="panel-head__meta" data-testid="mat-meta">
            共 {{ total }} 条<template v-if="serialCountOnPage > 0"> · 本页 {{ serialCountOnPage }} 条一物一码</template>
          </span>
        </div>
      </div>

      <t-alert v-if="loadError" theme="error" :message="loadError" class="list-alert">
        <template #operation>
          <t-button size="small" variant="outline" @click="loadMaterials()">重试</t-button>
        </template>
      </t-alert>

      <t-table
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        :hover="true"
        data-testid="material-table"
        @page-change="handlePageChange"
      >
        <template #serial_required="{ row }">
          <!-- 一物一码是 W2 的核心概念：有就挂 tag + ★，扫一眼就能捞出来 -->
          <t-tooltip v-if="isSerialRequired(row.serial_required)" content="一物一码：出入库需逐件登记序列号">
            <t-tag theme="warning" variant="light" size="small" data-testid="serial-flag" class="serial-tag">
              {{ serialRequiredLabel(row.serial_required) }}
            </t-tag>
          </t-tooltip>
          <span v-else class="serial-no" data-testid="serial-flag">{{ serialRequiredLabel(row.serial_required) }}</span>
        </template>

        <template #material_type="{ row }">
          <t-tag size="small" variant="outline">{{ materialTypeLabel(row.material_type) }}</t-tag>
        </template>

        <template #unit_id="{ row }">
          {{ unitNameOf(row.unit_id, units) }}
        </template>

        <template #status="{ row }">
          <t-tag size="small" :theme="row.status === 1 ? 'success' : 'default'" variant="light">
            {{ materialStatusLabel(row.status) }}
          </t-tag>
        </template>

        <template #op="{ row }">
          <t-link v-privilege="[PERM.materialUpdate]" theme="primary" data-testid="btn-edit" @click="openEdit(row)">
            编辑
          </t-link>
        </template>

        <template #empty>
          <div class="panel-empty" data-testid="materials-empty">
            <t-icon class="panel-empty__icon" name="box" />
            <p class="panel-empty__title">{{ isFiltered ? '没有匹配的物料' : '还没有物料' }}</p>
            <p class="panel-empty__desc">
              {{ isFiltered ? '换个关键词 / 分类试试，或点「重置」看全部' : '点右上角「新建物料」建第一个；一物一码物料记得打开那个开关' }}
            </p>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- 新建 / 编辑弹窗 -->
    <t-dialog
      v-model:visible="dialogVisible"
      :header="isEdit ? '编辑物料' : '新建物料'"
      :confirm-btn="{ content: isEdit ? '保存' : '创建', loading: saving }"
      :close-on-overlay-click="false"
      width="620px"
      data-testid="mat-dialog"
      @confirm="handleConfirm"
    >
      <t-alert v-if="submitError" theme="error" :message="submitError" class="dlg-alert" />

      <t-form :data="form" label-width="110px">
        <t-form-item label="物料编码" name="material_code" :status="formErrors.material_code ? 'error' : undefined" :tips="formErrors.material_code">
          <!-- CODE-06 交互方式 B：编码框 + 「生成编码」按钮（编辑态都禁用） -->
          <!-- CODE-08：系统参数关闭「允许手工指定编码」后编码栏只读、按钮隐藏 -->
          <div class="dlg-inline dlg-inline--grow">
            <t-input
              v-model="form.material_code"
              :disabled="manualCodeDisabled(isEdit, allowManualCode)"
              :maxlength="40"
              placeholder="留空自动生成，如 MAT-0001"
              class="dlg-inline__grow"
              data-testid="mat-code"
            />
            <t-button
              v-if="allowManualCode"
              variant="outline"
              theme="primary"
              :disabled="isEdit"
              :loading="genCodeLoading"
              data-testid="mat-gen-code"
              @click="handleGenCode"
            >
              生成编码
            </t-button>
          </div>
          <template #help>
            <span class="dlg-help">
              {{
                isEdit
                  ? '编码是物料身份，不支持修改'
                  : manualCodeHelp(allowManualCode, '不点「生成编码」则保存时自动生成；生成后仍可手改（已有编码体系可直接填）')
              }}
            </span>
          </template>
        </t-form-item>

        <t-form-item label="物料名称" name="material_name" :status="formErrors.material_name ? 'error' : undefined" :tips="formErrors.material_name">
          <t-input v-model="form.material_name" :maxlength="120" placeholder="如 成品F" data-testid="mat-name" />
        </t-form-item>

        <t-form-item label="分类" name="material_type">
          <t-select v-model="form.material_type" :options="typeOptions" data-testid="mat-form-type" />
        </t-form-item>

        <t-form-item label="计量单位" name="unit_id" :status="formErrors.unit_id ? 'error' : undefined" :tips="formErrors.unit_id">
          <t-select
            v-model="form.unit_id"
            :options="unitSelectOptions"
            placeholder="选基础单位"
            data-testid="mat-unit"
          />
          <template #help>
            <span class="dlg-help" data-testid="mat-unit-help">
              只能选基础单位（个 / 千克…）：本版本不支持多单位换算，折算单位（箱 / 打）不能作为物料单位，
              否则该物料的采购 / 收货 / 调拨单据都会被拒
            </span>
          </template>
        </t-form-item>

        <t-form-item label="规格" name="spec">
          <t-input v-model="form.spec" :maxlength="200" placeholder="选填，如 整机" data-testid="mat-spec" />
        </t-form-item>

        <!-- 安全库存（P2-2 ②）：填了才生效 —— 低库存提醒的唯一阈值 -->
        <t-form-item
          label="安全库存"
          name="safety_stock"
          :status="formErrors.safety_stock ? 'error' : undefined"
          :tips="formErrors.safety_stock"
        >
          <div class="dlg-inline">
            <t-input
              v-model="form.safety_stock"
              class="stock-input"
              placeholder="留空 = 不提醒"
              type="number"
              :min="0"
              data-testid="mat-safety-stock"
            />
            <span class="dlg-help" data-testid="mat-safety-stock-help">{{ SAFETY_STOCK_HINT }}</span>
          </div>
        </t-form-item>

        <!-- 标准售价（E2E-FIX-4 ①）：取价链第 ④ 级，口径 = 不含税（与订单行单价同口径） -->
        <t-form-item
          label="标准售价"
          name="standard_price"
          :status="formErrors.standard_price ? 'error' : undefined"
          :tips="formErrors.standard_price"
        >
          <div class="dlg-inline">
            <t-input
              v-model="form.standard_price"
              class="stock-input"
              placeholder="留空 = 不维护"
              type="number"
              :min="0"
              data-testid="mat-standard-price"
            />
            <span class="dlg-help" data-testid="mat-standard-price-help">{{ STANDARD_PRICE_HINT }}</span>
          </div>
        </t-form-item>

        <!-- 成本法：本版本只有移动加权（V0201 收敛）。下拉只有一项且置灰 —— 不给「选了不生效」的机会 -->
        <t-form-item label="成本法" name="costing_method">
          <div class="dlg-inline">
            <t-select
              v-model="form.costing_method"
              class="costing-select"
              :options="costingOptions"
              :disabled="true"
              data-testid="mat-costing"
            />
            <span class="dlg-help" data-testid="mat-costing-help">{{ COSTING_METHOD_HINT }}</span>
          </div>
        </t-form-item>

        <t-form-item label="一物一码">
          <div class="dlg-inline">
            <!-- 编辑态置灰：一物一码语义一旦创建就定死，随便翻会让历史序列号与新规则打架 -->
            <t-tooltip :content="serialLockReason" :disabled="!isEdit" placement="top-left">
              <span class="gate-wrap">
                <t-switch v-model="form.serial_required" :disabled="isEdit" data-testid="mat-serial-switch" />
              </span>
            </t-tooltip>
            <span class="dlg-help" data-testid="mat-serial-help">{{ serialHint }}</span>
          </div>
        </t-form-item>

        <t-form-item label="启用">
          <t-switch v-model="enabledSwitch" data-testid="mat-status" />
        </t-form-item>

        <t-form-item label="备注" name="remark">
          <t-input v-model="form.remark" :maxlength="255" placeholder="选填" data-testid="mat-remark" />
        </t-form-item>
      </t-form>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';
import {
  createMaterial,
  fetchMaterialNextCode,
  fetchMaterialPage,
  fetchUnitOptions,
  updateMaterial,
  type MaterialRow,
  type UnitRow,
} from '@/api/business/erp';
import { ApiError } from '@/api/request';
import ImportEntryButton from '@/components/ImportEntryButton.vue';
import { loadAllowManualCode, manualCodeDisabled, manualCodeHelp } from '../shared/code-policy';
import {
  COSTING_METHOD_HINT,
  MATERIAL_PAGE_SIZE,
  MATERIAL_TYPE_OPTIONS,
  SAFETY_STOCK_HINT,
  STANDARD_PRICE_HINT,
  SERIAL_FILTER_OPTIONS,
  buildMaterialCreatePayload,
  buildMaterialQuery,
  buildMaterialUpdatePayload,
  clampPage,
  costingMethodOptions,
  emptyMaterialFilter,
  emptyMaterialForm,
  hasFieldError,
  isSerialRequired,
  materialFormFromRow,
  materialStatusLabel,
  materialTypeLabel,
  serialRequiredLabel,
  unitNameOf,
  unitOptions,
  validateMaterialForm,
  type MaterialFieldErrors,
  type MaterialFilterState,
  type MaterialFormState,
} from './material-query';

/** 本页用到的权限码（码值与 `backend/migrations/business/V0141__erp_permissions.up.sql` 的 `auth_code` 逐字一致） */
const PERM = {
  /** 列表（页面入口） */
  materialList: 'business:erp:material:list',
  /** 新建 */
  materialCreate: 'business:erp:material:create',
  /** 编辑 */
  materialUpdate: 'business:erp:material:update',
} as const;

/** 表格列（`colKey` 与后端出参字段同名，插槽按 colKey 命中） */
const columns: PrimaryTableCol[] = [
  { colKey: 'material_code', title: '物料编码', width: 170, fixed: 'left', ellipsis: true },
  { colKey: 'material_name', title: '物料名称', minWidth: 220, ellipsis: true },
  { colKey: 'material_type', title: '分类', width: 110 },
  { colKey: 'unit_id', title: '单位', width: 110 },
  { colKey: 'serial_required', title: '一物一码', width: 130 },
  { colKey: 'status', title: '状态', width: 100 },
  { colKey: 'op', title: '操作', width: 100, fixed: 'right' },
];

/** 分类下拉（物料类型；`MATERIAL_TYPE_OPTIONS` 是只读常量，这里转成可写数组给 t-select） */
const typeOptions = MATERIAL_TYPE_OPTIONS.map((item) => ({ ...item }));
/** 一物一码下拉：全部 / ★是 / 否 */
const serialOptions = SERIAL_FILTER_OPTIONS.map((item) => ({ ...item }));

// ==================== 列表状态 ====================

/** 筛选行 */
const filter = reactive<MaterialFilterState>(emptyMaterialFilter());
/** 当前页数据 */
const rows = ref<MaterialRow[]>([]);
/** 总条数（服务端分页） */
const total = ref(0);
/** 当前页码 */
const current = ref(1);
/** 每页条数 */
const pageSize = MATERIAL_PAGE_SIZE;
/** 加载中 */
const loading = ref(false);
/** 加载错误 */
const loadError = ref('');
/** 计量单位（把 unit_id 译成名字 + 弹窗下拉） */
const units = ref<UnitRow[]>([]);

/** 是否处于筛选态（空态文案要分「没数据」和「没匹配」） */
const isFiltered = computed(
  () => Boolean(filter.keyword.trim()) || Boolean(filter.material_type) || filter.serial_required !== 'all',
);

/** 本页一物一码条数（面板副标题用） */
const serialCountOnPage = computed(() => rows.value.filter((row) => isSerialRequired(row.serial_required)).length);

/** 表格分页器（服务端分页：只喂 current / pageSize / total，切页自己拉接口） */
const pagination = computed(() => ({
  current: current.value,
  pageSize,
  total: total.value,
  showJumper: true,
  showPageSize: false,
}));

/** 单位下拉项 */
const unitSelectOptions = computed(() => unitOptions(units.value).map((item) => ({ ...item })));

/**
 * 成本法下拉项（本版本只有一项；老数据里的 FIFO / STANDARD 会作为禁用项显示）。
 *
 * 依赖 `form.costing_method`（编辑时来自行上的值），所以是 computed 而不是常量数组。
 */
const costingOptions = computed(() => costingMethodOptions(form.costing_method).map((item) => ({ ...item })));

/** 错误文案（后端 4xx 的 msg 比「请求失败」有用得多） */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

/**
 * 拉列表。
 *
 * @param page 目标页码（默认当前页；筛选变化时传 1）
 */
async function loadMaterials(page: number = current.value): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const query = buildMaterialQuery(filter, page, pageSize);
    const data = await fetchMaterialPage(query);
    rows.value = data.list ?? [];
    total.value = data.total ?? 0;
    // 页码夹紧：筛到只剩 1 页时别停在不存在的第 3 页
    current.value = clampPage(data.page ?? page, total.value, pageSize);
  } catch (error) {
    rows.value = [];
    total.value = 0;
    loadError.value = `物料列表加载失败：${messageOf(error)}`;
  } finally {
    loading.value = false;
  }
}

/** 查询（回到第 1 页） */
function handleSearch(): void {
  current.value = 1;
  void loadMaterials(1);
}

/** 重置筛选并回第 1 页 */
function handleReset(): void {
  Object.assign(filter, emptyMaterialFilter());
  current.value = 1;
  void loadMaterials(1);
}

/** 切页（服务端分页） */
function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  current.value = pageInfo.current;
  void loadMaterials(pageInfo.current);
}

// ==================== 新建 / 编辑弹窗 ====================

/** 弹窗可见 */
const dialogVisible = ref(false);
/** 编辑对象；`null` = 新建 */
const editing = ref<MaterialRow | null>(null);
/** 表单 */
const form = reactive<MaterialFormState>(emptyMaterialForm());
/** 表单错误 */
const formErrors = ref<MaterialFieldErrors>({});
/** 提交错误 */
const submitError = ref('');
/** 提交中 */
const saving = ref(false);

const isEdit = computed(() => editing.value !== null);

/** 启用开关（`status` 1/0 与布尔互转，模板里不写 `=== 1`） */
const enabledSwitch = computed({
  get: () => form.status === 1,
  set: (value: boolean) => {
    form.status = value ? 1 : 0;
  },
});

/** 编辑态置灰的原因（tooltip） */
const serialLockReason = computed(
  () =>
    `「${editing.value?.material_code ?? ''}」已创建，一物一码不建议修改：已经按「${isSerialRequired(editing.value?.serial_required) ? '需登记' : '无需登记'}序列号」`
    + '入过库的历史单据不会跟着变，翻了会让序列号语义自相矛盾。确需变更请新建物料。',
);

/** 开关旁的即时说明文案 */
const serialHint = computed(() => {
  if (isEdit.value) {
    return isSerialRequired(editing.value?.serial_required) ? '当前：一物一码（已锁定）' : '当前：非一物一码（已锁定）';
  }
  return form.serial_required ? '开启后：出入库必须逐件登记序列号' : '关闭：按数量管理，不追序列号';
});

/** 打开新建 */
function openCreate(): void {
  editing.value = null;
  Object.assign(form, emptyMaterialForm());
  formErrors.value = {};
  submitError.value = '';
  dialogVisible.value = true;
}

/** 打开编辑（回填） */
function openEdit(row: MaterialRow): void {
  editing.value = row;
  Object.assign(form, materialFormFromRow(row));
  formErrors.value = {};
  submitError.value = '';
  dialogVisible.value = true;
}

// ==================== 生成编码（CODE-06 交互方式 B） ====================

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
 * 后端 `GET /business/erp/material/next-code` 只读 `biz_seq`（方案 B）：
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
    const preview = await fetchMaterialNextCode();
    form.material_code = preview.code;
    // 之前「超长」之类的提示已不成立，清掉免得红框跟着新号走
    delete formErrors.value.material_code;
  } catch (error) {
    MessagePlugin.error(`生成编码失败：${messageOf(error)}`);
  } finally {
    genCodeLoading.value = false;
  }
}

/** 提交（新建 / 编辑） */
async function handleConfirm(): Promise<void> {
  const errors = validateMaterialForm(form, isEdit.value);
  formErrors.value = errors;
  if (hasFieldError(errors)) {
    return;
  }
  saving.value = true;
  submitError.value = '';
  try {
    if (editing.value) {
      await updateMaterial(editing.value.id, buildMaterialUpdatePayload(form, editing.value));
      MessagePlugin.success('物料已保存');
    } else {
      const created = await createMaterial(buildMaterialCreatePayload(form));
      // 没点「生成编码」直接保存时编码是后端取的号 —— 回填到表单并在提示里报出来，
      // 用户当场就能看到自己这条到底叫 MAT-000几。
      form.material_code = created.material_code;
      MessagePlugin.success(`物料已创建 · 编码 ${created.material_code}`);
    }
    dialogVisible.value = false;
    await loadMaterials(editing.value ? current.value : 1);
  } catch (error) {
    submitError.value = messageOf(error);
  } finally {
    saving.value = false;
  }
}

// ==================== 初始化 ====================

onMounted(async () => {
  // 单位与物料并行：单位拉不到不影响列表，只是「单位」列显示 —
  const [unitList] = await Promise.all([fetchUnitOptions().catch(() => [] as UnitRow[]), loadMaterials(1)]);
  units.value = unitList;
  // CODE-08：编码策略单独取（失败按「允许手填」兜底，不打断列表）
  allowManualCode.value = await loadAllowManualCode();
});
</script>

<style scoped lang="less">
.material-page {
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

.filter-row__select--wide {
  width: 200px;
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

/* 一物一码标记：tag 给「是」，纯文本给「否」—— 视觉权重故意拉开 */
.serial-tag {
  font-weight: 600;
}

.serial-no {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.dlg-inline {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}

/* 成本法：单值下拉，宽度对齐其它表单项；旁边跟灰字说明 */
.costing-select {
  width: 220px;
}

/* 安全库存输入框：与成本法同宽，后面跟口径提示 */
.stock-input {
  width: 220px;
}

.costing-select :deep(.t-is-disabled),
.costing-select.t-is-disabled {
  color: var(--td-text-color-secondary);
}

/* CODE-06：「编码框 + 生成编码」一行铺满（输入框吃掉剩余宽度，按钮不缩） */
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

/* 禁用的开关外面包一层 span，否则 t-tooltip 在 disabled 元素上不触发 */
.gate-wrap {
  display: inline-flex;
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
  max-width: 420px;
  line-height: 1.6;
}
</style>
