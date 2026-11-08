<!--
  单据附件面板（ARCH-3 DC-17 / P1-4）

  在四个单据详情抽屉里复用同一个组件：合同 / 采购单 / 收货单 / 应付单（付款相关）。
  交互照 `views/business/task/detail.vue` 的协作区附件（拖拽 / 点击选择 / 下载 / 删除），
  存储与策略全部交给后端（`business/storage`）——本组件**只**管「选文件 + 传 + 列 + 删」。

  # 为什么角色和备注放在工具条而不是弹窗
  单据附件 90% 的操作是「扫完就传」。多一层弹窗等于每次多两次点击；角色下拉默认值
  由调用方按单据类型给（合同→合同扫描件、收货→签收单、付款→发票影像）。

  # 删除按钮
  显示与否完全由后端 `can_delete` 决定（上传人本人 ∪ `business:attachment:delete`），
  前端不自己算一遍权限 —— 两处判据必然漂移，最后就是「按钮在、点了 403」。
-->
<template>
  <div class="doc-attach">
    <div class="doc-attach__head">
      <span class="doc-attach__title">附件</span>
      <span class="doc-attach__hint">已上传 {{ total }} / {{ maxCount }}</span>
    </div>

    <div v-if="!docId" class="doc-attach__empty">保存单据后可上传附件</div>

    <template v-else>
      <div class="doc-attach__toolbar">
        <t-select
          v-model="role"
          size="small"
          :options="roleOptions"
          class="doc-attach__role"
          :data-testid="`${testidPrefix}-role`"
        />
        <t-input
          v-model="remark"
          size="small"
          placeholder="备注（可空，≤200 字）"
          class="doc-attach__remark-input"
          :maxlength="200"
          :data-testid="`${testidPrefix}-remark`"
        />
      </div>

      <div
        class="doc-attach__drop"
        :class="{ 'doc-attach__drop--over': dragOver }"
        :data-testid="`${testidPrefix}-drop`"
        @dragover.prevent="dragOver = true"
        @dragleave.prevent="dragOver = false"
        @drop.prevent="onDrop"
        @click="pickFile"
      >
        <t-icon name="upload" size="18px" />
        <span>拖拽文件到此处，或点击选择（大小 / 格式按存储策略，单文件上限 100 MB）</span>
        <input ref="fileInputRef" type="file" class="doc-attach__input" @change="onFilePicked" />
      </div>

      <div v-if="loading" class="doc-attach__empty">加载中…</div>
      <div v-else-if="items.length === 0" class="doc-attach__empty">暂无附件</div>
      <div v-else class="doc-attach__list">
        <div
          v-for="item in items"
          :key="item.id"
          class="doc-attach__item"
          :data-testid="`${testidPrefix}-item`"
        >
          <t-tag size="small" variant="light" theme="primary">{{ item.file_role_label }}</t-tag>
          <span class="doc-attach__name" :title="item.file_name">{{ item.file_name }}</span>
          <span class="doc-attach__meta">{{ formatFileSize(item.file_size) }}</span>
          <span class="doc-attach__meta">{{ item.uploader_name || '—' }}</span>
          <span class="doc-attach__meta">{{ formatDateTime(item.created_at) }}</span>
          <span v-if="item.remark" class="doc-attach__note" :title="item.remark">{{ item.remark }}</span>
          <t-button size="small" variant="text" @click="openPreview(item)">预览</t-button>
          <t-button size="small" variant="text" @click="download(item)">下载</t-button>
          <t-button
            v-if="item.can_delete"
            size="small"
            variant="text"
            theme="danger"
            @click="remove(item)"
          >
            删除
          </t-button>
        </div>
      </div>
    </template>

    <!-- ATT-01：统一附件预览（本单据的全部附件可左右切换；Office/压缩包只给下载） -->
    <attachment-preview
      v-model:visible="previewVisible"
      :items="previewItems"
      :index="previewIndex"
      title="附件预览"
    />
  </div>
</template>

<script setup lang="ts">
/**
 * 单据附件面板。
 *
 * # props
 *
 * - `docType`：单据类型（决定后端校验哪张表是否存在）；
 * - `docId`：单据主键（详情抽屉打开后才有值，空值时不发请求）；
 * - `defaultRole`：上传时默认选中的角色（缺省 `OTHER`）。
 *
 * # 事件
 *
 * - `changed`：附件增删后触发（调用方若要显示「N 个附件」徽标可监听）。
 */
import { computed, ref, watch } from 'vue';
import { DialogPlugin, MessagePlugin } from 'tdesign-vue-next';

import {
  deleteDocAttachment,
  downloadDocAttachment,
  listDocAttachments,
  saveBlob,
  uploadDocAttachment,
  FILE_ROLE_OPTIONS,
  type DocAttachmentItem,
  type DocType,
  type FileRole,
} from '@/api/business/erp/attachment';
import AttachmentPreview from '@/components/AttachmentPreview.vue';
import type { PreviewItem } from '@/components/attachment-preview-pure';
import { ApiError } from '@/api/request';
import { formatDateTime, formatFileSize } from '@/utils/format';

const props = withDefaults(
  defineProps<{
    docType: DocType;
    docId?: string | null;
    defaultRole?: FileRole;
    /** data-testid 前缀（四个页面各不同，便于 e2e 定位） */
    testidPrefix?: string;
  }>(),
  { docId: null, defaultRole: 'OTHER', testidPrefix: 'doc-attach' },
);

const emit = defineEmits<{ (event: 'changed', total: number): void }>();

const loading = ref(false);
const dragOver = ref(false);
const items = ref<DocAttachmentItem[]>([]);
const total = ref(0);
const maxCount = ref(50);
const role = ref<FileRole>(props.defaultRole);
const remark = ref('');
const fileInputRef = ref<HTMLInputElement | null>(null);

const roleOptions = computed(() =>
  FILE_ROLE_OPTIONS.map((option) => ({ label: option.label, value: option.value })),
);

/** 拉列表（单据不存在 / 无权限时后端给 404 / 403，这里只提示不阻断详情页） */
async function load(): Promise<void> {
  if (!props.docId) {
    items.value = [];
    total.value = 0;
    return;
  }
  loading.value = true;
  try {
    const result = await listDocAttachments(props.docType, props.docId);
    items.value = result.list;
    total.value = result.total;
    maxCount.value = result.max_count;
  } catch (error) {
    items.value = [];
    total.value = 0;
    MessagePlugin.warning(error instanceof ApiError ? error.message : '附件加载失败');
  } finally {
    loading.value = false;
  }
}

watch(
  () => [props.docType, props.docId] as const,
  () => {
    role.value = props.defaultRole;
    void load();
  },
  { immediate: true },
);

function pickFile(): void {
  fileInputRef.value?.click();
}

function onFilePicked(event: Event): void {
  const input = event.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = '';
  if (file) {
    void upload(file);
  }
}

function onDrop(event: DragEvent): void {
  dragOver.value = false;
  const file = event.dataTransfer?.files?.[0];
  if (file) {
    void upload(file);
  }
}

/** 上传单个文件（策略校验 / 配额都在后端，前端只做「到上限就别再发」的省流量检查） */
async function upload(file: File): Promise<void> {
  if (!props.docId) {
    return;
  }
  if (total.value >= maxCount.value) {
    MessagePlugin.warning(`单张单据最多 ${maxCount.value} 个附件，请先删除不再需要的`);
    return;
  }
  loading.value = true;
  try {
    const result = await uploadDocAttachment(
      props.docType,
      props.docId,
      file,
      role.value,
      remark.value,
    );
    remark.value = '';
    await load();
    emit('changed', total.value);
    MessagePlugin.success(
      result.duplicated ? `已上传 ${file.name}（内容秒传，复用已有文件记录）` : `已上传 ${file.name}`,
    );
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '上传失败');
  } finally {
    loading.value = false;
  }
}

async function download(item: DocAttachmentItem): Promise<void> {
  try {
    const blob = await downloadDocAttachment(item.id);
    saveBlob(blob, item.file_name);
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '下载失败');
  }
}

/** 预览弹窗是否显示 */
const previewVisible = ref(false);
/** 预览初始定位（点哪个附件就从哪个开始，左右切换走同一张单据内的顺序） */
const previewIndex = ref(0);

/**
 * 预览列表。
 *
 * 注意下载入口是**绑定行 id**（`item.id`），不是 `file_id` —— 同一份字节被多个单据
 * 秒传复用时 `file_id` 相同，只有绑定行才知道「当前用户看的是哪张单据的附件」，
 * 权限判据也在绑定行上。
 */
const previewItems = computed<PreviewItem[]>(() =>
  items.value.map((item) => ({
    key: item.id,
    fileId: item.file_id,
    name: item.file_name,
    size: item.file_size,
    mimeType: item.mime_type,
    uploader: item.uploader_name,
    uploadedAt: item.created_at,
    remark: item.remark,
    tag: item.file_role_label,
    load: () => downloadDocAttachment(item.id),
  })),
);

function openPreview(item: DocAttachmentItem): void {
  const index = items.value.findIndex((row) => row.id === item.id);
  previewIndex.value = index >= 0 ? index : 0;
  previewVisible.value = true;
}

function remove(item: DocAttachmentItem): void {
  const dialog = DialogPlugin.confirm({
    header: '删除附件',
    body: `确认删除「${item.file_name}」？删除后列表不再显示，文件记录标记删除（物理对象延迟回收）。`,
    theme: 'warning',
    confirmBtn: { content: '删除', theme: 'danger' },
    onConfirm: async () => {
      try {
        await deleteDocAttachment(item.id);
        dialog.destroy();
        await load();
        emit('changed', total.value);
        MessagePlugin.success('附件已删除');
      } catch (error) {
        MessagePlugin.error(error instanceof ApiError ? error.message : '删除失败');
      }
    },
  });
}

defineExpose({ reload: load });
</script>

<style scoped>
.doc-attach {
  padding: 4px 0;
}

.doc-attach__head {
  display: flex;
  align-items: baseline;
  gap: 8px;
  margin-bottom: 6px;
}

.doc-attach__title {
  font-weight: 600;
  font-size: 14px;
}

.doc-attach__hint {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.doc-attach__toolbar {
  display: flex;
  gap: 8px;
  margin-bottom: 8px;
}

.doc-attach__role {
  width: 150px;
}

.doc-attach__remark-input {
  flex: 1;
  max-width: 320px;
}

.doc-attach__drop {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 12px 14px;
  border: 1px dashed var(--td-component-border);
  border-radius: 6px;
  color: var(--td-text-color-secondary);
  cursor: pointer;
  transition: border-color 0.2s;
}

.doc-attach__drop--over {
  border-color: var(--td-brand-color);
  color: var(--td-brand-color);
}

.doc-attach__input {
  display: none;
}

.doc-attach__empty {
  padding: 10px 2px;
  color: var(--td-text-color-placeholder);
  font-size: 13px;
}

.doc-attach__list {
  margin-top: 8px;
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.doc-attach__item {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 6px 8px;
  border-radius: 4px;
  background: var(--td-bg-color-container-hover);
  font-size: 13px;
}

.doc-attach__name {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.doc-attach__meta {
  color: var(--td-text-color-placeholder);
  white-space: nowrap;
}

.doc-attach__note {
  max-width: 160px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--td-text-color-secondary);
}
</style>
