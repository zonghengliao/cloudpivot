/**
 * 库位新建 / 编辑弹窗。
 *
 * # 字段口径（照后端 `CreateLocationReq` / `UpdateLocationReq`）
 *
 * | 字段 | 新建 | 编辑 | 说明 |
 * |---|---|---|---|
 * | 库位编码 | 必填 | 只读 | 编码是身份，编辑接口没有该字段 |
 * | 名称 | 选填 | 可改 | ≤120 |
 * | 所属仓库 | **必选**（只能选叶子） | 只读 | 后端编辑接口没有 `warehouse_id`：换仓库 = 换身份 |
 * | 类型 | 6 选 1 | 可改 | `STORAGE / PICKING / RECEIVING / SHIPPING / QUARANTINE / SCRAP` |
 *
 * 所属仓库选择器里**非叶子仓库直接置灰**：选了后端也会 `400 + 66005`
 *（`handler.rs::location_create` 的 `warehouse.is_leaf == 0` 分支），不如提前拦。
 */
<template>
  <t-dialog
    :visible="visible"
    :header="isEdit ? '编辑库位' : '新建库位'"
    :confirm-btn="{ content: isEdit ? '保存' : '创建', loading: saving }"
    :close-on-overlay-click="false"
    width="560px"
    @update:visible="emit('update:visible', $event)"
    @confirm="handleConfirm"
    @close="emit('update:visible', false)"
  >
    <t-alert v-if="submitError" theme="error" :message="submitError" class="dlg-alert" />

    <t-form :data="form" label-width="96px">
      <t-form-item label="库位编码" name="location_code" :status="statusOf('location_code')" :tips="formErrors.location_code">
        <t-input
          v-model="form.location_code"
          :disabled="isEdit"
          :maxlength="40"
          placeholder="如 A-01-01（同一仓库内唯一）"
          data-testid="loc-code"
        />
      </t-form-item>

      <t-form-item label="库位名称" name="location_name" :status="statusOf('location_name')" :tips="formErrors.location_name">
        <t-input v-model="form.location_name" :maxlength="120" placeholder="如 A区一排一列" data-testid="loc-name" />
      </t-form-item>

      <t-form-item label="所属仓库" name="warehouse_id" :status="statusOf('warehouse_id')" :tips="formErrors.warehouse_id">
        <t-select
          v-if="!isEdit"
          v-model="form.warehouse_id"
          :options="warehouseOptions"
          placeholder="只能选叶子仓库（非叶子仓库不能挂库位）"
          clearable
          filterable
          data-testid="loc-warehouse"
        />
        <span v-else class="dlg-readonly" data-testid="loc-warehouse-readonly">{{ warehousePathText || '—' }}</span>
      </t-form-item>

      <t-form-item label="类型" name="location_type">
        <t-select v-model="form.location_type" :options="LOCATION_TYPE_OPTIONS" data-testid="loc-type" />
      </t-form-item>

      <t-form-item label="启用">
        <t-switch v-model="enabledSwitch" data-testid="loc-status" />
      </t-form-item>
    </t-form>
  </t-dialog>
</template>

<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import {
  createLocation,
  updateLocation,
  type Location,
  type WarehouseTreeNode,
} from '@/api/business/erp';
import {
  LOCATION_TYPE_OPTIONS,
  buildLocationCreatePayload,
  buildLocationUpdatePayload,
  emptyLocationForm,
  isEmptyUpdate,
  leafWarehouseOptions,
  locationFormFromModel,
  validateLocationForm,
  warehousePathText as pathTextOf,
  type FieldErrors,
  type LocationFormState,
} from '../warehouse-tree';

const props = defineProps<{
  /** 是否显示 */
  visible: boolean;
  /** 完整仓库树（所属仓库选择器用） */
  tree: WarehouseTreeNode[];
  /** 编辑对象；`null` = 新建 */
  editing: Location | null;
  /** 新建时预选的所属仓库 */
  defaultWarehouseId: string;
}>();

const emit = defineEmits<{
  'update:visible': [value: boolean];
  /** 保存成功 */
  saved: [payload: { id: string; isEdit: boolean; name: string }];
}>();

const form = reactive<LocationFormState>(emptyLocationForm());
const formErrors = ref<FieldErrors>({});
const submitError = ref('');
const saving = ref(false);

const isEdit = computed(() => props.editing !== null);

const enabledSwitch = computed({
  get: () => form.status === 1,
  set: (value: boolean) => {
    form.status = value ? 1 : 0;
  },
});

/** 所属仓库下拉：只列叶子仓库（带完整路径，平铺也分得清） */
const warehouseOptions = computed(() => leafWarehouseOptions(props.tree));

/** 当前所属仓库（校验非叶子用） */
const currentWarehouse = computed<WarehouseTreeNode | null>(() =>
  form.warehouse_id ? findNode(props.tree, form.warehouse_id) : null,
);

/** 编辑态展示所属仓库路径 */
const warehousePathText = computed(() =>
  form.warehouse_id ? pathTextOf(props.tree, form.warehouse_id) : '',
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

/** 每次打开重置表单 */
watch(
  () => props.visible,
  (visible) => {
    if (!visible) {
      return;
    }
    formErrors.value = {};
    submitError.value = '';
    Object.assign(
      form,
      props.editing ? locationFormFromModel(props.editing) : emptyLocationForm(props.defaultWarehouseId),
    );
  },
  { immediate: true },
);

/** 提交 */
async function handleConfirm(): Promise<void> {
  submitError.value = '';
  const errors = validateLocationForm(form, currentWarehouse.value);
  formErrors.value = errors;
  if (Object.keys(errors).length > 0) {
    submitError.value = '表单还有 ' + Object.keys(errors).length + ' 处需要修正';
    return;
  }
  saving.value = true;
  try {
    if (props.editing) {
      const payload = buildLocationUpdatePayload(form, props.editing);
      if (isEmptyUpdate(payload)) {
        MessagePlugin.info('没有改动，无需保存');
        emit('update:visible', false);
        return;
      }
      const saved = await updateLocation(props.editing.id, payload);
      MessagePlugin.success('库位已保存');
      emit('saved', { id: saved.id, isEdit: true, name: saved.location_code });
    } else {
      const saved = await createLocation(buildLocationCreatePayload(form));
      MessagePlugin.success(`库位「${saved.location_code}」已创建`);
      emit('saved', { id: saved.id, isEdit: false, name: saved.location_code });
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

.dlg-readonly {
  color: var(--td-text-color-secondary);
}
</style>
