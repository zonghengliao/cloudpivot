<template>
  <div class="ann-manage-page">
    <!-- 筛选 -->
    <t-card :bordered="false" class="filter-card">
      <t-row :gutter="[16, 12]">
        <t-col :span="3">
          <t-input v-model="query.keyword" placeholder="标题 / 摘要 / 编号" clearable @enter="handleSearch" />
        </t-col>
        <t-col :span="2">
          <t-select v-model="query.status" placeholder="状态" clearable :options="statusOptions" />
        </t-col>
        <t-col :span="2">
          <t-select v-model="query.scope_type" placeholder="发布范围" clearable :options="scopeOptions" />
        </t-col>
        <t-col :span="2">
          <t-select v-model="query.level" placeholder="级别" clearable :options="levelOptions" />
        </t-col>
        <t-col :span="3">
          <t-date-range-picker v-model="publishRange" placeholder="发布时间范围" clearable />
        </t-col>
        <t-col :span="4">
          <t-space>
            <t-button theme="primary" @click="handleSearch">
              <template #icon><t-icon name="search" /></template>
              查询
            </t-button>
            <t-button variant="outline" @click="handleReset">重置</t-button>
          </t-space>
        </t-col>
      </t-row>
    </t-card>

    <!-- 列表 -->
    <t-card :bordered="false" class="table-card">
      <div class="toolbar">
        <t-space>
          <t-button v-privilege="[PERM.announcementCreate]" theme="primary" @click="openCreate">
            <template #icon><t-icon name="add" /></template>
            新建公告
          </t-button>
          <t-button variant="outline" :loading="loading" @click="loadList">
            <template #icon><t-icon name="refresh" /></template>
            刷新
          </t-button>
        </t-space>
        <span class="toolbar__count">共 {{ pagination.total }} 条</span>
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
          <div class="cell-title">
            <t-icon v-if="row.is_top" class="cell-title__pin" name="pin" />
            <t-link theme="primary" @click="openDetail(row)">{{ row.title }}</t-link>
          </div>
          <div class="cell-title__no">{{ row.ann_no }}</div>
        </template>
        <template #level="{ row }">
          <t-tag :theme="levelTheme(row.level)" size="small" variant="light">{{ levelLabel(row.level) }}</t-tag>
        </template>
        <template #scope_type="{ row }">
          <t-tag size="small" variant="outline" theme="default">{{ scopeLabel(row.scope_type) }}</t-tag>
        </template>
        <template #status="{ row }">
          <t-tag :theme="statusTheme(row.status)" size="small" variant="light">{{ statusLabel(row.status) }}</t-tag>
        </template>
        <template #read_rate="{ row }">
          <div class="cell-read">
            <span>{{ row.read_count }} / {{ row.receiver_count }}</span>
            <t-progress
              class="cell-read__bar"
              :percentage="Math.round((row.read_rate || 0) * 100)"
              :label="false"
              theme="line"
            />
          </div>
        </template>
        <template #publish_at="{ row }">{{ formatDateTime(row.publish_at) }}</template>
        <template #expire_at="{ row }">{{ formatDateTime(row.expire_at) }}</template>
        <template #op="{ row }">
          <t-space size="small" break-line>
            <t-link v-privilege="[PERM.announcementUpdate]" theme="primary" @click="openEdit(row)">编辑</t-link>
            <t-link v-privilege="[PERM.announcementPublish]" theme="primary" @click="confirmPublish(row)">发布</t-link>
            <t-link v-privilege="[PERM.announcementOffline]" theme="warning" @click="openOffline(row)">下线</t-link>
            <t-link v-privilege="[PERM.announcementUpdate]" theme="default" @click="openTop(row)">
              {{ row.is_top ? '取消置顶' : '置顶' }}
            </t-link>
            <t-link v-privilege="[PERM.announcementList]" theme="default" @click="openReaders(row)">阅读明细</t-link>
            <t-link v-privilege="[PERM.announcementDelete]" theme="danger" @click="confirmRemove(row)">删除</t-link>
          </t-space>
        </template>
        <template #empty>
          <div class="empty-state">
            <t-icon class="empty-state__icon" name="notification" />
            <p class="empty-state__title">还没有公告</p>
            <p class="empty-state__desc">新建草稿后点「发布」才会推送到用户的公告中心</p>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- 新建 / 编辑 -->
    <t-dialog
      v-model:visible="editorVisible"
      :header="editingId ? '编辑公告' : '新建公告'"
      :confirm-btn="{ content: '保存草稿', loading: saving }"
      width="1040px"
      :close-on-overlay-click="false"
      @confirm="handleSave"
    >
      <t-form ref="formRef" :data="form" :rules="formRules" label-width="92px" class="editor-form">
        <t-row :gutter="[16, 0]">
          <t-col :span="8">
            <t-form-item label="标题" name="title">
              <t-input v-model="form.title" placeholder="如：国庆节放假安排通知" :maxlength="200" />
            </t-form-item>
          </t-col>
          <t-col :span="4">
            <t-form-item label="级别" name="level">
              <t-select v-model="form.level" :options="levelOptions" />
            </t-form-item>
          </t-col>
          <t-col :span="8">
            <t-form-item label="摘要" name="summary">
              <t-input v-model="form.summary" placeholder="留空则由后端从正文自动截取" :maxlength="500" />
            </t-form-item>
          </t-col>
          <t-col :span="4">
            <t-form-item label="有效期至">
              <t-date-picker
                v-model="form.expire_at"
                enable-time-picker
                clearable
                format="YYYY-MM-DD HH:mm:ss"
                placeholder="不填则永不过期"
              />
            </t-form-item>
          </t-col>
          <t-col :span="4">
            <t-form-item label="发布范围">
              <t-select v-model="form.scope_type" :options="scopeOptions" @change="onScopeTypeChange" />
            </t-form-item>
          </t-col>
          <t-col :span="8">
            <t-form-item label="范围明细">
              <t-select
                v-if="form.scope_type === 'ROLE'"
                v-model="form.scope_ids"
                multiple
                filterable
                :options="roleOptions"
                placeholder="选择角色（可多选）"
                :loading="scopeLoading"
              />
              <t-select
                v-else-if="form.scope_type === 'DEPT'"
                v-model="form.scope_ids"
                multiple
                filterable
                :options="deptOptions"
                placeholder="选择部门（含下级，可多选）"
                :loading="scopeLoading"
              />
              <t-select
                v-else-if="form.scope_type === 'USER'"
                v-model="form.scope_ids"
                multiple
                filterable
                :options="userOptions"
                placeholder="搜索用户名 / 姓名后选择"
                :loading="scopeLoading"
                @search="searchUsers"
              />
              <span v-else class="form-hint">全员（发布后所有在职用户可见）</span>
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
                      placeholder="支持 Markdown：标题、列表、引用、代码块、链接"
                      :autosize="{ minRows: 14, maxRows: 22 }"
                    />
                  </t-tab-panel>
                  <t-tab-panel value="preview" label="预览（后端渲染）">
                    <div class="editor-body__preview">
                      <MarkdownViewer
                        :html="previewHtml"
                        :loading="previewLoading"
                        :show-toc="false"
                        empty-text="点「刷新预览」用后端渲染器生成"
                        max-height="420px"
                      />
                    </div>
                  </t-tab-panel>
                </t-tabs>
                <div class="editor-body__bar">
                  <span class="editor-body__hint">{{ form.content_md.length }} 字符</span>
                  <t-button size="small" variant="outline" :loading="previewLoading" @click="refreshPreview">
                    <template #icon><t-icon name="browse" /></template>
                    刷新预览
                  </t-button>
                </div>
                <div v-if="previewWarnings.length > 0" class="editor-body__warnings">
                  <t-icon name="error-circle" />
                  <span v-for="(warning, index) in previewWarnings" :key="index">{{ warning }}</span>
                </div>
              </div>
            </t-form-item>
          </t-col>
          <t-col :span="12">
            <t-form-item label="附件">
              <div class="attachment-editor">
                <FileUploader category="announcement" @uploaded="onAttachmentUploaded" @error="onAttachmentError" />
                <span class="form-hint">上传后随公告一起保存；单个文件由存储后端的策略限制</span>
                <div v-if="attachments.length > 0" class="attachment-editor__list">
                  <div v-for="file in attachments" :key="file.file_id" class="attachment-editor__item">
                    <t-icon name="file" />
                    <span class="attachment-editor__name">{{ file.file_name }}</span>
                    <span class="attachment-editor__size">{{ formatFileSize(file.file_size) }}</span>
                    <t-link theme="danger" @click="removeAttachment(file.file_id)">移除</t-link>
                  </div>
                </div>
              </div>
            </t-form-item>
          </t-col>
        </t-row>
      </t-form>
    </t-dialog>

    <!-- 详情（管理视角） -->
    <t-dialog v-model:visible="detailVisible" header="公告详情" width="880px" :footer="false">
      <t-loading :loading="detailLoading" size="small">
        <template v-if="detailRow">
          <div class="detail-meta">
            <div class="detail-meta__title">{{ detailRow.title }}</div>
            <t-space size="small" break-line>
              <t-tag size="small" variant="light">{{ detailRow.ann_no }}</t-tag>
              <t-tag size="small" variant="light" :theme="statusTheme(detailRow.status)">
                {{ statusLabel(detailRow.status) }}
              </t-tag>
              <t-tag size="small" variant="outline">{{ scopeLabel(detailRow.scope_type) }}</t-tag>
              <span>版本 v{{ detailRow.version }}</span>
              <span>发布人 {{ detailRow.publisher_name || '—' }}</span>
              <span>受众 {{ detailRow.receiver_count }} / 已读 {{ detailRow.read_count }}</span>
            </t-space>
          </div>
          <MarkdownViewer :html="detailRow.content_html" :show-toc="true" max-height="58vh" />
        </template>
      </t-loading>
    </t-dialog>

    <!-- 下线（原因必填，进变更日志 action_label） -->
    <t-dialog
      v-model:visible="offlineVisible"
      header="下线公告"
      :confirm-btn="{ content: '确认下线', theme: 'warning', loading: offlineSaving }"
      width="480px"
      @confirm="handleOffline"
    >
      <p class="dialog-tip">下线后所有用户不可见（已发出的站内信不撤回，用户点开会提示已下线）。</p>
      <t-textarea v-model="offlineReason" placeholder="下线原因（必填，会记入变更日志）" :maxlength="200" />
    </t-dialog>

    <!-- 置顶 -->
    <t-dialog
      v-model:visible="topVisible"
      :header="topForm.is_top ? '置顶公告' : '取消置顶'"
      :confirm-btn="{ content: '确定', loading: topSaving }"
      width="460px"
      @confirm="handleTop"
    >
      <t-form label-width="88px">
        <t-form-item label="置顶">
          <t-switch v-model="topForm.is_top" />
        </t-form-item>
        <t-form-item v-if="topForm.is_top" label="到期时间">
          <t-date-picker
            v-model="topForm.top_expire_at"
            enable-time-picker
            clearable
            format="YYYY-MM-DD HH:mm:ss"
            placeholder="置顶必须设置到期时间"
          />
        </t-form-item>
      </t-form>
    </t-dialog>

    <!-- 阅读明细 -->
    <t-drawer v-model:visible="readersVisible" header="阅读明细" size="720px" :footer="false">
      <div class="readers-head">
        <t-space size="small">
          <t-radio-group v-model="readersQuery.state" variant="default-filled" size="small" @change="loadReaders">
            <t-radio-button value="">全部</t-radio-button>
            <t-radio-button value="READ">已读</t-radio-button>
            <t-radio-button value="UNREAD">未读</t-radio-button>
          </t-radio-group>
          <t-input
            v-model="readersQuery.keyword"
            placeholder="用户名 / 姓名 / 部门"
            clearable
            size="small"
            @enter="loadReaders"
          />
        </t-space>
        <span class="readers-head__stat">
          已读 {{ readers.read_count }} / {{ readers.receiver_count }}（{{ Math.round((readers.read_rate || 0) * 100) }}%）
        </span>
      </div>
      <t-table
        row-key="user_id"
        :data="readers.list"
        :columns="readerColumns"
        :loading="readersLoading"
        :pagination="{ ...readersPagination, total: readers.total }"
        size="small"
        @page-change="onReadersPageChange"
      >
        <template #read="{ row }">
          <t-tag :theme="row.read ? 'success' : 'default'" size="small" variant="light">
            {{ row.read ? '已读' : '未读' }}
          </t-tag>
        </template>
        <template #read_at="{ row }">{{ formatDateTime(row.read_at) }}</template>
      </t-table>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
/**
 * 公告管理（管理侧，入口码 `business:announcement:list`）。
 *
 * # 覆盖的五件事
 *
 * 列表（关键词 / 状态 / 范围 / 级别 / 发布时间窗 + 分页）、增（草稿）、改（编辑回显 +
 * 后端预览）、删（软删）、**权限隐藏**（每个按钮 `v-privilege`，无权限不渲染）。
 * 另有发布 / 下线（原因必填）/ 置顶（到期时间必填）/ 阅读明细。
 *
 * # 三个后端约束（前端必须配合，否则报 400）
 *
 * 1. `is_top=true` 时 `top_expire_at` **必填**；
 * 2. `scope_type != ALL` 时 `scope_ids` 不能为空；
 * 3. 时间入参是 **RFC3339**（页面用 `dayjs().toISOString()` 转换，picker 只负责展示）。
 */
import { onMounted, reactive, ref } from 'vue';
import { MessagePlugin, DialogPlugin } from 'tdesign-vue-next';
import dayjs from 'dayjs';
import MarkdownViewer from '@/components/MarkdownViewer.vue';
import FileUploader from '@/components/FileUploader.vue';
import { ApiError } from '@/api/request';
import { PERM } from '@/constants/permissions';
import {
  createAnnouncement,
  deleteAnnouncement,
  getManageAnnouncement,
  offlineAnnouncement,
  pageAnnouncementReaders,
  pageManageAnnouncements,
  previewAnnouncement,
  publishAnnouncement,
  setAnnouncementTop,
  updateAnnouncement,
  type AnnouncementManageDetail,
  type AnnouncementSaveReq,
  type ManageCard,
  type ManagePageQuery,
  type ReadersResult,
} from '@/api/business/announcement-api';
import { listRoles } from '@/api/system/role-api';
import { getDeptTree, type DeptNode } from '@/api/system/dept-api';
import { pageUsers } from '@/api/system/user-api';
import type { FileRes } from '@/api/support/file-api';
import { formatDateTime, formatFileSize } from '@/utils/format';

/** 表格行 */
const rows = ref<ManageCard[]>([]);
const loading = ref(false);
const pagination = reactive({ current: 1, pageSize: 10, total: 0, showJumper: true });
const query = reactive<ManagePageQuery>({
  keyword: '',
  status: '',
  scope_type: '',
  level: '',
});
const publishRange = ref<string[]>([]);

/** 列定义（顺序即视觉顺序：标题 → 级别 → 范围 → 状态 → 已读 → 时间 → 操作） */
const columns = [
  { colKey: 'title', title: '标题 / 编号', width: 240 },
  { colKey: 'level', title: '级别', width: 84 },
  { colKey: 'scope_type', title: '范围', width: 84 },
  { colKey: 'status', title: '状态', width: 90 },
  { colKey: 'read_rate', title: '已读 / 受众', width: 140 },
  { colKey: 'publish_at', title: '发布时间', width: 160 },
  { colKey: 'expire_at', title: '有效期至', width: 160 },
  { colKey: 'op', title: '操作', width: 320, fixed: 'right' as const },
];

const statusOptions = [
  { label: '草稿', value: 'DRAFT' },
  { label: '已发布', value: 'PUBLISHED' },
  { label: '已下线', value: 'OFFLINE' },
  { label: '已过期', value: 'EXPIRED' },
];
const scopeOptions = [
  { label: '全员', value: 'ALL' },
  { label: '按角色', value: 'ROLE' },
  { label: '按部门', value: 'DEPT' },
  { label: '指定用户', value: 'USER' },
];
const levelOptions = [
  { label: '普通', value: 'NORMAL' },
  { label: '重要', value: 'IMPORTANT' },
  { label: '紧急', value: 'URGENT' },
];

function statusLabel(status: string): string {
  return statusOptions.find((item) => item.value === status)?.label || status || '—';
}
function statusTheme(status: string): 'default' | 'primary' | 'success' | 'warning' | 'danger' {
  const key = (status || '').toUpperCase();
  if (key === 'PUBLISHED') {
    return 'success';
  }
  if (key === 'OFFLINE') {
    return 'warning';
  }
  if (key === 'EXPIRED') {
    return 'default';
  }
  return 'primary';
}
function scopeLabel(scope: string): string {
  return scopeOptions.find((item) => item.value === scope)?.label || scope || '—';
}
function levelLabel(level: string): string {
  return levelOptions.find((item) => item.value === level)?.label || level || '普通';
}
function levelTheme(level: string): 'primary' | 'warning' | 'danger' {
  const key = (level || '').toUpperCase();
  if (key === 'URGENT') {
    return 'danger';
  }
  if (key === 'IMPORTANT') {
    return 'warning';
  }
  return 'primary';
}

/** 列表 */
async function loadList(): Promise<void> {
  loading.value = true;
  try {
    const [from, to] = publishRange.value ?? [];
    const data = await pageManageAnnouncements({
      ...query,
      page: pagination.current,
      page_size: pagination.pageSize,
      publish_from: from ? dayjs(from).startOf('day').toISOString() : undefined,
      publish_to: to ? dayjs(to).endOf('day').toISOString() : undefined,
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
  query.keyword = '';
  query.status = '';
  query.scope_type = '';
  query.level = '';
  publishRange.value = [];
  handleSearch();
}

function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  pagination.current = pageInfo.current;
  pagination.pageSize = pageInfo.pageSize;
  void loadList();
}

// ===== 表单（新建 / 编辑） =====

const editorVisible = ref(false);
const editorTab = ref<'edit' | 'preview'>('edit');
const editingId = ref('');
const saving = ref(false);
const formRef = ref();
const previewHtml = ref('');
const previewLoading = ref(false);
const previewWarnings = ref<string[]>([]);
const attachments = ref<{ file_id: string; file_name: string; file_size: number }[]>([]);

/** 表单模型（时间字段用 picker 的字符串形态，提交前转 RFC3339） */
const form = reactive({
  title: '',
  summary: '',
  content_md: '',
  level: 'NORMAL',
  scope_type: 'ALL',
  scope_ids: [] as string[],
  expire_at: '' as string,
});

const formRules = {
  title: [{ required: true, message: '标题必填', type: 'error' as const }],
  content_md: [{ required: true, message: '正文必填（不能发空公告）', type: 'error' as const }],
};

/** 范围选择器选项 */
const roleOptions = ref<{ label: string; value: string }[]>([]);
const deptOptions = ref<{ label: string; value: string }[]>([]);
const userOptions = ref<{ label: string; value: string }[]>([]);
const scopeLoading = ref(false);

/** 部门树拍平成选项（缩进表达层级，多选框比树选择器好用） */
function flattenDepts(nodes: DeptNode[], depth = 0, out: { label: string; value: string }[] = []) {
  for (const node of nodes) {
    out.push({ label: `${'　'.repeat(depth)}${node.dept_name}`, value: node.id });
    if (node.children?.length) {
      flattenDepts(node.children, depth + 1, out);
    }
  }
  return out;
}

/** 切范围类型时清空已选（不同范围的 id 空间不一样，留着会串） */
function onScopeTypeChange(): void {
  form.scope_ids = [];
  void ensureScopeOptions();
}

/** 按需加载范围选项（角色 / 部门一次拉全，用户按关键字搜） */
async function ensureScopeOptions(): Promise<void> {
  scopeLoading.value = true;
  try {
    if (form.scope_type === 'ROLE' && roleOptions.value.length === 0) {
      const roles = await listRoles();
      roleOptions.value = roles.map((role) => ({ label: `${role.role_name}（${role.role_code}）`, value: role.id }));
    }
    if (form.scope_type === 'DEPT' && deptOptions.value.length === 0) {
      const tree = await getDeptTree();
      deptOptions.value = flattenDepts(tree);
    }
  } catch (error) {
    showError(error);
  } finally {
    scopeLoading.value = false;
  }
}

/** 用户搜索（后端分页接口，前端按需合并选项） */
async function searchUsers(keyword: string): Promise<void> {
  if (!keyword) {
    return;
  }
  scopeLoading.value = true;
  try {
    const data = await pageUsers({ page: 1, page_size: 20, username: keyword });
    const merged = new Map(userOptions.value.map((item) => [item.value, item]));
    for (const user of data.list) {
      merged.set(user.id, { label: `${user.real_name || user.username}（${user.username}）`, value: user.id });
    }
    userOptions.value = Array.from(merged.values());
  } catch (error) {
    showError(error);
  } finally {
    scopeLoading.value = false;
  }
}

function resetForm(): void {
  form.title = '';
  form.summary = '';
  form.content_md = '';
  form.level = 'NORMAL';
  form.scope_type = 'ALL';
  form.scope_ids = [];
  form.expire_at = '';
  attachments.value = [];
  previewHtml.value = '';
  previewWarnings.value = [];
  editorTab.value = 'edit';
}

function openCreate(): void {
  editingId.value = '';
  resetForm();
  editorVisible.value = true;
}

/** 编辑回显（时间 ISO → picker 字符串；null → 空串） */
async function openEdit(row: ManageCard): Promise<void> {
  editingId.value = row.id;
  resetForm();
  editorVisible.value = true;
  try {
    const detail = await getManageAnnouncement(row.id);
    form.title = detail.title;
    form.summary = detail.summary;
    form.content_md = detail.content_md;
    form.level = detail.level || 'NORMAL';
    form.scope_type = detail.scope_type || 'ALL';
    form.scope_ids = [...(detail.scope_ids ?? [])];
    form.expire_at = detail.expire_at ? dayjs(detail.expire_at).format('YYYY-MM-DD HH:mm:ss') : '';
    attachments.value = (detail.attachments ?? []).map((file) => ({
      file_id: file.file_id,
      file_name: file.file_name,
      file_size: file.file_size,
    }));
    previewHtml.value = detail.content_html;
    await ensureScopeOptions();
    if (form.scope_type === 'USER') {
      // 指定用户的回显：先把已选 id 塞进选项，避免只显示裸 id
      const known = new Set(userOptions.value.map((item) => item.value));
      form.scope_ids.forEach((id) => {
        if (!known.has(id)) {
          userOptions.value.push({ label: id, value: id });
        }
      });
    }
  } catch (error) {
    showError(error);
  }
}

/** 后端权威预览（不用前端自造渲染器，避免与正式渲染不一致） */
async function refreshPreview(): Promise<void> {
  if (!form.content_md.trim()) {
    void MessagePlugin.warning('先写点正文再预览');
    return;
  }
  previewLoading.value = true;
  try {
    const res = await previewAnnouncement(form.content_md);
    previewHtml.value = res.content_html;
    previewWarnings.value = res.warnings ?? [];
    editorTab.value = 'preview';
  } catch (error) {
    showError(error);
  } finally {
    previewLoading.value = false;
  }
}

/** 保存（新建 = 草稿；编辑 = 覆盖但需重新发布才对外生效） */
async function handleSave(): Promise<void> {
  const result = await formRef.value?.validate();
  if (result !== true) {
    return;
  }
  if (form.scope_type !== 'ALL' && form.scope_ids.length === 0) {
    void MessagePlugin.warning(`发布范围为「${scopeLabel(form.scope_type)}」时必须选择具体对象`);
    return;
  }
  saving.value = true;
  try {
    const payload: AnnouncementSaveReq = {
      title: form.title,
      summary: form.summary || undefined,
      content_md: form.content_md,
      level: form.level,
      scope_type: form.scope_type,
      scope_ids: form.scope_type === 'ALL' ? [] : form.scope_ids,
      expire_at: form.expire_at ? dayjs(form.expire_at).toISOString() : null,
      attachments: attachments.value.map((file, index) => ({ file_id: file.file_id, sort_order: index })),
    };
    if (editingId.value) {
      await updateAnnouncement(editingId.value, payload);
      void MessagePlugin.success('已保存（发布后才对外生效）');
    } else {
      await createAnnouncement(payload);
      void MessagePlugin.success('草稿已创建，别忘了点「发布」');
    }
    editorVisible.value = false;
    await loadList();
  } catch (error) {
    showError(error);
  } finally {
    saving.value = false;
  }
}

function onAttachmentUploaded(file: FileRes): void {
  attachments.value.push({ file_id: file.id, file_name: file.original_name || file.file_name, file_size: file.file_size });
  void MessagePlugin.success('附件已上传，保存公告后生效');
}

function onAttachmentError(message: string): void {
  void MessagePlugin.error(message);
}

function removeAttachment(fileId: string): void {
  attachments.value = attachments.value.filter((file) => file.file_id !== fileId);
}

// ===== 详情 =====

const detailVisible = ref(false);
const detailLoading = ref(false);
const detailRow = ref<AnnouncementManageDetail | null>(null);

async function openDetail(row: ManageCard): Promise<void> {
  detailVisible.value = true;
  detailLoading.value = true;
  try {
    detailRow.value = await getManageAnnouncement(row.id);
  } catch (error) {
    detailRow.value = null;
    showError(error);
  } finally {
    detailLoading.value = false;
  }
}

// ===== 发布 / 下线 / 置顶 / 删除 =====

async function confirmPublish(row: ManageCard): Promise<void> {
  const confirm = await new Promise<boolean>((resolve) => {
    // TDesign 的确认框：包一层 Promise 便于 await（发布是不可逆的对外动作）
    const dialog = DialogPlugin.confirm({
      header: '发布公告',
      body: `发布后「${row.title}」将推送给范围内 ${row.receiver_count || '?'} 位用户（含站内信），确认发布？`,
      theme: 'warning',
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
  if (!confirm) {
    return;
  }
  try {
    const res = await publishAnnouncement(row.id);
    void MessagePlugin.success(`已发布：受众 ${res.receiver_count} 人，扇出 ${res.fanout}${res.warning ? `（${res.warning}）` : ''}`);
    await loadList();
  } catch (error) {
    showError(error);
  }
}

const offlineVisible = ref(false);
const offlineReason = ref('');
const offlineSaving = ref(false);
const offlineTarget = ref<ManageCard | null>(null);

function openOffline(row: ManageCard): void {
  offlineTarget.value = row;
  offlineReason.value = '';
  offlineVisible.value = true;
}

async function handleOffline(): Promise<void> {
  if (!offlineTarget.value) {
    return;
  }
  if (!offlineReason.value.trim()) {
    void MessagePlugin.warning('下线原因必填');
    return;
  }
  offlineSaving.value = true;
  try {
    await offlineAnnouncement(offlineTarget.value.id, offlineReason.value.trim());
    void MessagePlugin.success('已下线');
    offlineVisible.value = false;
    await loadList();
  } catch (error) {
    showError(error);
  } finally {
    offlineSaving.value = false;
  }
}

const topVisible = ref(false);
const topSaving = ref(false);
const topTarget = ref<ManageCard | null>(null);
const topForm = reactive({ is_top: true, top_expire_at: '' });

function openTop(row: ManageCard): void {
  topTarget.value = row;
  topForm.is_top = !row.is_top;
  topForm.top_expire_at = row.top_expire_at ? dayjs(row.top_expire_at).format('YYYY-MM-DD HH:mm:ss') : '';
  topVisible.value = true;
}

async function handleTop(): Promise<void> {
  if (!topTarget.value) {
    return;
  }
  if (topForm.is_top && !topForm.top_expire_at) {
    void MessagePlugin.warning('置顶必须设置到期时间（后端约束）');
    return;
  }
  topSaving.value = true;
  try {
    await setAnnouncementTop(
      topTarget.value.id,
      topForm.is_top,
      topForm.is_top && topForm.top_expire_at ? dayjs(topForm.top_expire_at).toISOString() : null,
    );
    void MessagePlugin.success(topForm.is_top ? '已置顶' : '已取消置顶');
    topVisible.value = false;
    await loadList();
  } catch (error) {
    showError(error);
  } finally {
    topSaving.value = false;
  }
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

async function confirmRemove(row: ManageCard): Promise<void> {
  const ok = await askConfirm('删除公告', `确认删除「${row.title}」？软删除后用户侧立即不可见（变更日志会留下凭证）。`);
  if (!ok) {
    return;
  }
  try {
    await deleteAnnouncement(row.id);
    void MessagePlugin.success('已删除');
    await loadList();
  } catch (error) {
    showError(error);
  }
}

// ===== 阅读明细 =====

const readersVisible = ref(false);
const readersLoading = ref(false);
const readersQuery = reactive({ state: '', keyword: '' });
const readersPagination = reactive({ current: 1, pageSize: 10 });
const readers = reactive<ReadersResult>({
  announcement_id: '',
  receiver_count: 0,
  read_count: 0,
  read_rate: 0,
  total: 0,
  page: 1,
  page_size: 10,
  list: [],
});
const readerColumns = [
  { colKey: 'username', title: '用户名', width: 120 },
  { colKey: 'real_name', title: '姓名', width: 100 },
  { colKey: 'dept_name', title: '部门', width: 140 },
  { colKey: 'read', title: '状态', width: 80 },
  { colKey: 'read_at', title: '阅读时间', width: 160 },
];
const readersTargetId = ref('');

async function openReaders(row: ManageCard): Promise<void> {
  readersTargetId.value = row.id;
  readersQuery.state = '';
  readersQuery.keyword = '';
  readersPagination.current = 1;
  readersVisible.value = true;
  await loadReaders();
}

async function loadReaders(): Promise<void> {
  if (!readersTargetId.value) {
    return;
  }
  readersLoading.value = true;
  try {
    const data = await pageAnnouncementReaders(readersTargetId.value, {
      page: readersPagination.current,
      page_size: readersPagination.pageSize,
      state: readersQuery.state || undefined,
      keyword: readersQuery.keyword || undefined,
    });
    Object.assign(readers, data);
  } catch (error) {
    showError(error);
  } finally {
    readersLoading.value = false;
  }
}

function onReadersPageChange(pageInfo: { current: number; pageSize: number }): void {
  readersPagination.current = pageInfo.current;
  readersPagination.pageSize = pageInfo.pageSize;
  void loadReaders();
}

function showError(error: unknown): void {
  void MessagePlugin.error(error instanceof ApiError ? error.message : '请求失败，请稍后重试');
}

onMounted(() => {
  void loadList();
});
</script>

<style scoped lang="less">
.ann-manage-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 12px;

  &__count {
    font-size: 13px;
    color: var(--td-text-color-placeholder);
  }
}

.cell-title {
  display: flex;
  align-items: center;
  gap: 4px;

  &__pin {
    color: var(--td-warning-color);
  }

  &__no {
    margin-top: 2px;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }
}

.cell-read {
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: 12px;
  color: var(--td-text-color-secondary);

  &__bar {
    width: 100%;
  }
}

.editor-form {
  max-height: 66vh;
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
    min-height: 320px;
    padding: 4px 0;
  }

  &__bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-top: 8px;
  }

  &__hint {
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__warnings {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-top: 8px;
    padding: 6px 10px;
    border-radius: 6px;
    background: var(--td-warning-color-1);
    color: var(--td-warning-color-8);
    font-size: 12px;
  }
}

.attachment-editor {
  width: 100%;

  &__list {
    margin-top: 10px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  &__item {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
    border-radius: 6px;
    background: var(--td-bg-color-container-hover);
    font-size: 13px;
    color: var(--td-text-color-primary);
  }

  &__name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  &__size {
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }
}

.form-hint {
  margin-left: 8px;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.dialog-tip {
  margin: 0 0 10px;
  font-size: 13px;
  color: var(--td-text-color-secondary);
}

.detail-meta {
  margin-bottom: 12px;
  padding-bottom: 12px;
  border-bottom: 1px solid var(--td-component-stroke);

  &__title {
    margin-bottom: 8px;
    font-size: 17px;
    font-weight: 600;
    color: var(--td-text-color-primary);
  }
}

.readers-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 12px;
  flex-wrap: wrap;

  &__stat {
    font-size: 13px;
    color: var(--td-text-color-secondary);
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
