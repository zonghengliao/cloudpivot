<template>
  <div class="aiws">
    <div class="aiws__hint">
      <t-icon name="folder-1" />
      <span>
        只读浏览 AI 执行者的 workspace<template v-if="profile">（profile：<code>{{ profile }}</code>）</template>
        —— 这里的文件是 AI 直接写在磁盘上的原始产出，不经 storage；单个不超过 {{ maxBytesLabel }}。
      </span>
      <t-button size="small" variant="text" :loading="loading" @click="load">刷新</t-button>
    </div>

    <t-loading :loading="loading" size="small">
      <div v-if="files.list.length > 0" class="aiws__list">
        <div v-for="file in files.list" :key="file.name" class="aiws__item">
          <t-icon :name="iconOf(file.preview_kind)" class="aiws__icon" />
          <button type="button" class="aiws__name" :title="`预览 ${file.name}`" @click="openPreview(file)">
            {{ file.name }}
          </button>
          <span class="aiws__size">{{ formatSize(file.size) }}</span>
          <span class="aiws__mtime">{{ formatTime(file.mtime) }}</span>
          <t-button size="small" variant="text" theme="primary" @click="download(file)">下载</t-button>
        </div>
      </div>
      <div v-else-if="!loading" class="aiws__empty">
        <t-icon name="file-1" size="20px" />
        <span>{{ emptyText }}</span>
      </div>
    </t-loading>

    <t-drawer
      v-model:visible="preview.visible"
      :header="`预览 · ${preview.name}`"
      size="min(960px, 92vw)"
      :footer="false"
      destroy-on-close
      @close="closePreview"
    >
      <t-loading :loading="preview.loading" size="small">
        <div v-if="preview.error" class="aiws__error">{{ preview.error }}</div>
        <template v-else-if="preview.data">
          <div class="aiws__meta">
            <span>{{ formatSize(preview.data.size) }}</span>
            <span>{{ preview.data.content_type }}</span>
            <span v-if="preview.data.truncated" class="aiws__warn">
              内容已截断（只显示前 {{ preview.data.content_type === 'csv' ? '100 行' : '20 万字符' }}），完整内容请下载
            </span>
            <t-button size="small" theme="primary" @click="downloadByName(preview.data.name)">下载原文件</t-button>
          </div>

          <!-- Markdown：复用批次 C 的 MarkdownViewer（不重写渲染器） -->
          <MarkdownViewer
            v-if="preview.data.content_type === 'markdown'"
            :html="renderMarkdownLite(preview.data.content)"
            :show-toc="false"
            max-height="calc(88vh - 180px)"
          />

          <!-- CSV：简单表格（前 100 行，服务端已截行） -->
          <div v-else-if="preview.data.content_type === 'csv'" class="aiws__table">
            <table>
              <thead>
                <tr>
                  <th v-for="(cell, index) in csv.header" :key="index">{{ cell }}</th>
                </tr>
              </thead>
              <tbody>
                <tr v-for="(row, rowIndex) in csv.rows" :key="rowIndex">
                  <td v-for="(cell, cellIndex) in row" :key="cellIndex">{{ cell }}</td>
                </tr>
              </tbody>
            </table>
          </div>

          <!-- 图片：base64 直接内联 -->
          <div v-else-if="preview.data.content_type === 'image'" class="aiws__image">
            <img :src="imageSrc" :alt="preview.data.name" />
          </div>

          <!-- PDF：转 Blob URL 交给浏览器内嵌阅读器 -->
          <div v-else-if="preview.data.content_type === 'pdf'" class="aiws__pdf">
            <iframe v-if="pdfUrl" :src="pdfUrl" :title="preview.data.name" />
            <p v-else class="aiws__empty-inline">
              浏览器无法内嵌这份 PDF，请直接下载后打开。
            </p>
          </div>

          <!-- 纯文本 / 日志 / JSON -->
          <pre v-else class="aiws__text">{{ preview.data.content }}</pre>
        </template>
      </t-loading>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
/**
 * AI 产出文件（批次 E · E13）。
 *
 * # 这是「AI 写在磁盘上的原始产出」的只读视图
 *
 * 与「附件」tab 的区别：附件是 AI **主动上传**进 storage 的那一份；这里的文件是
 * AI 在 `~/.hermes/workspace/<profile>/` 里直接写下的产物（`report.md` 等），
 * 云枢此前完全看不到。后端**只读**开放三个接口（列表 / 预览 / 下载），本组件
 * 不放任何写操作按钮。
 *
 * # 预览走抽屉而不是新窗口
 *
 * 抽屉保留任务详情上下文，看完关掉就回到原处；下载走 `Blob + <a download>`，
 * 不用裸链接（接口要 Bearer，裸链接触发的是浏览器导航，不带 Authorization）。
 *
 * # 渲染器按类型分流（与后端 `preview_kind` 一一对应）
 *
 * | content_type | 渲染 |
 * |--------------|------|
 * | `markdown` | 复用 `MarkdownViewer`（批次 C 已交付，**不重写**） |
 * | `text` | `<pre>` |
 * | `csv` | 简单表格（服务端已截前 100 行） |
 * | `image` | `<img src="data:...;base64,...">` |
 * | `pdf` | base64 → Blob URL → `<iframe>` |
 */
import { computed, reactive, ref, watch } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';

import MarkdownViewer from '@/components/MarkdownViewer.vue';
import { renderMarkdownLite } from '@/utils/markdown-lite';
import {
  downloadAiWorkspaceFile,
  listAiWorkspaceFiles,
  previewAiWorkspaceFile,
  type AiWorkspaceFileItem,
  type AiWorkspaceFiles,
  type AiWorkspacePreview,
} from '@/api/business/task-api';

const props = defineProps<{
  /** 任务 id（切任务时重新拉取） */
  taskId: string;
}>();

/**
 * `loaded`：把「文件总数」抛给父级（E9 任务详情主 Tab 的「AI 产出 N」徽标要用）。
 *
 * 徽标必须和列表同源（都来自 `GET /ai-workspace-files` 的 `total`），
 * 否则父级为了一个数字得再拉一次同一个接口，还容易和列表对不上。
 */
const emit = defineEmits<{
  (event: 'loaded', total: number): void;
}>();

/** 列表状态。 */
const loading = ref(false);
const files = reactive<AiWorkspaceFiles>({
  list: [],
  total: 0,
  profile: '',
  max_bytes: 10 * 1024 * 1024,
  workspace_exists: true,
});
const profile = computed(() => files.profile);
const maxBytesLabel = computed(() => formatSize(files.max_bytes));

/** 空态文案（区分「目录没了」与「AI 还没产出」）。 */
const emptyText = computed(() =>
  files.workspace_exists === false
    ? '该 AI 的 workspace 目录不存在（可能还没跑过任何任务）'
    : '该 AI 未在 workspace 留下文件',
);

/** 预览状态。 */
const preview = reactive<{
  visible: boolean;
  loading: boolean;
  name: string;
  error: string;
  data: AiWorkspacePreview | null;
}>({ visible: false, loading: false, name: '', error: '', data: null });

/** PDF 的 Blob URL（关闭抽屉时回收，避免内存泄漏）。 */
const pdfUrl = ref('');

/** 图片的 data URL。 */
const imageSrc = computed(() => {
  const data = preview.data;
  if (!data || data.content_type !== 'image') {
    return '';
  }
  return `data:${data.mime_type};base64,${data.content}`;
});

/** CSV 解析结果（表头 + 数据行）。 */
const csv = computed<{ header: string[]; rows: string[][] }>(() => {
  const raw = preview.data?.content ?? '';
  const lines = raw.split(/\r?\n/).filter((line) => line.trim().length > 0);
  if (lines.length === 0) {
    return { header: [], rows: [] };
  }
  const split = (line: string): string[] => line.split(',').map((cell) => cell.trim());
  return { header: split(lines[0]), rows: lines.slice(1).map(split) };
});

/** 拉取列表。 */
async function load(): Promise<void> {
  if (!props.taskId) {
    return;
  }
  loading.value = true;
  try {
    const result = await listAiWorkspaceFiles(props.taskId);
    files.list = result.list;
    files.total = result.total;
    files.profile = result.profile;
    files.max_bytes = result.max_bytes;
    files.workspace_exists = result.workspace_exists;
    // 徽标计数：成功拉到列表才回抛（失败时不改父级，避免把数量刷成 0 骗人）
    emit('loaded', result.total);
  } catch (error) {
    files.list = [];
    files.total = 0;
    MessagePlugin.error(readError(error, '读取 AI 产出文件失败'));
  } finally {
    loading.value = false;
  }
}

/** 打开预览抽屉。 */
async function openPreview(file: AiWorkspaceFileItem): Promise<void> {
  preview.visible = true;
  preview.loading = true;
  preview.error = '';
  preview.data = null;
  preview.name = file.name;
  revokePdf();
  try {
    const data = await previewAiWorkspaceFile(props.taskId, file.name);
    preview.data = data;
    if (data.content_type === 'pdf' && data.encoding === 'base64') {
      pdfUrl.value = base64ToBlobUrl(data.content, 'application/pdf');
    }
  } catch (error) {
    preview.error = readError(error, '预览失败');
  } finally {
    preview.loading = false;
  }
}

/** 关闭预览（回收 Blob URL）。 */
function closePreview(): void {
  revokePdf();
  preview.data = null;
  preview.error = '';
}

function revokePdf(): void {
  if (pdfUrl.value) {
    URL.revokeObjectURL(pdfUrl.value);
    pdfUrl.value = '';
  }
}

/** 下载（走 Blob，不裸链）。 */
async function download(file: AiWorkspaceFileItem): Promise<void> {
  await downloadByName(file.name);
}

async function downloadByName(name: string): Promise<void> {
  try {
    const blob = await downloadAiWorkspaceFile(props.taskId, name);
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement('a');
    anchor.href = url;
    anchor.download = name;
    document.body.appendChild(anchor);
    anchor.click();
    document.body.removeChild(anchor);
    // 释放延后一拍：立刻 revoke 在部分浏览器里会把下载掐断。
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  } catch (error) {
    MessagePlugin.error(readError(error, '下载失败'));
  }
}

/** base64 → Blob URL（PDF 内嵌用）。 */
function base64ToBlobUrl(base64: string, mime: string): string {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    bytes[i] = binary.charCodeAt(i);
  }
  return URL.createObjectURL(new Blob([bytes], { type: mime }));
}

/** 图标（按预览类型）。 */
function iconOf(kind: string): string {
  switch (kind) {
    case 'markdown':
      return 'file-word';
    case 'csv':
      return 'file-excel';
    case 'image':
      return 'image';
    case 'pdf':
      return 'file-pdf';
    default:
      return 'file-1';
  }
}

/** 字节可读化。 */
function formatSize(bytes: number): string {
  if (!bytes || bytes < 0) {
    return '0 B';
  }
  if (bytes < 1024) {
    return `${bytes} B`;
  }
  if (bytes < 1024 * 1024) {
    return `${(bytes / 1024).toFixed(1)} KB`;
  }
  return `${(bytes / 1024 / 1024).toFixed(2)} MB`;
}

/** RFC3339 → `YYYY-MM-DD HH:mm`（本地时区）。 */
function formatTime(raw: string): string {
  if (!raw) {
    return '—';
  }
  const date = new Date(raw);
  if (Number.isNaN(date.getTime())) {
    return raw;
  }
  const pad = (value: number): string => String(value).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** 统一错误文案（后端 `{code,msg}` 优先，其次 axios 的 message）。 */
function readError(error: unknown, fallback: string): string {
  if (typeof error === 'object' && error !== null && 'msg' in error) {
    const msg = (error as { msg?: unknown }).msg;
    if (typeof msg === 'string' && msg.length > 0) {
      return msg;
    }
  }
  if (error instanceof Error && error.message) {
    return error.message;
  }
  return fallback;
}

/** 切任务时重拉 + 关掉预览（避免看到上一个任务的文件）。 */
watch(
  () => props.taskId,
  () => {
    preview.visible = false;
    closePreview();
    void load();
  },
  { immediate: true },
);
</script>

<style scoped>
.aiws {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.aiws__hint {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 12px;
  border-radius: 6px;
  background: var(--td-bg-color-container-hover, #f3f3f3);
  color: var(--td-text-color-secondary, #666);
  font-size: 12px;
}

.aiws__hint code {
  padding: 0 4px;
  border-radius: 3px;
  background: var(--td-bg-color-container, #fff);
}

.aiws__list {
  display: flex;
  flex-direction: column;
}

.aiws__item {
  display: grid;
  grid-template-columns: 20px 1fr 90px 150px 64px;
  align-items: center;
  gap: 8px;
  padding: 8px 4px;
  border-bottom: 1px solid var(--td-component-stroke, #e7e7e7);
}

.aiws__icon {
  color: var(--td-brand-color, #0052d9);
}

.aiws__name {
  padding: 0;
  border: none;
  background: none;
  color: var(--td-brand-color, #0052d9);
  font-size: 14px;
  text-align: left;
  cursor: pointer;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.aiws__name:hover {
  text-decoration: underline;
}

.aiws__size,
.aiws__mtime {
  color: var(--td-text-color-secondary, #666);
  font-size: 12px;
  font-variant-numeric: tabular-nums;
}

.aiws__empty {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 24px 0;
  color: var(--td-text-color-placeholder, #999);
  font-size: 13px;
}

.aiws__error {
  padding: 16px 0;
  color: var(--td-error-color, #d54941);
  font-size: 13px;
}

.aiws__meta {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 12px;
  color: var(--td-text-color-secondary, #666);
  font-size: 12px;
}

.aiws__warn {
  color: var(--td-warning-color, #e37318);
}

.aiws__text {
  max-height: calc(88vh - 180px);
  margin: 0;
  padding: 12px;
  overflow: auto;
  border-radius: 6px;
  background: var(--td-bg-color-container-hover, #f7f7f7);
  font-family: var(--td-font-family-mono, ui-monospace, SFMono-Regular, Menlo, monospace);
  font-size: 12px;
  line-height: 1.6;
  white-space: pre-wrap;
  word-break: break-all;
}

.aiws__table {
  max-height: calc(88vh - 180px);
  overflow: auto;
  border: 1px solid var(--td-component-stroke, #e7e7e7);
  border-radius: 6px;
}

.aiws__table table {
  width: 100%;
  border-collapse: collapse;
  font-size: 12px;
}

.aiws__table th,
.aiws__table td {
  padding: 6px 10px;
  border-bottom: 1px solid var(--td-component-stroke, #e7e7e7);
  text-align: left;
  white-space: nowrap;
}

.aiws__table thead th {
  position: sticky;
  top: 0;
  background: var(--td-bg-color-container-hover, #f3f3f3);
  font-weight: 600;
}

.aiws__image {
  max-height: calc(88vh - 180px);
  overflow: auto;
  text-align: center;
}

.aiws__image img {
  max-width: 100%;
}

.aiws__pdf {
  height: calc(88vh - 180px);
}

.aiws__pdf iframe {
  width: 100%;
  height: 100%;
  border: 1px solid var(--td-component-stroke, #e7e7e7);
  border-radius: 6px;
}

.aiws__empty-inline {
  color: var(--td-text-color-placeholder, #999);
  font-size: 13px;
}
</style>
