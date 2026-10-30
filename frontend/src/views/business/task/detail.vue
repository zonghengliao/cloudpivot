<template>
  <t-drawer
    v-model:visible="innerVisible"
    :header="drawerHeader"
    size="680px"
    :footer="false"
    class="task-detail"
    @closed="handleClosed"
  >
    <t-loading :loading="loading" size="small">
      <!-- 错误态：不白屏，给重试 -->
      <t-alert v-if="errorMessage" theme="error" :message="errorMessage" class="task-detail__alert">
        <template #operation>
          <t-button size="small" variant="outline" @click="loadDetail">重试</t-button>
        </template>
      </t-alert>

      <template v-else-if="detail">
        <!-- 头部：编号 + 标题 + 状态（同一行）；信息区压成单行紧凑；操作按钮 -->
        <div class="head">
          <div class="head__title-row">
            <span class="head__no">{{ detail.task_no }}</span>
            <span class="head__title">{{ detail.title }}</span>
            <StatusBadge domain="task" :status="detail.status" />
          </div>
          <!-- E6a：从子任务行点进来的，给一条回父任务的路（不关抽屉，避免丢列表滚动位置） -->
          <div v-if="detail.parent_task_no" class="head__parent">
            <t-link theme="primary" @click="openParent">← 返回父任务 {{ detail.parent_task_no }}</t-link>
          </div>
          <!-- E9：信息区从「多行 11 项」压成一行（优先级 · 负责人 · 截止 · 进度 · 工时 · 标记），
               完整字段（创建人 / 部门 / 项目 / 时间…）挪到「详情」Tab，一眼能扫完 -->
          <div class="head__meta">
            <PriorityTag :priority="detail.priority" />
            <span class="head__chip">
              <t-icon name="user" class="head__chip-icon" />
              {{ detail.owner_name || '未指派' }}
            </span>
            <span class="head__chip" :class="{ 'head__chip--danger': isOverdue }">
              <t-icon name="calendar" class="head__chip-icon" />
              {{ detail.due_date || '无期限' }}
            </span>
            <span class="head__chip head__chip--progress">
              <t-progress :percentage="detail.progress" theme="line" :label="false" class="head__bar" />
              <span class="head__pct">{{ detail.progress }}%</span>
            </span>
            <span class="head__chip">
              预估 {{ formatHours(detail.estimate_hours) }}
              <template v-if="detail.actual_hours !== null">· 实际 {{ formatHours(detail.actual_hours) }}</template>
            </span>
            <t-tag v-if="detail.need_review" size="small" variant="outline" theme="default">需验收</t-tag>
            <t-tag v-if="isOverdue" size="small" variant="light" theme="danger">已逾期</t-tag>
            <t-tag v-if="detail.source !== 'SELF'" size="small" variant="outline" theme="default">
              {{ sourceLabel(detail.source) }}
            </t-tag>
          </div>
          <div class="head__actions">
            <!-- 状态流转按钮（E5）：按「当前状态 + 我的角色位 + business:task:status」动态渲染；
                 没有可做的流转 → 整块不出现（不再是「画出来置灰」） -->
            <StatusActions :task="detail" @changed="handleStatusChanged" />
            <t-space size="small" class="head__ops">
              <t-button
                v-privilege="[PERM.taskUpdate]"
                size="small"
                theme="primary"
                variant="outline"
                @click="emit('edit', detail.id)"
              >
                编辑
              </t-button>
              <t-button v-privilege="[PERM.taskDelete]" size="small" theme="danger" variant="outline" @click="confirmRemove">
                删除
              </t-button>
            </t-space>
          </div>
        </div>

        <!-- ==================== 主 Tab（E9：5 区块 → 4 主 Tab，降低信息密度） ==================== -->
        <t-tabs v-model="mainTab" class="main-tabs">
          <!-- ---------- 详情：描述（默认折叠）+ 基本信息 + 协作人 ---------- -->
          <t-tab-panel value="detail" label="详情">
            <div class="block">
              <div class="block__title block__title--with-action">
                <span>描述</span>
                <t-button
                  v-if="descOverflow"
                  size="small"
                  variant="text"
                  theme="primary"
                  @click="descExpanded = !descExpanded"
                >
                  {{ descExpanded ? '收起' : '展开' }}
                </t-button>
              </div>
              <pre
                v-if="detail.description"
                class="desc"
                :class="{ 'desc--collapsed': !descExpanded && descOverflow }"
              >{{ detail.description }}</pre>
              <p v-else class="placeholder">暂无描述</p>
            </div>

        <!-- 信息区 -->
        <div class="block">
          <div class="block__title">基本信息</div>
          <dl class="info">
            <div class="info__item">
              <dt>负责人</dt>
              <dd>{{ detail.owner_name || '—' }}</dd>
            </div>
            <div class="info__item">
              <dt>创建人</dt>
              <dd>{{ detail.creator_name || '—' }}</dd>
            </div>
            <div class="info__item">
              <dt>分派人</dt>
              <dd>{{ detail.assigner_name || '—' }}</dd>
            </div>
            <div class="info__item">
              <dt>部门</dt>
              <dd>{{ detail.dept_name || '—' }}</dd>
            </div>
            <div class="info__item">
              <dt>所属项目</dt>
              <dd>
                <t-link v-if="detail.project_id" theme="primary" @click="openProject">
                  {{ detail.project_name || detail.project_code || '未命名项目' }}
                </t-link>
                <span v-else>—</span>
              </dd>
            </div>
            <div class="info__item">
              <dt>计划开始</dt>
              <dd>{{ detail.start_date || '—' }}</dd>
            </div>
            <div class="info__item">
              <dt>截止日期</dt>
              <dd :class="{ 'info__value--overdue': isOverdue }">{{ detail.due_date || '无期限' }}</dd>
            </div>
            <div class="info__item">
              <dt>创建时间</dt>
              <dd>{{ formatDateTime(detail.created_at) }}</dd>
            </div>
            <div class="info__item">
              <dt>最近更新</dt>
              <dd>{{ formatDateTime(detail.updated_at) }}</dd>
            </div>
            <div v-if="detail.done_at" class="info__item">
              <dt>完成时间</dt>
              <dd>{{ formatDateTime(detail.done_at) }}</dd>
            </div>
            <div v-if="detail.parent_task_no" class="info__item">
              <dt>父任务</dt>
              <dd>{{ detail.parent_task_no }}</dd>
            </div>
          </dl>
          <t-alert v-if="detail.cancel_reason" theme="warning" :message="`取消原因：${detail.cancel_reason}`" />
          <t-alert v-if="detail.reject_reason" theme="error" :message="`打回原因：${detail.reject_reason}`" />
        </div>


            <!-- 协作人（T1 详情已下发 members；为空时隐藏） -->
            <div v-if="detail.members.length > 0" class="block">
              <div class="block__title">
                协作人
                <span class="block__count">{{ detail.members.length }}</span>
              </div>
              <t-space size="small" break-line>
                <t-tag v-for="member in detail.members" :key="member.user_id" size="small" variant="outline">
                  {{ member.user_name }}
                </t-tag>
              </t-space>
            </div>
          </t-tab-panel>

          <!-- ---------- 子任务（E6a：**常显**，0 个子任务时也能看到入口） ---------- -->
          <t-tab-panel value="subtask" :label="`子任务 ${detail.child_count}`">
        <div class="block">
          <div class="block__title block__title--with-action">
            <span>
              子任务
              <span class="block__count">{{ detail.child_count }}</span>
              <span v-if="detail.child_count > 0" class="block__sub">
                已完成 {{ detail.child_done_count }} / {{ detail.child_count }}
              </span>
            </span>
            <!-- 权限只认后端注解里的 business:task:subtask（与 business:task:create 是两个码）；
                 父任务终态时后端会 409，所以这里先置灰避免白点一次 -->
            <t-button
              v-privilege="[PERM.taskSubtask]"
              size="small"
              theme="primary"
              variant="outline"
              :disabled="isTerminal || isSubtask"
              @click="openSubtaskDialog"
            >
              + 新建子任务
            </t-button>
          </div>
          <div v-if="detail.children.length > 0" class="subtask">
            <div
              v-for="child in detail.children"
              :key="child.id"
              class="subtask__item subtask__item--clickable"
              role="link"
              tabindex="0"
              @click="openChild(child.id)"
              @keydown.enter="openChild(child.id)"
            >
              <StatusBadge domain="task" :status="child.status" :show-dot="false" />
              <span class="subtask__title">{{ child.title }}</span>
              <span class="subtask__meta">{{ child.owner_name }}</span>
              <span class="subtask__meta">{{ child.progress }}%</span>
            </div>
          </div>
          <p v-else class="placeholder">暂无子任务</p>
        </div>

          </t-tab-panel>

          <!-- ---------- 协作：原「评论 / 附件 / 工时」三 Tab 内嵌成二级分段控件 ----------
               （Tab 套 Tab 太深，用 t-radio-group 分段最轻；三段内容用 v-show 常驻，
                 切段不重拉、不丢输入框草稿） -->
          <t-tab-panel value="collab" :label="`协作 ${collabCount}`">
          <t-radio-group
            v-model="collabTab"
            class="collab__seg"
            size="small"
            variant="default-filled"
            @change="handleTabChange"
          >
            <t-radio-button value="comment">评论 {{ comments.total }}</t-radio-button>
            <t-radio-button value="attachment">附件 {{ attachments.total }}</t-radio-button>
            <t-radio-button value="worklog">工时 {{ worklogs.total }}</t-radio-button>
          </t-radio-group>

          <!-- ---------- 评论 ---------- -->
          <div v-show="collabTab === 'comment'" class="collab__pane">
              <!-- 发表框：@ 提及 = 强制通知（被选中的每个人都会收到） -->
              <div class="composer" @keydown="onComposerKeydown">
                <t-textarea
                  v-model="composer.content"
                  placeholder="写下评论…（支持 Markdown；Ctrl / ⌘ + Enter 快捷发送）"
                  :autosize="{ minRows: 3, maxRows: 6 }"
                  :maxlength="COMMENT_MAX_CHARS"
                />
                <div class="composer__row">
                  <MentionSelect v-model="composer.mentions" class="composer__mention" />
                  <span class="composer__count">
                    {{ composer.content.trim().length }} / {{ COMMENT_MAX_CHARS }}
                  </span>
                </div>
                <div class="composer__ops">
                  <span class="composer__tip">Ctrl / ⌘ + Enter 发送</span>
                  <t-button
                    size="small"
                    theme="primary"
                    :loading="composer.submitting"
                    :disabled="!composer.content.trim()"
                    @click="submitComment"
                  >
                    发送
                  </t-button>
                </div>
              </div>

              <div v-if="comments.list.length > 0" class="timeline">
                <div
                  v-for="item in comments.list"
                  :key="item.id"
                  class="timeline__item"
                  :class="{
                    'timeline__item--system': item.readonly,
                    'timeline__item--ai': item.comment_type === 'AI_REPORT',
                  }"
                >
                  <t-avatar size="small" class="timeline__avatar">{{ avatarText(item.user_name) }}</t-avatar>
                  <div class="timeline__body">
                    <div class="timeline__head">
                      <span class="timeline__author">{{ item.user_name }}</span>
                      <t-tag
                        v-if="item.comment_type !== 'PLAIN'"
                        size="small"
                        variant="light"
                        :theme="commentTheme(item.comment_type)"
                      >
                        {{ commentLabel(item.comment_type) }}
                      </t-tag>
                      <span class="timeline__time">{{ formatDateTime(item.created_at) }}</span>
                      <t-button
                        v-if="item.can_delete"
                        size="small"
                        variant="text"
                        theme="danger"
                        class="timeline__del"
                        @click="removeComment(item)"
                      >
                        删除
                      </t-button>
                    </div>
                    <MarkdownViewer
                      :html="renderMarkdownLite(item.content)"
                      :show-toc="false"
                      class="timeline__content"
                    />
                    <div v-if="item.mentions.length > 0" class="timeline__mentions">
                      <t-icon name="usergroup" />
                      提及 {{ item.mentions.length }} 人（已发强制通知）
                    </div>
                  </div>
                </div>
              </div>
              <p v-else class="placeholder">还没有评论，说点什么吧</p>

              <div v-if="comments.list.length < comments.total" class="more">
                <t-button size="small" variant="text" :loading="comments.loading" @click="loadComments(comments.page + 1)">
                  加载更多（{{ comments.list.length }}/{{ comments.total }}）
                </t-button>
              </div>
          </div>

          <!-- ---------- 附件 ---------- -->
          <div v-show="collabTab === 'attachment'" class="collab__pane">
              <div
                class="attach__drop"
                :class="{ 'attach__drop--over': dragOver }"
                @dragover.prevent="dragOver = true"
                @dragleave.prevent="dragOver = false"
                @drop.prevent="onDrop"
                @click="pickFile"
              >
                <t-icon name="upload" size="20px" />
                <span>拖拽文件到此处，或点击选择（单个按策略上限；最多 {{ attachments.max_count }} 个）</span>
                <input ref="fileInputRef" type="file" class="attach__input" @change="onFilePicked" />
              </div>
              <div class="attach__count">已上传 {{ attachments.total }} / {{ attachments.max_count }}</div>

              <div v-if="attachments.list.length > 0" class="attach__list">
                <div v-for="file in attachments.list" :key="file.id" class="attach__item">
                  <t-icon :name="fileIcon(file.file_name)" />
                  <span class="attach__name" :title="file.file_name">{{ file.file_name }}</span>
                  <t-tag size="small" variant="light" :theme="file.source === 'AI' ? 'warning' : 'default'">
                    {{ file.source === 'AI' ? `AI${file.ai_profile ? `·${file.ai_profile}` : ''}` : '人工' }}
                  </t-tag>
                  <span class="attach__meta">{{ formatFileSize(file.file_size) }}</span>
                  <span class="attach__meta">{{ file.uploader_name || '—' }}</span>
                  <t-button size="small" variant="text" @click="downloadAttachmentItem(file)">下载</t-button>
                  <t-button v-if="file.can_delete" size="small" variant="text" theme="danger" @click="removeAttachment(file)">
                    删除
                  </t-button>
                </div>
              </div>
              <p v-else class="placeholder">暂无附件</p>
          </div>

          <!-- ---------- 工时 ---------- -->
          <div v-show="collabTab === 'worklog'" class="collab__pane">
              <div class="worklog__summary">
                <span>明细合计 <strong>{{ worklogs.sum_hours }}</strong> 小时</span>
                <span>任务实际工时 <strong>{{ worklogs.actual_hours === null ? '—' : worklogs.actual_hours }}</strong></span>
                <t-button size="small" theme="primary" variant="outline" :loading="worklogs.loading" @click="openWorklogDialog">
                  登记工时
                </t-button>
              </div>

              <div v-if="worklogs.list.length > 0" class="worklog__list">
                <div v-for="row in worklogs.list" :key="row.id" class="worklog__item">
                  <span class="worklog__date">{{ row.work_date }}</span>
                  <span class="worklog__user">{{ row.user_name }}</span>
                  <span class="worklog__hours">{{ row.hours }} h</span>
                  <span class="worklog__note" :title="row.description || ''">{{ row.description || '—' }}</span>
                  <t-button v-if="row.can_delete" size="small" variant="text" theme="danger" @click="removeWorklog(row)">
                    删除
                  </t-button>
                </div>
              </div>
              <p v-else class="placeholder">还没有工时记录（工时只由服务端汇总，前端不填实际工时）</p>
          </div>
          </t-tab-panel>

          <!-- ---------- AI 产出（E13：AI 直接写在 workspace 里的原始文件，只读） ----------
               仅 execMode=AI 出现；`destroy-on-hide=false` 是为了「一进抽屉徽标就有数字」——
               TDesign 的面板默认在隐藏时销毁，那样徽标要等用户点开这个 Tab 才拿得到总量 -->
          <t-tab-panel v-if="isAiTask" value="ai" :label="aiTabLabel" :destroy-on-hide="false">
            <AiWorkspaceFiles :task-id="activeTaskId" @loaded="onAiLoaded" />
          </t-tab-panel>
        </t-tabs>
      </template>
    </t-loading>

    <!-- 工时登记弹窗 -->
    <t-dialog
      v-model:visible="worklogDialog.visible"
      header="登记工时"
      :confirm-btn="{ content: '登记', loading: worklogDialog.submitting }"
      @confirm="submitWorklog"
    >
      <t-form label-align="top">
        <t-form-item label="工时（小时，0.25–24）">
          <t-input-number v-model="worklogDialog.hours" :min="0.25" :max="24" :step="0.5" :decimal-places="1" />
        </t-form-item>
        <t-form-item label="日期（不得晚于今天）">
          <t-date-picker v-model="worklogDialog.workDate" allow-input clearable :disable-date="disableFutureDate" />
        </t-form-item>
        <t-form-item label="说明（可空）">
          <t-input v-model="worklogDialog.note" :maxlength="500" placeholder="做了什么" />
        </t-form-item>
      </t-form>
    </t-dialog>

    <!-- 新建子任务弹窗（E6a：复用 POST /business/task/{id}/subtask，不新增接口） -->
    <t-dialog
      v-model:visible="subtaskDialog.visible"
      header="新建子任务"
      width="520px"
      :confirm-btn="{ content: '创建', loading: subtaskDialog.submitting }"
      @confirm="submitSubtask"
    >
      <t-form label-align="top">
        <t-form-item label="标题（必填）">
          <t-input v-model="subtaskDialog.title" :maxlength="200" placeholder="拆成一个能独立完成的小任务" />
        </t-form-item>
        <t-form-item label="预估工时（小时，可空）">
          <t-input-number
            v-model="subtaskEstimateProxy"
            theme="normal"
            :min="0"
            :max="9999"
            :step="0.5"
            :decimal-places="1"
            placeholder="不填 = 未估"
          />
        </t-form-item>
        <t-form-item label="负责人（可空，留空 = 我自己）">
          <UserSelect v-model="subtaskDialog.ownerId" />
        </t-form-item>
        <t-form-item label="优先级（默认继承父任务）">
          <t-select v-model="subtaskDialog.priority" :options="priorityOpts" />
        </t-form-item>
        <t-form-item label="截止日期（可空，留空 = 继承父任务）">
          <t-date-picker
            v-model="subtaskDialog.dueDate"
            clearable
            format="YYYY-MM-DD"
            value-type="YYYY-MM-DD"
            placeholder="留空继承父任务"
          />
        </t-form-item>
      </t-form>
    </t-dialog>
  </t-drawer>
</template>

<script setup lang="ts">
/**
 * 任务详情（抽屉形态：列表页点标题 / 「查看」进来）。
 *
 * # 为什么是抽屉而不是独立页
 *
 * 任务详情的信息量不大（<1 屏），抽屉能保留列表的筛选与滚动位置，
 * 关掉抽屉不用重新查询 —— 运营场景里「看一眼就走」远多于「深读」。
 *
# 三个「有则显示、无则隐藏」的区块
 *
 * 子任务（`children`）/ 协作人（`members`）在 T1 的 `TaskDetailRes` 里**已经有**，
 * 所以直接渲染；空数组时整块不出现，不留白框。
 *
 * # T6b：协作区（评论 / 附件 / 工时）
 *
 * 三块都走**独立接口**（`GET /{id}/comments`、`/attachments`、`/worklogs`），
 * 不再依赖详情里的 `attachments` 只读快照 —— 评论要分页与删除、附件要下载与
 * 删除、工时是详情里完全没有的数据面。打开抽屉时三份并发拉，切 tab 不再等待。
 *
 * 只读（系统生成）的评论由后端下发 `readonly` 标记，前端只负责淡化样式 +
 * 不渲染删除按钮：**权限判定只有后端一份**，前端不按 `comment_type` 自己推导。
 */
import { computed, reactive, ref, watch } from 'vue';
import { useRouter } from 'vue-router';
import { MessagePlugin, DialogPlugin } from 'tdesign-vue-next';
import dayjs from 'dayjs';
import StatusBadge from './components/StatusBadge.vue';
import PriorityTag from './components/PriorityTag.vue';
import MentionSelect from './components/MentionSelect.vue';
import StatusActions from './components/StatusActions.vue';
import UserSelect from './components/UserSelect.vue';
import MarkdownViewer from '@/components/MarkdownViewer.vue';
import AiWorkspaceFiles from '@/views/business/task/components/AiWorkspaceFiles.vue';
import { priorityOptions } from '@/constants/status-map';
import { ApiError } from '@/api/request';
import {
  addTaskWorklog,
  createSubtask,
  deleteTask,
  deleteTaskAttachment,
  deleteTaskComment,
  deleteTaskWorklog,
  downloadTaskAttachment,
  getTask,
  listTaskAttachments,
  listTaskComments,
  listTaskWorklogs,
  postTaskComment,
  uploadTaskAttachment,
  type TaskAttachmentItem,
  type TaskCommentCreateResult,
  type TaskCommentItem,
  type TaskCommentType,
  type TaskDetail,
  type TaskWorklogItem,
} from '@/api/business/task-api';
import { PERM } from '@/constants/permissions';
import { formatDateTime, formatFileSize } from '@/utils/format';
import { renderMarkdownLite } from '@/utils/markdown-lite';

const props = defineProps<{
  /** 是否可见（v-model） */
  visible: boolean;
  /** 要展示的任务 id */
  taskId?: string | null;
}>();

/** 路由（点「所属项目」跳项目页用） */
const router = useRouter();

const emit = defineEmits<{
  /** 可见性变化 */
  (event: 'update:visible', value: boolean): void;
  /** 点「编辑」 */
  (event: 'edit', id: string): void;
  /** 删除成功 */
  (event: 'deleted', id: string): void;
  /** 状态流转成功（列表页据此刷新当前视图：列表 / 看板） */
  (event: 'changed', id: string): void;
}>();

/** 详情数据 */
const detail = ref<TaskDetail | null>(null);
/** 加载中 */
const loading = ref(false);
/** 错误文案（非空即渲染错误态 + 重试） */
const errorMessage = ref('');

/**
 * 描述展开态（E9：默认折叠，超过 5 行才出现「展开」）。
 *
 * 任务描述动辄几十行，展开着会把「详情」Tab 顶到看不见基本信息 ——
 * 默认只露 5 行，要读全文点一下。
 */
const descExpanded = ref(false);

/**
 * 描述估算行数。
 *
 * `pre` 保留显式换行；没有换行的长段落会按容器宽度折行，这里按「每行 ~42 字」
 * （13px 字号 / 680px 抽屉内宽的经验值）折算，只用来判断要不要显示「展开」。
 */
const descLineCount = computed(() => {
  const text = detail.value?.description ?? '';
  if (!text) {
    return 0;
  }
  return text.split('\n').reduce((sum, line) => sum + Math.max(1, Math.ceil(line.length / 42)), 0);
});

/** 超过 5 行 → 折叠 + 给「展开」 */
const descOverflow = computed(() => descLineCount.value > 5);

/** 抽屉可见性代理 */
const innerVisible = computed({
  get: () => props.visible,
  set: (value: boolean) => emit('update:visible', value),
});

/** 抽屉标题：有数据用任务号，否则用通用文案 */
const drawerHeader = computed(() => (detail.value ? `任务详情 · ${detail.value.task_no}` : '任务详情'));

/** 是否逾期：有截止日、早于今天、且未完成/未取消 */
const isOverdue = computed(() => {
  const current = detail.value;
  if (!current?.due_date) {
    return false;
  }
  if (current.status === 'DONE' || current.status === 'CANCELLED') {
    return false;
  }
  return dayjs(current.due_date).startOf('day').isBefore(dayjs().startOf('day'));
});

/** 是否终态（DONE / CANCELLED）：终态任务不能再加子任务（后端 409 + 63006/63012） */
/** 是否 AI 任务（只有它们才有 Hermes workspace，见 E13） */
const isAiTask = computed(() => detail.value?.exec_mode === 'AI');

const isTerminal = computed(() => detail.value?.status === 'DONE' || detail.value?.status === 'CANCELLED');

/** 当前看的是不是子任务（子任务不能再往下拆：后端 `validate_parent` 400） */
const isSubtask = computed(() => Boolean(detail.value?.parent_id));

/** 优先级下拉（P0 最高 → P3 最低，文案来自 status-map，与新建表单同源） */
const priorityOpts = priorityOptions();

/** 新建子任务弹窗（E6a） */
const subtaskDialog = reactive({
  visible: false,
  title: '',
  estimateHours: null as number | null,
  ownerId: null as string | null,
  priority: 'P2',
  /** 截止日：空串 = 不填（`t-date-picker` 的 modelValue 不吃 `null`，见 TaskForm 同款写法） */
  dueDate: '',
  submitting: false,
});

/**
 * 预估工时的双向代理。
 *
 * `t-input-number` 的 modelValue 不接受 `null`，而「不填」用 `null` 表达 ——
 * 这里做一次 `null ↔ undefined`（清空后回落到 `null`，提交时才是「不传这个字段」）。
 */
const subtaskEstimateProxy = computed<number | undefined>({
  get: () => subtaskDialog.estimateHours ?? undefined,
  set: (value) => {
    subtaskDialog.estimateHours = typeof value === 'number' && !Number.isNaN(value) ? value : null;
  },
});

/** 来源文案 */
function sourceLabel(source: string): string {
  const map: Record<string, string> = { SELF: '自建', ASSIGNED: '被分派', TRANSFERRED: '转派' };
  return map[source] ?? source;
}

/**
 * 点「所属项目」→ 跳项目页并直接打开该项目详情（T7c）。
 *
 * 项目页支持 `?id=` 自动展开详情抽屉，所以这里只 push 一个 query。
 * 后端对「我在该项目下有任务」的用户放开了项目壳的可见性，这条链接不会 404。
 */
function openProject(): void {
  if (!detail.value?.project_id) {
    return;
  }
  void router.push({ path: '/project', query: { id: detail.value.project_id } });
}

/** 工时展示：null → 未估 */
function formatHours(hours: number | null): string {
  return hours === null || hours === undefined ? '未估' : `${hours} 小时`;
}

/**
 * 当前正在看哪个任务（E6a 前就是 `props.taskId`）。
 *
 * 点详情里的子任务行要「就地换一个人看」而不关闭抽屉，所以当前 id 必须可写；
 * 外部 `taskId` 变了（列表页点了另一行）就跟着变，点「返回父任务」只改本地。
 */
const currentId = ref<string | null>(props.taskId ?? null);

/**
 * 当前实际展示的任务 id（E9）。
 *
 * 协作三份（评论 / 附件 / 工时）和 AI 产出都按「正在看的那个任务」拉 ——
 * 早先这里写的是 `props.taskId`，点进子任务后评论还停在父任务上（数字对不上）。
 */
const activeTaskId = computed(() => currentId.value ?? props.taskId ?? '');

/** 拉详情（用 `currentId`，不再直接用 prop：子任务行点击后 prop 没变） */
async function loadDetail(): Promise<void> {
  if (!currentId.value) {
    return;
  }
  loading.value = true;
  errorMessage.value = '';
  try {
    detail.value = await getTask(currentId.value);
    // E9：换任务（含点进子任务）时描述重新折叠、AI 徽标清零重算；
    // 若当前停在「AI 产出」而新任务不是 AI 任务，回到「详情」（否则会停在一个不存在的 Tab）
    descExpanded.value = false;
    aiCount.value = null;
    if (mainTab.value === 'ai' && !isAiTask.value) {
      mainTab.value = 'detail';
    }
    // 协作区三份并发拉（详情接口不带评论 / 工时；附件快照也不含下载与来源明细）
    void loadComments(1);
    void loadAttachments();
    void loadWorklogs();
  } catch (error) {
    detail.value = null;
    errorMessage.value = error instanceof ApiError ? error.message : '任务详情加载失败，请重试';
  } finally {
    loading.value = false;
  }
}

/** 打开子任务详情（复用同一个抽屉，不关闭再打开 —— 关掉会丢掉列表页滚动位置） */
function openChild(childId: string): void {
  if (!childId || childId === currentId.value) {
    return;
  }
  currentId.value = childId;
  mainTab.value = 'detail';
  collabTab.value = 'comment';
  void loadDetail();
}

/** 从子任务回退到父任务（父 header 上的「← 返回父任务」） */
function openParent(): void {
  const parentId = detail.value?.parent_id;
  if (!parentId) {
    return;
  }
  currentId.value = parentId;
  mainTab.value = 'detail';
  collabTab.value = 'comment';
  void loadDetail();
}

/** 打开「新建子任务」弹窗：优先级默认继承当前任务（后端缺省也是继承父任务） */
function openSubtaskDialog(): void {
  const current = detail.value;
  if (!current) {
    return;
  }
  subtaskDialog.title = '';
  subtaskDialog.estimateHours = null;
  subtaskDialog.ownerId = null;
  subtaskDialog.priority = current.priority || 'P2';
  subtaskDialog.dueDate = '';
  subtaskDialog.submitting = false;
  subtaskDialog.visible = true;
}

/** 提交新建子任务：成功 → 关弹窗 + 重拉详情（父任务进度由后端重算） + 通知列表页刷新 */
async function submitSubtask(): Promise<void> {
  const parentId = currentId.value;
  if (!parentId) {
    return;
  }
  const title = subtaskDialog.title.trim();
  if (!title) {
    MessagePlugin.warning('请填写子任务标题');
    return;
  }
  subtaskDialog.submitting = true;
  try {
    await createSubtask(parentId, {
      title,
      // 可空字段「没填就不传」：后端优先级 = 继承父任务、截止日 = 继承父任务、负责人 = 我自己
      estimate_hours: subtaskDialog.estimateHours,
      owner_id: subtaskDialog.ownerId,
      priority: subtaskDialog.priority || undefined,
      due_date: subtaskDialog.dueDate || null,
    });
    subtaskDialog.visible = false;
    MessagePlugin.success('子任务已创建');
    await loadDetail();
    emit('changed', parentId);
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '新建子任务失败，请重试');
  } finally {
    subtaskDialog.submitting = false;
  }
}

/**
 * 状态流转成功（`StatusActions` 的回执）。
 *
 * **只重拉详情，不在本地改 `detail`**：后端会联动 `progress` / `done_at` /
 * `reject_reason` / 父任务进度，本地拼一份「看起来对」的数据迟早跟库不一致。
 * 同时向上 emit `changed`，让列表页刷新当前视图（列表分页 / 看板五列）。
 */
function handleStatusChanged(): void {
  void loadDetail();
  if (currentId.value) {
    emit('changed', currentId.value);
  }
}

/**
 * 打开抽屉时加载（同一任务重复打开也重新拉，保证看到最新进度）。
 *
 * E6a：`currentId` 的同步**必须放在这一个 watcher 里**（不再单独 watch `props.taskId`）——
 * 拆成两个 watcher 时执行顺序靠 uid，实测会出现「先 loadDetail、后同步 currentId」，
 * 结果是抽屉拉起后一直是空的（`currentId` 还是 null，直接 early return，连请求都没发）。
 */
watch(
  () => [props.visible, props.taskId] as const,
  ([visible, taskId]) => {
    if (!visible) {
      return;
    }
    currentId.value = taskId ?? null;
    void loadDetail();
  },
  { immediate: true },
);

/** 删除确认 */
function confirmRemove(): void {
  const current = detail.value;
  if (!current) {
    return;
  }
  const dialog = DialogPlugin.confirm({
    header: '删除任务',
    body: `确认删除「${current.title}」？删除后列表与详情都不再可见（软删除，可由运维恢复）。`,
    theme: 'warning',
    confirmBtn: { content: '删除', theme: 'danger' },
    onConfirm: async () => {
      try {
        await deleteTask(current.id);
        MessagePlugin.success('任务已删除');
        dialog.destroy();
        innerVisible.value = false;
        emit('deleted', current.id);
      } catch (error) {
        MessagePlugin.error(error instanceof ApiError ? error.message : '删除失败，请稍后重试');
      }
    },
  });
}

/** 关闭后清掉旧数据，避免下次打开先闪上一个任务 */
function handleClosed(): void {
  detail.value = null;
  errorMessage.value = '';
  resetCollab();
  // E6a：抽屉里可能已经顺着子任务行「走进去了」，关闭时回到父任务，
  // 否则下次对同一 taskId 重新打开会继续停在子任务（prop 没变，watch 不触发）。
  currentId.value = props.taskId ?? null;
  subtaskDialog.visible = false;
}

// ======================================================================
// T6b：协作区（评论 / 附件 / 工时）
// ======================================================================
//
// 三块**都走独立接口**（不再依赖详情里的 `attachments` 只读快照）：
// 评论要分页 + 删除、附件要下载 + 删除、工时是详情里根本没有的新数据面。
// 打开抽屉时并发拉三份，避免切 tab 时才转圈。

/**
 * 主 Tab（E9：详情 / 子任务 / 协作 / AI 产出）。
 *
 * 默认停在「详情」—— 有子任务、没子任务都一样：进来先看任务本身，
 * 子任务数量在 Tab 徽标上已经能看到，不需要自动跳过去。
 */
const mainTab = ref<'detail' | 'subtask' | 'collab' | 'ai'>('detail');

/** 协作 Tab 内的二级分段（原三 Tab 内嵌，避免 Tab 套 Tab 太深） */
const collabTab = ref<'comment' | 'attachment' | 'worklog'>('comment');

/**
 * 「协作 N」徽标数量 = 评论 + 附件 + 工时 三类合计。
 *
 * 三个数字各自在同名的分段按钮上；主 Tab 只给一个总数（点进去再细看）。
 */
const collabCount = computed(() => comments.total + attachments.total + worklogs.total);

/**
 * 「AI 产出 N」徽标数量。
 *
 * 由 `AiWorkspaceFiles` 的 `loaded` 事件回抛（和列表同源，父级不重复请求）；
 * `null` = 还没拿到 → 徽标不带数字，避免先显示一个假的 0。
 */
const aiCount = ref<number | null>(null);

const aiTabLabel = computed(() => (aiCount.value === null ? 'AI 产出' : `AI 产出 ${aiCount.value}`));

/** AI 产出列表回抛总数（子组件 `loaded` 事件） */
function onAiLoaded(total: number): void {
  aiCount.value = total;
}

/** 评论列表状态（分页累加：`加载更多` 只追加） */
const comments = reactive({
  list: [] as TaskCommentItem[],
  total: 0,
  page: 1,
  pageSize: 20,
  loading: false,
});

/** 人类评论正文上限（与后端 `MAX_USER_COMMENT_CHARS` 同口径：超长服务端 400） */
const COMMENT_MAX_CHARS = 5000;

/** 评论发表框 */
const composer = reactive({
  content: '',
  mentions: [] as string[],
  submitting: false,
});

/** 附件状态 */
const attachments = reactive({
  list: [] as TaskAttachmentItem[],
  total: 0,
  max_count: 50,
  loading: false,
});

/** 拖拽悬停态（视觉反馈，不代表任何业务状态） */
const dragOver = ref(false);
/** 隐藏的 file input（点整块拖拽区时触发） */
const fileInputRef = ref<HTMLInputElement | null>(null);

/** 工时状态（`actual_hours` 由服务端汇总下发，前端只展示） */
const worklogs = reactive({
  list: [] as TaskWorklogItem[],
  total: 0,
  sum_hours: 0,
  actual_hours: null as number | null,
  max_records: 500,
  loading: false,
});

/** 工时登记弹窗 */
const worklogDialog = reactive({
  visible: false,
  hours: 2,
  workDate: dayjs().format('YYYY-MM-DD'),
  note: '',
  submitting: false,
});

/** 头像文字（姓名首字；空名回落 `?`） */
function avatarText(name: string): string {
  return (name || '?').trim().slice(0, 1).toUpperCase();
}

/** 评论类型中文标签（`PLAIN` 不显示标签） */
function commentLabel(type: TaskCommentType): string {
  const map: Record<TaskCommentType, string> = {
    PLAIN: '评论',
    AI_REPORT: 'AI 执行报告',
    STATUS_CHANGE: '状态变更',
    ASSIGN: '分派',
    PROGRESS_AUTO: '进度汇总',
    AI_PROGRESS: 'AI 进度',
  };
  return map[type] ?? type;
}

/** 评论类型底色（AI 报告用 warning，系统类型用 default，人写的无标签） */
function commentTheme(type: TaskCommentType): 'default' | 'primary' | 'warning' | 'success' | 'danger' {
  if (type === 'AI_REPORT') {
    return 'warning';
  }
  if (type === 'STATUS_CHANGE' || type === 'ASSIGN') {
    return 'primary';
  }
  return 'default';
}

/** 按扩展名给图标（图标名必须是 TDesign 图标集里存在的，避免渲染空） */
function fileIcon(name: string): string {
  const ext = (name.split('.').pop() || '').toLowerCase();
  if (['png', 'jpg', 'jpeg', 'gif', 'webp', 'svg'].includes(ext)) {
    return 'image';
  }
  if (['zip', 'rar', '7z', 'tar', 'gz'].includes(ext)) {
    return 'file-zip';
  }
  if (['md', 'txt', 'log', 'csv', 'json'].includes(ext)) {
    return 'file-1';
  }
  return 'file';
}

/**
 * 工时日期不得晚于今天（与服务端校验同口径，前端只是第一道提示）。
 *
 * 入参类型必须能接 TDesign 的 `DateValue`（`string | number | Date`），
 * 声明成 `Date` 会被 `vue-tsc` 判成「函数参数类型不兼容」。
 */
function disableFutureDate(value: unknown): boolean {
  const target = dayjs(value as dayjs.ConfigType);
  return target.isValid() && target.endOf('day').isAfter(dayjs().endOf('day'));
}

/** 拉评论（`page > 1` 表示加载更多 → 追加） */
async function loadComments(page = 1): Promise<void> {
  if (!activeTaskId.value) {
    return;
  }
  comments.loading = true;
  try {
    const result = await listTaskComments(activeTaskId.value, { page, page_size: comments.pageSize });
    comments.list = page > 1 ? [...comments.list, ...result.list] : result.list;
    comments.total = result.total;
    comments.page = result.page;
    comments.pageSize = result.page_size;
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '评论加载失败');
  } finally {
    comments.loading = false;
  }
}

/**
 * Ctrl / ⌘ + Enter 快捷发送。
 *
 * 监听挂在 **composer 容器**上而不是 `t-textarea` 上：TDesign 的 textarea 根节点
 * 是包裹层，`@keydown` 直接写在组件上不保证落到真正的 `<textarea>`；原生 keydown
 * 会冒泡到容器，这里命中率 100%。`preventDefault` 是为了不让换行插进正文。
 */
function onComposerKeydown(event: KeyboardEvent): void {
  if (event.key !== 'Enter' || !(event.ctrlKey || event.metaKey)) {
    return;
  }
  event.preventDefault();
  void submitComment();
}

/**
 * 把发表回执拼成本地列表项（**不重拉整页**：顺序与服务端一致即可）。
 *
 * - 时间线是正序 → 新评论追加到已加载列表末尾（即最新的位置）；
 * - `can_delete` = true（人写的评论作者本人可删，与后端 `comment_delete_verdict` 同口径）；
 * - `readonly` = false（后端只允许 `PLAIN`，`PLAIN` 不是只读类型）。
 */
function toCommentItem(created: TaskCommentCreateResult, content: string): TaskCommentItem {
  return {
    id: created.id,
    comment_type: 'PLAIN',
    user_id: created.user_id,
    user_name: created.user_name,
    content,
    mentions: created.mentions ?? [],
    parent_comment_id: null,
    created_at: created.created_at,
    readonly: false,
    can_delete: true,
  };
}

/**
 * 发表评论（`POST /api/business/task/{id}/comments`）。
 *
 * 红线：**失败不清空输入** —— 只有服务端确认落库后才重置，否则用户白写。
 */
async function submitComment(): Promise<void> {
  const taskId = activeTaskId.value;
  const content = composer.content.trim();
  if (!taskId || !content || composer.submitting) {
    return;
  }
  composer.submitting = true;
  try {
    const created = await postTaskComment(taskId, { content, mentions: composer.mentions });
    // 成功才清空；随后把新评论按时间位置插进列表 + total + 1。
    composer.content = '';
    composer.mentions = [];
    comments.list = [...comments.list, toCommentItem(created, content)];
    comments.total += 1;
    MessagePlugin.success('评论已发表');
    if (created.mentions_ignored && created.mentions_ignored.length > 0) {
      MessagePlugin.warning(`${created.mentions_ignored.length} 个被 @ 的账号不存在，已忽略`);
    }
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '评论发表失败');
  } finally {
    composer.submitting = false;
  }
}

/** 删除评论（二次确认；系统评论没有删除按钮，服务端也会 403） */
function removeComment(item: TaskCommentItem): void {
  const taskId = activeTaskId.value;
  if (!taskId) {
    return;
  }
  const dialog = DialogPlugin.confirm({
    header: '删除评论',
    body: '确认删除这条评论？删除后列表不再显示（软删，可审计）。',
    theme: 'warning',
    confirmBtn: { content: '删除', theme: 'danger' },
    onConfirm: async () => {
      try {
        await deleteTaskComment(taskId, item.id);
        dialog.destroy();
        await loadComments(1);
        MessagePlugin.success('评论已删除');
      } catch (error) {
        MessagePlugin.error(error instanceof ApiError ? error.message : '删除失败');
      }
    },
  });
}

/** 拉附件 */
async function loadAttachments(): Promise<void> {
  if (!activeTaskId.value) {
    return;
  }
  attachments.loading = true;
  try {
    const result = await listTaskAttachments(activeTaskId.value);
    attachments.list = result.list;
    attachments.total = result.total;
    attachments.max_count = result.max_count;
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '附件加载失败');
  } finally {
    attachments.loading = false;
  }
}

/** 点击拖拽区 → 唤起系统文件选择 */
function pickFile(): void {
  fileInputRef.value?.click();
}

/** 选择文件后上传（清空 value 以便同一个文件能再次选择） */
function onFilePicked(event: Event): void {
  const input = event.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = '';
  if (file) {
    void uploadOne(file);
  }
}

/** 拖拽放下 */
function onDrop(event: DragEvent): void {
  dragOver.value = false;
  const file = event.dataTransfer?.files?.[0];
  if (file) {
    void uploadOne(file);
  }
}

/** 上传单个文件（策略校验 / 50 个上限都在后端） */
async function uploadOne(file: File): Promise<void> {
  const taskId = activeTaskId.value;
  if (!taskId) {
    return;
  }
  if (attachments.total >= attachments.max_count) {
    MessagePlugin.warning(`单任务附件最多 ${attachments.max_count} 个，请先删除不再需要的`);
    return;
  }
  attachments.loading = true;
  try {
    await uploadTaskAttachment(taskId, file);
    await loadAttachments();
    MessagePlugin.success(`已上传 ${file.name}`);
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '上传失败');
  } finally {
    attachments.loading = false;
  }
}

/** 下载附件（带 Bearer 取 Blob 再触发保存；裸链接会 401） */
async function downloadAttachmentItem(file: TaskAttachmentItem): Promise<void> {
  const taskId = activeTaskId.value;
  if (!taskId) {
    return;
  }
  try {
    const blob = await downloadTaskAttachment(taskId, file.id);
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement('a');
    anchor.href = url;
    anchor.download = file.file_name || 'attachment';
    document.body.appendChild(anchor);
    anchor.click();
    document.body.removeChild(anchor);
    URL.revokeObjectURL(url);
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '下载失败');
  }
}

/** 删除附件 */
function removeAttachment(file: TaskAttachmentItem): void {
  const taskId = activeTaskId.value;
  if (!taskId) {
    return;
  }
  const dialog = DialogPlugin.confirm({
    header: '删除附件',
    body: `确认删除「${file.file_name}」？删除后列表不再显示，文件记录标记删除（物理对象延迟回收）。`,
    theme: 'warning',
    confirmBtn: { content: '删除', theme: 'danger' },
    onConfirm: async () => {
      try {
        await deleteTaskAttachment(taskId, file.id);
        dialog.destroy();
        await loadAttachments();
        MessagePlugin.success('附件已删除');
      } catch (error) {
        MessagePlugin.error(error instanceof ApiError ? error.message : '删除失败');
      }
    },
  });
}

/** 拉工时 */
async function loadWorklogs(): Promise<void> {
  if (!activeTaskId.value) {
    return;
  }
  worklogs.loading = true;
  try {
    const result = await listTaskWorklogs(activeTaskId.value);
    worklogs.list = result.list;
    worklogs.total = result.total;
    worklogs.sum_hours = result.sum_hours;
    worklogs.actual_hours = result.actual_hours;
    worklogs.max_records = result.max_records;
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '工时加载失败');
  } finally {
    worklogs.loading = false;
  }
}

/** 打开登记弹窗（默认今天 + 2 小时） */
function openWorklogDialog(): void {
  worklogDialog.hours = 2;
  worklogDialog.workDate = dayjs().format('YYYY-MM-DD');
  worklogDialog.note = '';
  worklogDialog.visible = true;
}

/** 提交工时（`actual_hours` 只由服务端汇总，回执里带回来直接展示） */
async function submitWorklog(): Promise<void> {
  const taskId = activeTaskId.value;
  const hours = worklogDialog.hours;
  // 日期在 `clearable` 清空后运行时可能是 null（类型声明是 string），按空串处理
  const rawDate: unknown = worklogDialog.workDate;
  const workDate = typeof rawDate === 'string' ? rawDate.trim() : '';
  if (!taskId || typeof hours !== 'number' || !workDate) {
    MessagePlugin.warning('请填写工时与日期');
    return;
  }
  worklogDialog.submitting = true;
  try {
    await addTaskWorklog(taskId, {
      hours,
      work_date: workDate,
      note: worklogDialog.note.trim() || undefined,
    });
    worklogDialog.visible = false;
    await loadWorklogs();
    MessagePlugin.success('工时已登记（实际工时由服务端汇总）');
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '工时登记失败');
  } finally {
    worklogDialog.submitting = false;
  }
}

/** 删除工时 */
function removeWorklog(row: TaskWorklogItem): void {
  const taskId = activeTaskId.value;
  if (!taskId) {
    return;
  }
  const dialog = DialogPlugin.confirm({
    header: '删除工时',
    body: `确认删除 ${row.work_date} 的 ${row.hours} 小时？删除后实际工时会重新汇总。`,
    theme: 'warning',
    confirmBtn: { content: '删除', theme: 'danger' },
    onConfirm: async () => {
      try {
        await deleteTaskWorklog(taskId, row.id);
        dialog.destroy();
        await loadWorklogs();
        MessagePlugin.success('工时已删除');
      } catch (error) {
        MessagePlugin.error(error instanceof ApiError ? error.message : '删除失败');
      }
    },
  });
}

/** 切协作分段时补拉（打开抽屉时三份已并发拉过，这里只兜底失败后的重试） */
function handleTabChange(): void {
  if (collabTab.value === 'comment' && comments.list.length === 0 && comments.total === 0) {
    void loadComments(1);
  }
  if (collabTab.value === 'attachment' && attachments.list.length === 0) {
    void loadAttachments();
  }
  if (collabTab.value === 'worklog' && worklogs.list.length === 0) {
    void loadWorklogs();
  }
}

/** 关闭抽屉时清空协作区状态（避免下次打开先闪上一个任务的评论） */
function resetCollab(): void {
  mainTab.value = 'detail';
  collabTab.value = 'comment';
  aiCount.value = null;
  descExpanded.value = false;
  comments.list = [];
  comments.total = 0;
  comments.page = 1;
  composer.content = '';
  composer.mentions = [];
  attachments.list = [];
  attachments.total = 0;
  worklogs.list = [];
  worklogs.total = 0;
  worklogs.sum_hours = 0;
  worklogs.actual_hours = null;
  dragOver.value = false;
  worklogDialog.visible = false;
}
</script>

<style scoped lang="less">
.task-detail {
  :deep(.t-drawer__body) {
    padding-top: 8px;
  }

  &__alert {
    margin-bottom: 12px;
  }
}

.head {
  padding-bottom: 16px;
  border-bottom: 1px solid var(--td-component-stroke);

  &__title-row {
    display: flex;
    align-items: baseline;
    gap: 8px;
    flex-wrap: wrap;
  }

  &__no {
    font-family: var(--app-font-mono, ui-monospace, monospace);
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__title {
    font-size: 18px;
    font-weight: 600;
    line-height: 1.4;
    color: var(--td-text-color-primary);
    word-break: break-word;
  }

  &__meta {
    display: flex;
    align-items: center;
    gap: 8px 12px;
    flex-wrap: wrap;
    margin-top: 8px;
    font-size: 12px;
    color: var(--td-text-color-secondary);
  }

  // E9：单行紧凑信息块（负责人 / 截止 / 进度 / 工时）
  &__chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    white-space: nowrap;
    line-height: 20px;

    &--progress {
      gap: 6px;
    }

    // 逾期：只用语义色，不改字号（避免一行里出现跳动）
    &--danger {
      color: var(--td-error-color);
      font-weight: 600;
    }
  }

  &__chip-icon {
    font-size: 14px;
    color: var(--td-text-color-placeholder);
  }

  // 头部进度条：固定窄宽，不跟文字抢位置
  &__bar {
    width: 72px;
  }

  &__pct {
    font-variant-numeric: tabular-nums;
    color: var(--td-text-color-primary);
  }

  &__actions {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    flex-wrap: wrap;
    margin-top: 12px;
  }

  // 编辑 / 删除：没有状态按钮时也靠右（`space-between` 单子节点会掉到左边）
  &__ops {
    margin-left: auto;
  }

  // E6a：子任务详情里的「← 返回父任务」
  &__parent {
    margin-top: -4px;
    margin-bottom: 6px;
    font-size: 12px;
  }
}

.block {
  margin-top: 20px;

  &__title {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 8px;
    font-size: 14px;
    font-weight: 600;
    color: var(--td-text-color-primary);
  }

  &__count {
    padding: 0 6px;
    font-size: 12px;
    font-weight: 400;
    color: var(--td-text-color-placeholder);
    background: var(--td-bg-color-container-hover);
    border-radius: 8px;
  }

  // E6a：标题 + 右侧动作按钮（「子任务」区块的新建入口）
  &__title--with-action {
    justify-content: space-between;
  }

  // 子任务完成度小字（「已完成 1 / 3」），不抢标题的字重
  &__sub {
    font-size: 12px;
    font-weight: 400;
    color: var(--td-text-color-placeholder);
  }
}

// ==================== E9：主 Tab ====================

// Tab 栏吸顶：详情内容再长，切 Tab 也不用先滚回顶部
.main-tabs {
  :deep(.t-tabs__nav) {
    position: sticky;
    top: 0;
    z-index: 2;
    padding-top: 4px;
    background: var(--td-bg-color-container);
    // 抽屉头部没有阴影，靠一条分隔线把 Tab 栏与内容分开
    border-bottom: 1px solid var(--td-component-stroke);
  }

  :deep(.t-tabs__content) {
    padding-top: 16px;
  }

  // 面板里的 .block 自带 20px 上边距，首个区块不需要再多一层
  :deep(.t-tabs__content > .t-tab-panel > .block:first-child) {
    margin-top: 0;
  }
}

.collab {
  &__seg {
    margin-bottom: 12px;
  }

  &__pane {
    // 分段控件与内容之间的呼吸（v-show 常驻，三段间距一致）
    padding-top: 4px;
  }
}

.info {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 8px 16px;
  margin: 0;

  &__item {
    display: flex;
    gap: 8px;
    font-size: 13px;
    line-height: 22px;
  }

  dt {
    flex-shrink: 0;
    width: 64px;
    color: var(--td-text-color-placeholder);
  }

  dd {
    margin: 0;
    color: var(--td-text-color-primary);
    word-break: break-all;
  }

  &__value--overdue {
    color: var(--td-error-color);
    font-weight: 600;
  }
}

.desc {
  position: relative;
  margin: 0;
  padding: 12px;
  font-family: inherit;
  font-size: 13px;
  line-height: 1.7;
  white-space: pre-wrap;
  word-break: break-word;
  color: var(--td-text-color-primary);
  background: var(--td-bg-color-container-hover);
  border-radius: var(--td-radius-medium);

  // E9：默认只露 5 行（5 × 1.7em），底部一层渐隐提示「还有内容」
  &--collapsed {
    max-height: calc(1.7em * 5);
    overflow: hidden;

    &::after {
      content: '';
      position: absolute;
      right: 0;
      bottom: 0;
      left: 0;
      height: 2em;
      background: linear-gradient(to bottom, transparent, var(--td-bg-color-container-hover));
      pointer-events: none;
    }
  }
}

.placeholder {
  margin: 0;
  font-size: 13px;
  color: var(--td-text-color-placeholder);
}

.subtask {
  border: 1px solid var(--td-component-stroke);
  border-radius: var(--td-radius-medium);
  overflow: hidden;

  &__item {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 12px;
    font-size: 13px;

    & + & {
      border-top: 1px solid var(--td-component-stroke);
    }
  }

  &__title {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--td-text-color-primary);
  }

  &__meta {
    flex-shrink: 0;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  // E6a：子任务行可点（打开子任务详情），给 hover 与键盘焦点反馈
  &__item--clickable {
    cursor: pointer;

    &:hover,
    &:focus-visible {
      background: var(--td-bg-color-container-hover);
    }
  }
}

.attachment {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 0;
  font-size: 13px;
  color: var(--td-text-color-secondary);

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

// ==================== T6b：协作区（评论 / 附件 / 工时） ====================

.collab {
  margin-bottom: 8px;
}

.more {
  margin-top: 8px;
  text-align: center;
}

.composer {
  display: flex;
  flex-direction: column;
  gap: 8px;
  margin-bottom: 12px;
  padding: 10px;
  background: var(--td-bg-color-container-hover);
  border-radius: var(--td-radius-medium);

  &__row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }

  &__ops {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 8px;
  }

  &__mention {
    flex: 1;
    min-width: 160px;
    max-width: 320px;
  }

  &__count,
  &__tip {
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }
}

.timeline {
  display: flex;
  flex-direction: column;
  gap: 10px;

  &__item {
    display: flex;
    gap: 8px;
    padding: 8px 10px;
    border: 1px solid var(--td-component-stroke);
    border-radius: var(--td-radius-medium);
    background: var(--td-bg-color-container);

    // 系统生成（STATUS_CHANGE / ASSIGN / PROGRESS_AUTO / AI_PROGRESS）：淡化 + 虚线框，
    // 视觉上明确「这条不是人写的、也不可改删」（后端 `readonly=true` 是唯一判定）
    &--system {
      opacity: 0.72;
      border-style: dashed;
      background: var(--td-bg-color-container-hover);
    }

    // AI 执行报告：换底色，和人的评论区分开
    &--ai {
      border-color: var(--td-warning-color-3);
      background: var(--td-warning-color-1);
    }
  }

  &__avatar {
    flex-shrink: 0;
    margin-top: 2px;
  }

  &__body {
    flex: 1;
    min-width: 0;
  }

  &__head {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }

  &__author {
    font-size: 13px;
    font-weight: 600;
    color: var(--td-text-color-primary);
  }

  &__time {
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__del {
    margin-left: auto;
  }

  &__content {
    margin-top: 4px;
    font-size: 13px;
    line-height: 1.7;
  }

  &__mentions {
    display: flex;
    align-items: center;
    gap: 4px;
    margin-top: 6px;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }
}

.attach {
  &__drop {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    padding: 16px;
    font-size: 13px;
    color: var(--td-text-color-secondary);
    cursor: pointer;
    border: 1px dashed var(--td-component-stroke);
    border-radius: var(--td-radius-medium);
    background: var(--td-bg-color-container-hover);

    &--over {
      border-color: var(--td-brand-color);
      color: var(--td-brand-color);
    }
  }

  &__input {
    display: none;
  }

  &__count {
    margin: 6px 0;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__list {
    border: 1px solid var(--td-component-stroke);
    border-radius: var(--td-radius-medium);
    overflow: hidden;
  }

  &__item {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
    font-size: 13px;
    color: var(--td-text-color-secondary);

    & + & {
      border-top: 1px solid var(--td-component-stroke);
    }
  }

  &__name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--td-text-color-primary);
  }

  &__meta {
    flex-shrink: 0;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }
}

.worklog {
  &__summary {
    display: flex;
    align-items: center;
    gap: 16px;
    flex-wrap: wrap;
    margin-bottom: 8px;
    font-size: 13px;
    color: var(--td-text-color-secondary);

    strong {
      color: var(--td-text-color-primary);
    }

    // 登记按钮推到最右
    > .t-button {
      margin-left: auto;
    }
  }

  &__list {
    border: 1px solid var(--td-component-stroke);
    border-radius: var(--td-radius-medium);
    overflow: hidden;
  }

  &__item {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 6px 10px;
    font-size: 13px;

    & + & {
      border-top: 1px solid var(--td-component-stroke);
    }
  }

  &__date {
    flex-shrink: 0;
    font-family: var(--app-font-mono, ui-monospace, monospace);
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__user {
    flex-shrink: 0;
    color: var(--td-text-color-secondary);
  }

  &__hours {
    flex-shrink: 0;
    font-weight: 600;
    color: var(--td-text-color-primary);
  }

  &__note {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--td-text-color-placeholder);
  }
}
</style>
