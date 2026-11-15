<!--
  文件管理（support/file）：上传 + 列表 + 预览（P3-13）。

  注意与 `views/business/storage/file` 不是同一页：这一页是**平台自带文件服务**的文件库
  （`/api/support/file/*`），那一页是业务域的存储桶文件。两页各自的权限码也不同。

  # 预览要用「当前页的行」
  预览面板支持左右切换，切换范围就是列表当前页 —— 所以页面仍要镜像一份行（`@loaded`），
  这不是把 ListPage 的状态抄回来，是预览组件需要 `items` 数组。
-->
<template>
  <div class="file-page">
    <ListPage
      ref="listRef"
      title="文件"
      testid="file"
      table-testid="file-table"
      meta-testid="file-meta"
      btn-prefix="btn-file"
      :page-size="FILE_PAGE_SIZE"
      :columns="columns"
      :filters="filters"
      :filter-state="filter"
      :fetcher="fetcher"
      :query-builder="queryBuilder"
      @loaded="onLoaded"
    >
      <template #toolbar>
        <t-input
          v-model="uploadCategory"
          v-privilege="[PERM.upload]"
          class="file-page__category"
          placeholder="上传分类（可选）"
          clearable
          data-testid="file-upload-category"
        />
        <file-uploader
          v-privilege="[PERM.upload]"
          :category="uploadCategory"
          @uploaded="onUploaded"
          @error="showError"
        />
      </template>

      <template #category="{ row }">{{ categoryLabel(row.category) }}</template>
      <template #uploader_name="{ row }">
        <span data-testid="file-uploader-name">{{ uploaderLabel(row) }}</span>
      </template>
      <template #file_size="{ row }">{{ formatFileSize(row.file_size) }}</template>
      <template #created_at="{ row }">{{ formatDateTime(row.created_at) }}</template>
      <template #op="{ row }">
        <t-space size="small">
          <t-link theme="primary" data-testid="file-row-preview" @click.stop="openPreview(row)">
            预览
          </t-link>
          <t-link theme="primary" data-testid="file-row-download" @click.stop="handleDownload(row)">
            下载
          </t-link>
          <t-link
            v-privilege="[PERM.delete]"
            theme="danger"
            data-testid="file-row-delete"
            @click.stop="confirmRemove(row)"
          >
            删除
          </t-link>
        </t-space>
      </template>
    </ListPage>

    <!-- ATT-01：统一附件预览（同页文件一起进列表，支持左右切换） -->
    <attachment-preview
      v-model:visible="previewVisible"
      :items="previewItems"
      :index="previewIndex"
      title="文件预览"
    />
  </div>
</template>

<script setup lang="ts">
import { computed, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin } from 'tdesign-vue-next';
import FileUploader from '@/components/FileUploader.vue';
import AttachmentPreview from '@/components/AttachmentPreview.vue';
import type { PreviewItem } from '@/components/attachment-preview-pure';
import { deleteFile, downloadFileBlob, pageFiles, type FileRes } from '@/api/support/file-api';
import { ApiError } from '@/api/request';
import { formatDateTime, formatFileSize } from '@/utils/format';
import ListPage from '@/components/ListPage/index.vue';
import type {
  ColumnDef,
  FilterDef,
  ListFetcher,
  ListQuery,
  ListQueryBuilder,
} from '@/components/ListPage/types';
import { uploaderLabel } from './uploader-label';

/**
 * 本页权限码（与 `sys_menu.auth_code` 一致）。
 *
 * 就近放在页面里（不改 `src/constants/permissions.ts`）：那个文件是全站单点，
 * 多张卡会同时往它里面加码，谁后提交谁撞冲突。
 */
const PERM = {
  upload: 'support:file:upload',
  delete: 'support:file:delete',
} as const;

/** 每页条数（改造前是 10，保持手感一致） */
const FILE_PAGE_SIZE = 10;

/** 查询条件（`''` = 不过滤） */
interface FileFilterState {
  /** 原始文件名 */
  original_name: string;
  /** 文件分类 */
  category: string;
  /** MIME 类型 */
  mime_type: string;
}

const filter = reactive<FileFilterState>({ original_name: '', category: '', mime_type: '' });
const listRef = ref<InstanceType<typeof ListPage> | null>(null);
/** 上传时携带的分类 */
const uploadCategory = ref('');
/** 当前页的行（预览面板的 items 来源） */
const currentRows = ref<FileRes[]>([]);

const columns: ColumnDef[] = [
  { colKey: 'original_name', title: '文件名称', ellipsis: true },
  { colKey: 'category', title: '分类', width: 120 },
  { colKey: 'mime_type', title: '文件类型', width: 180, ellipsis: true },
  { colKey: 'file_size', title: '大小', width: 110 },
  { colKey: 'uploader_name', title: '上传人', width: 140, cell: 'uploader_name' },
  { colKey: 'created_at', title: '上传时间', width: 180 },
  { colKey: 'op', title: '操作', width: 170, fixed: 'right', exportable: false },
];

/** 筛选行：三个关键词（回车 / 点查询生效，与原页一致） */
const filters: FilterDef[] = [
  { key: 'original_name', type: 'keyword', placeholder: '文件名称', width: 220, testid: 'file-filter-name' },
  { key: 'category', type: 'keyword', placeholder: '分类', width: 180, testid: 'file-filter-category' },
  { key: 'mime_type', type: 'keyword', placeholder: '文件类型', width: 180, testid: 'file-filter-mime' },
];

/** 拉一页 */
const fetcher: ListFetcher<FileRes> = (query) => pageFiles(query as Parameters<typeof pageFiles>[0]);

/** 入参拼装（键名与改造前逐字一致；空值整键缺席） */
const queryBuilder: ListQueryBuilder = (state, page, pageSize): ListQuery => {
  const { original_name: name, category, mime_type: mime } = state as FileFilterState;
  const keyword = String(name ?? '').trim();
  const categoryText = String(category ?? '').trim();
  const mimeText = String(mime ?? '').trim();
  return {
    page,
    page_size: pageSize,
    ...(keyword ? { original_name: keyword } : {}),
    ...(categoryText ? { category: categoryText } : {}),
    ...(mimeText ? { mime_type: mimeText } : {}),
  };
};

/** 一页拉完：镜像行（预览面板按当前页左右切换） */
function onLoaded(payload: { rows: FileRes[] }): void {
  currentRows.value = payload.rows;
}

/** 预览弹窗是否显示 */
const previewVisible = ref(false);
/** 预览初始定位（点哪一行就从哪一张开始） */
const previewIndex = ref(0);

/**
 * 预览列表：用当前页的行，左右切换即页内翻动。
 *
 * `load` 不传 → 组件默认走 `support/file/download/{id}`（带鉴权头取 blob）。
 * `uploader` 用显示名（ATT-03：不再把 UUID 递给预览面板）。
 */
const previewItems = computed<PreviewItem[]>(() =>
  currentRows.value.map((row) => ({
    key: row.id,
    fileId: row.id,
    name: row.original_name || row.file_name,
    size: row.file_size,
    mimeType: row.mime_type,
    uploader: uploaderLabel(row),
    uploadedAt: row.created_at,
  })),
);

/** 打开预览（越界/找不到时退化成第 0 个，不让组件收到 -1） */
function openPreview(row: FileRes): void {
  const index = currentRows.value.findIndex((item) => item.id === row.id);
  previewIndex.value = index >= 0 ? index : 0;
  previewVisible.value = true;
}

/** 分类中文名映射：库中分类存的是英文短码，展示统一转中文；未知分类回退原值 */
const CATEGORY_LABELS: Record<string, string> = {
  attachment: '附件',
  avatar: '头像',
  document: '文档',
};

/** 分类展示名：有中文映射显示中文，否则原样显示（用户自建分类） */
function categoryLabel(category: string | null | undefined): string {
  if (!category) {
    return '-';
  }
  return CATEGORY_LABELS[category] ?? category;
}

/** 统一展示接口错误 */
function showError(message: string): void {
  void MessagePlugin.error(message);
}

/** 重新拉列表（上传 / 删除后） */
async function reloadList(): Promise<void> {
  await listRef.value?.reload();
}

/** 上传成功：回第 1 页（新文件按时间倒序在最前） */
function onUploaded(file: FileRes): void {
  void MessagePlugin.success(`「${file.original_name}」上传成功`);
  listRef.value?.search();
}

/** 通过 blob 下载，保证请求携带鉴权头 */
async function handleDownload(row: FileRes): Promise<void> {
  try {
    const blob = await downloadFileBlob(row.id);
    const url = URL.createObjectURL(blob);
    const link = document.createElement('a');
    link.href = url;
    link.download = row.original_name || row.file_name || 'download';
    document.body.appendChild(link);
    link.click();
    link.remove();
    URL.revokeObjectURL(url);
  } catch (error) {
    showError(error instanceof ApiError ? error.message : '文件下载失败');
  }
}

function confirmRemove(row: FileRes): void {
  const dialog = DialogPlugin.confirm({
    header: '删除文件',
    body: `确认删除文件「${row.original_name}」？该操作不可恢复。`,
    theme: 'danger',
    confirmBtn: { content: '删除', theme: 'danger' },
    onConfirm: () => {
      dialog.destroy();
      void (async () => {
        try {
          await deleteFile(row.id);
          void MessagePlugin.success('文件已删除');
          await reloadList();
        } catch (error) {
          showError(error instanceof ApiError ? error.message : '删除失败');
        }
      })();
    },
  });
}
</script>

<style scoped lang="less">
.file-page {
  padding: 16px;

  &__category {
    width: 180px;
  }
}
</style>
