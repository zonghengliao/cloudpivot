<template>
  <div class="help-manage-page">
    <!-- 分类树维护 -->
    <t-card :bordered="false" class="tree-card">
      <div class="tree-head">
        <span>分类树（深度 ≤ 3）</span>
        <t-space size="small">
          <t-button v-privilege="[PERM.helpCategoryCreate]" size="small" theme="primary" variant="outline" @click="openCategoryCreate()">
            <template #icon><t-icon name="add" /></template>
            新建
          </t-button>
          <t-button size="small" variant="text" :loading="categoryLoading" @click="loadCategories">
            <template #icon><t-icon name="refresh" /></template>
          </t-button>
        </t-space>
      </div>
      <t-tree
        v-if="treeData.length > 0"
        :data="treeData"
        :keys="{ value: 'value', label: 'label', children: 'children' }"
        activable
        hover
        expand-all
        :active="activeCategory ? [activeCategory] : []"
        @active="onCategoryActive"
      />
      <div v-else class="tree-empty">还没有分类{{ PERM.helpCategoryCreate ? '，先建一个' : '' }}</div>

      <div v-if="currentCategory" class="tree-actions">
        <span class="tree-actions__name">{{ currentCategory.name }}</span>
        <t-space size="small">
          <t-button v-privilege="[PERM.helpCategoryCreate]" size="small" variant="text" @click="openCategoryCreate(currentCategory.id)">
            加子分类
          </t-button>
          <t-button v-privilege="[PERM.helpCategoryUpdate]" size="small" variant="text" @click="openCategoryEdit(currentCategory)">
            编辑
          </t-button>
          <t-button v-privilege="[PERM.helpCategoryDelete]" size="small" variant="text" theme="danger" @click="confirmCategoryRemove(currentCategory)">
            删除
          </t-button>
        </t-space>
      </div>
    </t-card>

    <!-- 文档列表 -->
    <t-card :bordered="false" class="table-card">
      <t-row :gutter="[12, 12]" class="filter-row">
        <t-col :span="4">
          <t-input v-model="query.keyword" placeholder="标题 / 摘要 / 正文关键字" clearable @enter="handleSearch" />
        </t-col>
        <t-col :span="3">
          <t-select v-model="query.status" placeholder="状态" clearable :options="statusOptions" />
        </t-col>
        <t-col :span="5">
          <t-space>
            <t-button theme="primary" @click="handleSearch">
              <template #icon><t-icon name="search" /></template>
              查询
            </t-button>
            <t-button variant="outline" @click="handleReset">重置</t-button>
            <t-checkbox v-model="includeUnpublished" @change="handleSearch">含草稿 / 已下架</t-checkbox>
          </t-space>
        </t-col>
      </t-row>

      <div class="toolbar">
        <t-space>
          <t-button v-privilege="[PERM.helpDocCreate]" theme="primary" @click="openDocCreate">
            <template #icon><t-icon name="add" /></template>
            新建文档
          </t-button>
          <t-button variant="outline" :loading="loading" @click="loadDocs">
            <template #icon><t-icon name="refresh" /></template>
            刷新
          </t-button>
        </t-space>
        <span class="toolbar__count">共 {{ pagination.total }} 篇</span>
      </div>

      <t-table
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        stripe
        @page-change="handlePageChange"
      >
        <template #title="{ row }">
          <t-link theme="primary" @click="openDocEdit(row)">{{ row.title }}</t-link>
          <div class="cell-slug">{{ row.slug }}</div>
        </template>
        <template #status="{ row }">
          <t-tag :theme="statusTheme(row.status)" size="small" variant="light">{{ statusLabel(row.status) }}</t-tag>
        </template>
        <template #updated_at="{ row }">{{ formatDateTime(row.updated_at) }}</template>
        <template #op="{ row }">
          <t-space size="small">
            <t-link v-privilege="[PERM.helpDocUpdate]" theme="primary" @click="openDocEdit(row)">编辑</t-link>
            <t-link v-privilege="[PERM.helpDocPublish]" theme="primary" @click="confirmPublish(row)">
              {{ row.status === 'PUBLISHED' ? '重新发布' : '发布' }}
            </t-link>
            <t-link v-privilege="[PERM.helpDocList]" theme="default" @click="openVersions(row)">版本</t-link>
            <t-link v-privilege="[PERM.helpDocDelete]" theme="danger" @click="confirmDocRemove(row)">删除</t-link>
          </t-space>
        </template>
        <template #empty>
          <div class="empty-state">
            <t-icon class="empty-state__icon" name="book" />
            <p class="empty-state__title">还没有文档</p>
            <p class="empty-state__desc">新建草稿后点「发布」才会出现在帮助中心</p>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- 文档编辑（Markdown 编辑器 + 预览 + 发布） -->
    <t-dialog
      v-model:visible="editorVisible"
      :header="editingId ? '编辑文档' : '新建文档'"
      :confirm-btn="{ content: '保存', loading: saving }"
      width="1080px"
      :close-on-overlay-click="false"
      @confirm="handleSave"
    >
      <t-form ref="formRef" :data="form" :rules="formRules" label-width="86px" class="editor-form">
        <t-row :gutter="[16, 0]">
          <t-col :span="6">
            <t-form-item label="标题" name="title">
              <t-input v-model="form.title" placeholder="如：如何申请年假" :maxlength="200" />
            </t-form-item>
          </t-col>
          <t-col :span="3">
            <t-form-item label="分类" name="category_id">
              <t-select v-model="form.category_id" :options="categoryOptions" filterable placeholder="选择分类" />
            </t-form-item>
          </t-col>
          <t-col :span="3">
            <t-form-item label="排序号">
              <t-input-number v-model="form.sort_order" :min="0" :max="9999" theme="normal" />
            </t-form-item>
          </t-col>
          <t-col :span="6">
            <t-form-item label="slug" name="slug">
              <t-input v-model="form.slug" placeholder="小写字母数字与短横，如 apply-annual-leave">
                <template #suffix>
                  <t-link theme="primary" @click="generateSlug">生成</t-link>
                </template>
              </t-input>
            </t-form-item>
          </t-col>
          <t-col :span="3">
            <t-form-item label="摘要">
              <t-input v-model="form.summary" placeholder="列表页一句话" :maxlength="300" />
            </t-form-item>
          </t-col>
          <t-col :span="3">
            <t-form-item label="关键字">
              <t-input v-model="form.keywords" placeholder="逗号分隔" />
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item label="正文" name="content_md">
              <div class="editor-body">
                <t-tabs v-model="editorTab">
                  <t-tab-panel value="edit" label="Markdown 编辑">
                    <t-textarea
                      v-model="form.content_md"
                      class="editor-body__input"
                      placeholder="# 一级标题&#10;&#10;正文支持标题 / 列表 / 引用 / 代码块 / 链接"
                      :autosize="{ minRows: 16, maxRows: 24 }"
                    />
                  </t-tab-panel>
                  <t-tab-panel value="preview" label="预览（本地子集渲染）">
                    <div class="editor-body__preview">
                      <MarkdownViewer
                        :html="previewHtml"
                        :toc="previewToc"
                        :show-toc="true"
                        empty-text="左侧写点正文就有预览"
                        max-height="440px"
                      />
                    </div>
                  </t-tab-panel>
                </t-tabs>
                <div class="editor-body__bar">
                  <span class="editor-body__hint">
                    {{ form.content_md.length }} 字符 · 预览是本地子集渲染，正式渲染以后端为准
                  </span>
                </div>
              </div>
            </t-form-item>
          </t-col>
        </t-row>
      </t-form>
      <template #footer>
        <t-space>
          <t-button variant="outline" @click="editorVisible = false">取消</t-button>
          <t-button v-privilege="[PERM.helpDocPublish]" theme="primary" variant="outline" :loading="publishing" @click="handleSaveAndPublish">
            保存并发布
          </t-button>
          <t-button theme="primary" :loading="saving" @click="handleSave">保存</t-button>
        </t-space>
      </template>
    </t-dialog>

    <!-- 分类编辑 -->
    <t-dialog
      v-model:visible="categoryVisible"
      :header="categoryForm.id ? '编辑分类' : '新建分类'"
      :confirm-btn="{ content: '保存', loading: categorySaving }"
      width="480px"
      @confirm="handleCategorySave"
    >
      <t-form :data="categoryForm" label-width="80px">
        <t-form-item label="上级">
          <t-select v-model="categoryForm.parent_id" :options="parentOptions" clearable placeholder="不选则为根分类" />
        </t-form-item>
        <t-form-item label="名称" required-mark>
          <t-input v-model="categoryForm.name" placeholder="如：常见问题" :maxlength="50" />
        </t-form-item>
        <t-form-item label="编码" required-mark>
          <t-input v-model="categoryForm.code" placeholder="如：faq（小写字母数字与短横）" :maxlength="50" />
        </t-form-item>
        <t-form-item label="图标">
          <t-input v-model="categoryForm.icon" placeholder="如：help-circle" />
        </t-form-item>
        <t-form-item label="描述">
          <t-input v-model="categoryForm.description" :maxlength="200" />
        </t-form-item>
        <t-form-item label="排序号">
          <t-input-number v-model="categoryForm.sort_order" :min="0" :max="9999" theme="normal" />
        </t-form-item>
        <t-form-item label="状态">
          <t-switch v-model="categoryForm.status" :custom-value="[1, 0]" />
        </t-form-item>
      </t-form>
    </t-dialog>

    <!-- 版本 / 回滚 -->
    <t-drawer v-model:visible="versionsVisible" header="版本历史" size="600px" :footer="false">
      <div class="versions-tip">版本号只增不减：保存、发布、回滚都会生成新版本，历史一行不删。</div>
      <t-loading :loading="versionsLoading" size="small">
        <t-timeline v-if="versions.length > 0">
          <t-timeline-item
            v-for="version in versions"
            :key="version.version"
            :label="formatDateTime(version.created_at)"
          >
            <div class="version-item">
              <div class="version-item__head">
                <t-tag size="small" variant="light" :theme="version.version === currentVersion ? 'primary' : 'default'">
                  v{{ version.version }}
                </t-tag>
                <span class="version-item__title">{{ version.title }}</span>
              </div>
              <div class="version-item__meta">
                <span>{{ version.create_by || '—' }}</span>
                <span>{{ version.content_bytes }} 字节</span>
                <span v-if="version.change_note">说明：{{ version.change_note }}</span>
              </div>
              <t-link
                v-if="version.version !== currentVersion"
                v-privilege="[PERM.helpDocPublish]"
                theme="primary"
                @click="confirmRollback(version.version)"
              >
                回滚到此版本
              </t-link>
            </div>
          </t-timeline-item>
        </t-timeline>
        <div v-else class="tree-empty">还没有版本快照（发布一次就有了）</div>
      </t-loading>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
/**
 * 帮助文档管理（管理侧，入口码 `system:help:doc:list`）。
 *
 * # 覆盖的五件事
 *
 * 列表（分类 / 关键字 / 状态 + 含草稿开关）、增（草稿）、改（Markdown 编辑器 + 本地预览，
 * 保存已发布文档会自动退回草稿 —— 后端行为，页面必须提示）、删（软删）、
 * **权限隐藏**（分类维护四码 + 文档五码，逐按钮 `v-privilege`）。
 * 另有发布 / 版本列表 / 回滚（回滚=用旧内容生成新版本）。
 *
 * # 两个易错点（都写在代码里防呆）
 *
 * 1. `slug` 有正则约束 `^[a-z0-9][a-z0-9-]{1,119}$` —— 中文标题必须点「生成」；
 * 2. 分类树深度 ≤ 3 由后端递归 CTE 校验，页面把「加子分类」放在选中节点上，避免建出第 4 层。
 */
import { computed, onMounted, reactive, ref, watch } from 'vue';
import { MessagePlugin, DialogPlugin } from 'tdesign-vue-next';
import MarkdownViewer from '@/components/MarkdownViewer.vue';
import { ApiError } from '@/api/request';
import { PERM } from '@/constants/permissions';
import {
  createCategory,
  createDoc,
  deleteCategory,
  deleteDoc,
  getCategoryTree,
  getDoc,
  listDocVersions,
  pageDocs,
  publishDoc,
  rollbackDoc,
  updateCategory,
  updateDoc,
  type CategorySaveReq,
  type DocPageQuery,
  type DocSaveReq,
  type HelpCategoryNode,
  type HelpDocCard,
  type HelpVersion,
} from '@/api/business/help-api';
import { extractTocFromMarkdown, renderMarkdownLite } from '@/utils/markdown-lite';
import { formatDateTime } from '@/utils/format';

// ===== 分类 =====

/** t-tree 的节点结构（显式声明，避免 `unknown[]` 与 TreeOptionData 不兼容） */
interface CategoryTreeNode {
  /** 分类 id */
  value: string;
  /** 展示名（停用加后缀） */
  label: string;
  /** 子节点 */
  children?: CategoryTreeNode[];
}

const treeData = ref<CategoryTreeNode[]>([]);
const categoryTree = ref<HelpCategoryNode[]>([]);
const categoryLoading = ref(false);
const activeCategory = ref('');
const currentCategory = ref<HelpCategoryNode | null>(null);

/** 分类树 → t-tree 结构 */
function toTreeNodes(nodes: HelpCategoryNode[]): CategoryTreeNode[] {
  return nodes.map((node) => ({
    value: node.id,
    label: node.status === 1 ? node.name : `${node.name}（停用）`,
    children: node.children?.length ? toTreeNodes(node.children) : undefined,
  }));
}

/** 分类树 → 下拉选项（缩进表达层级） */
function toOptions(nodes: HelpCategoryNode[], depth = 0, out: { label: string; value: string }[] = []) {
  for (const node of nodes) {
    out.push({ label: `${'　'.repeat(depth)}${node.name}`, value: node.id });
    if (node.children?.length) {
      toOptions(node.children, depth + 1, out);
    }
  }
  return out;
}

const categoryOptions = computed(() => toOptions(categoryTree.value));
const parentOptions = computed(() => toOptions(categoryTree.value));

async function loadCategories(): Promise<void> {
  categoryLoading.value = true;
  try {
    const tree = await getCategoryTree({ include_empty: true, include_disabled: true });
    categoryTree.value = tree;
    treeData.value = toTreeNodes(tree);
  } catch (error) {
    showError(error);
  } finally {
    categoryLoading.value = false;
  }
}

function onCategoryActive(value: unknown): void {
  const ids = Array.isArray(value) ? value : [value];
  const id = typeof ids[0] === 'string' ? ids[0] : '';
  activeCategory.value = id;
  currentCategory.value = findNode(categoryTree.value, id);
  query.category_id = id;
  handleSearch();
}

function findNode(nodes: HelpCategoryNode[], id: string): HelpCategoryNode | null {
  for (const node of nodes) {
    if (node.id === id) {
      return node;
    }
    const found = findNode(node.children ?? [], id);
    if (found) {
      return found;
    }
  }
  return null;
}

const categoryVisible = ref(false);
const categorySaving = ref(false);
const categoryForm = reactive({
  id: '',
  parent_id: '' as string,
  name: '',
  code: '',
  icon: '',
  description: '',
  sort_order: 0,
  status: 1,
});

function openCategoryCreate(parentId?: string): void {
  categoryForm.id = '';
  categoryForm.parent_id = parentId ?? '';
  categoryForm.name = '';
  categoryForm.code = '';
  categoryForm.icon = '';
  categoryForm.description = '';
  categoryForm.sort_order = 0;
  categoryForm.status = 1;
  categoryVisible.value = true;
}

function openCategoryEdit(node: HelpCategoryNode): void {
  categoryForm.id = node.id;
  categoryForm.parent_id = node.parent_id ?? '';
  categoryForm.name = node.name;
  categoryForm.code = node.code;
  categoryForm.icon = node.icon;
  categoryForm.description = node.description;
  categoryForm.sort_order = node.sort_order;
  categoryForm.status = node.status;
  categoryVisible.value = true;
}

async function handleCategorySave(): Promise<void> {
  if (!categoryForm.name.trim() || !categoryForm.code.trim()) {
    void MessagePlugin.warning('名称与编码必填');
    return;
  }
  categorySaving.value = true;
  try {
    const payload: CategorySaveReq = {
      parent_id: categoryForm.parent_id || null,
      name: categoryForm.name.trim(),
      code: categoryForm.code.trim(),
      icon: categoryForm.icon || undefined,
      description: categoryForm.description || undefined,
      sort_order: categoryForm.sort_order,
      status: categoryForm.status,
    };
    if (categoryForm.id) {
      await updateCategory(categoryForm.id, payload);
      void MessagePlugin.success('分类已更新');
    } else {
      await createCategory(payload);
      void MessagePlugin.success('分类已创建');
    }
    categoryVisible.value = false;
    await loadCategories();
  } catch (error) {
    showError(error);
  } finally {
    categorySaving.value = false;
  }
}

async function confirmCategoryRemove(node: HelpCategoryNode): Promise<void> {
  const ok = await askConfirm('删除分类', `确认删除分类「${node.name}」？分类下有文档时后端会拒绝（先移走文档再删）。`);
  if (!ok) {
    return;
  }
  try {
    await deleteCategory(node.id);
    void MessagePlugin.success('分类已删除');
    if (activeCategory.value === node.id) {
      activeCategory.value = '';
      currentCategory.value = null;
      query.category_id = '';
    }
    await loadCategories();
    await loadDocs();
  } catch (error) {
    showError(error);
  }
}

// ===== 文档列表 =====

const rows = ref<HelpDocCard[]>([]);
const loading = ref(false);
const includeUnpublished = ref(true);
const query = reactive<DocPageQuery>({ keyword: '', status: '', category_id: '' });
const pagination = reactive({ current: 1, pageSize: 10, total: 0, showJumper: true });

const statusOptions = [
  { label: '草稿', value: 'DRAFT' },
  { label: '已发布', value: 'PUBLISHED' },
  { label: '已下架', value: 'OFFLINE' },
];

const columns = [
  { colKey: 'title', title: '标题 / slug', width: 260 },
  { colKey: 'category_name', title: '分类', width: 130 },
  { colKey: 'status', title: '状态', width: 90 },
  { colKey: 'version', title: '版本', width: 70 },
  { colKey: 'view_count', title: '阅读量', width: 80 },
  { colKey: 'updated_at', title: '更新时间', width: 170 },
  { colKey: 'op', title: '操作', width: 250, fixed: 'right' as const },
];

function statusLabel(status: string): string {
  return statusOptions.find((item) => item.value === status)?.label || status || '—';
}
function statusTheme(status: string): 'default' | 'success' | 'warning' {
  if (status === 'PUBLISHED') {
    return 'success';
  }
  if (status === 'OFFLINE') {
    return 'warning';
  }
  return 'default';
}

async function loadDocs(): Promise<void> {
  loading.value = true;
  try {
    const data = await pageDocs({
      ...query,
      page: pagination.current,
      page_size: pagination.pageSize,
      include_unpublished: includeUnpublished.value || undefined,
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
  void loadDocs();
}

function handleReset(): void {
  query.keyword = '';
  query.status = '';
  query.category_id = activeCategory.value;
  handleSearch();
}

function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  pagination.current = pageInfo.current;
  pagination.pageSize = pageInfo.pageSize;
  void loadDocs();
}

// ===== 文档编辑 =====

const editorVisible = ref(false);
const editorTab = ref<'edit' | 'preview'>('edit');
const editingId = ref('');
const saving = ref(false);
const publishing = ref(false);
const formRef = ref();

const form = reactive({
  category_id: '',
  title: '',
  slug: '',
  summary: '',
  keywords: '',
  sort_order: 0,
  content_md: '',
});

const formRules = {
  title: [{ required: true, message: '标题必填', type: 'error' as const }],
  category_id: [{ required: true, message: '分类必填', type: 'error' as const }],
  slug: [
    { required: true, message: 'slug 必填（可点「生成」）', type: 'error' as const },
    {
      pattern: /^[a-z0-9][a-z0-9-]{1,119}$/,
      message: 'slug 只能是小写字母 / 数字 / 短横，2–120 字符',
      type: 'error' as const,
    },
  ],
  content_md: [{ required: true, message: '正文必填', type: 'error' as const }],
};

/** 本地预览（子集渲染 + 白名单净化在 MarkdownViewer 内） */
const previewHtml = computed(() => renderMarkdownLite(form.content_md));
const previewToc = computed(() => extractTocFromMarkdown(form.content_md));

function generateSlug(): void {
  const base = form.title
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '');
  const suffix = new Date().toISOString().slice(2, 10).replace(/-/g, '');
  const candidate = base.length >= 2 ? base.slice(0, 60) : `doc-${Date.now().toString(36)}`;
  form.slug = `${candidate}-${suffix}`.replace(/^-+/, '');
}

function resetForm(): void {
  form.category_id = activeCategory.value || '';
  form.title = '';
  form.slug = '';
  form.summary = '';
  form.keywords = '';
  form.sort_order = 0;
  form.content_md = '';
  editorTab.value = 'edit';
}

function openDocCreate(): void {
  editingId.value = '';
  resetForm();
  editorVisible.value = true;
}

async function openDocEdit(row: HelpDocCard): Promise<void> {
  editingId.value = row.id;
  resetForm();
  editorVisible.value = true;
  try {
    // 列表行不带 content_md，编辑回显要拉详情
    const detail = await getDoc(row.id);
    form.category_id = detail.category_id;
    form.title = detail.title;
    form.slug = detail.slug;
    form.summary = detail.summary;
    form.keywords = detail.keywords;
    form.content_md = detail.content_md;
  } catch (error) {
    showError(error);
  }
}

/** 保存（`publishAfter=true` 时保存完立即发布） */
async function handleSave(): Promise<void> {
  await saveDoc(false);
}

async function handleSaveAndPublish(): Promise<void> {
  await saveDoc(true);
}

async function saveDoc(publishAfter: boolean): Promise<void> {
  const valid = await formRef.value?.validate();
  if (valid !== true) {
    return;
  }
  if (publishAfter) {
    publishing.value = true;
  } else {
    saving.value = true;
  }
  try {
    const payload: DocSaveReq = {
      category_id: form.category_id,
      title: form.title,
      slug: form.slug,
      summary: form.summary || undefined,
      content_md: form.content_md,
      keywords: form.keywords || undefined,
      sort_order: form.sort_order,
    };
    let docId = editingId.value;
    if (docId) {
      await updateDoc(docId, payload);
    } else {
      const created = await createDoc(payload);
      docId = created.id;
      editingId.value = docId;
    }
    if (publishAfter) {
      await publishDoc(docId, { change_note: '页面「保存并发布」' });
      void MessagePlugin.success('已保存并发布');
      editorVisible.value = false;
    } else {
      void MessagePlugin.success('已保存（草稿）。已发布文档编辑后会退回草稿，需重新发布');
    }
    await loadDocs();
  } catch (error) {
    showError(error);
  } finally {
    saving.value = false;
    publishing.value = false;
  }
}

async function confirmPublish(row: HelpDocCard): Promise<void> {
  try {
    await publishDoc(row.id, { change_note: `从列表发布（v${row.version}）` });
    void MessagePlugin.success('已发布');
    await loadDocs();
  } catch (error) {
    showError(error);
  }
}

async function confirmDocRemove(row: HelpDocCard): Promise<void> {
  const ok = await askConfirm('删除文档', `确认删除「${row.title}」？软删后帮助中心立即不可见（版本历史保留）。`);
  if (!ok) {
    return;
  }
  try {
    await deleteDoc(row.id);
    void MessagePlugin.success('已删除');
    await loadDocs();
  } catch (error) {
    showError(error);
  }
}

// ===== 版本 / 回滚 =====

const versionsVisible = ref(false);
const versionsLoading = ref(false);
const versions = ref<HelpVersion[]>([]);
const currentVersion = ref(0);

async function openVersions(row: HelpDocCard): Promise<void> {
  versionsVisible.value = true;
  versionsLoading.value = true;
  currentVersion.value = row.version;
  try {
    versions.value = await listDocVersions(row.id);
    editingId.value = row.id;
  } catch (error) {
    versions.value = [];
    showError(error);
  } finally {
    versionsLoading.value = false;
  }
}

async function confirmRollback(version: number): Promise<void> {
  try {
    const detail = await rollbackDoc(editingId.value, version, { change_note: `回滚到 v${version}` });
    currentVersion.value = detail.version;
    void MessagePlugin.success(`已回滚：内容取自 v${version}，生成了新版本 v${detail.version}`);
    versions.value = await listDocVersions(editingId.value);
    await loadDocs();
  } catch (error) {
    showError(error);
  }
}

function showError(error: unknown): void {
  void MessagePlugin.error(error instanceof ApiError ? error.message : '请求失败，请稍后重试');
}

/** 删除前二次确认（不可撤销动作一律弹确认，避免误点） */
async function askConfirm(header: string, body: string): Promise<boolean> {
  return new Promise<boolean>((resolve) => {
    const dialog = DialogPlugin.confirm({
      header,
      body,
      theme: 'danger',
      onConfirm: () => {
        dialog.destroy();
        resolve(true);
      },
      onClose: () => {
        dialog.destroy();
        resolve(false);
      },
    });
  });
}

// 新建文档时，若还没选分类，默认带上当前选中分类
watch(activeCategory, (value) => {
  if (!editingId.value && !form.category_id) {
    form.category_id = value;
  }
});

onMounted(() => {
  void loadCategories();
  void loadDocs();
});
</script>

<style scoped lang="less">
.help-manage-page {
  display: flex;
  gap: 12px;
  height: 100%;
  min-height: 0;
}

.tree-card {
  width: 268px;
  flex: 0 0 268px;
  overflow-y: auto;
}

.tree-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 10px;
  font-size: 13px;
  font-weight: 600;
  color: var(--td-text-color-secondary);
}

.tree-empty {
  padding: 20px 0;
  text-align: center;
  font-size: 13px;
  color: var(--td-text-color-placeholder);
}

.tree-actions {
  margin-top: 12px;
  padding-top: 10px;
  border-top: 1px solid var(--td-component-stroke);

  &__name {
    display: block;
    margin-bottom: 6px;
    font-size: 13px;
    font-weight: 600;
    color: var(--td-text-color-primary);
  }
}

.table-card {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  min-height: 0;
}

.filter-row {
  margin-bottom: 4px;
}

.toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin: 12px 0;

  &__count {
    font-size: 13px;
    color: var(--td-text-color-placeholder);
  }
}

.cell-slug {
  margin-top: 2px;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.editor-form {
  max-height: 68vh;
  overflow-y: auto;
  padding-right: 8px;
}

.editor-body {
  width: 100%;

  &__input {
    width: 100%;
    font-family: var(--app-font-mono, ui-monospace, SFMono-Regular, Menlo, Consolas, monospace);
    font-size: 13px;
    line-height: 1.7;
  }

  &__preview {
    min-height: 340px;
    padding: 4px 0;
  }

  &__bar {
    margin-top: 8px;
  }

  &__hint {
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }
}

.versions-tip {
  margin-bottom: 12px;
  padding: 8px 12px;
  border-radius: 6px;
  background: var(--app-accent-soft);
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.version-item {
  &__head {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 4px;
  }

  &__title {
    font-size: 14px;
    color: var(--td-text-color-primary);
  }

  &__meta {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
    margin-bottom: 6px;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }
}

.empty-state {
  padding: 60px 0;
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
    font-size: 13px;
    color: var(--td-text-color-secondary);
  }
}
</style>
