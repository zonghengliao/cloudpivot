<template>
  <div class="help-center-page">
    <!-- 顶部搜索 -->
    <t-card :bordered="false" class="search-card">
      <div class="search-bar">
        <div class="search-bar__title">
          <t-icon name="question-circle" class="search-bar__icon" />
          <span>帮助中心</span>
        </div>
        <t-input
          v-model="keyword"
          class="search-bar__input"
          placeholder="搜索功能名 / 关键字，如「请假」「导入」"
          clearable
          @enter="handleSearch"
          @clear="clearSearch"
        >
          <template #prefix-icon><t-icon name="search" /></template>
          <template #suffix>
            <t-button size="small" theme="primary" variant="text" @click="handleSearch">搜索</t-button>
          </template>
        </t-input>
        <t-button v-privilege="[PERM.helpDocList]" variant="outline" @click="goManage">
          <template #icon><t-icon name="edit" /></template>
          文档管理
        </t-button>
      </div>
    </t-card>

    <div class="help-body">
      <!-- 左：分类树 -->
      <t-card :bordered="false" class="tree-card">
        <div class="tree-card__head">
          <span>目录</span>
          <t-button size="small" variant="text" :loading="categoryLoading" @click="loadCategories">
            <template #icon><t-icon name="refresh" /></template>
          </t-button>
        </div>
        <t-tree
          v-if="treeData.length > 0"
          :data="treeData"
          :keys="{ value: 'value', label: 'label', children: 'children' }"
          activable
          hover
          expand-all
          :active="activeCategory ? [activeCategory] : []"
          @active="onTreeActive"
        />
        <div v-else class="tree-empty">暂无分类</div>
      </t-card>

      <!-- 中：文档 / 命中列表 -->
      <t-card :bordered="false" class="list-card">
        <t-loading :loading="listLoading" size="small">
          <div v-if="searchMode" class="list-head">
            <span>「{{ keyword }}」的搜索结果</span>
            <t-link theme="default" @click="clearSearch">返回全部</t-link>
          </div>
          <div v-else class="list-head">
            <span>{{ activeCategoryName || '全部文档' }}</span>
            <span class="list-head__count">{{ pagination.total }} 篇</span>
          </div>

          <div class="doc-list">
            <div
              v-for="doc in searchMode ? [] : docs"
              :key="doc.id"
              class="doc-item"
              :class="{ 'doc-item--active': activeDocId === doc.id }"
              @click="openDoc(doc.id)"
            >
              <div class="doc-item__title">{{ doc.title }}</div>
              <div class="doc-item__summary">{{ doc.summary || '（无摘要）' }}</div>
              <div class="doc-item__foot">
                <span>{{ doc.category_name }}</span>
                <span>v{{ doc.version }}</span>
                <span>{{ doc.view_count }} 次阅读</span>
              </div>
            </div>

            <div
              v-for="hit in searchMode ? hits : []"
              :key="hit.id"
              class="doc-item"
              :class="{ 'doc-item--active': activeDocId === hit.id }"
              @click="openDoc(hit.id)"
            >
              <div class="doc-item__title">{{ hit.title }}</div>
              <!-- 后端 snippet 已转义/带高亮标记，这里只按文本展示，避免二次注入 -->
              <div class="doc-item__summary doc-item__summary--snippet">{{ plainSnippet(hit.snippet) }}</div>
              <div class="doc-item__foot">
                <span>{{ hit.category_name }}</span>
                <span class="doc-item__terms">命中：{{ hit.matched_terms.join('、') }}</span>
              </div>
            </div>

            <div v-if="!listLoading && currentCount === 0" class="empty-state">
              <t-icon class="empty-state__icon" name="search" />
              <p class="empty-state__title">{{ searchMode ? '没有匹配的文档' : '这个分类下还没有文档' }}</p>
              <p class="empty-state__desc">换个关键字，或联系管理员补充文档</p>
            </div>
          </div>
        </t-loading>

        <t-pagination
          v-if="!searchMode && pagination.total > pagination.pageSize"
          class="list-card__pager"
          :current="pagination.current"
          :page-size="pagination.pageSize"
          :total="pagination.total"
          size="small"
          @change="onPageChange"
        />
      </t-card>

      <!-- 右：正文 + TOC -->
      <t-card :bordered="false" class="content-card">
        <template v-if="detail">
          <div class="content-head">
            <h2 class="content-head__title">{{ detail.title }}</h2>
            <div class="content-head__meta">
              <t-tag size="small" variant="light">{{ detail.category_name }}</t-tag>
              <span>版本 v{{ detail.version }}</span>
              <span>{{ detail.view_count }} 次阅读</span>
              <span>更新于 {{ formatDateTime(detail.updated_at) }}</span>
            </div>
          </div>
          <MarkdownViewer :html="detail.content_html" :toc="detail.toc" max-height="calc(100vh - 300px)" />
        </template>
        <div v-else class="empty-state empty-state--detail">
          <t-icon class="empty-state__icon" name="book" />
          <p class="empty-state__title">选择左侧文档查看正文</p>
          <p class="empty-state__desc">右侧目录可点击跳转，滚动时自动高亮当前小节</p>
        </div>
      </t-card>
    </div>
  </div>
</template>

<script setup lang="ts">
/**
 * 帮助中心（用户侧，`system:help:read`，所有未软删角色默认授予）。
 *
 * # 四块能力
 *
 * 分类树（后端 `category/tree`，带已发布文档数）/ 文档列表（按分类 + 分页）/
 * 全文检索（`ILIKE` + 后端转义，命中片段直接展示）/ 正文（`content_html` + `toc`，
 * 由 `MarkdownViewer` 做过白名单净化后渲染，TOC 点击滚动）。
 *
 * # 检索结果为什么只显示纯文本片段
 *
 * 后端 `snippet` 里含 `<mark>` 之类的标记，但用户输入的 `q` 可能带尖括号；
 * 页面统一 `plainSnippet()` 去标签 + 转义后按文本显示，**不**用 `v-html`。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import { useRouter } from 'vue-router';
import MarkdownViewer from '@/components/MarkdownViewer.vue';
import { ApiError } from '@/api/request';
import { PERM } from '@/constants/permissions';
import {
  getCategoryTree,
  getDoc,
  markDocRead,
  pageDocs,
  searchDocs,
  type HelpCategoryNode,
  type HelpDocCard,
  type HelpDocDetail,
  type HelpSearchHit,
} from '@/api/business/help-api';
import { formatDateTime } from '@/utils/format';

const router = useRouter();

const keyword = ref('');
const searchMode = ref(false);
const treeData = ref<{ value: string; label: string; children?: { value: string; label: string }[] }[]>([]);
const categoryLoading = ref(false);
const listLoading = ref(false);
const activeCategory = ref('');
const docs = ref<HelpDocCard[]>([]);
const hits = ref<HelpSearchHit[]>([]);
const detail = ref<HelpDocDetail | null>(null);
const activeDocId = ref('');
const pagination = reactive({ current: 1, pageSize: 12, total: 0 });

const activeCategoryName = computed(() => findCategoryName(treeData.value, activeCategory.value));
const currentCount = computed(() => (searchMode.value ? hits.value.length : docs.value.length));

/** 在树里找分类名（用于列表标题） */
function findCategoryName(
  nodes: { value: string; label: string; children?: { value: string; label: string }[] }[],
  id: string,
): string {
  if (!id) {
    return '';
  }
  for (const node of nodes) {
    if (node.value === id) {
      return node.label;
    }
    for (const child of node.children ?? []) {
      if (child.value === id) {
        return child.label;
      }
    }
  }
  return '';
}

/** 分类树 → t-tree 数据结构（label 带文档数） */
interface TreeDataNode {
  /** 分类 id */
  value: string;
  /** 展示名（含文档数） */
  label: string;
  /** 子节点 */
  children?: TreeDataNode[];
}

function toTreeNodes(nodes: HelpCategoryNode[]): TreeDataNode[] {
  return nodes.map((node) => ({
    value: node.id,
    label: node.doc_count > 0 ? `${node.name}（${node.doc_count}）` : node.name,
    children: node.children?.length ? toTreeNodes(node.children) : undefined,
  }));
}

async function loadCategories(): Promise<void> {
  categoryLoading.value = true;
  try {
    const tree = await getCategoryTree({ include_empty: true });
    treeData.value = toTreeNodes(tree);
  } catch (error) {
    showError(error);
  } finally {
    categoryLoading.value = false;
  }
}

function onTreeActive(value: unknown): void {
  const ids = Array.isArray(value) ? value : [value];
  const id = typeof ids[0] === 'string' ? ids[0] : '';
  activeCategory.value = id;
  searchMode.value = false;
  keyword.value = '';
  pagination.current = 1;
  void loadDocs();
}

async function loadDocs(): Promise<void> {
  listLoading.value = true;
  try {
    const data = await pageDocs({
      page: pagination.current,
      page_size: pagination.pageSize,
      category_id: activeCategory.value || undefined,
      include_children: true,
    });
    docs.value = data.list;
    pagination.total = data.total;
    if (docs.value.length > 0 && !docs.value.some((doc) => doc.id === activeDocId.value)) {
      void openDoc(docs.value[0].id);
    }
  } catch (error) {
    showError(error);
  } finally {
    listLoading.value = false;
  }
}

async function handleSearch(): Promise<void> {
  const q = keyword.value.trim();
  if (!q) {
    clearSearch();
    return;
  }
  searchMode.value = true;
  listLoading.value = true;
  try {
    const data = await searchDocs({ q, page: 1, page_size: 30 });
    hits.value = data.list;
    if (hits.value.length > 0) {
      void openDoc(hits.value[0].id);
    } else {
      detail.value = null;
      activeDocId.value = '';
    }
  } catch (error) {
    showError(error);
  } finally {
    listLoading.value = false;
  }
}

function clearSearch(): void {
  searchMode.value = false;
  keyword.value = '';
  hits.value = [];
  void loadDocs();
}

/** 打开文档：拉详情 + 上报阅读量（后端去重，重复打开不重复计数） */
async function openDoc(id: string): Promise<void> {
  activeDocId.value = id;
  try {
    detail.value = await getDoc(id);
    const result = await markDocRead(id);
    if (detail.value) {
      detail.value.view_count = result.view_count;
    }
  } catch (error) {
    detail.value = null;
    showError(error);
  }
}

/** 检索片段去标签（后端可能返回 `<mark>`；统一按纯文本展示） */
function plainSnippet(snippet: string): string {
  return (snippet || '').replace(/<[^>]+>/g, '');
}

function onPageChange(pageInfo: { current: number; pageSize: number }): void {
  pagination.current = pageInfo.current;
  pagination.pageSize = pageInfo.pageSize;
  void loadDocs();
}

function goManage(): void {
  void router.push('/help/manage');
}

function showError(error: unknown): void {
  void MessagePlugin.error(error instanceof ApiError ? error.message : '请求失败，请稍后重试');
}

onMounted(() => {
  void loadCategories();
  void loadDocs();
});
</script>

<style scoped lang="less">
.help-center-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
  height: 100%;
}

.search-card {
  flex: 0 0 auto;
}

.search-bar {
  display: flex;
  align-items: center;
  gap: 12px;

  &__title {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 16px;
    font-weight: 600;
    color: var(--td-text-color-primary);
    white-space: nowrap;
  }

  &__icon {
    color: var(--td-brand-color);
  }

  &__input {
    max-width: 520px;
    flex: 1;
  }
}

.help-body {
  display: flex;
  gap: 12px;
  flex: 1;
  min-height: 0;
}

.tree-card {
  width: 230px;
  flex: 0 0 230px;
  overflow-y: auto;

  &__head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 8px;
    font-size: 13px;
    font-weight: 600;
    color: var(--td-text-color-secondary);
  }
}

.tree-empty {
  padding: 24px 0;
  text-align: center;
  font-size: 13px;
  color: var(--td-text-color-placeholder);
}

.list-card {
  width: 320px;
  flex: 0 0 320px;
  display: flex;
  flex-direction: column;
  min-height: 0;

  &__pager {
    margin-top: 10px;
    display: flex;
    justify-content: center;
  }
}

.list-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 8px;
  font-size: 13px;
  font-weight: 600;
  color: var(--td-text-color-secondary);

  &__count {
    font-weight: 400;
    color: var(--td-text-color-placeholder);
  }
}

.doc-list {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
}

.doc-item {
  padding: 10px 12px;
  border-radius: 8px;
  cursor: pointer;

  &:hover {
    background: var(--td-bg-color-container-hover);
  }

  &--active {
    background: var(--app-accent-soft);
  }

  &__title {
    font-size: 14px;
    color: var(--td-text-color-primary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  &__summary {
    margin: 6px 0;
    font-size: 12px;
    line-height: 1.5;
    color: var(--td-text-color-secondary);
    display: -webkit-box;
    -webkit-line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;

    &--snippet {
      word-break: break-all;
    }
  }

  &__foot {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__terms {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
}

.content-card {
  flex: 1;
  min-width: 0;
  min-height: 0;
  display: flex;
  flex-direction: column;
}

.content-head {
  padding-bottom: 12px;
  margin-bottom: 12px;
  border-bottom: 1px solid var(--td-component-stroke);

  &__title {
    margin: 0 0 8px;
    font-size: 19px;
    color: var(--td-text-color-primary);
  }

  &__meta {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 10px;
    font-size: 12px;
    color: var(--td-text-color-secondary);
  }
}

.empty-state {
  padding: 60px 0;
  text-align: center;

  &--detail {
    padding: 140px 0;
  }

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
    font-size: 13px;
    color: var(--td-text-color-secondary);
  }
}
</style>
