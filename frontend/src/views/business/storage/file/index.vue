<template>
  <div class="storage-file-page">
    <t-card :bordered="false" class="filter-card">
      <t-row :gutter="16">
        <t-col :span="3">
          <t-input v-model="query.original_name" placeholder="文件名" clearable @enter="handleSearch" />
        </t-col>
        <t-col :span="3">
          <t-input v-model="query.category" placeholder="分类（如 attachment）" clearable @enter="handleSearch" />
        </t-col>
        <t-col :span="3">
          <t-input v-model="query.storage_code" placeholder="存储后端编码" clearable @enter="handleSearch" />
        </t-col>
        <t-col :span="3">
          <t-space>
            <t-button theme="primary" @click="handleSearch">
              <template #icon><t-icon name="search" /></template>
              查询
            </t-button>
            <t-button theme="default" @click="handleReset">重置</t-button>
          </t-space>
        </t-col>
      </t-row>
    </t-card>

    <t-card :bordered="false" class="table-card">
      <div class="toolbar">
        <t-space>
          <input ref="fileInputRef" class="file-input" type="file" multiple @change="onFilesPicked" />
          <t-button v-privilege="[PERM.storageUpload]" theme="primary" @click="pickFiles">
            <template #icon><t-icon name="cloud-upload" /></template>
            上传文件
          </t-button>
          <t-button
            v-privilege="[PERM.storageDelete]"
            theme="danger"
            variant="outline"
            :disabled="selectedIds.length === 0"
            @click="confirmBatchRemove"
          >
            <template #icon><t-icon name="delete" /></template>
            批量删除
          </t-button>
          <t-button theme="default" :loading="loading" @click="loadList">
            <template #icon><t-icon name="refresh" /></template>
            刷新
          </t-button>
        </t-space>
        <span class="toolbar__count">
          <template v-if="selectedIds.length">已选 {{ selectedIds.length }} 项 / </template>共 {{ pagination.total }} 条
        </span>
      </div>

      <!-- 上传进度：直传与中转共用同一份进度条（真实 XHR onprogress） -->
      <div v-if="uploadTasks.length" class="upload-panel">
        <div v-for="task in uploadTasks" :key="task.key" class="upload-panel__item">
          <t-icon class="upload-panel__icon" :name="taskIcon(task)" />
          <span class="upload-panel__name" :title="task.name">{{ task.name }}</span>
          <t-progress
            class="upload-panel__progress"
            :percentage="task.percent"
            :status="task.state === 'error' ? 'error' : task.state === 'done' ? 'success' : 'active'"
            :label="false"
            size="small"
          />
          <span class="upload-panel__meta">{{ taskMessage(task) }}</span>
        </div>
        <t-link class="upload-panel__clear" theme="default" @click="clearFinished">清除已完成</t-link>
      </div>

      <t-table
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        :stripe="true"
        @page-change="handlePageChange"
        @select-change="onSelectChange"
      >
        <template #original_name="{ row }">
          <t-link theme="primary" class="cell-file" @click="openPreview(row)">
            <t-icon class="cell-file__icon" :name="fileIcon(row)" />
            {{ row.original_name || row.file_name }}
          </t-link>
        </template>
        <template #file_size="{ row }">{{ formatFileSize(row.file_size) }}</template>
        <template #upload_mode="{ row }">
          <t-tag size="small" variant="light" :theme="row.upload_mode === 'direct' ? 'primary' : 'default'">
            {{ row.upload_mode === 'direct' ? '直传' : '中转' }}
          </t-tag>
        </template>
        <template #storage_code="{ row }">
          <span>{{ row.storage_code || '默认' }}</span>
        </template>
        <template #created_at="{ row }">{{ formatDateTime(row.created_at) }}</template>
        <template #op="{ row }">
          <t-space size="small">
            <t-link v-privilege="[PERM.storageDownload]" theme="primary" @click="handleDownload(row)">下载</t-link>
            <t-link v-privilege="[PERM.storageDelete]" theme="danger" @click="confirmRemove(row)">删除</t-link>
          </t-space>
        </template>
        <template #empty>
          <div class="empty-state">
            <t-icon class="empty-state__icon" name="folder" />
            <p class="empty-state__title">还没有文件</p>
            <p class="empty-state__desc">上传后会出现在这里，图片与 PDF 支持在线预览</p>
            <t-button v-privilege="[PERM.storageUpload]" theme="primary" @click="pickFiles">上传文件</t-button>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- 预览抽屉：图片内嵌、PDF 用 iframe、其它类型提示下载 -->
    <t-drawer v-model:visible="previewVisible" :header="previewTitle" size="720px" :footer="false">
      <div class="preview">
        <img v-if="previewKind === 'image'" class="preview__image" :src="previewUrl" :alt="previewTitle" />
        <iframe v-else-if="previewKind === 'pdf'" class="preview__pdf" :src="previewUrl" :title="previewTitle" />
        <div v-else class="preview__fallback">
          <t-icon class="empty-state__icon" name="file" />
          <p>该类型不支持在线预览，请下载后查看</p>
          <t-button theme="primary" @click="handleDownload(previewRow!)">下载</t-button>
        </div>
      </div>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
import { onBeforeUnmount, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import {
  completeStorageUpload,
  deleteStorageFile,
  downloadStorageFile,
  pageStorageFiles,
  presignStorageUpload,
  putFileWithProgress,
  uploadStorageFile,
  type StorageFileRes,
} from '@/api/business/storage';
import { formatDateTime, formatFileSize } from '@/utils/format';
import { fallbackFileName, saveBlob } from '@/utils/download';
import { PERM } from '@/constants/permissions';

/** 表格分页事件载荷 */
interface TablePageInfo {
  /** 当前页码 */
  current: number;
  /** 每页条数 */
  pageSize: number;
}

/** 单个上传任务的界面状态 */
interface UploadTask {
  /** 唯一键（file.name + 时间戳） */
  key: string;
  /** 文件名 */
  name: string;
  /** 0~100 */
  percent: number;
  /** 状态 */
  state: 'pending' | 'uploading' | 'done' | 'error';
  /** 状态说明 */
  message: string;
}

const rows = ref<StorageFileRes[]>([]);
const loading = ref(false);
const selectedIds = ref<string[]>([]);
const query = reactive({ original_name: '', category: '', storage_code: '' });
const pagination = reactive({ current: 1, pageSize: 10, total: 0 });
const uploadTasks = ref<UploadTask[]>([]);
const fileInputRef = ref<HTMLInputElement>();

const columns: PrimaryTableCol[] = [
  { colKey: 'row-select', type: 'multiple', width: 50 },
  { colKey: 'original_name', title: '文件名', width: 260, ellipsis: true },
  { colKey: 'category', title: '分类', width: 110 },
  { colKey: 'file_size', title: '大小', width: 110 },
  { colKey: 'upload_mode', title: '上传方式', width: 100 },
  { colKey: 'storage_code', title: '存储后端', width: 130 },
  { colKey: 'uploaded_by', title: '上传人', width: 130 },
  { colKey: 'created_at', title: '上传时间', width: 170 },
  { colKey: 'op', title: '操作', width: 140, fixed: 'right' },
];

async function loadList(): Promise<void> {
  loading.value = true;
  try {
    const data = await pageStorageFiles({
      page: pagination.current,
      page_size: pagination.pageSize,
      original_name: query.original_name || undefined,
      category: query.category || undefined,
      storage_code: query.storage_code || undefined,
    });
    rows.value = data.list;
    pagination.total = data.total;
  } catch (error) {
    showError(error);
  } finally {
    loading.value = false;
  }
}

function handleSearch(): void {
  pagination.current = 1;
  void loadList();
}

function handleReset(): void {
  query.original_name = '';
  query.category = '';
  query.storage_code = '';
  handleSearch();
}

function handlePageChange(pageInfo: TablePageInfo): void {
  pagination.current = pageInfo.current;
  pagination.pageSize = pageInfo.pageSize;
  void loadList();
}

function onSelectChange(keys: Array<string | number>): void {
  selectedIds.value = keys.map((key) => String(key));
}

/** 文件图标：按扩展名粗分，避免所有文件都是同一个图标 */
function fileIcon(row: StorageFileRes): string {
  const ext = (row.extension || '').toLowerCase();
  if (['png', 'jpg', 'jpeg', 'gif', 'bmp', 'webp', 'svg'].includes(ext)) {
    return 'image';
  }
  if (ext === 'pdf') {
    return 'file-pdf';
  }
  if (['xls', 'xlsx', 'csv'].includes(ext)) {
    return 'file-excel';
  }
  if (['doc', 'docx'].includes(ext)) {
    return 'file-word';
  }
  return 'file';
}

// ==================== 预览 ====================
const previewVisible = ref(false);
const previewRow = ref<StorageFileRes | null>(null);
const previewKind = ref<'image' | 'pdf' | 'other'>('other');
const previewUrl = ref('');
const previewTitle = ref('');

function previewType(row: StorageFileRes): 'image' | 'pdf' | 'other' {
  const ext = (row.extension || '').toLowerCase();
  if (['png', 'jpg', 'jpeg', 'gif', 'bmp', 'webp', 'svg'].includes(ext)) {
    return 'image';
  }
  if (ext === 'pdf') {
    return 'pdf';
  }
  return 'other';
}

async function openPreview(row: StorageFileRes): Promise<void> {
  previewRow.value = row;
  previewTitle.value = row.original_name || row.file_name;
  previewKind.value = previewType(row);
  if (previewKind.value === 'other') {
    previewVisible.value = true;
    return;
  }
  try {
    // 预览也要带鉴权头，所以取 blob 后用 object URL 渲染
    const blob = await downloadStorageFile(row.id);
    if (previewUrl.value) {
      URL.revokeObjectURL(previewUrl.value);
    }
    previewUrl.value = URL.createObjectURL(blob);
    previewVisible.value = true;
  } catch (error) {
    showError(error);
  }
}

// ==================== 上传（服务端中转 / 预签名直传） ====================
/** 超过该大小优先走直传（策略允许时），否则走后端中转 */
const DIRECT_THRESHOLD = 10 * 1024 * 1024;
/**
 * 计算文件哈希的上限：超过 32 MB 不再整文件算 sha256。
 *
 * 秒传是「省流量」的优化，不该让上传等待：整文件哈希在大文件上要读完整个文件才发第一个字节，
 * 用户看到的是进度条长时间停在 0%。超过上限时放弃秒传，直接进入直传。
 */
const HASH_LIMIT = 32 * 1024 * 1024;

function pickFiles(): void {
  fileInputRef.value?.click();
}

function onFilesPicked(event: Event): void {
  const target = event.target as HTMLInputElement;
  const files = Array.from(target.files ?? []);
  target.value = '';
  if (files.length === 0) {
    return;
  }
  void uploadSequentially(files);
}

/** 计算 sha256（秒传判断依据）；失败时返回空串走普通上传 */
async function sha256(file: File): Promise<string> {
  try {
    const buffer = await file.arrayBuffer();
    const digest = await crypto.subtle.digest('SHA-256', buffer);
    return Array.from(new Uint8Array(digest))
      .map((byte) => byte.toString(16).padStart(2, '0'))
      .join('');
  } catch {
    return '';
  }
}

async function uploadSequentially(files: File[]): Promise<void> {
  const category = query.category || 'attachment';
  for (const file of files) {
    uploadTasks.value.push({
      key: `${file.name}-${Date.now()}-${Math.random().toString(16).slice(2, 8)}`,
      name: file.name,
      percent: 0,
      state: 'pending',
      message: '排队中',
    });
    // 必须拿回数组里的**响应式代理**再改字段：直接改 push 前的原始对象不会触发视图更新，
    // 表现就是进度条永远停在「0% · 排队中」（本卡实测踩到过）。
    const tracked = uploadTasks.value[uploadTasks.value.length - 1];
    await uploadOne(file, category, tracked);
  }
  await loadList();
}

async function uploadOne(file: File, category: string, task: UploadTask): Promise<void> {
  task.state = 'uploading';
  task.message = '准备上传';
  try {
    // 大文件跳过整文件哈希（见 HASH_LIMIT 说明），避免进度条卡在 0%
    const hash = file.size <= HASH_LIMIT ? await sha256(file) : '';
    if (file.size >= DIRECT_THRESHOLD) {
      try {
        const presign = await presignStorageUpload({
          category,
          file_name: file.name,
          file_size: file.size,
          file_hash: hash || undefined,
          mime_type: file.type || undefined,
        });
        if (presign.duplicated) {
          task.percent = 100;
          task.state = 'done';
          task.message = '秒传完成（服务器已有相同文件）';
          return;
        }
        task.message = '直传中';
        await putFileWithProgress(presign.upload_url, presign.headers, file, (percent) => {
          task.percent = percent;
        });
        task.message = '登记中';
        await completeStorageUpload({
          category,
          file_key: presign.file_key,
          file_name: file.name,
          file_size: file.size,
          file_hash: hash || undefined,
          mime_type: file.type || undefined,
        });
        task.percent = 100;
        task.state = 'done';
        task.message = '直传完成';
        return;
      } catch (directError) {
        // 直传不可用（本地后端 / 策略关闭 / 跨域失败）时回落后端中转，不打扰用户
        task.message = '直传不可用，改为服务端上传';
        task.percent = 0;
        // 记录原始原因便于排查
        console.warn('[storage] direct upload fallback:', directError);
      }
    }
    await uploadStorageFile(file, category);
    task.percent = 100;
    task.state = 'done';
    task.message = '上传完成';
  } catch (error) {
    task.state = 'error';
    task.message = error instanceof ApiError ? error.message : '上传失败';
  }
}

function taskIcon(task: UploadTask): string {
  if (task.state === 'done') {
    return 'check-circle-filled';
  }
  if (task.state === 'error') {
    return 'error-circle-filled';
  }
  return 'cloud-upload';
}

function taskMessage(task: UploadTask): string {
  if (task.state === 'uploading') {
    return `${task.percent}% · ${task.message}`;
  }
  return task.message;
}

function clearFinished(): void {
  uploadTasks.value = uploadTasks.value.filter((task) => task.state === 'uploading' || task.state === 'pending');
}

// ==================== 下载 / 删除 ====================
async function handleDownload(row: StorageFileRes): Promise<void> {
  try {
    const blob = await downloadStorageFile(row.id);
    saveBlob(blob, fallbackFileName(row.original_name, row.file_name || 'download'));
  } catch (error) {
    showError(error);
  }
}

function confirmRemove(row: StorageFileRes): void {
  const dialog = DialogPlugin.confirm({
    header: '删除文件',
    body: `确定删除「${row.original_name || row.file_name}」？删除后不可恢复。`,
    theme: 'danger',
    confirmBtn: { content: '删除', theme: 'danger' },
    onConfirm: () => {
      dialog.destroy();
      void (async () => {
        try {
          await deleteStorageFile(row.id);
          void MessagePlugin.success('已删除');
          await loadList();
        } catch (error) {
          showError(error);
        }
      })();
    },
  });
}

function confirmBatchRemove(): void {
  const count = selectedIds.value.length;
  const dialog = DialogPlugin.confirm({
    header: '批量删除',
    body: `确定删除选中的 ${count} 个文件？删除后不可恢复。`,
    theme: 'danger',
    confirmBtn: { content: '删除', theme: 'danger' },
    onConfirm: () => {
      dialog.destroy();
      void (async () => {
        let success = 0;
        let failed = 0;
        for (const id of selectedIds.value) {
          try {
            await deleteStorageFile(id);
            success += 1;
          } catch {
            failed += 1;
          }
        }
        if (failed === 0) {
          void MessagePlugin.success(`成功 ${success}，失败 0`);
        } else {
          void MessagePlugin.warning(`成功 ${success}，失败 ${failed}`);
        }
        selectedIds.value = [];
        await loadList();
      })();
    },
  });
}

function showError(error: unknown): void {
  void MessagePlugin.error(error instanceof ApiError ? error.message : '请求失败，请稍后重试');
}

onMounted(() => {
  void loadList();
});

onBeforeUnmount(() => {
  if (previewUrl.value) {
    URL.revokeObjectURL(previewUrl.value);
  }
});
</script>

<style lang="less" scoped>
.storage-file-page {
  padding: 16px;

  .filter-card {
    margin-bottom: 12px;
  }

  .toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 12px;

    &__count {
      font-size: 13px;
      color: var(--td-text-color-secondary);
    }
  }

  .file-input {
    display: none;
  }

  .upload-panel {
    margin-bottom: 12px;
    padding: 12px 16px;
    border: 1px solid var(--td-component-stroke);
    border-radius: 6px;
    background: var(--td-bg-color-container);

    &__item {
      display: flex;
      align-items: center;
      gap: 12px;
      padding: 4px 0;
    }

    &__icon {
      font-size: 16px;
      color: var(--td-brand-color-7);
    }

    &__name {
      width: 220px;
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
      font-size: 13px;
    }

    &__progress {
      flex: 1;
    }

    &__meta {
      width: 220px;
      font-size: 12px;
      color: var(--td-text-color-secondary);
    }

    &__clear {
      margin-top: 4px;
      font-size: 12px;
    }
  }

  .cell-file {
    display: inline-flex;
    align-items: center;
    gap: 6px;

    &__icon {
      color: var(--td-text-color-secondary);
    }
  }

  .preview {
    height: 100%;

    &__image {
      display: block;
      max-width: 100%;
      margin: 0 auto;
    }

    &__pdf {
      width: 100%;
      height: calc(100vh - 160px);
      border: none;
    }

    &__fallback {
      padding: 64px 0;
      text-align: center;
      color: var(--td-text-color-secondary);
    }
  }

  .empty-state {
    padding: 48px 0;
    text-align: center;

    &__icon {
      font-size: 48px;
      color: var(--td-text-color-placeholder);
    }

    &__title {
      margin: 12px 0 4px;
      font-size: 15px;
      color: var(--td-text-color-primary);
    }

    &__desc {
      margin-bottom: 16px;
      font-size: 13px;
      color: var(--td-text-color-secondary);
    }
  }
}
</style>
