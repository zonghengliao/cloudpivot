<!--
  文件管理（storage/file）。

  # 列表部分（P3-11）
  筛选 / 分页 / 批量删除 / 行操作 / 空态交给 `components/ListPage`：本页只声明
  「列 + 筛选项 + 怎么拉一页 + 入参怎么拼 + 批量动作怎么跑」。

  # 批量删除改用 D7 的 `BatchActionBar`
  改造前是页面自己 for 循环 + 「成功 N 失败 M」toast。后端没有文件批量接口，但
  `BatchRunner` 的契约是「给 ids、回 `BatchResult`」——逐条调 `DELETE /storage/{id}`
  再把成功/失败聚合成 `BatchResult` 就能完整复用那套交互：二次确认（正文带影响条数）、
  逐条失败原因清单、跑完自动刷新 + 清勾选。**逐条独立事务**：一条失败不影响其余条。

  # 上传进度面板放哪
  改造前它压在表格上方（表格卡内）。迁到 ListPage 后放进 `#filter-footer`（筛选卡整宽底部）：
  上传是「筛选区之上」的页面级动作，进度条跟着筛选卡走，不挤占表格可读区。

  # 上传人译名（缺陷②）
  `uploaded_by` 是 UUID，列表原来直接渲染主键。AUD-02 起后端直接给 `uploader_name`
  （一次批量 join），前端只在「后端没给（老后端 / 已删账号）」时才去用户表补译名，
  失败不报错（拿不到就保持 UUID 回退，不该为一句译名弹红条打断上传主流程）。
-->
<template>
  <div class="storage-file-page">
    <ListPage
      ref="listRef"
      title="文件"
      testid="storage-file"
      :columns="columns"
      :filters="filters"
      :filter-state="filter"
      :fetcher="fetcher"
      :query-builder="queryBuilder"
      :page-size="PAGE_SIZE"
      :actions="actions"
      :batch-runner="batchRunner"
      @loaded="onLoaded"
    >
      <template #toolbar>
        <input ref="fileInputRef" class="file-input" type="file" multiple @change="onFilesPicked" />
        <t-button v-privilege="[PERM.storageUpload]" theme="primary" data-testid="storage-file-upload" @click="pickFiles">
          <template #icon><t-icon name="cloud-upload" /></template>
          上传文件
        </t-button>
      </template>

      <!-- 上传进度：直传与中转共用同一份进度条（真实 XHR onprogress） -->
      <template #filter-footer>
        <div v-if="uploadTasks.length" class="upload-panel" data-testid="storage-upload-panel">
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
      </template>

      <template #original_name="{ row }">
        <t-link theme="primary" class="cell-file" data-testid="storage-file-preview" @click="openPreview(row)">
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
      <!-- 上传人：后端姓名 → 前端译名 → 「—」（不再回退渲染 UUID） -->
      <template #uploaded_by="{ row }">
        <span class="uploader" :title="uploaderLabelOf(row, uploaderNames)" :data-testid="`file-uploader-${row.id}`">
          {{ uploaderLabelOf(row, uploaderNames) }}
        </span>
      </template>
      <template #created_at="{ row }">{{ formatDateTime(row.created_at) }}</template>

      <template #empty="{ filtered }">
        <div class="panel-empty" data-testid="storage-file-empty">
          <t-icon class="panel-empty__icon" name="folder" />
          <p class="panel-empty__title">{{ filtered ? '没有匹配的文件' : '还没有文件' }}</p>
          <p class="panel-empty__desc">
            {{
              filtered
                ? '换个文件名 / 分类 / 存储后端试试，或点「重置」看全部'
                : '上传后会出现在这里，图片与 PDF 支持在线预览'
            }}
          </p>
          <t-button v-privilege="[PERM.storageUpload]" theme="primary" @click="pickFiles">上传文件</t-button>
        </div>
      </template>
    </ListPage>

    <!-- 预览抽屉：图片内嵌、PDF 用 iframe、其它类型提示下载 -->
    <t-drawer v-model:visible="previewVisible" :header="previewTitle" size="720px" :footer="false">
      <div class="preview">
        <img v-if="previewKind === 'image'" class="preview__image" :src="previewUrl" :alt="previewTitle" />
        <iframe v-else-if="previewKind === 'pdf'" class="preview__pdf" :src="previewUrl" :title="previewTitle" />
        <div v-else class="preview__fallback">
          <t-icon class="panel-empty__icon" name="file" />
          <p>该类型不支持在线预览，请下载后查看</p>
          <t-button theme="primary" @click="handleDownload(previewRow!)">下载</t-button>
        </div>
      </div>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
import { onBeforeUnmount, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin } from 'tdesign-vue-next';
import ListPage from '@/components/ListPage/index.vue';
import type {
  ColumnDef,
  FilterDef,
  ListActions,
  ListFetcher,
  ListFilter,
  ListQueryBuilder,
} from '@/components/ListPage/types';
import type { BatchResult, BatchRunner } from '@/utils/batch-action';
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
import { pageUsers } from '@/api/system/user-api';
import { UPLOADER_PAGE_SIZE, mergeUserNames, pendingUploaderIds, uploaderLabelOf } from './uploader-names';

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

/** 每页条数（与改造前一致） */
const PAGE_SIZE = 10;

// ==================== 列表（P3-11） ====================

const filter = reactive<ListFilter>({ original_name: '', category: '', storage_code: '' });

/** 三个筛选都是文本模糊（分类与后端编码也一样），宽度按原筛选行比例给 */
const filters: FilterDef[] = [
  { key: 'original_name', type: 'keyword', placeholder: '文件名', width: 240, testid: 'storage-file-name' },
  { key: 'category', type: 'keyword', placeholder: '分类（如 attachment）', width: 200, testid: 'storage-file-category' },
  { key: 'storage_code', type: 'keyword', placeholder: '存储后端编码', width: 200, testid: 'storage-file-code' },
];

/**
 * 列。勾选列由 ListPage 按 `actions.batch` 自动补在最左（页面不再声明 `row-select`）；
 * 操作列保留（列宽 140），两个行操作走 `actions.row` 声明式渲染。
 */
const columns: ColumnDef[] = [
  { colKey: 'original_name', title: '文件名', width: 260, ellipsis: true },
  { colKey: 'category', title: '分类', width: 110 },
  { colKey: 'file_size', title: '大小', width: 110 },
  { colKey: 'upload_mode', title: '上传方式', width: 100 },
  { colKey: 'storage_code', title: '存储后端', width: 130 },
  { colKey: 'uploaded_by', title: '上传人', width: 130 },
  { colKey: 'created_at', title: '上传时间', width: 170 },
  { colKey: 'op', title: '操作', width: 140, fixed: 'right', exportable: false },
];

const fetcher: ListFetcher<StorageFileRes> = (query) => pageStorageFiles(query as Parameters<typeof pageStorageFiles>[0]);

/** 入参拼装：空串整键缺席 */
const queryBuilder: ListQueryBuilder = (_state, page, size) => {
  const text = (value: unknown): string | undefined => {
    const trimmed = String(value ?? '').trim();
    return trimmed || undefined;
  };
  return {
    page,
    page_size: size,
    original_name: text(filter.original_name),
    category: text(filter.category),
    storage_code: text(filter.storage_code),
  };
};

const listRef = ref<InstanceType<typeof ListPage> | null>(null);

/** 一页拉完：补上传人译名（不阻塞渲染） */
function onLoaded(payload: { rows: StorageFileRes[] }): void {
  void resolveUploaderNames(payload.rows);
}

/** 重拉当前页（上传完成 / 单行删除后调） */
function reloadList(): void {
  listRef.value?.reload();
}

// ==================== 行操作 + 批量（声明式） ====================

const actions: ListActions = {
  batch: [
    {
      key: 'delete',
      label: '批量删除',
      theme: 'danger',
      // 高危：二次确认弹窗正文带「影响 N 条」
      needConfirm: true,
    },
  ],
  row: [
    {
      key: 'download',
      label: '下载',
      permission: PERM.storageDownload,
      testid: 'storage-file-download',
      handler: (row: StorageFileRes) => handleDownload(row),
    },
    {
      key: 'delete',
      label: '删除',
      theme: 'danger',
      permission: PERM.storageDelete,
      testid: 'storage-file-delete',
      handler: (row: StorageFileRes) => confirmRemove(row),
    },
  ],
};

/**
 * 批量删除：后端没有文件批量接口 → 逐条删、逐条记结果。
 *
 * `BatchActionBar` 只认 `BatchResult`，所以这里自己聚合（成功/失败都带 id，
 * 失败再带后端报文），「哪一条为什么没删掉」在结果弹窗里逐条列出来。
 */
const batchRunner: BatchRunner = async (_action, ids): Promise<BatchResult> => {
  const succeeded: Array<{ id: string; no?: string | null }> = [];
  const failed: Array<{ id: string; no?: string | null; code: number; error: string }> = [];
  for (const id of ids) {
    try {
      await deleteStorageFile(id);
      succeeded.push({ id });
    } catch (error) {
      failed.push({
        id,
        code: error instanceof ApiError ? error.code : 0,
        error: error instanceof ApiError ? error.message : '删除失败',
      });
    }
  }
  return { succeeded, failed, total: ids.length, success_count: succeeded.length, fail_count: failed.length };
};

// ==================== 上传人译名（缺陷②） ====================

/**
 * `sys_file.uploaded_by` 是 UUID，列表原来直接渲染账号主键。
 *
 * 译名数据走 `GET /api/system/user/page`（与资产页 / 流程主数据译名同一条路子）。
 * **失败不报错**：这里的失败通常就是「当前账号没有 `system:user:list`」，此时列里继续显示
 * UUID（与改之前一致），不该为了一句译名弹红条打断文件管理的主流程。
 */
const uploaderNames = ref<ReadonlyMap<string, string>>(new Map());

async function resolveUploaderNames(list: StorageFileRes[]): Promise<void> {
  // AUD-02：后端已给 `uploader_name` 的行不必再拉用户表（老后端 / 已删账号才回落）
  const missing = list.filter((row) => !(row.uploader_name ?? '').trim());
  if (pendingUploaderIds(missing, uploaderNames.value).length === 0) {
    return;
  }
  try {
    const page = await pageUsers({ page: 1, page_size: UPLOADER_PAGE_SIZE });
    uploaderNames.value = mergeUserNames(uploaderNames.value, page.list ?? []);
  } catch {
    // 无权限 / 接口异常：保留 UUID 回退（见函数注释，不打扰用户）
  }
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

const uploadTasks = ref<UploadTask[]>([]);
const fileInputRef = ref<HTMLInputElement>();

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
  // 分类跟着当前筛选走（改造前就是这样：在「分类=attachment」下上传即归该类）
  const category = String(filter.category ?? '').trim() || 'attachment';
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
  reloadList();
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
          reloadList();
        } catch (error) {
          showError(error);
        }
      })();
    },
  });
}

function showError(error: unknown): void {
  void MessagePlugin.error(error instanceof ApiError ? error.message : '请求失败，请稍后重试');
}

onBeforeUnmount(() => {
  if (previewUrl.value) {
    URL.revokeObjectURL(previewUrl.value);
  }
});
</script>

<style lang="less" scoped>
.storage-file-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.file-input {
  display: none;
}

.upload-panel {
  margin-top: 12px;
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

/* 上传人：姓名可能较长（回退 UUID 时更长），一行截断 + title 看全 */
.uploader {
  display: inline-block;
  max-width: 100%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  vertical-align: bottom;
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

.panel-empty {
  padding: 40px 0;
  text-align: center;

  &__icon {
    font-size: 40px;
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
</style>
