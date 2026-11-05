<template>
  <div class="linkage-page">
    <!-- ==================== ① 说明 + 主操作 ==================== -->
    <t-card :bordered="false" class="intro-card">
      <div class="intro">
        <div class="intro__text">
          <h3 class="intro__title">
            跨单据联动规则
            <t-tag size="small" variant="light" theme="primary">白名单动作 · 禁配 SQL</t-tag>
          </h3>
          <p class="intro__desc">
            一行规则 = 一个触发点（源单据 + 触发状态）上的一个<b>预定义动作</b>。单据走到该状态后，
            引擎按执行顺序跑这些动作；动作只能用下拉里的白名单，
            <b>不能配置任何 SQL / 语句</b>（参数里出现
            <code>sql / statement / query</code> 这类键会被后端直接拒绝）。
          </p>
        </div>
        <t-space class="intro__actions">
          <t-button
            v-privilege="[PERM.flowLinkageManage]"
            theme="primary"
            data-testid="btn-linkage-create"
            @click="openCreate"
          >
            <template #icon><t-icon name="add" /></template>
            新增规则
          </t-button>
          <t-button variant="outline" :loading="loading" data-testid="btn-linkage-refresh" @click="loadList">
            <template #icon><t-icon name="refresh" /></template>
            刷新
          </t-button>
        </t-space>
      </div>
    </t-card>

    <!-- ==================== ② 筛选 ==================== -->
    <t-card :bordered="false" class="filter-card">
      <div class="filter-row">
        <t-input
          v-model="filter.keyword"
          class="filter-row__keyword"
          placeholder="规则名 / 动作码 / 触发状态"
          clearable
          data-testid="linkage-filter-keyword"
        >
          <template #prefix-icon><t-icon name="search" /></template>
        </t-input>
        <t-select
          v-model="filter.entity"
          class="filter-row__entity"
          :options="entityFilterOptions"
          placeholder="全部源单据类型"
          clearable
          data-testid="linkage-filter-entity"
        />
        <t-checkbox v-model="filter.onlyEnabled" data-testid="linkage-filter-enabled">只看启用</t-checkbox>
        <span class="filter-row__spacer" />
        <span class="filter-row__count" data-testid="linkage-count">
          共 {{ filteredRows.length }} 条规则（启用 {{ enabledCount }} 条 / 全部 {{ total }} 条）
        </span>
      </div>
    </t-card>

    <!-- ==================== ③ 列表 ==================== -->
    <t-card :bordered="false" class="table-card">
      <t-alert v-if="errorMessage" class="table-alert" theme="error" :message="errorMessage">
        <template #operation>
          <t-button size="small" variant="outline" @click="loadList">重试</t-button>
        </template>
      </t-alert>

      <t-table
        row-key="id"
        :data="filteredRows"
        :columns="columns"
        :loading="loading"
        :hover="true"
        stripe
        data-testid="linkage-table"
      >
        <template #name="{ row }">
          <div class="cell-name">
            <span class="cell-name__text">{{ row.name }}</span>
            <t-tag v-if="!isHealthy(row)" size="small" theme="danger" variant="light">配置异常</t-tag>
          </div>
          <div v-if="!isHealthy(row)" class="cell-problem">{{ row.problems.join('；') }}</div>
        </template>

        <template #trigger="{ row }">
          <div class="cell-trigger">
            <span class="cell-trigger__entity">{{ row.trigger_entity_label }}</span>
            <code class="cell-trigger__code">{{ row.trigger_entity }}</code>
          </div>
          <t-tag size="small" theme="primary" variant="outline">{{ row.trigger_status }}</t-tag>
        </template>

        <template #action="{ row }">
          <div class="cell-action">
            <span class="cell-action__label">{{ row.action_label }}</span>
            <code class="cell-action__code">{{ row.action_code }}</code>
          </div>
          <t-space size="4px">
            <t-tag size="small" :theme="phaseTheme(row.phase)" variant="light">
              {{ phaseLabel(row.phase) }}
            </t-tag>
            <t-tag size="small" :theme="severityTheme(row.severity)" variant="light">
              {{ severityLabel(row.severity) }}
            </t-tag>
          </t-space>
        </template>

        <template #params="{ row }">
          <span class="cell-params" :title="formatParams(row.action_params)">
            {{ formatParams(row.action_params) }}
          </span>
        </template>

        <template #seq="{ row }">
          <span class="cell-seq">{{ row.seq }}</span>
        </template>

        <template #enabled="{ row }">
          <t-switch
            :value="row.enabled"
            :disabled="togglingId === row.id"
            :loading="togglingId === row.id"
            @change="(value: unknown) => handleToggleChange(row, value === true)"
          />
          <span class="cell-enabled__text">{{ row.enabled ? '启用' : '停用' }}</span>
        </template>

        <template #updated_at="{ row }">
          <span class="cell-time">{{ formatDateTime(row.updated_at) }}</span>
        </template>

        <template #op="{ row }">
          <t-space size="8px">
            <t-link
              v-privilege="[PERM.flowLinkageList]"
              theme="primary"
              data-testid="linkage-preview"
              @click="openPreview(row.trigger_entity, row.trigger_status)"
            >
              预览影响
            </t-link>
            <t-link
              v-privilege="[PERM.flowLinkageManage]"
              theme="primary"
              data-testid="linkage-edit"
              @click="openEditor(row)"
            >
              编辑
            </t-link>
            <t-link
              v-privilege="[PERM.flowLinkageManage]"
              theme="danger"
              data-testid="linkage-delete"
              @click="handleDelete(row)"
            >
              删除
            </t-link>
          </t-space>
        </template>
      </t-table>

      <t-empty
        v-if="!loading && !errorMessage && filteredRows.length === 0"
        class="empty"
        :description="total === 0 ? '还没有任何联动规则' : '没有符合筛选条件的规则'"
      >
        <template #action>
          <div class="empty__hint">
            0 条规则 = 引擎一条动作都不执行（现有业务行为不变）。点右上角「新增规则」配第一条，
            例如「收货入库单过账后 → 发通知给仓管」。
          </div>
        </template>
      </t-empty>
    </t-card>

    <!-- ==================== ④ 新增 / 编辑弹窗 ==================== -->
    <t-dialog
      v-model:visible="editorVisible"
      :header="editing ? `编辑规则 · ${editing.name}` : '新增联动规则'"
      :confirm-btn="{
        content: editing ? '保存修改' : '创建规则',
        theme: 'primary',
        loading: saving,
        disabled: !confirmed || !form.action_code || !form.name.trim(),
      }"
      :cancel-btn="{ content: '取消' }"
      width="820px"
      :close-on-overlay-click="false"
      @confirm="handleSave"
      @close="closeEditor"
    >
      <div class="editor">
        <t-alert theme="info" :message="'动作只能从白名单里选；编辑时「源单据类型 / 触发状态 / 动作」三项不可改（它们决定这条规则的身份），要换动作请删掉重建。'" />

        <t-form label-align="top" class="editor__form">
          <t-row :gutter="[16, 8]">
            <t-col :span="12">
              <t-form-item label="规则名" required-mark>
                <t-input
                  v-model="form.name"
                  placeholder="人读的名字，例：收货过账后通知仓管"
                  :maxlength="128"
                  data-testid="linkage-form-name"
                />
              </t-form-item>
            </t-col>
            <t-col :span="6">
              <t-form-item label="源单据类型" required-mark>
                <t-select
                  v-model="form.trigger_entity"
                  :options="triggerSelectOptions"
                  :disabled="Boolean(editing)"
                  placeholder="选择源单据"
                  data-testid="linkage-form-entity"
                />
              </t-form-item>
            </t-col>
            <t-col :span="6">
              <t-form-item label="触发状态" required-mark>
                <t-select
                  v-model="form.trigger_status"
                  :options="statusSelectOptions"
                  :disabled="Boolean(editing)"
                  placeholder="选择状态"
                  data-testid="linkage-form-status"
                />
              </t-form-item>
            </t-col>
            <t-col :span="12">
              <t-form-item label="目标动作（白名单）" required-mark>
                <t-select
                  v-model="form.action_code"
                  :options="actionSelectOptions"
                  :disabled="Boolean(editing)"
                  placeholder="选择白名单动作"
                  filterable
                  data-testid="linkage-form-action"
                />
                <div v-if="currentAction" class="editor__action-hint">
                  <t-space size="6px">
                    <t-tag size="small" :theme="phaseTheme(currentAction.phase)" variant="light">
                      {{ phaseLabel(currentAction.phase) }}
                    </t-tag>
                    <t-tag size="small" :theme="severityTheme(currentAction.severity)" variant="light">
                      {{ severityLabel(currentAction.severity) }}
                    </t-tag>
                  </t-space>
                  <div class="editor__reuse">复用的既有实现：{{ currentAction.reuse }}</div>
                </div>
              </t-form-item>
            </t-col>
            <t-col :span="6">
              <t-form-item label="执行顺序（同一触发点内升序）">
                <t-input-number v-model="form.seq" :min="0" :max="9999" theme="normal" />
              </t-form-item>
            </t-col>
            <t-col :span="6">
              <t-form-item label="是否启用">
                <t-switch v-model="form.enabled" />
              </t-form-item>
            </t-col>
          </t-row>

          <!-- 参数表单：控件由后端下发的声明驱动 -->
          <div class="params">
            <div class="params__title">
              动作参数
              <span class="params__hint">键集合由该动作声明决定，不能自由加键（禁配 SQL）</span>
            </div>
            <t-alert v-if="!currentAction" theme="info" message="先在「目标动作」里选一个动作，这里会出现它声明的参数。" />
            <t-row v-else :gutter="[16, 8]">
              <t-col v-for="param in currentAction.params" :key="param.key" :span="12">
                <t-form-item :label="`${param.key}（${kindLabel(param.kind.kind)}）`" :required-mark="param.required">
                  <!-- 枚举 -->
                  <t-select
                    v-if="param.kind.kind === 'enum'"
                    :value="paramText(param.key)"
                    :options="enumOptions(param)"
                    :placeholder="param.required ? '必选' : '可空（不填则用默认）'"
                    clearable
                    @change="(value: unknown) => setParam(param.key, value === undefined || value === null ? '' : String(value))"
                  />
                  <!-- 布尔 -->
                  <t-switch
                    v-else-if="param.kind.kind === 'bool'"
                    :value="paramBool(param.key)"
                    @change="(value: unknown) => setParam(param.key, value === true)"
                  />
                  <!-- 整数 / 十进制 -->
                  <t-input-number
                    v-else-if="param.kind.kind === 'int' || param.kind.kind === 'decimal'"
                    :value="paramText(param.key)"
                    :theme="'normal'"
                    :decimal-places="param.kind.kind === 'int' ? 0 : 4"
                    :placeholder="param.required ? '必填' : '可空'"
                    @change="(value: unknown) => setParamNumber(param.key, value)"
                  />
                  <!-- UUID 列表 -->
                  <t-textarea
                    v-else-if="param.kind.kind === 'uuid_list'"
                    :value="paramText(param.key)"
                    :autosize="{ minRows: 2, maxRows: 4 }"
                    placeholder="多个 UUID 用逗号 / 空格 / 换行分隔"
                    @change="(value: unknown) => setParam(param.key, value === undefined || value === null ? '' : String(value))"
                  />
                  <!-- 文本 / UUID -->
                  <t-input
                    v-else
                    :value="paramText(param.key)"
                    :placeholder="param.required ? '必填' : '可空'"
                    :maxlength="param.kind.max_len ?? undefined"
                    @change="(value: unknown) => setParam(param.key, value === undefined || value === null ? '' : String(value))"
                  />
                  <div class="params__note">{{ param.note }}</div>
                </t-form-item>
              </t-col>
            </t-row>
          </div>

          <!-- 影响预览（保存前先算一遍） -->
          <div class="editor__preview">
            <t-button
              variant="outline"
              size="small"
              :loading="draftPreviewLoading"
              data-testid="linkage-form-preview"
              @click="handlePreviewDraft"
            >
              <template #icon><t-icon name="browse" /></template>
              预览影响（不保存）
            </t-button>
            <div v-if="draftPreview" class="editor__preview-body">
              <t-alert
                :theme="draftAlertTheme(draftPreview.draft)"
                :message="draftPreview.draft?.ok
                  ? '这条规则可以通过校验，保存后生效'
                  : `保存会被拒绝：${draftPreview.draft?.message || '未知原因'}`"
              />
              <ul class="editor__preview-list">
                <li v-for="(item, index) in previewHighlights(draftPreview)" :key="index">{{ item }}</li>
              </ul>
              <div class="editor__preview-hint">{{ draftPreview.next_run_hint }}</div>
            </div>
          </div>

          <div class="editor__confirm">
            <t-checkbox v-model="confirmed" data-testid="linkage-form-confirm">
              我确认这条规则会改变「单据状态变更后系统自动做的事」，并已看清上面的影响预览
            </t-checkbox>
          </div>

          <t-alert v-if="formError" class="editor__error" theme="error" :message="formError" />
        </t-form>
      </div>
    </t-dialog>

    <!-- ==================== ⑤ 影响预览弹窗 ==================== -->
    <t-dialog
      v-model:visible="previewVisible"
      header="执行影响预览"
      width="860px"
      :footer="false"
      @close="previewResult = null"
    >
      <div v-if="previewLoading" class="preview__loading">
        <t-loading size="small" text="计算执行链…" />
      </div>
      <t-alert v-else-if="previewError" theme="error" :message="previewError" />
      <div v-else-if="previewResult" class="preview">
        <div class="preview__head">
          <span class="preview__trigger">
            {{ previewResult.trigger_entity_label }}
            <code>{{ previewResult.trigger_entity }}</code>
            →
            <t-tag size="small" theme="primary" variant="outline">{{ previewResult.trigger_status }}</t-tag>
          </span>
          <t-space size="8px">
            <t-tag theme="primary" variant="light">启用规则 {{ previewResult.enabled_count }} 条</t-tag>
            <t-tag v-if="previewResult.rollback_risk" theme="danger" variant="light">有回滚风险</t-tag>
            <t-tag v-if="previewResult.after_commit_count > 0" theme="warning" variant="light">
              提交后执行 {{ previewResult.after_commit_count }} 个
            </t-tag>
          </t-space>
        </div>

        <ul class="preview__warnings">
          <li v-for="(item, index) in previewHighlights(previewResult)" :key="index">{{ item }}</li>
        </ul>

        <t-table
          v-if="previewResult.steps.length > 0"
          row-key="rule_id"
          :data="previewResult.steps"
          :columns="previewColumns"
          size="small"
          bordered
        >
          <template #action_label="{ row }">
            <div class="cell-action__label">{{ row.action_label }}</div>
            <code class="cell-action__code">{{ row.action_code }}</code>
          </template>
          <template #phase="{ row }">
            <t-tag size="small" :theme="phaseTheme(row.phase)" variant="light">{{ phaseLabel(row.phase) }}</t-tag>
            <t-tag size="small" :theme="severityTheme(row.severity)" variant="light">
              {{ severityLabel(row.severity) }}
            </t-tag>
          </template>
          <template #effect="{ row }">
            <span class="preview__effect">{{ row.effect || '—' }}</span>
          </template>
        </t-table>
        <t-empty v-else description="这个触发点当前没有启用的规则" />

        <div class="preview__hint">{{ previewResult.next_run_hint }}</div>
      </div>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 跨单据联动规则配置页（卡 t_d0daa4f0 / P1-FIX-5A）。
 *
 * 后端契约：`backend/src/business/flow/linkage/admin.rs`（列表 / 增改 / 启停 / 删除 / 预览影响）。
 * 纯逻辑在 `./linkage-logic.ts`（参数表单拼装 / 列表筛选 / 文案），单测 `tests/unit/flow-linkage.test.ts`。
 *
 * # 三个「不许自己发明」的点
 *
 * 1. 触发点、状态集、动作白名单、参数声明**全部来自 GET 下发**；前端没有第二份状态机/动作表。
 * 2. 动作下拉里 `implemented = false` 的项置灰（后端会 400，前端不给人试错的机会）。
 * 3. 写操作一律带 `confirm: true`，且弹窗里必须勾选确认框才能点保存（后端二次确认硬门的前端投影）。
 */
import { computed, reactive, ref, watch } from 'vue';
import { DialogPlugin, MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import { PERM } from '@/constants/permissions';
import {
  deleteLinkageRule,
  listLinkageRules,
  previewLinkage,
  saveLinkageRule,
  toggleLinkageRule,
  type LinkageAction,
  type LinkageParam,
  type LinkagePreviewRes,
  type LinkageRule,
  type LinkageTrigger,
} from '@/api/business/flow/linkage';
import {
  actionOptions,
  buildParamPayload,
  draftAlertTheme,
  emptyForm,
  filterRules,
  findAction,
  formatDateTime,
  formatParams,
  isHealthy,
  kindLabel,
  paramsToForm,
  phaseLabel,
  phaseTheme,
  previewHighlights,
  severityLabel,
  severityTheme,
  statusOptions,
  triggerOptions,
  type LinkageOption,
  type ParamForm,
} from './linkage-logic';

/** 表格列 */
const columns: PrimaryTableCol[] = [
  { colKey: 'name', title: '规则名', minWidth: 200 },
  { colKey: 'trigger', title: '触发点（源单据 → 状态）', width: 210 },
  { colKey: 'action', title: '目标动作（白名单）', width: 240 },
  { colKey: 'params', title: '动作参数', minWidth: 160 },
  { colKey: 'seq', title: '顺序', width: 70 },
  { colKey: 'enabled', title: '启用', width: 110 },
  { colKey: 'updated_at', title: '更新时间', width: 150 },
  { colKey: 'op', title: '操作', width: 210, fixed: 'right' },
];

/** 预览弹窗的步骤表列 */
const previewColumns: PrimaryTableCol[] = [
  { colKey: 'seq', title: '顺序', width: 64 },
  { colKey: 'name', title: '规则名', minWidth: 160 },
  { colKey: 'action_label', title: '动作', width: 190 },
  { colKey: 'phase', title: '事务 / 失败等级', width: 210 },
  { colKey: 'effect', title: '会动到什么（复用的既有实现）', minWidth: 240 },
];

// ==================== 列表 ====================

/** 规则（含禁用行） */
const rules = ref<LinkageRule[]>([]);
/** 触发点字典 */
const triggers = ref<LinkageTrigger[]>([]);
/** 动作字典 */
const actions = ref<LinkageAction[]>([]);
/** 加载中 */
const loading = ref(false);
/** 错误提示（可重试，不白屏） */
const errorMessage = ref('');
/** 正在启停的规则 id（按行 loading） */
const togglingId = ref('');

/** 筛选条件 */
const filter = reactive({ entity: '', keyword: '', onlyEnabled: false });

/** 实体筛选下拉（空 = 全部） */
const entityFilterOptions = computed<LinkageOption[]>(() => triggerOptions(triggers.value));

/** 筛选后的行 */
const filteredRows = computed(() => filterRules(rules.value, filter));

/** 源单据筛选变化 → 重新拉（后端支持按触发点过滤，这里顺带验证该参数） */
watch(
  () => filter.entity,
  () => {
    void loadList();
  },
);

/** 全部条数 */
const total = computed(() => rules.value.length);

/** 启用条数 */
const enabledCount = computed(() => rules.value.filter((rule) => rule.enabled).length);

/** 取错误文案（后端 400 的中文原因比「请求失败」有用得多） */
function errorText(error: unknown, fallback: string): string {
  return error instanceof ApiError ? error.message : fallback;
}

/** 拉列表（一次拿到规则 + 字典） */
async function loadList(): Promise<void> {
  loading.value = true;
  errorMessage.value = '';
  try {
    const data = await listLinkageRules(filter.entity || undefined);
    rules.value = data.rules ?? [];
    triggers.value = data.triggers ?? [];
    actions.value = data.actions ?? [];
  } catch (error) {
    rules.value = [];
    errorMessage.value = errorText(error, '联动规则加载失败');
  } finally {
    loading.value = false;
  }
}

// ==================== 弹窗表单 ====================

/** 弹窗可见 */
const editorVisible = ref(false);
/** 正在编辑的行（null = 新增） */
const editing = ref<LinkageRule | null>(null);
/** 表单主体 */
const form = reactive({
  name: '',
  trigger_entity: '',
  trigger_status: '',
  action_code: '',
  seq: 0,
  enabled: true,
});
/** 参数表单（键 = 动作声明的 key） */
const paramForm = ref<ParamForm>({});
/** 二次确认勾选 */
const confirmed = ref(false);
/** 保存中 */
const saving = ref(false);
/** 表单级错误（保存失败原因） */
const formError = ref('');
/** 草稿预览结果 */
const draftPreview = ref<LinkagePreviewRes | null>(null);
/** 草稿预览加载中 */
const draftPreviewLoading = ref(false);

/** 当前选中的动作声明 */
const currentAction = computed<LinkageAction | undefined>(() =>
  findAction(actions.value, form.action_code),
);

/** 触发点下拉 */
const triggerSelectOptions = computed<LinkageOption[]>(() => triggerOptions(triggers.value));

/** 状态下拉（跟随源单据） */
const statusSelectOptions = computed<LinkageOption[]>(() =>
  statusOptions(triggers.value, form.trigger_entity),
);

/** 动作下拉（未接管置灰） */
const actionSelectOptions = computed<LinkageOption[]>(() => actionOptions(actions.value));

/** 枚举参数的候选值 */
function enumOptions(param: LinkageParam): LinkageOption[] {
  return (param.kind.values ?? []).map((value) => ({ label: value, value }));
}

/** 源单据变了：状态不在新单据的状态集里就清掉 */
watch(
  () => form.trigger_entity,
  (entity) => {
    const allowed = statusOptions(triggers.value, entity).map((item) => item.value);
    if (form.trigger_status && !allowed.includes(form.trigger_status)) {
      form.trigger_status = allowed[0] ?? '';
    }
    if (!form.trigger_status && allowed.length > 0) {
      form.trigger_status = allowed[0];
    }
    draftPreview.value = null;
  },
);

/** 动作变了：参数表单按新动作的声明重建（旧键不能带过去） */
watch(
  () => form.action_code,
  (code) => {
    paramForm.value = emptyForm(findAction(actions.value, code));
    draftPreview.value = null;
  },
);

/** 打开新增 */
function openCreate(): void {
  editing.value = null;
  form.name = '';
  form.trigger_entity = triggers.value[0]?.entity ?? '';
  form.trigger_status = triggers.value[0]?.statuses[0] ?? '';
  form.action_code = '';
  form.seq = 0;
  form.enabled = true;
  paramForm.value = {};
  confirmed.value = false;
  formError.value = '';
  draftPreview.value = null;
  editorVisible.value = true;
}

/** 打开编辑（三项唯一键只读） */
function openEditor(row: LinkageRule): void {
  editing.value = row;
  form.name = row.name;
  form.trigger_entity = row.trigger_entity;
  form.trigger_status = row.trigger_status;
  form.action_code = row.action_code;
  form.seq = row.seq;
  form.enabled = row.enabled;
  const action = findAction(actions.value, row.action_code);
  paramForm.value = paramsToForm(action, row.action_params);
  confirmed.value = false;
  formError.value = '';
  draftPreview.value = null;
  editorVisible.value = true;
}

/** 关闭弹窗（清掉临时态，避免下次打开看到上次的预览） */
function closeEditor(): void {
  editorVisible.value = false;
  editing.value = null;
  draftPreview.value = null;
  formError.value = '';
}

/** 参数表单：文本类取值（联合类型收窄到 string） */
function paramText(key: string): string {
  const value = paramForm.value[key];
  return typeof value === 'string' ? value : '';
}

/** 参数表单：布尔取值 */
function paramBool(key: string): boolean {
  return paramForm.value[key] === true;
}

/** 参数表单：写值（整体替换，保证响应式） */
function setParam(key: string, value: string | boolean): void {
  paramForm.value = { ...paramForm.value, [key]: value };
}

/** 参数表单：数字框写入（`undefined` / 空串 = 清空，其余按字符串存） */
function setParamNumber(key: string, value: unknown): void {
  if (value === undefined || value === null || value === '') {
    setParam(key, '');
    return;
  }
  setParam(key, String(value));
}

/** 参数表单 → payload（有错误直接展示，不发请求） */
function buildPayload(): Record<string, unknown> | null {
  const { payload, errors } = buildParamPayload(currentAction.value, paramForm.value);
  if (errors.length > 0) {
    formError.value = errors.join('；');
    return null;
  }
  return payload;
}

/** 保存前预览影响 */
async function handlePreviewDraft(): Promise<void> {
  formError.value = '';
  if (!form.trigger_entity || !form.trigger_status) {
    formError.value = '先选择源单据类型与触发状态';
    return;
  }
  if (!form.action_code) {
    formError.value = '先选择目标动作';
    return;
  }
  const payload = buildPayload();
  if (!payload) {
    return;
  }
  draftPreviewLoading.value = true;
  try {
    draftPreview.value = await previewLinkage({
      trigger_entity: form.trigger_entity,
      trigger_status: form.trigger_status,
      draft: { action_code: form.action_code, action_params: payload },
    });
  } catch (error) {
    formError.value = errorText(error, '预览失败');
  } finally {
    draftPreviewLoading.value = false;
  }
}

/** 保存（新增 / 编辑共用；后端唯一键 = 触发点 + 动作） */
async function handleSave(): Promise<void> {
  formError.value = '';
  if (!form.name.trim()) {
    formError.value = '规则名不能为空';
    return;
  }
  if (!form.trigger_entity || !form.trigger_status || !form.action_code) {
    formError.value = '源单据类型 / 触发状态 / 目标动作都要选';
    return;
  }
  if (!confirmed.value) {
    formError.value = '请勾选确认框：改联动规则需要二次确认';
    return;
  }
  const payload = buildPayload();
  if (!payload) {
    return;
  }
  saving.value = true;
  try {
    await saveLinkageRule({
      name: form.name.trim(),
      trigger_entity: form.trigger_entity,
      trigger_status: form.trigger_status,
      action_code: form.action_code,
      action_params: payload,
      enabled: form.enabled,
      seq: form.seq ?? 0,
      confirm: true,
    });
    MessagePlugin.success(editing.value ? '规则已更新（下一次单据状态变更起生效）' : '规则已创建（下一次单据状态变更起生效）');
    editorVisible.value = false;
    await loadList();
  } catch (error) {
    formError.value = errorText(error, '保存失败');
  } finally {
    saving.value = false;
  }
}

// ==================== 启停 / 删除 ====================

/** 启停（弹二次确认；取消则开关视觉回弹） */
function handleToggleChange(row: LinkageRule, next: boolean): void {
  const dialog = DialogPlugin.confirm({
    header: next ? '启用这条联动规则？' : '停用这条联动规则？',
    body: next
      ? `「${row.name}」将在 ${
          row.trigger_entity_label
        } 走到 ${row.trigger_status} 后被引擎执行。改联动 = 改业务规则，请确认。`
      : `「${row.name}」将不再执行（配置保留，可随时重新启用）。`,
    theme: next ? 'warning' : 'default',
    confirmBtn: next ? '确认启用' : '确认停用',
    onConfirm: async () => {
      dialog.hide();
      togglingId.value = row.id;
      try {
        await toggleLinkageRule({ id: row.id, enabled: next, confirm: true });
        MessagePlugin.success(next ? '已启用' : '已停用');
        await loadList();
      } catch (error) {
        MessagePlugin.error(errorText(error, '启停失败'));
        await loadList();
      } finally {
        togglingId.value = '';
      }
    },
  });
}

/** 删除（软删：配置保留为已删除，重新保存同一动作会复活） */
function handleDelete(row: LinkageRule): void {
  const dialog = DialogPlugin.confirm({
    header: '删除这条联动规则？',
    body: `「${row.name}」（${row.trigger_entity} → ${row.trigger_status} · ${row.action_code}）将不再执行。删除是软删，历史仍可追；要恢复请重新创建同一触发点的同一动作。`,
    theme: 'danger',
    confirmBtn: '确认删除',
    onConfirm: async () => {
      dialog.hide();
      try {
        await deleteLinkageRule({ id: row.id, confirm: true });
        MessagePlugin.success('已删除');
        await loadList();
      } catch (error) {
        MessagePlugin.error(errorText(error, '删除失败'));
      }
    },
  });
}

// ==================== 影响预览弹窗 ====================

/** 预览弹窗可见 */
const previewVisible = ref(false);
/** 预览加载中 */
const previewLoading = ref(false);
/** 预览结果 */
const previewResult = ref<LinkagePreviewRes | null>(null);
/** 预览错误 */
const previewError = ref('');

/** 打开某个触发点的执行影响预览 */
async function openPreview(entity: string, status: string): Promise<void> {
  previewVisible.value = true;
  previewLoading.value = true;
  previewError.value = '';
  previewResult.value = null;
  try {
    previewResult.value = await previewLinkage({ trigger_entity: entity, trigger_status: status });
  } catch (error) {
    previewError.value = errorText(error, '预览失败');
  } finally {
    previewLoading.value = false;
  }
}

// ==================== 初始化 ====================

void loadList();
</script>

<style scoped>
.linkage-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.intro-card,
.filter-card,
.table-card {
  border-radius: 6px;
}

.intro__title {
  display: flex;
  align-items: center;
  gap: 8px;
  margin: 0 0 6px;
  font-size: 16px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.intro {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
}

.intro__desc {
  margin: 0;
  max-width: 860px;
  font-size: 13px;
  line-height: 1.7;
  color: var(--td-text-color-secondary);
}

.intro__desc code,
.cell-trigger__code,
.cell-action__code {
  padding: 0 4px;
  border-radius: 2px;
  background: var(--td-bg-color-container-hover);
  font-family: var(--td-font-family-mono, monospace);
  font-size: 12px;
  color: var(--td-text-color-secondary);
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

.filter-row__entity {
  width: 220px;
}

.filter-row__spacer {
  flex: 1;
}

.filter-row__count {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.table-alert {
  margin-bottom: 12px;
}

.cell-name__text {
  font-weight: 500;
  color: var(--td-text-color-primary);
}

.cell-name {
  display: flex;
  align-items: center;
  gap: 6px;
}

.cell-problem {
  margin-top: 2px;
  font-size: 12px;
  color: var(--td-error-color);
}

.cell-trigger {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-bottom: 4px;
}

.cell-trigger__entity {
  font-size: 13px;
  color: var(--td-text-color-primary);
}

.cell-action__label {
  font-size: 13px;
  color: var(--td-text-color-primary);
}

.cell-action__code {
  display: inline-block;
  margin-bottom: 4px;
}

.cell-params {
  display: inline-block;
  max-width: 220px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.cell-seq,
.cell-time {
  font-variant-numeric: tabular-nums;
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.cell-enabled__text {
  margin-left: 6px;
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.empty {
  padding: 24px 0;
}

.empty__hint {
  max-width: 620px;
  font-size: 12px;
  line-height: 1.7;
  color: var(--td-text-color-placeholder);
}

.editor__form {
  margin-top: 12px;
}

/* TDesign 的 controls-content 默认是 flex（为行内多控件准备的）：
   这里每个表单项是一个纵向堆叠「控件 + 说明文字」，必须改成块级，否则说明会挤到控件右边。 */
.editor :deep(.t-form__controls),
.editor :deep(.t-form__controls-content) {
  display: block;
  width: 100%;
}

/* 弹窗内容可能很长（参数多 / 预览说明长）：内容区内部滚动，标题与按钮始终在视口里。
   直接约束内容块（而不是 :deep(.t-dialog__body)）：弹窗是 teleport 渲染的，
   内容块带本组件的 scoped 属性，规则最稳。 */
.editor {
  max-height: min(64vh, 620px);
  overflow-y: auto;
  padding-right: 8px;
}

.preview {
  max-height: min(64vh, 620px);
  overflow-y: auto;
  padding-right: 8px;
}

.editor__action-hint {
  margin-top: 6px;
}

.editor__reuse {
  margin-top: 4px;
  font-size: 12px;
  line-height: 1.6;
  color: var(--td-text-color-placeholder);
  word-break: break-word;
}

.params {
  margin: 8px 0 4px;
  padding: 12px;
  border-radius: 6px;
  background: var(--td-bg-color-container-hover);
}

.params__title {
  margin-bottom: 8px;
  font-size: 13px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.params__hint {
  margin-left: 8px;
  font-size: 12px;
  font-weight: 400;
  color: var(--td-text-color-placeholder);
}

.params__note {
  margin-top: 2px;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.editor__preview {
  margin-top: 4px;
}

.editor__preview-body {
  margin-top: 8px;
}

.editor__preview-list {
  margin: 8px 0 0;
  padding-left: 18px;
  font-size: 12px;
  line-height: 1.8;
  color: var(--td-text-color-secondary);
}

.editor__preview-hint {
  margin-top: 4px;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.editor__confirm {
  margin-top: 12px;
}

.editor__error {
  margin-top: 8px;
}

.preview {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.preview__head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  flex-wrap: wrap;
}

.preview__trigger {
  font-size: 14px;
  font-weight: 500;
  color: var(--td-text-color-primary);
}

.preview__trigger code {
  padding: 0 4px;
  border-radius: 2px;
  background: var(--td-bg-color-container-hover);
  font-family: var(--td-font-family-mono, monospace);
  font-size: 12px;
}

.preview__warnings {
  margin: 0;
  padding-left: 18px;
  font-size: 12px;
  line-height: 1.8;
  color: var(--td-text-color-secondary);
}

.preview__effect {
  font-size: 12px;
  line-height: 1.6;
  color: var(--td-text-color-secondary);
  word-break: break-word;
}

.preview__hint {
  font-size: 12px;
  line-height: 1.7;
  color: var(--td-text-color-placeholder);
}

.preview__loading {
  padding: 24px 0;
  text-align: center;
}
</style>
