<template>
  <div class="rule-page">
    <!-- ==================== ① 筛选 / 分页 / 行操作交给 ListPage（P3-12） ==================== -->
    <ListPage
      ref="listRef"
      title="佣金规则"
      testid="rule"
      table-testid="rule-table"
      meta-testid="rule-meta"
      btn-prefix="btn-rule"
      :columns="columns"
      :filters="filters"
      :filter-state="filter"
      :initial-page="current"
      :fetcher="fetcher"
      :query-builder="queryBuilder"
      :page-size="RULE_PAGE_SIZE"
      @loaded="onLoaded"
    >
      <!-- 命中优先级 / 生效范围 / 只影响之后生成的结算单：整宽提示条 -->
      <template #filter-footer>
        <p class="page-head__tip">
          命中优先级：<b>销售员 &gt; 物料 &gt; 全局</b>；同级取生效日更新的。
          「按物料」本期绑定的是<b>具体物料</b>（基础版没有物料分类表）。
          规则只影响之后生成的结算单 —— 已审批 / 已发放的结算单不会被改写。
        </p>
      </template>

      <template #head-extra>
        <span class="rule-head">
          <t-tag size="small" variant="light" theme="primary">按销售员 / 按物料 / 全局</t-tag>
          <span class="page-head__hint">
            停用 / 过期规则仍留在列表里（历史明细要引用它），只是不再参与匹配
          </span>
        </span>
      </template>

      <template #toolbar>
        <t-tooltip
          :content="
            canCreate
              ? '新建一条佣金规则（同范围多条并存时按「销售员 > 物料 > 全局 → 生效日新 → 创建新」取一条）'
              : '当前账号没有 business:erp:commission-rule:create 权限'
          "
          placement="top-right"
        >
          <span class="gate-wrap">
            <t-button
              theme="primary"
              data-testid="btn-rule-new"
              :disabled="!canCreate"
              @click="openCreate"
            >
              <template #icon><t-icon name="add" /></template>
              新建规则
            </t-button>
          </span>
        </t-tooltip>
      </template>

      <template #scope_type="{ row }">
        <t-tag size="small" variant="outline" :theme="scopeTheme(row.scope_type)">
          {{ scopeTypeLabel(row.scope_type) }}
        </t-tag>
      </template>
      <template #scope_display="{ row }">
        <span class="scope-cell">{{ scopeDisplay(row as CommissionRuleRow) }}</span>
      </template>
      <template #calc_type="{ row }">
        <span>{{ calcTypeLabel(row.calc_type) }}</span>
      </template>
      <template #rate="{ row }">
        <span class="mono" data-testid="rule-rate">{{ rateText(row as CommissionRuleRow) }}</span>
      </template>
      <template #amount_range="{ row }">
        <span class="mono" data-testid="rule-amount-range">
          {{ amountRangeText(row as CommissionRuleRow) }}
        </span>
      </template>
      <template #validity="{ row }">
        <span class="mono validity-cell">
          {{ validityText(row.effective_from, row.effective_to) }}
        </span>
      </template>
      <template #status="{ row }">
        <div class="status-cell">
          <t-tag size="small" variant="light" :theme="ruleStatusTheme(row.status)">
            {{ ruleStatusLabel(row.status) }}
          </t-tag>
          <t-tag
            v-if="!isRuleEffective(row as CommissionRuleRow)"
            size="small"
            variant="light"
            theme="default"
            data-testid="rule-off-period"
          >
            不在有效期
          </t-tag>
        </div>
      </template>

      <!-- 行操作：编辑 / 启停 / 删除各自带权限码，停用与删除语义不同（停用保留历史引用） -->
      <template #op="{ row }">
        <div class="op-row">
          <t-link
            v-privilege="[PERM.update]"
            theme="primary"
            data-testid="btn-rule-edit"
            @click="openEdit(row as CommissionRuleRow)"
          >
            编辑
          </t-link>
          <t-link
            v-privilege="[PERM.update]"
            :theme="row.status === 'ACTIVE' ? 'warning' : 'success'"
            data-testid="btn-rule-toggle"
            @click="confirmToggle(row as CommissionRuleRow)"
          >
            {{ row.status === 'ACTIVE' ? '停用' : '启用' }}
          </t-link>
          <t-link
            v-privilege="[PERM.remove]"
            theme="danger"
            data-testid="btn-rule-delete"
            @click="confirmDelete(row as CommissionRuleRow)"
          >
            删除
          </t-link>
        </div>
      </template>

      <template #empty>
        <div class="panel-empty" data-testid="rule-empty">
          <t-icon name="file" class="panel-empty__icon" />
          <p class="panel-empty__title">{{ emptyText }}</p>
        </div>
      </template>
    </ListPage>

    <!-- ==================== ② 新建 / 编辑弹窗 ==================== -->
    <t-dialog
      v-model:visible="formVisible"
      :header="editingId ? '编辑佣金规则' : '新建佣金规则'"
      :width="780"
      :confirm-btn="{ content: editingId ? '保存' : '创建', loading: submitting, disabled: submitting }"
      :cancel-btn="{ disabled: submitting }"
      :close-on-overlay-click="false"
      data-testid="rule-form-dialog"
      @confirm="submitForm"
    >
      <div class="form-body">
        <t-form label-width="120px" class="form">
          <t-form-item label="规则名" required-mark>
            <t-input
              v-model="form.rule_name"
              :maxlength="RULE_NAME_MAX"
              placeholder="人读名，例如「华东区销售 5%」「A 物料 10%」"
              data-testid="rule-form-name"
            />
          </t-form-item>

          <t-form-item label="适用范围" required-mark>
            <t-radio-group
              :value="form.scope_type"
              data-testid="rule-form-scope"
              @change="handleScopeChange"
            >
              <t-radio v-for="item in SCOPE_OPTIONS" :key="item.value" :value="item.value">
                {{ item.label }}
              </t-radio>
            </t-radio-group>
          </t-form-item>

          <!-- 范围对象：随 scope_type 变；候选拉不到时降级成手填 id -->
          <t-form-item v-if="form.scope_type === 'SALESPERSON'" label="销售员" required-mark>
            <t-select
              v-if="!userOptionsFailed"
              v-model="form.scope_id"
              :options="userOptions"
              filterable
              clearable
              placeholder="选择销售员（只列启用账号）"
              data-testid="rule-form-salesperson"
            />
            <t-input
              v-else
              v-model="form.scope_id"
              placeholder="候选列表无权限读取：直接粘贴销售员 UUID（sys_user.id）"
              data-testid="rule-form-scope-id"
            />
          </t-form-item>

          <t-form-item v-else-if="form.scope_type === 'MATERIAL_CATEGORY'" label="物料" required-mark>
            <t-select
              v-if="!materialOptionsFailed"
              v-model="form.scope_id"
              :options="materialOptions"
              filterable
              clearable
              placeholder="选择物料（基础版按具体物料绑定，不是分类）"
              data-testid="rule-form-material"
            />
            <t-input
              v-else
              v-model="form.scope_id"
              placeholder="候选列表无权限读取：直接粘贴物料 UUID（biz_material.id）"
              data-testid="rule-form-scope-id"
            />
          </t-form-item>

          <t-form-item v-else label="范围对象">
            <span class="form-hint">全局规则不绑定对象（后端要求 scope_id 为空）</span>
          </t-form-item>

          <t-form-item label="计算方式" required-mark>
            <t-radio-group
              :value="form.calc_type"
              data-testid="rule-form-calc"
              @change="handleCalcChange"
            >
              <t-radio v-for="item in CALC_OPTIONS" :key="item.value" :value="item.value">
                {{ item.label }}
              </t-radio>
            </t-radio-group>
          </t-form-item>

          <t-form-item :label="form.calc_type === 'PERCENT' ? '费率' : '固定金额'" required-mark>
            <div class="rate-cell">
              <t-input-number
                v-model="form.rate"
                theme="normal"
                :decimal-places="4"
                :min="0"
                :max="form.calc_type === 'PERCENT' ? 100 : undefined"
                align="right"
                :placeholder="ratePlaceholder(form.calc_type)"
                data-testid="rule-form-rate"
              />
              <span class="rate-cell__suffix">{{ rateSuffix(form.calc_type) }}</span>
            </div>
          </t-form-item>

          <t-form-item label="起算金额">
            <t-input-number
              v-model="form.min_amount"
              theme="normal"
              :decimal-places="4"
              :min="0"
              align="right"
              placeholder="佣金基数低于此值不计佣，默认 0"
              data-testid="rule-form-min"
            />
          </t-form-item>

          <t-form-item label="封顶金额">
            <t-input-number
              v-model="form.max_amount"
              theme="normal"
              :decimal-places="4"
              :min="0"
              align="right"
              placeholder="留空 = 不封顶"
              data-testid="rule-form-max"
            />
          </t-form-item>

          <t-form-item label="生效起始日" required-mark>
            <t-date-picker
              v-model="form.effective_from"
              value-type="YYYY-MM-DD"
              format="YYYY-MM-DD"
              data-testid="rule-form-from"
            />
          </t-form-item>

          <t-form-item label="生效截止日">
            <t-date-picker
              v-model="form.effective_to"
              value-type="YYYY-MM-DD"
              format="YYYY-MM-DD"
              placeholder="留空 = 长期有效"
              clearable
              data-testid="rule-form-to"
            />
          </t-form-item>

          <t-form-item label="状态">
            <t-radio-group v-model="form.status" data-testid="rule-form-status">
              <t-radio value="ACTIVE">启用（参与计佣）</t-radio>
              <t-radio value="INACTIVE">停用（保留历史，不再命中）</t-radio>
            </t-radio-group>
          </t-form-item>

          <t-form-item label="备注">
            <t-textarea
              v-model="form.remark"
              :maxlength="RULE_REMARK_MAX"
              placeholder="选填，例如活动名 / 审批单号"
              data-testid="rule-form-remark"
            />
          </t-form-item>
        </t-form>

        <t-alert
          v-if="optionHint"
          theme="warning"
          :message="optionHint"
          class="form-alert"
          data-testid="rule-form-option-hint"
        />
        <t-alert
          v-if="formError"
          theme="error"
          :message="formError"
          class="form-alert"
          data-testid="rule-form-error"
        />
      </div>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 佣金规则页（销售链 · SO-37 / ARCH-7 §15.4.2 ②）
 *
 * # 一句话业务
 *
 * 配置「谁 / 哪类物料 / 全局」按什么比例（或固定额）提成，带起算金额与封顶、有效期；
 * 结算时按 `销售员 > 物料 > 全局` 取一条规则算钱。
 *
 * # 三条会算错钱的约定
 *
 * 1. **`PERCENT` 的 `rate` 是百分数**（填 5 = 5%），不是 0.05 —— 切换计算方式时
 *    必须清空 `rate`（`onCalcTypeChange`），否则 5% 会变成 5 元/单；
 * 2. **`ALL` 不能带 `scope_id`**（库层 `ck_biz_cmrule_scope_id` 强制），切换范围时
 *    清空对象（`onScopeTypeChange`）；
 * 3. **停用 ≠ 删除**：停用改 `status=INACTIVE`，历史明细仍引用这条规则；删除只用于
 *    「录错了」，已产生明细的规则删掉会让历史明细指向一条不存在的规则。
 *
 * # 权限
 *
 * 三个码各自对应一个按钮（`v-privilege` 只隐藏按钮，后端还会再判）；
 * 列表本身需要 `business:erp:commission-rule:list`（路由 meta 也配了）。
 *
 * # 候选下拉的降级
 *
 * 销售员候选要 `system:user:list`、物料候选要 `business:erp:material:list`，
 * 销售主管 / 销售员这两个角色**都没有**（V0267 的矩阵里没授）。所以候选拉取失败时
 * 不报错，改成「手填 UUID」并给一条黄色提示 —— 页面可用性优先，别让配规则这件事
 * 因为权限矩阵的缺口彻底走不通。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin } from 'tdesign-vue-next';
import { useUserStore } from '@/stores/user';
import { ApiError } from '@/api/request';
import {
  createCommissionRule,
  deleteCommissionRule,
  fetchCommissionRulePage,
  updateCommissionRule,
  type CommissionCalcType,
  type CommissionRuleFilter,
  type CommissionRuleRow,
  type CommissionScopeType,
} from '@/api/business/erp/commission';
import { fetchMaterialPage, type MaterialRow } from '@/api/business/erp';
import { pageUsers, type UserRes } from '@/api/system/user-api';
import ListPage from '@/components/ListPage/index.vue';
import type { FilterDef, ListFetcher, ListQueryBuilder } from '@/components/ListPage/types';
import { todayString } from '@/views/business/erp/shared/money';
import {
  CALC_OPTIONS,
  RULE_COLUMNS,
  RULE_NAME_MAX,
  RULE_PAGE_SIZE,
  RULE_REMARK_MAX,
  SCOPE_FILTER_OPTIONS,
  SCOPE_OPTIONS,
  RULE_STATUS_OPTIONS,
  amountRangeText,
  buildRulePayload,
  calcTypeLabel,
  emptyRuleFilter,
  emptyRuleForm,
  onCalcTypeChange,
  onScopeTypeChange,
  ratePlaceholder,
  rateSuffix,
  rateText,
  ruleEmptyText,
  ruleFormFrom,
  ruleStatusLabel,
  ruleStatusTheme,
  scopeDisplay,
  scopeTypeLabel,
  validateRuleForm,
  validityText,
  type RuleFormDraft,
} from './rule-logic';

/** 本页权限码（与 `V0310__commission_permissions.up.sql` 的 auth_code 逐字一致） */
const PERM = {
  list: 'business:erp:commission-rule:list',
  create: 'business:erp:commission-rule:create',
  update: 'business:erp:commission-rule:update',
  remove: 'business:erp:commission-rule:delete',
} as const;

const userStore = useUserStore();
const canCreate = computed(() => userStore.hasPermission(PERM.create));

const columns = RULE_COLUMNS;
const scopeFilterOptions = SCOPE_FILTER_OPTIONS;
const statusOptions = RULE_STATUS_OPTIONS;

/** 错误 → 人话（`ApiError` 带后端 msg，别吃掉） */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

/** 范围标签配色（销售员最具体 = 品牌蓝，物料 = 成功，全局 = 灰） */
function scopeTheme(
  value: string,
): 'primary' | 'success' | 'default' | 'warning' | 'danger' {
  if (value === 'SALESPERSON') {
    return 'primary';
  }
  if (value === 'MATERIAL_CATEGORY') {
    return 'success';
  }
  return 'default';
}

/** 规则当前是否在有效期内（只做展示提醒，匹配由后端判） */
function isRuleEffective(row: CommissionRuleRow): boolean {
  const today = todayString();
  if (row.effective_from && today < row.effective_from) {
    return false;
  }
  if (row.effective_to && today > row.effective_to) {
    return false;
  }
  return true;
}

// ==================== 列表（交给 ListPage） ====================

/** 筛选行（`filterState` 就地读写；`ruleEmptyText` 引用同一个对象） */
const filter = reactive<CommissionRuleFilter>(emptyRuleFilter());
/** 当前页（保存后回页用；每拉完一页由 `onLoaded` 同步） */
const current = ref(1);
/** ListPage 实例（新建 / 编辑 / 启停 / 删除后重拉列表） */
const listRef = ref<InstanceType<typeof ListPage> | null>(null);

/** 空态文案 */
const emptyText = computed(() => ruleEmptyText(filter));

/**
 * 筛选项声明。
 *
 * 改造前三个控件**都**不 auto-search（只有「查询」按钮触发），这里同样不声明
 * `searchOnChange` —— 关键词走回车、下拉选完点「查询」，手感与改造前一致。
 */
const filters: FilterDef[] = [
  {
    key: 'keyword',
    type: 'keyword',
    placeholder: '规则名 / 销售员 / 物料',
    width: 260,
    testid: 'rule-filter-keyword',
  },
  {
    key: 'scope_type',
    type: 'select',
    placeholder: '全部范围',
    width: 170,
    options: scopeFilterOptions,
    testid: 'rule-filter-scope',
  },
  {
    key: 'status',
    type: 'select',
    placeholder: '全部状态',
    width: 170,
    options: statusOptions,
    testid: 'rule-filter-status',
  },
];

/** 拉一页 */
const fetcher: ListFetcher<CommissionRuleRow> = (query) =>
  fetchCommissionRulePage(query as CommissionRuleFilter);

/** 入参拼装（沿用改造前「整份 filter + 页码」的口径，键名不变） */
const queryBuilder: ListQueryBuilder = (state, page, pageSize) => ({
  ...(state as CommissionRuleFilter),
  page,
  page_size: pageSize,
});

/** 重拉到指定页（不传 = 当前页；删到最后一页停在空页由组件回夹） */
async function reloadList(page: number = current.value): Promise<void> {
  await listRef.value?.load(page);
}

/** 一页拉完：同步页码（保存后回页要用） */
function onLoaded(payload: { page: number }): void {
  current.value = payload.page;
}

// ==================== 候选下拉（销售员 / 物料） ====================

const users = ref<UserRes[]>([]);
const materials = ref<MaterialRow[]>([]);
const userOptionsFailed = ref(false);
const materialOptionsFailed = ref(false);

const userOptions = computed(() =>
  users.value.map((item) => ({
    label: `${item.real_name || item.username}（${item.username}）`,
    value: item.id,
  })),
);

const materialOptions = computed(() =>
  materials.value.map((item) => ({
    label: `${item.material_code} ${item.material_name}${item.spec ? ` / ${item.spec}` : ''}`,
    value: item.id,
  })),
);

/** 候选不可用时的提示（只在打开弹窗、且确实需要候选时出现） */
const optionHint = computed(() => {
  if (form.scope_type === 'SALESPERSON' && userOptionsFailed.value) {
    return '当前账号没有 system:user:list 权限，销售员候选拉不到：请粘贴销售员 UUID（可让管理员在「用户管理」里查）。';
  }
  if (form.scope_type === 'MATERIAL_CATEGORY' && materialOptionsFailed.value) {
    return '当前账号没有 business:erp:material:list 权限，物料候选拉不到：请粘贴物料 UUID（可让管理员在「物料管理」里查）。';
  }
  return '';
});

/** 拉候选：两边互不依赖，单边失败只置降级标记，不影响列表 */
async function loadOptions(): Promise<void> {
  const [userResult, materialResult] = await Promise.allSettled([
    pageUsers({ page: 1, page_size: 200, status: 1 }),
    fetchMaterialPage({ page: 1, page_size: 200, status: 1 }),
  ]);
  if (userResult.status === 'fulfilled') {
    users.value = userResult.value.list ?? [];
    userOptionsFailed.value = users.value.length === 0;
  } else {
    users.value = [];
    userOptionsFailed.value = true;
  }
  if (materialResult.status === 'fulfilled') {
    materials.value = materialResult.value.list ?? [];
    materialOptionsFailed.value = materials.value.length === 0;
  } else {
    materials.value = [];
    materialOptionsFailed.value = true;
  }
}

// ==================== 新建 / 编辑 ====================

const formVisible = ref(false);
const submitting = ref(false);
const editingId = ref('');
const formError = ref('');
const form = reactive<RuleFormDraft>(emptyRuleForm(todayString()));

function resetForm(today = todayString()): void {
  Object.assign(form, emptyRuleForm(today));
  formError.value = '';
  editingId.value = '';
}

function openCreate(): void {
  resetForm();
  formVisible.value = true;
}

function openEdit(row: CommissionRuleRow): void {
  resetForm();
  Object.assign(form, ruleFormFrom(row));
  editingId.value = row.id;
  formVisible.value = true;
}

/** 范围类型变化：`ALL` 必须清掉 `scope_id`，否则库层 CHECK 会 400 */
function handleScopeChange(value: unknown): void {
  const next = value as CommissionScopeType;
  onScopeTypeChange(form, next);
}

/** 计算方式变化：清空 `rate`（百分数 ↔ 金额语义完全不同） */
function handleCalcChange(value: unknown): void {
  const next = value as CommissionCalcType;
  onCalcTypeChange(form, next);
}

async function submitForm(): Promise<void> {
  if (submitting.value) {
    return;
  }
  const error = validateRuleForm(form);
  if (error) {
    formError.value = error;
    return;
  }
  formError.value = '';
  submitting.value = true;
  try {
    const payload = buildRulePayload(form);
    if (editingId.value) {
      await updateCommissionRule(editingId.value, payload);
      MessagePlugin.success('佣金规则已保存');
    } else {
      await createCommissionRule(payload);
      MessagePlugin.success('佣金规则已创建');
    }
    formVisible.value = false;
    await reloadList(editingId.value ? current.value : 1);
  } catch (err) {
    // 后端错误原样展示（库层 CHECK / 唯一键 / 权限），不要编安慰话
    formError.value = messageOf(err);
  } finally {
    submitting.value = false;
  }
}

// ==================== 停用 / 启用 / 删除 ====================

/** 停用 / 启用：走编辑接口只改 status（停用不是删除，历史明细仍引用它） */
function confirmToggle(row: CommissionRuleRow): void {
  const nextStatus = row.status === 'ACTIVE' ? 'INACTIVE' : 'ACTIVE';
  const verb = nextStatus === 'INACTIVE' ? '停用' : '启用';
  const dialog = DialogPlugin.confirm({
    header: `${verb}佣金规则`,
    body: `确认${verb}「${row.rule_name}」？${
      nextStatus === 'INACTIVE'
        ? '停用后不再参与计佣，已生成的结算单 / 明细不受影响（删除只用于录错了的规则）。'
        : '启用后立即参与计佣匹配（生效日 / 有效期仍然生效）。'
    }`,
    confirmBtn: verb,
    theme: nextStatus === 'INACTIVE' ? 'warning' : 'info',
    onConfirm: async () => {
      try {
        await updateCommissionRule(row.id, {
          ...buildRulePayload(ruleFormFrom(row)),
          status: nextStatus,
        });
        MessagePlugin.success(`已${verb}`);
        dialog.destroy();
        await reloadList();
      } catch (error) {
        MessagePlugin.error(`${verb}失败：${messageOf(error)}`);
      }
    },
  });
}

/** 删除（软删）：只用于「录错了」，已产生明细的规则应该停用 */
function confirmDelete(row: CommissionRuleRow): void {
  const dialog = DialogPlugin.confirm({
    header: '删除佣金规则',
    body: `确认删除「${row.rule_name}」？软删后历史明细仍能读到，但这条规则不再出现在列表里。若只是不想再用，请点「停用」。`,
    confirmBtn: '确认删除',
    theme: 'danger',
    onConfirm: async () => {
      try {
        await deleteCommissionRule(row.id);
        MessagePlugin.success('已删除');
        dialog.destroy();
        await reloadList();
      } catch (error) {
        MessagePlugin.error(`删除失败：${messageOf(error)}`);
      }
    },
  });
}

onMounted(() => {
  void loadOptions();
});
</script>

<style scoped lang="less">
.rule-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

/* 表头右侧：范围标签 + 「停用 / 过期仍留档」说明 */
.rule-head {
  display: inline-flex;
  align-items: center;
  gap: 8px;
}

.page-head__tip {
  margin: 0;
  font-size: 12px;
  line-height: 20px;
  color: var(--td-text-color-secondary);
}

.page-head__hint {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.gate-wrap {
  display: inline-flex;
}
.mono {
  font-family: var(--td-font-family-mono, monospace);
}

.scope-cell {
  display: inline-block;
  max-width: 100%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.validity-cell {
  white-space: nowrap;
}

.status-cell {
  display: flex;
  align-items: center;
  gap: 4px;
  flex-wrap: wrap;
}

.op-row {
  display: flex;
  align-items: center;
  gap: 12px;
  white-space: nowrap;
}

.form-body {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.form :deep(.t-form__item) {
  margin-bottom: 12px;
}

/*
 * 表单控件铺满一行：TDesign 的 `t-date-picker` / `t-select` 默认宽度按内容，
 * 日期在 780px 弹窗里会被截成「2026-11-0…」（真机验收 2026-11-07 实测）。
 * 只对表单内的这两类控件设 100%，不动 `t-input-number`（它有自己的宽度与后缀）。
 */
.form :deep(.t-form__controls-content .t-date-picker),
.form :deep(.t-form__controls-content .t-select) {
  width: 100%;
}

.form-hint {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.rate-cell {
  display: flex;
  align-items: center;
  gap: 8px;
}

.rate-cell__suffix {
  font-size: 13px;
  color: var(--td-text-color-secondary);
}

.form-alert {
  margin-top: 8px;
}
</style>
