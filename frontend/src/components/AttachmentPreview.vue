<!--
  附件在线预览（ATT-01）。

  # 一句话
  统一「点开附件」这一件事：图片缩放旋转、PDF 翻页、Office / 压缩包只给下载。

  # 为什么必须走 fetch + blob URL
  下载接口都带 `Authorization: Bearer`，`<img src="/api/support/file/download/xxx">` 这种裸链接
  不带鉴权头 → 401；就算后端改成 cookie 鉴权，裸链接在「无权限」时拿到的是 JSON 错误体，
  浏览器会把它当图片解析失败，用户看到的是碎图而不是提示。所以统一
  `fetch（带 token）→ Blob → URL.createObjectURL`，并且**每个 URL 都必须有对应的
  `revokeObjectURL`**：切图 / 关闭弹窗 / 组件卸载三处都释放，否则整段会话里
  每看一张图就泄漏一份完整文件字节（PDF 几十 MB 时很明显）。

  # 三种界面的分流
  - `image`：`<img>` + 滚轮缩放（0.1/格）+ 按钮缩放（0.25/格）+ 旋转 90° + 拖拽平移；
  - `pdf`：pdf.js 画布渲染（不用 `<iframe>`：iframe 的原生阅读器在 headless / 不同浏览器里
    行为不一致，翻页与缩放没法统一控制，也没法做无内存泄漏的 blob 生命周期）；
  - 其余：图标 + 「下载」，点击才去取字节（Office 文档动辄几十 MB，不该为了显示一个
    图标先下载一遍）。

  # 与外部的契约
  `items` 由调用方抹平来源差异（`sys_file` / 单据附件），需要自定义下载入口时传 `load`。
  组件不猜业务、不拉列表，只负责「展示 + 取字节 + 释放」。
-->
<template>
  <t-dialog
    v-model:visible="visibleModel"
    :header="dialogTitle"
    :footer="false"
    :width="'min(1180px, 96vw)'"
    :close-on-overlay-click="true"
    destroy-on-close
    class="attach-preview__dialog"
  >
    <div v-if="currentItem" class="attach-preview" data-testid="attachment-preview">
      <!-- 顶部：左右切换 + 序号 + 下载 -->
      <div class="attach-preview__bar">
        <div class="attach-preview__bar-left">
          <t-button
            size="small"
            variant="outline"
            data-testid="attachment-preview-prev"
            :disabled="!canPrev"
            @click="goTo(currentIndex - 1)"
          >
            上一张
          </t-button>
          <t-button
            size="small"
            variant="outline"
            data-testid="attachment-preview-next"
            :disabled="!canNext"
            @click="goTo(currentIndex + 1)"
          >
            下一张
          </t-button>
          <span class="attach-preview__counter" data-testid="attachment-preview-counter">
            {{ currentIndex + 1 }} / {{ items.length }}
          </span>
        </div>
        <div class="attach-preview__bar-right">
          <t-button
            size="small"
            theme="primary"
            variant="outline"
            :loading="downloading"
            data-testid="attachment-preview-download"
            @click="download"
          >
            <template #icon><t-icon name="download" /></template>
            下载
          </t-button>
        </div>
      </div>

      <div class="attach-preview__main">
        <!-- 左：文件区 -->
        <div class="attach-preview__stage" :class="{ 'attach-preview__stage--drag': dragging }">
          <img
            v-if="kind === 'image' && objectUrl"
            class="attach-preview__img"
            :src="objectUrl"
            :alt="currentItem.name"
            :style="{ transform: imgTransform }"
            data-testid="attachment-preview-img"
            @wheel.prevent="onWheel"
            @pointerdown="onPointerDown"
            @pointermove="onPointerMove"
            @pointerup="onPointerUp"
            @pointercancel="onPointerUp"
          />

          <div v-else-if="kind === 'pdf'" class="attach-preview__pdf">
            <div class="attach-preview__pdf-toolbar">
              <t-button
                size="small"
                variant="text"
                data-testid="attachment-preview-pdf-prev"
                :disabled="pdfPage <= 1"
                @click="onPdfPrev"
              >
                上一页
              </t-button>
              <span class="attach-preview__pdf-page" data-testid="attachment-preview-pdf-page">
                {{ pdfPage }} / {{ pdfTotal || '-' }}
              </span>
              <t-button
                size="small"
                variant="text"
                data-testid="attachment-preview-pdf-next"
                :disabled="!pdfTotal || pdfPage >= pdfTotal"
                @click="onPdfNext"
              >
                下一页
              </t-button>
              <t-divider layout="vertical" />
              <t-button size="small" variant="text" data-testid="attachment-preview-zoom-out" @click="onZoomOut">
                缩小
              </t-button>
              <span class="attach-preview__zoom" data-testid="attachment-preview-zoom">{{ zoomLabel }}</span>
              <t-button size="small" variant="text" data-testid="attachment-preview-zoom-in" @click="onZoomIn">
                放大
              </t-button>
            </div>
            <div class="attach-preview__pdf-canvas">
              <canvas ref="pdfCanvasRef" data-testid="attachment-preview-canvas" />
            </div>
          </div>

          <div v-else class="attach-preview__fallback" data-testid="attachment-preview-fallback">
            <t-icon :name="kind === 'archive' ? 'folder' : 'file'" size="64px" />
            <p class="attach-preview__fallback-text">{{ fallbackHint }}</p>
            <t-button
              theme="primary"
              :loading="downloading"
              data-testid="attachment-preview-fallback-download"
              @click="download"
            >
              下载
            </t-button>
          </div>

          <!-- 图片工具条：缩放 / 旋转 / 复位 -->
          <div v-if="kind === 'image'" class="attach-preview__image-toolbar">
            <t-button size="small" variant="text" data-testid="attachment-preview-zoom-out" @click="onZoomOut">
              缩小
            </t-button>
            <span class="attach-preview__zoom" data-testid="attachment-preview-zoom">{{ zoomLabel }}</span>
            <t-button size="small" variant="text" data-testid="attachment-preview-zoom-in" @click="onZoomIn">
              放大
            </t-button>
            <t-button size="small" variant="text" data-testid="attachment-preview-rotate" @click="onRotate">
              旋转
            </t-button>
            <t-button size="small" variant="text" data-testid="attachment-preview-reset" @click="onResetView">
              复位
            </t-button>
          </div>

          <div v-if="loading" class="attach-preview__mask" data-testid="attachment-preview-loading">
            <t-loading size="small" />
            <span>加载中…</span>
          </div>
          <div v-else-if="error" class="attach-preview__mask attach-preview__mask--error" data-testid="attachment-preview-error">
            {{ error }}
          </div>
        </div>

        <!-- 右：元信息 -->
        <aside class="attach-preview__meta" data-testid="attachment-preview-meta">
          <div class="attach-preview__meta-title">文件信息</div>
          <dl class="attach-preview__meta-list">
            <template v-for="row in metaRows" :key="row.key">
              <dt>{{ row.label }}</dt>
              <dd :class="{ 'attach-preview__meta-note': row.key === 'remark' }" :data-testid="`attachment-preview-meta-${row.key}`">
                {{ row.value }}
              </dd>
            </template>
          </dl>
        </aside>
      </div>
    </div>
  </t-dialog>
</template>

<script setup lang="ts">
/**
 * 附件在线预览组件。用法见文件头注释；两边接入点：
 * - `views/support/file/index.vue`（系统文件列表）
 * - `views/business/erp/shared/DocAttachmentPanel.vue`（单据附件）
 */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, shallowRef, watch } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import type { PDFDocumentProxy, RenderTask } from 'pdfjs-dist';
import pdfWorkerUrl from 'pdfjs-dist/build/pdf.worker.min.mjs?url';

import { downloadFileBlob } from '@/api/support/file-api';
import { ApiError } from '@/api/request';
import { saveBlob } from '@/utils/download';
import { formatDateTime, formatFileSize } from '@/utils/format';
import {
  ROTATE_STEP,
  WHEEL_ZOOM_STEP,
  ZOOM_STEP,
  clampPage,
  clampZoom,
  formatZoom,
  imageTransform,
  isPreviewable,
  kindLabel,
  resolvePreviewKind,
  rotateBy,
  unsupportedHint,
  zoomBy,
  type PreviewItem,
} from './attachment-preview-pure';
import { downloadHintFor } from './attachment-upload-pure';

const props = withDefaults(
  defineProps<{
    /** 是否显示（`v-model:visible`） */
    visible: boolean;
    /** 待预览列表（顺序即左右切换顺序） */
    items?: PreviewItem[];
    /** 打开时定位到第几个（从 0 开始，越界自动夹取） */
    index?: number;
    /** 弹窗标题前缀（文件名会拼在后面） */
    title?: string;
  }>(),
  { items: () => [], index: 0, title: '附件预览' },
);

const emit = defineEmits<{
  /** 关闭 / 打开 */
  (event: 'update:visible', value: boolean): void;
  /** 切换到第几个（调用方可据此同步高亮） */
  (event: 'change', index: number): void;
}>();

const visibleModel = computed({
  get: () => props.visible,
  set: (value: boolean) => emit('update:visible', value),
});

const items = computed<PreviewItem[]>(() => props.items ?? []);
const currentIndex = ref(0);
const currentItem = computed<PreviewItem | undefined>(() => items.value[currentIndex.value]);
const kind = computed(() => resolvePreviewKind(currentItem.value?.name ?? '', currentItem.value?.mimeType));
const previewable = computed(() => isPreviewable(kind.value));
const canPrev = computed(() => currentIndex.value > 0);
const canNext = computed(() => currentIndex.value < items.value.length - 1);
const dialogTitle = computed(() =>
  currentItem.value ? `${props.title} · ${currentItem.value.name}` : props.title,
);

/** 当前预览对象的 blob URL（null 表示没有可展示的字节） */
const objectUrl = ref('');
const loading = ref(false);
const downloading = ref(false);
const error = ref('');

/** 图片视图状态 */
const zoom = ref(1);
const rotation = ref(0);
const offsetX = ref(0);
const offsetY = ref(0);
const dragging = ref(false);
let dragOrigin = { x: 0, y: 0, offsetX: 0, offsetY: 0 };

/** PDF 视图状态 */
const pdfCanvasRef = ref<HTMLCanvasElement | null>(null);
const pdfDoc = shallowRef<PDFDocumentProxy | null>(null);
const pdfPage = ref(1);
const pdfTotal = ref(0);
const pdfScale = ref(1);
let pdfRenderTask: RenderTask | null = null;

const zoomLabel = computed(() => formatZoom(kind.value === 'pdf' ? pdfScale.value : zoom.value));
const imgTransform = computed(() => imageTransform(zoom.value, rotation.value, offsetX.value, offsetY.value));
const fallbackHint = computed(() => unsupportedHint(kind.value));

/** 右侧元信息（顺序固定：文件名 / 大小 / 类型 / 角色 / 上传人 / 上传时间 / 备注） */
const metaRows = computed<{ key: string; label: string; value: string }[]>(() => {
  const item = currentItem.value;
  if (!item) {
    return [];
  }
  const rows: { key: string; label: string; value: string }[] = [
    { key: 'name', label: '文件名', value: item.name || '-' },
    { key: 'size', label: '大小', value: formatFileSize(item.size) },
    { key: 'kind', label: '预览方式', value: kindLabel(kind.value) },
  ];
  if (item.tag) {
    rows.push({ key: 'tag', label: '文件角色', value: item.tag });
  }
  rows.push(
    { key: 'uploader', label: '上传人', value: item.uploader || '-' },
    { key: 'time', label: '上传时间', value: formatDateTime(item.uploadedAt) },
    { key: 'remark', label: '备注', value: item.remark || '-' },
  );
  return rows;
});

/**
 * 递增的加载序号。
 *
 * 每次切换 / 关闭都 +1，异步回调回来先比对：序号变了说明用户已经切走或关了弹窗，
 * 直接丢弃结果 —— 否则「切图时慢的那张后到」会把当前图覆盖成上一张，
 * 而且它创建的 blob URL 没人释放（内存泄漏）。
 */
let loadSeq = 0;

function describeError(value: unknown): string {
  if (value instanceof ApiError) {
    return value.message || '文件读取失败';
  }
  if (value instanceof Error && value.message) {
    return value.message;
  }
  return '文件读取失败';
}

function releaseObjectUrl(): void {
  if (objectUrl.value) {
    URL.revokeObjectURL(objectUrl.value);
    objectUrl.value = '';
  }
}

function cleanupPdf(): void {
  pdfRenderTask?.cancel();
  pdfRenderTask = null;
  const doc = pdfDoc.value;
  pdfDoc.value = null;
  pdfPage.value = 1;
  pdfTotal.value = 0;
  pdfScale.value = 1;
  if (doc) {
    void doc.destroy();
  }
}

function resetImage(): void {
  zoom.value = 1;
  rotation.value = 0;
  offsetX.value = 0;
  offsetY.value = 0;
  dragging.value = false;
}

/** 彻底清理：取消在途加载 + 释放全部 blob URL + 销毁 pdf 文档 */
function cleanup(): void {
  loadSeq += 1;
  releaseObjectUrl();
  cleanupPdf();
  resetImage();
  loading.value = false;
  downloading.value = false;
  error.value = '';
}

function normalizeIndex(index: number): number {
  const max = Math.max(0, items.value.length - 1);
  if (!Number.isFinite(index)) {
    return 0;
  }
  return Math.min(max, Math.max(0, Math.floor(index)));
}

/** 取字节：调用方给了 `load` 就用它（单据附件按绑定行 id 下载），否则走支持文件接口 */
async function fetchBlob(item: PreviewItem): Promise<Blob> {
  return item.load ? item.load() : downloadFileBlob(item.fileId);
}

async function loadCurrent(): Promise<void> {
  const seq = ++loadSeq;
  releaseObjectUrl();
  cleanupPdf();
  resetImage();
  error.value = '';
  loading.value = false;

  const item = currentItem.value;
  // Office / 压缩包 / 其他：只展示图标 + 下载按钮，不为「显示图标」先把几十 MB 拉下来
  if (!item || !previewable.value) {
    return;
  }

  loading.value = true;
  try {
    const blob = await fetchBlob(item);
    if (seq !== loadSeq) {
      return;
    }
    objectUrl.value = URL.createObjectURL(blob);
    if (kind.value === 'pdf') {
      await openPdf(blob, seq);
    }
  } catch (cause) {
    if (seq === loadSeq) {
      error.value = describeError(cause);
    }
  } finally {
    if (seq === loadSeq) {
      loading.value = false;
    }
  }
}

/** 用 pdf.js 打开文档（动态 import：不使用 PDF 的页面不会加载这 ~400KB） */
async function openPdf(blob: Blob, seq: number): Promise<void> {
  const pdfjs = await import('pdfjs-dist');
  pdfjs.GlobalWorkerOptions.workerSrc = pdfWorkerUrl;
  const data = new Uint8Array(await blob.arrayBuffer());
  const doc = await pdfjs.getDocument({ data }).promise;
  if (seq !== loadSeq) {
    void doc.destroy();
    return;
  }
  pdfDoc.value = doc;
  pdfTotal.value = doc.numPages;
  pdfPage.value = 1;
  pdfScale.value = 1;
  await nextTick();
  await renderPdfPage();
}

/** 渲染当前页到 canvas（按 devicePixelRatio 放大，避免高分屏糊） */
async function renderPdfPage(): Promise<void> {
  const doc = pdfDoc.value;
  const canvas = pdfCanvasRef.value;
  if (!doc || !canvas) {
    return;
  }
  pdfRenderTask?.cancel();
  const pageNumber = clampPage(pdfPage.value, pdfTotal.value);
  pdfPage.value = pageNumber;
  const page = await doc.getPage(pageNumber);
  if (pdfDoc.value !== doc) {
    return;
  }
  const context = canvas.getContext('2d');
  if (!context) {
    return;
  }
  const ratio = window.devicePixelRatio || 1;
  const viewport = page.getViewport({ scale: pdfScale.value * ratio });
  canvas.width = Math.floor(viewport.width);
  canvas.height = Math.floor(viewport.height);
  canvas.style.width = `${Math.floor(viewport.width / ratio)}px`;
  canvas.style.height = `${Math.floor(viewport.height / ratio)}px`;
  try {
    pdfRenderTask = page.render({ canvasContext: context, viewport });
    await pdfRenderTask.promise;
  } catch (cause) {
    // 快速翻页时上一次渲染会被 cancel，这不是错误
    if ((cause as { name?: string } | null)?.name !== 'RenderingCancelledError' &&
      (cause as { name?: string } | null)?.name !== 'RenderingCancelledException') {
      throw cause;
    }
  } finally {
    pdfRenderTask = null;
  }
}

function goTo(index: number): void {
  const next = normalizeIndex(index);
  if (next === currentIndex.value) {
    return;
  }
  currentIndex.value = next;
  emit('change', next);
  void loadCurrent();
}

function onZoomIn(): void {
  if (kind.value === 'pdf') {
    pdfScale.value = clampZoom(pdfScale.value + ZOOM_STEP);
    void renderPdfPage();
    return;
  }
  zoom.value = zoomBy(zoom.value, ZOOM_STEP);
}

function onZoomOut(): void {
  if (kind.value === 'pdf') {
    pdfScale.value = clampZoom(pdfScale.value - ZOOM_STEP);
    void renderPdfPage();
    return;
  }
  zoom.value = zoomBy(zoom.value, -ZOOM_STEP);
}

function onWheel(event: WheelEvent): void {
  if (kind.value !== 'image') {
    return;
  }
  zoom.value = zoomBy(zoom.value, event.deltaY > 0 ? -WHEEL_ZOOM_STEP : WHEEL_ZOOM_STEP);
}

function onRotate(): void {
  rotation.value = rotateBy(rotation.value, ROTATE_STEP);
}

function onResetView(): void {
  resetImage();
}

function onPdfPrev(): void {
  if (pdfPage.value <= 1) {
    return;
  }
  pdfPage.value = clampPage(pdfPage.value - 1, pdfTotal.value);
  void renderPdfPage();
}

function onPdfNext(): void {
  if (!pdfTotal.value || pdfPage.value >= pdfTotal.value) {
    return;
  }
  pdfPage.value = clampPage(pdfPage.value + 1, pdfTotal.value);
  void renderPdfPage();
}

function onPointerDown(event: PointerEvent): void {
  if (kind.value !== 'image') {
    return;
  }
  dragging.value = true;
  dragOrigin = { x: event.clientX, y: event.clientY, offsetX: offsetX.value, offsetY: offsetY.value };
  (event.target as HTMLElement | null)?.setPointerCapture?.(event.pointerId);
}

function onPointerMove(event: PointerEvent): void {
  if (!dragging.value) {
    return;
  }
  offsetX.value = dragOrigin.offsetX + (event.clientX - dragOrigin.x);
  offsetY.value = dragOrigin.offsetY + (event.clientY - dragOrigin.y);
}

function onPointerUp(event: PointerEvent): void {
  if (!dragging.value) {
    return;
  }
  dragging.value = false;
  (event.target as HTMLElement | null)?.releasePointerCapture?.(event.pointerId);
}

/**
 * 下载：Office / 压缩包的唯一出口；图片 / PDF 也保留这个入口。
 *
 * ATT-05：非可预览类型（Office / 压缩包 / 其他）下载完要提示「用本地软件打开」——
 * 预览弹窗里点下载的用户同样需要这句引导（`downloadHintFor` 与单据附件面板共用同一份文案，
 * 见 `attachment-upload-pure.ts`）。
 */
async function download(): Promise<void> {
  const item = currentItem.value;
  if (!item || downloading.value) {
    return;
  }
  downloading.value = true;
  try {
    saveBlob(await fetchBlob(item), item.name || item.fileId);
    const hint = downloadHintFor(item.name || '', item.mimeType);
    if (hint) {
      MessagePlugin.info(hint, 5000);
    }
  } catch (cause) {
    MessagePlugin.error(describeError(cause));
  } finally {
    downloading.value = false;
  }
}

function onKeydown(event: KeyboardEvent): void {
  if (!props.visible) {
    return;
  }
  const target = event.target as HTMLElement | null;
  if (target && ['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName)) {
    return;
  }
  if (event.key === 'ArrowLeft') {
    event.preventDefault();
    goTo(currentIndex.value - 1);
  } else if (event.key === 'ArrowRight') {
    event.preventDefault();
    goTo(currentIndex.value + 1);
  }
}

// 打开：定位 + 加载；关闭：**立刻**释放 blob URL（验收 ⑥ 就是看这一下）
watch(
  () => props.visible,
  (open) => {
    if (open) {
      currentIndex.value = normalizeIndex(props.index);
      void loadCurrent();
      return;
    }
    cleanup();
  },
  { immediate: true },
);

// 打开期间父级列表刷新（上传 / 删除）时重新对齐当前项
watch(
  () => items.value.map((item) => item.key).join('|'),
  () => {
    if (!props.visible) {
      return;
    }
    currentIndex.value = normalizeIndex(currentIndex.value);
    void loadCurrent();
  },
);

onMounted(() => window.addEventListener('keydown', onKeydown));

onBeforeUnmount(() => {
  window.removeEventListener('keydown', onKeydown);
  cleanup();
});
</script>

<style scoped lang="less">
.attach-preview {
  display: flex;
  flex-direction: column;
  gap: 12px;

  &__bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }

  &__bar-left,
  &__bar-right {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  &__counter {
    color: var(--td-text-color-secondary, #666);
    font-size: 13px;
  }

  &__main {
    display: flex;
    gap: 16px;
    min-height: 60vh;
  }

  &__stage {
    position: relative;
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    display: flex;
    align-items: center;
    justify-content: center;
    border-radius: 6px;
    background: var(--td-bg-color-page, #f3f4f6);
    touch-action: none;

    &--drag {
      cursor: grabbing;
    }
  }

  &__img {
    max-width: 100%;
    max-height: 60vh;
    transform-origin: center center;
    transition: transform 0.12s ease-out;
    user-select: none;
    cursor: grab;
  }

  &__image-toolbar {
    position: absolute;
    bottom: 12px;
    left: 50%;
    transform: translateX(-50%);
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 2px 8px;
    border-radius: 16px;
    background: var(--td-bg-color-container, #fff);
    box-shadow: 0 2px 8px rgba(0, 0, 0, 0.12);
  }

  &__zoom {
    min-width: 48px;
    text-align: center;
    font-size: 12px;
    color: var(--td-text-color-secondary, #666);
  }

  &__pdf {
    display: flex;
    flex-direction: column;
    width: 100%;
    height: 60vh;
  }

  &__pdf-toolbar {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 4px 8px;
    border-bottom: 1px solid var(--td-component-border, #e7e7e7);
  }

  &__pdf-page {
    font-size: 13px;
    color: var(--td-text-color-secondary, #666);
    min-width: 60px;
    text-align: center;
  }

  &__pdf-canvas {
    flex: 1 1 auto;
    overflow: auto;
    display: flex;
    align-items: flex-start;
    justify-content: center;
    padding: 8px;
  }

  &__fallback {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
    color: var(--td-text-color-secondary, #666);
  }

  &__fallback-text {
    margin: 0;
    font-size: 13px;
  }

  &__mask {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    background: var(--td-bg-color-page, #f3f4f6);
    color: var(--td-text-color-secondary, #666);
    font-size: 13px;

    &--error {
      color: var(--td-error-color, #d54941);
      padding: 24px;
      text-align: center;
    }
  }

  &__meta {
    flex: 0 0 300px;
    padding: 12px;
    border-left: 1px solid var(--td-component-border, #e7e7e7);
    overflow-y: auto;
  }

  &__meta-title {
    font-weight: 600;
    margin-bottom: 8px;
  }

  &__meta-list {
    margin: 0;
    display: grid;
    grid-template-columns: 72px 1fr;
    row-gap: 10px;
    column-gap: 8px;
    font-size: 13px;

    dt {
      color: var(--td-text-color-secondary, #666);
    }

    dd {
      margin: 0;
      word-break: break-all;
    }
  }

  &__meta-note {
    white-space: pre-wrap;
  }
}
</style>
