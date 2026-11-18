<!--
  成本方法配置页（M5-01【M5·成本】品种法 / 分批法 / 标准成本）。

  一页三件事：**列表（范围 / 方法 / 状态 / 关键字）→ 配置表单（版本化）→ 生效方法查询**。

  # 「按成本对象选方法」到底怎么配

  · 范围 `COST_OBJECT`：给某个生产批单独指定方法（优先于全公司默认）；
  · 范围 `GLOBAL`：全公司默认（一条开区间版本就够；要换方法时先把旧版停止日收到切换日前一天）。

  # 一期只有分批法算得出来（ARCH-9 D-3）

  品种法（三期 D-16）/ 标准成本（二期 D-15）**可以配置**，但点「计算成本」会被后端
  `409 + 66101` 拦下。本页在下拉、列表标签、表单红字、生效查询四处都标出来，
  不让用户配完才发现算不出来。

  # 口径锁定（第二次修改的坑）

  已生效过（`effective_from < 今天`）的版本只允许改 **停止日 / 状态 / 备注**：
  方法 / 范围 / 对象 / 生效日四个控件置灰（后端 `409 + 66105`），也不允许删除
  （`409 + 66104`）—— 它是「那段期间按哪种方法算」的凭据。换方法请新增一条。
-->
<template>
  <div class="cost-method-page">
    <!-- ==================== 生效方法查询（只读探针：配之前先看清楚会按什么算） ==================== -->
    <t-card :bordered="false" class="probe-card">
      <div class="probe-card__row">
        <span class="probe-card__title">生效方法查询</span>
        <t-select
          v-model="probe.cost_object_id"
          class="probe-card__select"
          :options="objectOptions"
          :loading="objectsLoading"
          filterable
          clearable
          placeholder="选成本对象（不选 = 只查全公司默认）"
          data-testid="cm-probe-object"
          @change="runProbe"
        />
        <t-date-picker
          v-model="probe.on"
          class="probe-card__date"
          value-type="YYYY-MM-DD"
          format="YYYY-MM-DD"
          placeholder="参照日（缺省今天）"
          clearable
          data-testid="cm-probe-date"
          @change="runProbe"
        />
        <t-button variant="outline" data-testid="cm-probe-run" @click="runProbe">
          <template #icon><t-icon name="search" /></template>
          查一下
        </t-button>
      </div>

      <div v-if="probeResult" class="probe-card__result" data-testid="cm-probe-result">
        <t-tag
          size="medium"
          :theme="probeResult.method_implemented ? 'success' : 'warning'"
          variant="light"
        >
          {{ probeResult.method_label }}
        </t-tag>
        <span class="probe-card__who">
          {{ probeResult.cost_object_no || '全公司默认' }} @ {{ probeResult.on_date }}
        </span>
        <t-tag size="small" variant="outline">
          来源：{{ probeResult.source_label || sourceLabel(probeResult.source) }}
        </t-tag>
        <span class="probe-card__hint" :class="{ 'probe-card__hint--warn': !probeResult.method_implemented }">
          {{ probeResult.engine_hint }}
        </span>
      </div>
      <div v-else class="probe-card__hint">
        选一个成本对象（或留空看全公司默认）+ 日期，就知道「那一期按哪种方法算、算不算得出来」。
      </div>
    </t-card>

    <!-- ==================== 列表（筛选 / 分页 / 行操作交给 ListPage） ==================== -->
    <ListPage
      ref="listRef"
      class="tool-row--dense"
      title="成本方法"
      testid="cm"
      table-testid="cm-table"
      meta-testid="cm-meta"
      btn-prefix="cm-btn"
      :columns="columns"
      :filters="filters"
      :filter-state="filter"
      :fetcher="fetcher"
      :query-builder="queryBuilder"
      :page-size="PAGE_SIZE"
      @loaded="onLoaded"
    >
      <template #toolbar>
        <t-button
          v-privilege="[PERM.create]"
          theme="primary"
          data-testid="cm-btn-create"
          @click="openCreate"
        >
          <template #icon><t-icon name="add" /></template>
          新增配置
        </t-button>
      </template>

      <template #head-extra>
        <span class="method-page__tip">
          一条配置 = 「谁（全公司 / 某个成本对象）从哪天到哪天用哪种核算方法」；换方法请收旧版停止日再新增，别改历史
        </span>
      </template>

      <template #method="{ row }">
        <div class="name-cell">
          <span class="name-cell__main">
            <t-tag size="small" :theme="row.method_implemented ? 'success' : 'default'" variant="light">
              {{ row.method_label || methodLabel(row.method) }}
            </t-tag>
            <t-tag v-if="!row.method_implemented" size="small" theme="warning" variant="outline">
              未实现
            </t-tag>
          </span>
          <span class="name-cell__sub">{{ row.method }}</span>
        </div>
      </template>
      <template #scope="{ row }">
        <div class="name-cell">
          <span class="name-cell__main">{{ scopeDisplay(row) }}</span>
          <span class="name-cell__sub">{{ row.scope_label || scopeLabel(row.scope) }}</span>
        </div>
      </template>
      <template #effective="{ row }">{{ effectiveRangeText(row) }}</template>
      <template #status="{ row }">
        <t-tag size="small" variant="light" :theme="row.status === 1 ? 'success' : 'default'">
          {{ row.status_label || statusLabel(row.status) }}
        </t-tag>
      </template>
      <template #op="{ row }">
        <div class="op-cell">
          <t-link
            v-privilege="[PERM.update]"
            theme="primary"
            data-testid="cm-btn-edit"
            @click.stop="openEdit(row)"
          >
            编辑
          </t-link>
          <t-link
            v-privilege="[PERM.update]"
            theme="primary"
            data-testid="cm-btn-toggle"
            @click.stop="toggleStatus(row)"
          >
            {{ row.status === 1 ? '停用' : '启用' }}
          </t-link>
          <t-link
            v-privilege="[PERM.delete]"
            theme="danger"
            :disabled="isLocked(row, today)"
            data-testid="cm-btn-delete"
            @click.stop="removeRow(row)"
          >
            删除
          </t-link>
        </div>
      </template>
    </ListPage>

    <!-- ==================== 新建 / 编辑 ==================== -->
    <t-dialog
      v-model:visible="dialog.visible"
      :header="dialog.editing ? '编辑方法配置' : '新增方法配置'"
      width="620px"
      :confirm-btn="{ content: '保存', loading: dialog.saving }"
      :cancel-btn="{ content: '取消' }"
      @confirm="submitForm"
    >
      <t-form label-width="110px" :data="form" class="method-form">
        <t-form-item label="核算方法" :error="errors.method">
          <t-select
            v-model="form.method"
            :options="methodOptions"
            :disabled="lockedEditing"
            data-testid="cm-form-method"
          />
          <div class="form-note" :class="{ 'form-note--warn': !isImplemented(form.method) }">
            {{ methodNote(form.method) }}
          </div>
        </t-form-item>

        <t-form-item label="配置范围" :error="errors.scope">
          <t-radio-group v-model="form.scope" :disabled="lockedEditing" data-testid="cm-form-scope">
            <t-radio v-for="option in scopeOptions" :key="option.value" :value="option.value">
              {{ option.label }}
            </t-radio>
          </t-radio-group>
          <div class="form-note">对象级优先于全公司默认；没有任何配置命中时按分批法算。</div>
        </t-form-item>

        <t-form-item v-if="form.scope === 'COST_OBJECT'" label="成本对象" :error="errors.scope_id">
          <t-select
            v-model="form.scope_id"
            :options="objectOptions"
            :loading="objectsLoading"
            filterable
            clearable
            :disabled="lockedEditing"
            placeholder="选择成本对象（生产批）"
            data-testid="cm-form-object"
          >
            <template #empty>
              <div class="form-note">
                没有可选对象：要么还没建成本对象，要么当前账号缺「成本对象」的查看权限
                （<code>business:erp:cost-object:list</code>）。
              </div>
            </template>
          </t-select>
        </t-form-item>

        <t-form-item label="生效日" :error="errors.effective_from">
          <t-date-picker
            v-model="form.effective_from"
            value-type="YYYY-MM-DD"
            format="YYYY-MM-DD"
            :disabled="lockedEditing"
            placeholder="切换日（含当天）"
            data-testid="cm-form-from"
          />
        </t-form-item>

        <t-form-item label="停止日" :error="errors.effective_to">
          <t-date-picker
            v-model="form.effective_to"
            value-type="YYYY-MM-DD"
            format="YYYY-MM-DD"
            clearable
            placeholder="留空 = 长期有效"
            data-testid="cm-form-to"
          />
        </t-form-item>

        <t-form-item label="状态">
          <t-radio-group v-model="form.status" data-testid="cm-form-status">
            <t-radio :value="1">启用</t-radio>
            <t-radio :value="0">停用</t-radio>
          </t-radio-group>
        </t-form-item>

        <t-form-item label="备注">
          <t-textarea
            v-model="form.remark"
            :maxlength="200"
            placeholder="切换原因 / 审批单号（可空）"
            data-testid="cm-form-remark"
          />
        </t-form-item>

        <t-alert
          v-if="lockedEditing"
          theme="warning"
          class="form-alert"
          message="这条配置已生效过：只能改停止日 / 状态 / 备注。要换方法，请把这条的停止日改到切换日前一天，再新增一条从切换日生效的版本。"
        />
        <t-alert
          v-if="overlapHint"
          theme="error"
          class="form-alert"
          :message="overlapHint"
          data-testid="cm-form-overlap"
        />
      </t-form>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin } from 'tdesign-vue-next';

import {
  createCostMethod,
  deleteCostMethod,
  fetchCostMethodPage,
  fetchEffectiveMethod,
  updateCostMethod,
  COST_METHOD_OVERLAP_CODE,
  COST_METHOD_PERMISSIONS,
  PAGE_SIZE,
  type CostMethodEffectiveRes,
  type CostMethodRow,
} from '@/api/business/erp/cost-method';
import { fetchCostObjectPage, type CostObjectRow } from '@/api/business/erp/cost-object';
import ListPage from '@/components/ListPage/index.vue';
import type {
  ColumnDef,
  FilterDef,
  ListFetcher,
  ListQueryBuilder,
} from '@/components/ListPage/types';
import { todayString } from '@/views/business/erp/shared/money';
import { errorCodeOf, errorText } from '@/views/business/erp/shared/error-text';
import {
  effectiveRangeText,
  emptyForm,
  findOverlapHint,
  formFromRow,
  isImplemented,
  isEmptyPayload,
  isLocked,
  isValid,
  methodLabel,
  methodNote,
  methodOptions,
  scopeDisplay,
  scopeLabel,
  scopeOptions,
  sourceLabel,
  statusLabel,
  toCreatePayload,
  toUpdatePayload,
  validateForm,
  type MethodFormErrors,
  type MethodFormState,
} from './cost-method-logic';

/** 本页权限码（V0460 种子逐一对应） */
const PERM = COST_METHOD_PERMISSIONS;

const today = todayString();

// ==================== 列表 ====================

const listRef = ref<InstanceType<typeof ListPage> | null>(null);

const filter = reactive<{
  scope: string;
  method: string;
  status: number | '';
  keyword: string;
}>({
  scope: '',
  method: '',
  status: '',
  keyword: '',
});

const columns: ColumnDef[] = [
  { colKey: 'method', title: '核算方法', width: 170 },
  { colKey: 'scope', title: '范围 / 成本对象', width: 190, ellipsis: true },
  { colKey: 'effective', title: '生效期', width: 210 },
  { colKey: 'status', title: '状态', width: 80 },
  { colKey: 'remark', title: '备注', width: 220, ellipsis: true },
  { colKey: 'op', title: '操作', width: 180, fixed: 'right', exportable: false },
];

const filters = computed<FilterDef[]>(() => [
  {
    key: 'scope',
    type: 'select',
    options: scopeOptions.map((option) => ({ label: option.label, value: option.value })),
    searchOnChange: true,
    placeholder: '全部范围',
    width: 140,
    testid: 'cm-filter-scope',
  },
  {
    key: 'method',
    type: 'select',
    options: methodOptions.map((option) => ({ label: option.label, value: option.value })),
    searchOnChange: true,
    placeholder: '全部方法',
    width: 190,
    testid: 'cm-filter-method',
  },
  {
    key: 'status',
    type: 'select',
    options: [
      { label: '启用', value: 1 },
      { label: '停用', value: 0 },
    ],
    searchOnChange: true,
    placeholder: '全部状态',
    width: 110,
    testid: 'cm-filter-status',
  },
  {
    key: 'keyword',
    type: 'keyword',
    placeholder: '对象号 / 备注 / 方法',
    width: 200,
    testid: 'cm-filter-keyword',
  },
]);

const fetcher: ListFetcher<CostMethodRow> = (query) =>
  fetchCostMethodPage(query as Parameters<typeof fetchCostMethodPage>[0]);

const queryBuilder: ListQueryBuilder = (state, page, pageSize) => {
  const value = state as { scope: string; method: string; status: number | ''; keyword: string };
  return {
    page,
    page_size: pageSize,
    scope: value.scope,
    method: value.method,
    status: value.status === '' ? undefined : value.status,
    keyword: value.keyword.trim(),
  };
};

/** 当前页数据（客户端重叠预检用；ListPage 是服务端分页，故只覆盖已加载的行 + 提示） */
const rows = ref<CostMethodRow[]>([]);

function onLoaded(payload?: { rows?: CostMethodRow[] }): void {
  if (payload?.rows) {
    rows.value = payload.rows;
  }
}

async function reload(): Promise<void> {
  await listRef.value?.reload();
}

// ==================== 成本对象下拉 ====================

const objects = ref<CostObjectRow[]>([]);
const objectsLoading = ref(false);

const objectOptions = computed(() =>
  objects.value.map((row) => ({
    value: row.id,
    label: `${row.object_no}${row.product_name ? ` · ${row.product_name}` : ''}${
      row.status === 'OPEN' ? '' : `（${row.status}）`
    }`,
  })),
);

/** 载一次成本对象（失败不阻塞页面：缺 `cost-object:list` 时下拉为空并给说明） */
async function loadObjects(): Promise<void> {
  objectsLoading.value = true;
  try {
    const page = await fetchCostObjectPage({ page: 1, page_size: 200 });
    objects.value = page.list ?? [];
  } catch (error) {
    MessagePlugin.warning(
      errorText(error, '加载成本对象失败（可继续用「全公司默认」范围）'),
    );
  } finally {
    objectsLoading.value = false;
  }
}

// ==================== 生效方法查询 ====================

const probe = reactive<{ cost_object_id: string; on: string }>({
  cost_object_id: '',
  on: todayString(),
});
const probeResult = ref<CostMethodEffectiveRes | null>(null);

async function runProbe(): Promise<void> {
  try {
    probeResult.value = await fetchEffectiveMethod({
      cost_object_id: probe.cost_object_id || undefined,
      on: probe.on || undefined,
    });
  } catch (error) {
    probeResult.value = null;
    MessagePlugin.error(errorText(error, '查询生效方法失败'));
  }
}

// ==================== 表单 ====================

const dialog = reactive<{ visible: boolean; editing: CostMethodRow | null; saving: boolean }>({
  visible: false,
  editing: null,
  saving: false,
});

const form = reactive<MethodFormState>(emptyForm(today));
const errors = ref<MethodFormErrors>({});

/** 编辑一条已生效过的版本：口径字段全部锁定 */
const lockedEditing = computed(() => (dialog.editing ? isLocked(dialog.editing, today) : false));

/** 客户端重叠提示（与后端 66102 同判据；只对已加载的行生效，服务端仍是最终判据） */
const overlapHint = computed<string>(() => {
  if (!dialog.visible) {
    return '';
  }
  const hit = findOverlapHint(rows.value, form, dialog.editing?.id);
  if (!hit) {
    return '';
  }
  return `${scopeDisplay(hit)} 已有一条 ${hit.method_label || methodLabel(hit.method)} 覆盖重叠区间（${effectiveRangeText(hit)}），当前填写会与它重叠。换方法的正确姿势：先把旧版本停止日改到切换日前一天，再新增切换日生效的版本。`;
});

function resetForm(state: MethodFormState): void {
  Object.assign(form, state);
  errors.value = {};
}

function openCreate(): void {
  dialog.editing = null;
  resetForm(emptyForm(today));
  dialog.visible = true;
}

function openEdit(row: CostMethodRow): void {
  dialog.editing = row;
  resetForm(formFromRow(row));
  dialog.visible = true;
}

async function submitForm(): Promise<void> {
  const found = validateForm(form);
  errors.value = found;
  if (!isValid(found)) {
    return;
  }
  if (overlapHint.value) {
    MessagePlugin.warning('配置区间与既有版本重叠，请先调整生效期或先收旧版本的停止日');
    return;
  }
  dialog.saving = true;
  try {
    if (dialog.editing) {
      const payload = toUpdatePayload(form, dialog.editing);
      if (isEmptyPayload(payload)) {
        MessagePlugin.info('没有改动');
        dialog.visible = false;
        return;
      }
      await updateCostMethod(dialog.editing.id, payload);
      MessagePlugin.success('已保存');
    } else {
      await createCostMethod(toCreatePayload(form));
      MessagePlugin.success('已新增配置');
    }
    dialog.visible = false;
    await reload();
    await runProbe();
  } catch (error) {
    if (errorCodeOf(error) === COST_METHOD_OVERLAP_CODE) {
      MessagePlugin.error(errorText(error, '生效期与既有版本重叠'));
    } else {
      MessagePlugin.error(errorText(error, '保存失败'));
    }
  } finally {
    dialog.saving = false;
  }
}

async function toggleStatus(row: CostMethodRow): Promise<void> {
  const next = row.status === 1 ? 0 : 1;
  try {
    await updateCostMethod(row.id, { status: next });
    MessagePlugin.success(next === 1 ? '已启用' : '已停用');
    await reload();
    await runProbe();
  } catch (error) {
    MessagePlugin.error(errorText(error, '改状态失败'));
  }
}

function removeRow(row: CostMethodRow): void {
  if (isLocked(row, today)) {
    MessagePlugin.warning('已生效过的配置不能删除（它是那段期间口径的凭据），请改成「停用」');
    return;
  }
  const dialogInstance = DialogPlugin.confirm({
    header: '删除配置',
    theme: 'warning',
    body: `确认删除 ${scopeDisplay(row)} 从 ${row.effective_from} 起的 ${row.method_label || methodLabel(row.method)} 配置？删除后不可恢复；已生效过的配置后端会拒绝删除（它会记在「那段期间按什么算」的凭据里）。`,
    confirmBtn: { content: '删除', theme: 'danger' },
    onConfirm: async () => {
      dialogInstance.hide();
      try {
        await deleteCostMethod(row.id);
        MessagePlugin.success('已删除');
        await reload();
      } catch (error) {
        MessagePlugin.error(errorText(error, '删除失败'));
      }
    },
  });
}

onMounted(async () => {
  await loadObjects();
  await runProbe();
});
</script>

<style scoped>
.cost-method-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.probe-card__row {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}

.probe-card__title {
  font-weight: 600;
}

.probe-card__select {
  width: 320px;
}

.probe-card__date {
  width: 180px;
}

.probe-card__result {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
  margin-top: 10px;
}

.probe-card__who {
  font-weight: 600;
}

.probe-card__hint {
  margin-top: 8px;
  color: var(--td-text-color-secondary);
  font-size: 12px;
  line-height: 1.6;
}

.probe-card__hint--warn {
  color: var(--td-warning-color);
}

.method-page__tip {
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.name-cell {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.name-cell__main {
  display: flex;
  align-items: center;
  gap: 6px;
}

.name-cell__sub {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.op-cell {
  display: flex;
  gap: 12px;
}

.form-note {
  margin-top: 4px;
  color: var(--td-text-color-placeholder);
  font-size: 12px;
  line-height: 1.6;
}

.form-note--warn {
  color: var(--td-warning-color);
}

.form-alert {
  margin-top: 8px;
}
</style>
