/**
 * 仓库新建 / 编辑弹窗。
 *
 * # 字段口径（照后端 `CreateWarehouseReq` / `UpdateWarehouseReq`）
 *
 * | 字段 | 新建 | 编辑 | 说明 |
 * |---|---|---|---|
 * | 编码 | 必填 | **只读** | `UpdateWarehouseReq` 里根本没有 `warehouse_code` —— 编码是身份，不给改 |
 * | 名称 | 必填 | 可改 | ≤120 |
 * | 父仓库 | 树选择 | **只读** | 后端编辑接口没有 `parent_id`（换父 = 换身份 + 重算路径），所以只能选定不能改 |
 * | 层级 | **只读推导** | 只读 | 由上级推导（顶级 = 1），后端要求 `(parent_id 为空) = (level = 1)` |
 * | 是否叶子 | **只读推导** | 只读 | 派生值：建了子仓库就变「否」，删掉全部下级自动复位「是」 |
 *
 * 最后两行是卡面要求的字段，但都**不能提交** —— 硬塞进请求体会与后端契约不一致（后端没有这两个入参），
 * 所以这里做成只读展示 + tooltip 解释来源，比给个假开关诚实。
 */
<template>
  <t-dialog
    :visible="visible"
    :header="isEdit ? '编辑仓库' : '新建仓库'"
    :confirm-btn="{ content: isEdit ? '保存' : '创建', loading: saving }"
    :close-on-overlay-click="false"
    width="620px"
    @update:visible="emit('update:visible', $event)"
    @confirm="handleConfirm"
    @close="emit('update:visible', false)"
  >
    <t-alert v-if="submitError" theme="error" :message="submitError" class="dlg-alert" />

    <t-form :data="form" label-width="96px" class="warehouse-form">
      <t-form-item label="仓库编码" name="warehouse_code" :status="statusOf('warehouse_code')" :tips="formErrors.warehouse_code">
        <!-- CODE-06 交互方式 B：编码框 + 「生成编码」按钮（编辑态都禁用） -->
        <div class="dlg-inline dlg-inline--grow">
          <t-input
            v-model="form.warehouse_code"
            :disabled="isEdit"
            :maxlength="40"
            placeholder="留空自动生成，如 WH-01"
            class="dlg-inline__grow"
            data-testid="wh-code"
          />
          <t-button
            variant="outline"
            theme="primary"
            :disabled="isEdit"
            :loading="genCodeLoading"
            data-testid="wh-gen-code"
            @click="handleGenCode"
          >
            生成编码
          </t-button>
        </div>
        <template #help>
          <span class="dlg-help">
            {{
              isEdit
                ? '编码是仓库身份，不支持修改'
                : '不点「生成编码」则保存时自动生成；生成后仍可手改（已有编码体系可直接填）'
            }}
          </span>
        </template>
      </t-form-item>

      <t-form-item label="仓库名称" name="warehouse_name" :status="statusOf('warehouse_name')" :tips="formErrors.warehouse_name">
        <t-input v-model="form.warehouse_name" :maxlength="120" placeholder="如 华东分仓" data-testid="wh-name" />
      </t-form-item>

      <t-form-item label="父仓库" name="parent_id" :status="statusOf('parent_id')" :tips="formErrors.parent_id">
        <t-tree-select
          v-if="!isEdit"
          v-model="parentValue"
          :data="parentOptions"
          :tree-props="{ expandAll: true }"
          :keys="TREE_KEYS"
          placeholder="不选 = 顶级总仓"
          clearable
          filterable
          data-testid="wh-parent"
        />
        <span v-else class="dlg-readonly" data-testid="wh-parent-readonly">{{ parentPathText || '（顶级仓库）' }}</span>
      </t-form-item>

      <t-form-item label="层级">
        <div class="dlg-inline">
          <t-tag size="small" variant="light" :theme="levelTheme(nextLevel)">{{ levelLabel(nextLevel) }}</t-tag>
          <span class="dlg-help">
            {{ nextLevel === 1 ? '不选父仓库 = 第 1 层（总仓）' : `跟随父仓库自动推导：第 ${nextLevel} 层` }}
            <template v-if="nextLevel > MAX_WAREHOUSE_LEVEL">（超出 3 层上限，后端会拒）</template>
          </span>
        </div>
      </t-form-item>

      <t-form-item label="仓库类型" name="warehouse_type">
        <t-select v-model="form.warehouse_type" :options="WAREHOUSE_TYPE_OPTIONS" data-testid="wh-type" />
      </t-form-item>

      <t-form-item label="地址" name="address" :status="statusOf('address')" :tips="formErrors.address">
        <t-input v-model="form.address" :maxlength="255" placeholder="选填" data-testid="wh-address" />
      </t-form-item>

      <t-form-item label="负责人" name="manager" :status="statusOf('manager')" :tips="formErrors.manager">
        <t-input v-model="form.manager" :maxlength="50" placeholder="选填，文本" data-testid="wh-manager" />
      </t-form-item>

      <t-form-item label="是否叶子">
        <div class="dlg-inline">
          <t-tag size="small" :theme="form.is_leaf ? 'success' : 'default'" variant="light" data-testid="wh-leaf">
            {{ form.is_leaf ? '叶子（可以建库位）' : '非叶子（不能建库位）' }}
          </t-tag>
          <t-tooltip content="叶子状态由「有没有下级仓库」自动推导，不是开关：建了子仓库就变否，删掉全部下级会自动复位">
            <t-icon name="help-circle" class="dlg-help-icon" />
          </t-tooltip>
        </div>
      </t-form-item>

      <t-form-item label="启用">
        <t-switch v-model="enabledSwitch" data-testid="wh-status" />
      </t-form-item>
    </t-form>
  </t-dialog>
</template>

<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import {
  createWarehouse,
  fetchWarehouseNextCode,
  updateWarehouse,
  type Warehouse,
  type WarehouseTreeNode,
} from '@/api/business/erp';
import {
  MAX_WAREHOUSE_LEVEL,
  WAREHOUSE_TYPE_OPTIONS,
  buildWarehouseCreatePayload,
  buildWarehouseUpdatePayload,
  deriveWarehouseLevel,
  emptyWarehouseForm,
  isEmptyUpdate,
  levelLabel,
  levelTheme,
  parentPickableTree,
  validateWarehouseForm,
  warehouseFormFromModel,
  warehousePathText,
  type FieldErrors,
  type WarehouseFormState,
} from '../warehouse-tree';

const props = defineProps<{
  /** 是否显示 */
  visible: boolean;
  /** 完整仓库树（父仓库选择器用） */
  tree: WarehouseTreeNode[];
  /** 编辑对象；`null` = 新建 */
  editing: Warehouse | null;
  /** 新建时预选的父仓库（当前选中节点的 id） */
  defaultParentId: string | null;
}>();

const emit = defineEmits<{
  'update:visible': [value: boolean];
  /** 保存成功（父页面负责重载树并定位到新节点） */
  saved: [payload: { id: string | null; isEdit: boolean; name: string }];
}>();

/** 树选择器 / t-tree 共用的字段映射（与后端出参字段名一致） */
const TREE_KEYS = { value: 'id', label: 'warehouse_name', children: 'children' } as const;

const form = reactive<WarehouseFormState>(emptyWarehouseForm());
const formErrors = ref<FieldErrors>({});
const submitError = ref('');
const saving = ref(false);

const isEdit = computed(() => props.editing !== null);

/** 启用开关（`status` 1/0 与布尔互转，避免模板里写 `=== 1`） */
const enabledSwitch = computed({
  get: () => form.status === 1,
  set: (value: boolean) => {
    form.status = value ? 1 : 0;
  },
});

/** 父仓库选择器：第 3 层仓库不能再有下级 —— 直接从数据里剪掉（`treeProps.disabled` 只吃布尔） */
const parentOptions = computed(() => parentPickableTree(props.tree));

/** 父仓库的 v-model 桥：表单里 `parent_id` 用 `null` 表示顶级，控件只认 `''` / id */
const parentValue = computed<string>({
  get: () => form.parent_id ?? '',
  set: (value) => {
    form.parent_id = value === '' || value === null || value === undefined ? null : String(value);
  },
});

/** 当前所选的父仓库节点 */
const parentNode = computed<Warehouse | null>(() =>
  form.parent_id ? findNode(props.tree, form.parent_id) : null,
);

/** 推导出的层级（新建时用于展示；提交时由 buildWarehouseCreatePayload 再算一次） */
const nextLevel = computed(() => deriveWarehouseLevel(parentNode.value));

/** 编辑态展示上级路径 */
const parentPathText = computed(() =>
  form.parent_id ? warehousePathText(props.tree, form.parent_id) : '',
);

/** 递归找节点 */
function findNode(nodes: WarehouseTreeNode[], id: string): WarehouseTreeNode | null {
  for (const node of nodes) {
    if (node.id === id) {
      return node;
    }
    const hit = findNode(node.children ?? [], id);
    if (hit) {
      return hit;
    }
  }
  return null;
}

/** 字段错误 → t-form-item 的 status */
function statusOf(field: string): 'error' | undefined {
  return formErrors.value[field] ? 'error' : undefined;
}

/** 每次打开重置表单（新建回填预选父仓库；编辑回填详情） */
watch(
  () => props.visible,
  (visible) => {
    if (!visible) {
      return;
    }
    formErrors.value = {};
    submitError.value = '';
    const next = props.editing
      ? warehouseFormFromModel(props.editing)
      : emptyWarehouseForm(
          props.defaultParentId ? findNode(props.tree, props.defaultParentId) : null,
        );
    Object.assign(form, next);
  },
  { immediate: true },
);

// ==================== 生成编码（CODE-06 交互方式 B） ====================

/** 「生成编码」按钮加载中 */
const genCodeLoading = ref(false);

/**
 * 取下一个可用编码填入编码框（**不占号**，可手改）。
 *
 * 后端 `GET /business/erp/warehouse/next-code` 只读 `biz_seq`（方案 B）：
 * - 点了又取消不会浪费流水；
 * - 并发下两人可能看到同一个号 ⇒ 保存时被抢走的人是 `409 + 66001`，
 *   错误面板会把后端原文（点名编码）弹出来，点一次「生成编码」换个号即可。
 * - 号段是 **2 位**纯流水（`WH-01`），第 100 个起自然变成 `WH-100`。
 */
async function handleGenCode(): Promise<void> {
  if (isEdit.value) {
    return;
  }
  genCodeLoading.value = true;
  try {
    const preview = await fetchWarehouseNextCode();
    form.warehouse_code = preview.code;
    // 之前「超长」之类的提示已不成立，清掉免得红框跟着新号走
    delete formErrors.value.warehouse_code;
  } catch (error) {
    MessagePlugin.error(
      `生成编码失败：${error instanceof ApiError ? error.message : '请稍后重试'}`,
    );
  } finally {
    genCodeLoading.value = false;
  }
}

/** 提交 */
async function handleConfirm(): Promise<void> {
  submitError.value = '';
  const errors = validateWarehouseForm(form, parentNode.value);
  formErrors.value = errors;
  if (Object.keys(errors).length > 0) {
    submitError.value = '表单还有 ' + Object.keys(errors).length + ' 处需要修正';
    return;
  }
  saving.value = true;
  try {
    if (props.editing) {
      const payload = buildWarehouseUpdatePayload(form, props.editing);
      if (isEmptyUpdate(payload)) {
        MessagePlugin.info('没有改动，无需保存');
        emit('update:visible', false);
        return;
      }
      const saved = await updateWarehouse(props.editing.id, payload);
      MessagePlugin.success('仓库已保存');
      emit('saved', { id: saved.id, isEdit: true, name: saved.warehouse_name });
    } else {
      const saved = await createWarehouse(buildWarehouseCreatePayload(form, parentNode.value));
      // 没点「生成编码」直接保存时编码是后端取的号 —— 回填到表单并在提示里报出来
      form.warehouse_code = saved.warehouse_code;
      MessagePlugin.success(`仓库「${saved.warehouse_name}」已创建 · 编码 ${saved.warehouse_code}`);
      emit('saved', { id: saved.id, isEdit: false, name: saved.warehouse_name });
    }
    emit('update:visible', false);
  } catch (error) {
    submitError.value = error instanceof ApiError ? error.message : '保存失败，请稍后重试';
  } finally {
    saving.value = false;
  }
}
</script>

<style scoped lang="less">
.dlg-alert {
  margin-bottom: 12px;
}

.dlg-inline {
  display: flex;
  align-items: center;
  gap: 8px;
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

.dlg-readonly {
  color: var(--td-text-color-secondary);
}

.dlg-help {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.dlg-help-icon {
  color: var(--td-text-color-placeholder);
  cursor: help;
}
</style>
