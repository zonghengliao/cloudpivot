<template>
  <div class="wb">
    <!-- ① 问候语条：登录后首屏的视觉锚点，副标题顺带报一句「今天有多少活」 -->
    <section class="wb__banner">
      <div class="wb__banner-text">
        <h2 class="wb__greeting">{{ greeting }}，{{ userStore.displayName || '同事' }}</h2>
        <p class="wb__subtitle">{{ subtitle }}</p>
      </div>
      <t-icon class="wb__banner-icon" name="dashboard" />
    </section>

    <!--
      主体两栏：左侧「工作任务 + 快捷入口」按自身内容高度排列，右侧固定 360px 放公告与消息。
      两个必需项——左栏 min-width:0（否则 flex item 的 min-width:auto 会被宽内容撑开，
      把右栏挤到下一行）；<1200px 时靠媒体查询切成上下堆叠。
    -->
    <t-row :gutter="[16, 16]" class="wb__body" align="stretch">
      <t-col class="wb__main">
        <div class="wb__stack">
          <!--
            ② 工作任务（今天最多 10 条）。口径是「逾期优先，其次今日」——
            逾期单本来也是今天要处理的，排最前才符合优先级直觉。
            点条目跳任务列表并自动开详情抽屉（`/task/list?id=`，列表页 onMounted 认这个 query，
            与通知里的旧链接 `/task/{id}` 同一套落地方式），不在首页再复制一份详情逻辑。
            全员可见：看的是「我自己的任务」，后端已按 owner_id 收口。
          -->
          <t-card title="工作任务" :bordered="false" class="wb__card wb__tasks">
            <template #actions>
              <t-link theme="primary" hover="color" @click="goTo(TASK_PATH)">
                全部
                <t-icon name="chevron-right" />
              </t-link>
            </template>

            <!-- 首屏骨架：5 行；二次刷新保留旧内容（与公告与消息卡同一口径） -->
            <div v-if="tasksLoading && !tasksLoaded" class="wb__tasks-skeleton">
              <div v-for="row in 5" :key="row" class="wb__tasks-skeleton-row" />
            </div>

            <template v-else>
              <ul v-if="taskRows.length" class="wb__tasks-list">
                <li v-for="row in taskRows" :key="row.id">
                  <button
                    type="button"
                    class="wb__tasks-item"
                    :class="{ 'wb__tasks-item--done': row.done }"
                    @click="openTask(row.id)"
                  >
                    <span class="wb__tasks-state" :class="`wb__tasks-state--${row.state}`">
                      <t-icon v-if="row.done" name="check" />
                    </span>
                    <span class="wb__tasks-main">
                      <span class="wb__tasks-title" :title="row.title">{{ row.title }}</span>
                      <span class="wb__tasks-meta">
                        <span
                          class="wb__tasks-due"
                          :class="{ 'wb__tasks-due--overdue': row.overdue }"
                        >
                          {{ row.dueText }}
                        </span>
                        <t-tag
                          v-if="row.priority"
                          class="wb__tasks-tag"
                          size="small"
                          variant="light"
                          :theme="row.priority.theme"
                        >
                          <span
                            class="wb__tasks-tag-dot"
                            :style="{ backgroundColor: row.priority.dotVar }"
                          />
                          {{ row.priority.name }}
                        </t-tag>
                      </span>
                    </span>
                  </button>
                </li>
              </ul>

              <div v-else class="wb__tasks-empty">
                <t-icon name="task" />
                <span>{{ tasksEmptyText }}</span>
                <t-button
                  v-if="!tasksDenied"
                  size="small"
                  variant="outline"
                  theme="primary"
                  @click="goTo(TASK_PATH)"
                >
                  去任务中心
                </t-button>
              </div>

              <!--
                底部统计取接口 counts 的**实际字段**（today / overdue / this_week）。
                没有「已完成」这一项：`/business/task/my` 只回 TODO/DOING/REVIEW 三个未终态，
                DONE 根本不在返回里，凭空写「已完成 M」只能是假的。
              -->
              <div v-if="taskCounts" class="wb__tasks-foot">
                <span>今日 {{ taskCounts.today }} 项</span>
                <span class="wb__tasks-foot-sep">·</span>
                <span>逾期 {{ taskCounts.overdue }} 项</span>
                <span class="wb__tasks-foot-sep">·</span>
                <span>本周 {{ taskCounts.this_week }} 项</span>
              </div>

              <div v-if="tasksError" class="wb__tasks-error">
                <t-icon name="error-circle" />
                <span>{{ tasksError }}</span>
                <t-link theme="primary" hover="color" @click="loadTasks()">重试</t-link>
              </div>
            </template>
          </t-card>

          <!--
            ⬇⬇ HOME-5「待我审批」卡片（左栏，紧跟「工作任务」之后）⬇⬇
            设计顺序：工作任务 → 待我审批 → 公告与消息 → 快捷入口。
            卡片自身管理数据（待办 10 条 + 今日已办）与三个动作（同意 / 驳回 / 加签），
            所以单独成组件，本容器不掺它的状态。
          -->
          <ApprovalTodoCard />

          <!--
            ④ 快捷入口。
            列数 = min(5, 可用入口数)：5 个一行是这批入口的原始设计，但只有 4 个可用时
            按 5 列排会在右边空出一格（非超管账号只有业务入口，正好是这种情况）。
            用 CSS 变量把「实际列数」交给内联 style，窄屏再由媒体查询压成 3 / 2 列。
          -->
          <t-card title="快捷入口" :bordered="false" class="wb__card">
            <div
              v-if="visibleEntries.length"
              class="wb__entries"
              :style="{ '--wb-entry-cols': String(entryColumns) }"
            >
              <button
                v-for="entry in visibleEntries"
                :key="entry.path"
                class="wb__entry"
                type="button"
                @click="goTo(entry.path)"
              >
                <span class="wb__entry-icon"><t-icon :name="entry.icon" /></span>
                <span class="wb__entry-title">{{ entry.title }}</span>
              </button>
            </div>
            <t-empty v-else description="暂无可访问的模块" />
          </t-card>
        </div>
      </t-col>

      <!--
        ③ 公告与消息：固定 360px，整块只放这一张卡 —— 所有人可见（公告 / 消息本身就是全员数据），
        不按角色收口。上区 5 条公告、下区 10 条消息；点条目在本页抽屉里读详情（不跳走，
        回到首页时状态不丢）。未读红点取接收行自己的 read_flag，点开即标已读并同步 notice store，
        这样顶栏铃铛的角标不会和卡里的红点对不上。
      -->
      <t-col class="wb__aside">
        <t-card title="公告与消息" :bordered="false" class="wb__card wb__feed">
          <template #actions>
            <div v-if="canReadAnnouncement || canReadNotice" class="wb__feed-actions">
              <template v-if="canReadAnnouncement">
                <t-link theme="primary" hover="color" @click="goTo(ANNOUNCEMENT_PATH)">
                  公告全部
                </t-link>
              </template>
              <span v-if="canReadAnnouncement && canReadNotice" class="wb__feed-actions-sep">/</span>
              <t-link v-if="canReadNotice" theme="primary" hover="color" @click="goTo(MESSAGE_PATH)">
                消息全部
              </t-link>
            </div>
          </template>

          <!-- 首屏骨架：首次加载不白屏；二次刷新保留旧内容（不闪骨架） -->
          <div v-if="feedLoading && !feedLoaded" class="wb__feed-skeleton">
            <div v-for="row in 12" :key="row" class="wb__feed-skeleton-row" />
          </div>

          <template v-else>
            <!-- 上半区：公告 5 条 -->
            <section class="wb__feed-section">
              <div class="wb__feed-head">
                <t-icon class="wb__feed-head-icon wb__feed-head-icon--ann" name="notification" />
                <span class="wb__feed-head-title">公告</span>
                <span v-if="canReadAnnouncement" class="wb__feed-head-count">
                  共 {{ announcementTotal }} 条
                </span>
              </div>
              <ul v-if="announcements.length" class="wb__feed-list">
                <li v-for="item in announcements" :key="item.id">
                  <button type="button" class="wb__feed-item" @click="openAnnouncement(item)">
                    <span class="wb__feed-item-title" :title="item.title">{{ item.title }}</span>
                    <span class="wb__feed-item-time">{{ formatFeedTime(item.publish_at) }}</span>
                  </button>
                </li>
              </ul>
              <div v-else class="wb__feed-empty">
                <t-icon name="notification" />
                <span>{{ canReadAnnouncement ? '暂无公告' : '当前账号没有公告查看权限' }}</span>
              </div>
            </section>

            <!-- 下半区：消息 10 条（未读带红点） -->
            <section class="wb__feed-section wb__feed-section--notice">
              <div class="wb__feed-head">
                <t-icon class="wb__feed-head-icon wb__feed-head-icon--msg" name="chat" />
                <span class="wb__feed-head-title">消息</span>
                <span v-if="canReadNotice" class="wb__feed-head-count">共 {{ noticeTotal }} 条</span>
              </div>
              <ul v-if="notices.length" class="wb__feed-list">
                <li v-for="item in notices" :key="item.receiver_id">
                  <button
                    type="button"
                    class="wb__feed-item"
                    :class="{ 'wb__feed-item--unread': !item.read_flag }"
                    @click="openNotice(item)"
                  >
                    <span class="wb__feed-item-title" :title="item.title">
                      <span v-if="!item.read_flag" class="wb__feed-dot" aria-label="未读" />
                      {{ item.title }}
                    </span>
                    <span class="wb__feed-item-time">{{ formatFeedTime(item.created_at) }}</span>
                  </button>
                </li>
              </ul>
              <div v-else class="wb__feed-empty">
                <t-icon name="chat" />
                <span>{{ canReadNotice ? '暂无消息' : '当前账号没有消息查看权限' }}</span>
              </div>
            </section>

            <!-- 底部统计条：条数按「本卡展示的」算（公告 ≤5 / 消息 ≤10），与顶部「共 N 条」区分 -->
            <div class="wb__feed-foot">
              <span>公告 {{ announcements.length }} 条</span>
              <span class="wb__feed-foot-sep">·</span>
              <span>消息 {{ notices.length }} 条</span>
              <span v-if="feedUnread > 0" class="wb__feed-foot-unread">（未读 {{ feedUnread }}）</span>
            </div>

            <div v-if="feedError" class="wb__feed-error">
              <t-icon name="error-circle" />
              <span>{{ feedError }}</span>
              <t-link theme="primary" hover="color" @click="loadFeed()">重试</t-link>
            </div>
          </template>
        </t-card>
      </t-col>
    </t-row>

    <!--
      详情抽屉：首页只做「扫一眼」，点开读全文不跳页 —— 读完关掉还在首页，
      不会丢掉「刚登录想看什么」的上下文。公告与消息各一个，互不干扰。
    -->
    <t-drawer
      v-model:visible="announcementDrawerVisible"
      header="公告详情"
      size="560px"
      :footer="false"
      destroy-on-close
    >
      <t-loading :loading="announcementDetailLoading" size="small">
        <template v-if="announcementDetail">
          <h3 class="wb__detail-title">
            <t-icon v-if="announcementDetail.is_top" class="wb__detail-pin" name="arrow-up" />
            {{ announcementDetail.title }}
          </h3>
          <div class="wb__detail-meta">
            <t-tag size="small" variant="light" :theme="levelTheme(announcementDetail.level)">
              {{ levelLabel(announcementDetail.level) }}
            </t-tag>
            <span>{{ announcementDetail.ann_no }}</span>
            <span>发布人：{{ announcementDetail.publisher_name || '系统' }}</span>
            <span>{{ formatDateTime(announcementDetail.publish_at) }}</span>
          </div>
          <MarkdownViewer
            :html="announcementDetail.content_html"
            :show-toc="false"
            empty-text="公告正文为空"
          />
          <div v-if="announcementDetail.attachments.length" class="wb__detail-files">
            <div class="wb__detail-files-title">
              附件（{{ announcementDetail.attachments.length }}）
            </div>
            <button
              v-for="file in announcementDetail.attachments"
              :key="file.id"
              type="button"
              class="wb__detail-file"
              @click="downloadAttachment(file)"
            >
              <t-icon name="file" />
              <span class="wb__detail-file-name" :title="file.file_name">{{ file.file_name }}</span>
              <span class="wb__detail-file-size">{{ formatFileSize(file.file_size) }}</span>
            </button>
          </div>
        </template>
        <div v-else-if="!announcementDetailLoading" class="wb__feed-empty">
          <t-icon name="notification" />
          <span>公告内容加载失败</span>
        </div>
      </t-loading>
    </t-drawer>

    <t-drawer v-model:visible="noticeDrawerVisible" header="消息详情" size="480px" :footer="false">
      <template v-if="activeNotice">
        <h3 class="wb__detail-title">{{ activeNotice.title }}</h3>
        <div class="wb__detail-meta">
          <t-tag size="small" variant="light" :theme="levelTheme(activeNotice.level)">
            {{ levelLabel(activeNotice.level) }}
          </t-tag>
          <span>{{ noticeTypeLabel(activeNotice.notice_type) }}</span>
          <span>{{ formatDateTime(activeNotice.created_at) }}</span>
          <span v-if="activeNotice.read_flag" class="wb__detail-read">已读</span>
        </div>
        <div class="wb__detail-body" v-html="renderNoticeContent(activeNotice)" />
        <div v-if="activeNotice.link_url" class="wb__detail-link">
          <t-link theme="primary" @click="openNoticeLink(activeNotice.link_url)">查看详情 →</t-link>
        </div>
      </template>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import { useRouter } from 'vue-router';
import dayjs from 'dayjs';
import { MessagePlugin } from 'tdesign-vue-next';
import MarkdownViewer from '@/components/MarkdownViewer.vue';
import ApprovalTodoCard from './ApprovalTodoCard.vue';
import {
  downloadAnnouncementAttachment,
  getAnnouncementDetail,
  markAnnouncementRead,
  pageVisibleAnnouncements,
  type AnnouncementAttachment,
  type AnnouncementCard,
  type AnnouncementDetail,
} from '@/api/business/announcement-api';
import {
  fetchUnreadCount,
  markNoticeRead,
  pageMyNotices,
  type MyNoticeRes,
} from '@/api/business/notice';
import { listMyTasks, type MyTaskCounts, type TaskCard } from '@/api/business/task-api';
import { PRIORITY_MAP, type PriorityMeta } from '@/constants/status-map';
import { ApiError } from '@/api/request';
import { useNoticeStore } from '@/stores/notice';
import { useUserStore } from '@/stores/user';
import { formatDateTime, formatFileSize } from '@/utils/format';
import { saveBlob } from '@/utils/download';

const router = useRouter();
const userStore = useUserStore();
const noticeStore = useNoticeStore();

// ---------- 问候语条 ----------

/** 今天（中文日期，副标题用） */
const today = dayjs().format('YYYY年MM月DD日');

/** 按时段生成问候语 */
const greeting = computed(() => {
  const hour = dayjs().hour();
  if (hour < 6) return '凌晨好';
  if (hour < 12) return '上午好';
  if (hour < 14) return '中午好';
  if (hour < 18) return '下午好';
  return '晚上好';
});

/**
 * 副标题：日期 + 一句概况。
 *
 * 概况只用**已拿到的真实计数**（`counts.today / overdue / this_week`）：
 * 任务接口未回或 403 时退回纯日期问候，绝不编「今天 8 项任务」这种数字。
 */
const subtitle = computed(() => {
  const counts = taskCounts.value;
  if (!counts) return `今天是 ${today}，祝你工作顺利。`;
  return `今天是 ${today} · 今天 ${counts.today} 项任务 · 逾期 ${counts.overdue} 项 · 本周 ${counts.this_week} 项`;
});

// ---------- 公告与消息卡 ----------

/** 公告中心路由（与 `router/index.ts` 的 `AnnouncementCenter` 逐字一致） */
const ANNOUNCEMENT_PATH = '/announcement';
/** 消息中心路由（`BusinessNoticeMessage`，菜单隐藏，从铃铛进） */
const MESSAGE_PATH = '/business/notice/message';
/** 首页公告区最多几条 */
const ANNOUNCEMENT_LIMIT = 5;
/** 首页消息区最多几条 */
const NOTICE_LIMIT = 10;

/** 公告列表（最多 5 条） */
const announcements = ref<AnnouncementCard[]>([]);
/** 公告总数（分页接口返回的 total，不是本页条数） */
const announcementTotal = ref(0);
/** 消息列表（最多 10 条） */
const notices = ref<MyNoticeRes[]>([]);
/** 消息总数 */
const noticeTotal = ref(0);
/** 卡片数据加载中标记 */
const feedLoading = ref(false);
/** 是否已成功拉过一次（决定要不要显示骨架屏；刷新时不再闪骨架） */
const feedLoaded = ref(false);
/** 卡片加载失败提示 */
const feedError = ref('');

/** 公告详情抽屉显隐 */
const announcementDrawerVisible = ref(false);
/** 公告详情加载中 */
const announcementDetailLoading = ref(false);
/** 公告详情 */
const announcementDetail = ref<AnnouncementDetail | null>(null);
/** 消息详情抽屉显隐 */
const noticeDrawerVisible = ref(false);
/** 当前打开的消息（列表行本身就带正文，无需再请求详情） */
const activeNotice = ref<MyNoticeRes | null>(null);

/**
 * 底部统计条的「未读」= 本卡展示的这 10 条里还没读的条数。
 *
 * 不直接用 store 的全量未读（可能有几十条没出现在卡里）：写成「消息 10 条（未读 17）」
 * 会让用户以为这 10 条里有 17 条没读。全量未读由顶栏铃铛负责，两处口径各司其职。
 */
const feedUnread = computed(() => notices.value.filter((item) => !item.read_flag).length);

/**
 * 公告 / 站内信的读取权限。
 *
 * 与后端 `#[nik::permission]` 的码逐字一致（`business:announcement:read` /
 * `business:notice:mine`）。没有权限就不发请求：非超管账号一进首页被 403 刷屏，
 * 是「前端明知会拒还硬发」造成的，不是后端的问题。
 */
const canReadAnnouncement = computed(() => userStore.hasPermission('business:announcement:read'));
const canReadNotice = computed(() => userStore.hasPermission('business:notice:mine'));

// ---------- 工作任务卡 ----------

/** 任务列表路由（`BusinessTaskList`；带 `?id=` 时由列表页自动开详情抽屉） */
const TASK_PATH = '/task/list';
/** 本卡最多几条（逾期 + 今日合并后的硬上限） */
const TASK_LIMIT = 10;
/** 值得在卡片上打标的优先级：P0/P1；P2/P3 不打标，避免 10 行全是标签 */
const URGENT_PRIORITIES: string[] = ['P0', 'P1'];

/** 待展示的任务（逾期优先 + 今日，合并后取前 10 条） */
const taskItems = ref<TaskCard[]>([]);
/** 分组条数：直接用接口 `counts` 的实际字段，不加工、不编造 */
const taskCounts = ref<MyTaskCounts | null>(null);
/** 卡片数据加载中标记 */
const tasksLoading = ref(false);
/** 是否已成功拉过一次（决定要不要显示骨架屏；刷新时不再闪骨架） */
const tasksLoaded = ref(false);
/** 卡片加载失败提示 */
const tasksError = ref('');
/** 当前账号没有 `business:task:read`（403）：卡片对全员可见，但没权限时不当作错误 */
const tasksDenied = ref(false);

/** 卡片上的一行（文案 / 状态 / 优先级在 computed 里算好，模板只取值） */
interface TaskRow {
  /** 任务主键（点击跳详情） */
  id: string;
  /** 标题（超长在样式里截断，hover 用原生 title 看全文） */
  title: string;
  /** 是否已完成（勾选 + 删除线） */
  done: boolean;
  /** 是否逾期（截止日 < 今天） */
  overdue: boolean;
  /** 时间文案：今天截止 / 逾期 N 天 / MM-DD 截止 */
  dueText: string;
  /** 状态样式键：todo / doing / review / done / cancelled */
  state: string;
  /** 紧急度标签（P0/P1 才有值；P2/P3 返回 null 不渲染） */
  priority: PriorityMeta | null;
}

const taskRows = computed<TaskRow[]>(() =>
  taskItems.value.map((item) => ({
    id: item.id,
    title: item.title,
    done: isTaskDone(item.status),
    overdue: isTaskOverdue(item),
    dueText: taskDueText(item),
    state: taskStateKey(item.status),
    priority: urgentPriority(item.priority),
  })),
);

/** 空态文案：没权限 ≠ 今天没任务，两句话不能混 */
const tasksEmptyText = computed(() =>
  tasksDenied.value ? '当前账号没有任务访问权限' : '今天没有待办任务',
);

/** 任务状态 → CSS 键（与 `@/constants/status-map` 的状态集合一致，未知状态按待办处理） */
function taskStateKey(status: string): string {
  switch ((status || '').toUpperCase()) {
    case 'DOING':
      return 'doing';
    case 'REVIEW':
      return 'review';
    case 'DONE':
      return 'done';
    case 'CANCELLED':
      return 'cancelled';
    default:
      return 'todo';
  }
}

/** 是否已完成（已完成的行打勾 + 删除线 + 置灰） */
function isTaskDone(status: string): boolean {
  return (status || '').toUpperCase() === 'DONE';
}

/**
 * 是否逾期：`due_date < 今天`（与后端 `MyBucket::Overdue` 同一口径）。
 * 已完成 / 已取消的单子不再算逾期 —— 它们已经出局，标红只会制造假警报。
 */
function isTaskOverdue(item: TaskCard): boolean {
  const status = (item.status || '').toUpperCase();
  if (status === 'DONE' || status === 'CANCELLED') return false;
  if (!item.due_date) return false;
  const due = dayjs(item.due_date);
  return due.isValid() && due.startOf('day').isBefore(dayjs().startOf('day'));
}

/**
 * 截止文案。
 *
 * 后端任务的 `due_date` 只有**日期**（`YYYY-MM-DD`，没有时分），所以这里只能写
 * 「今天截止 / 逾期 N 天」，不能像设计稿那样编一个「18:00」出来。
 */
function taskDueText(item: TaskCard): string {
  if (!item.due_date) return '无截止日';
  const due = dayjs(item.due_date);
  if (!due.isValid()) return '无截止日';
  const status = (item.status || '').toUpperCase();
  if (status === 'DONE') return `已完成 · ${due.format('MM-DD')}`;
  if (status === 'CANCELLED') return `已取消 · ${due.format('MM-DD')}`;
  const days = dayjs().startOf('day').diff(due.startOf('day'), 'day');
  if (days > 0) return `逾期 ${days} 天（${due.format('MM-DD')}）`;
  if (days === 0) return '今天截止';
  return `${due.format('MM-DD')} 截止`;
}

/** 紧急度标签：只认 P0/P1，颜色/文案全部来自 `PRIORITY_MAP`（不写死颜色） */
function urgentPriority(priority: string): PriorityMeta | null {
  const key = (priority || '').toUpperCase();
  if (!URGENT_PRIORITIES.includes(key)) return null;
  return PRIORITY_MAP[key as keyof typeof PRIORITY_MAP] ?? null;
}

// ---------- 快捷入口 ----------

/** 快捷入口配置：path 与路由表一致，permission 与后端菜单权限码一致 */
interface QuickEntry {
  /** 入口标题 */
  title: string;
  /** 跳转路由路径 */
  path: string;
  /** 图标名（TDesign 图标库） */
  icon: string;
  /** 访问所需权限码 */
  permission: string;
}

const QUICK_ENTRIES: QuickEntry[] = [
  // ── 业务动线（全员口径优先）：非超管账号通常一个 `system:*` 都没有，
  //    只放系统入口会让他们的「快捷入口」整卡空着（实测评测 EMPLOYEE 角色 0 命中）。
  //    path 与 `router` 逐字一致，permission 与 `sys_menu.auth_code` 逐字一致。
  { title: '任务中心', path: '/task/list', icon: 'task', permission: 'business:task:read' },
  {
    title: '审批中心',
    path: '/flow/task',
    icon: 'check-circle',
    permission: 'business:flow:instance:read',
  },
  { title: '项目', path: '/project', icon: 'folder', permission: 'business:project:read' },
  {
    title: '我的报销',
    path: '/business/erp/expense',
    icon: 'money',
    permission: 'business:erp:expense:list',
  },
  // ── 底座入口（超管 / 管理员口径）
  { title: '用户管理', path: '/system/user', icon: 'user', permission: 'system:user:list' },
  { title: '角色管理', path: '/system/role', icon: 'usergroup', permission: 'system:role:list' },
  { title: '菜单管理', path: '/system/menu', icon: 'menu', permission: 'system:menu:list' },
  { title: '部门管理', path: '/system/dept', icon: 'app', permission: 'system:dept:list' },
  { title: '字典管理', path: '/support/dict', icon: 'setting', permission: 'support:dict:list' },
  { title: '参数配置', path: '/support/config', icon: 'setting', permission: 'support:config:list' },
  { title: '文件管理', path: '/support/file', icon: 'folder', permission: 'support:file:list' },
  { title: '定时任务', path: '/support/job', icon: 'dashboard', permission: 'support:job:list' },
  { title: '在线用户', path: '/support/online', icon: 'user-time', permission: 'support:online:list' },
  { title: '操作日志', path: '/support/log', icon: 'lock-on', permission: 'support:log:list' },
];

/** 按权限过滤后的快捷入口 */
const visibleEntries = computed(() =>
  QUICK_ENTRIES.filter((entry) => userStore.hasPermission(entry.permission)),
);

/**
 * 快捷入口的列数：最多 5 列（原始设计），入口更少时按实际数量撑满一行 ——
 * 4 个入口排 5 列会在右边空一格，那比「格子变宽」更难看。
 * 窄屏由样式里的媒体查询再压成 3 列 / 2 列（内联只给 CSS 变量，不直接写列数，
 * 否则内联样式会盖掉媒体查询）。
 */
const entryColumns = computed(() => Math.min(5, Math.max(1, visibleEntries.value.length)));

function goTo(path: string): void {
  void router.push(path);
}

// ---------- 公告与消息卡：加载与交互 ----------

/** 列表时间只到分钟（`2026-11-09 10:23`）：秒级精度在卡片里只是噪声 */
function formatFeedTime(value?: string | null): string {
  if (!value) return '-';
  const parsed = dayjs(value);
  return parsed.isValid() ? parsed.format('YYYY-MM-DD HH:mm') : '-';
}

/** 级别 → 中文（公告与消息共用一张映射表，未登记的级别原样显示） */
function levelLabel(level: string): string {
  const map: Record<string, string> = {
    NORMAL: '普通',
    IMPORTANT: '重要',
    URGENT: '紧急',
    INFO: '通知',
    SUCCESS: '成功',
    WARN: '警告',
    ERROR: '错误',
  };
  return map[(level || '').toUpperCase()] || level || '普通';
}

/** 级别 → 语义色（状态色语义固定，不跟配色方案走） */
function levelTheme(level: string): 'primary' | 'success' | 'warning' | 'danger' {
  const key = (level || '').toUpperCase();
  if (key === 'URGENT' || key === 'ERROR') return 'danger';
  if (key === 'IMPORTANT' || key === 'WARN') return 'warning';
  if (key === 'SUCCESS') return 'success';
  return 'primary';
}

/** 消息类型 → 中文 */
function noticeTypeLabel(type: string): string {
  const map: Record<string, string> = {
    SYSTEM: '系统消息',
    TODO: '待办',
    ALERT: '告警',
    WORKFLOW: '审批',
    CIRCULAR: '公告',
  };
  return map[(type || '').toUpperCase()] || type || '消息';
}

/** 消息正文：TEXT 先转义再折行，HTML 原样（后端渲染时已转义变量），与消息中心同一口径 */
function renderNoticeContent(item: MyNoticeRes): string {
  const raw = item.content || '';
  if ((item.content_format || 'TEXT').toUpperCase() === 'HTML') {
    return raw;
  }
  return raw
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/\n/g, '<br />');
}

/**
 * 拉卡片数据：公告 5 条、消息 10 条、未读数三路并发。
 *
 * 用 `allSettled` 而不是 `all`：任意一路失败只让对应半区显示空态，
 * 另一区照常可用 —— 首页不该因为「未读数接口抽风」整块变空。
 *
 * **按权限发请求**：公告要 `business:announcement:read`、站内信要 `business:notice:mine`。
 * 账号没有某个权限时直接跳过那一路（空态写「没有查看权限」），不发注定 403 的请求 ——
 * 否则非超管一进首页，控制台就被红色 403 刷屏，而这本来是可以避免的。
 */
async function loadFeed(): Promise<void> {
  feedLoading.value = true;
  feedError.value = '';

  const annPromise = canReadAnnouncement.value
    ? pageVisibleAnnouncements({ page: 1, page_size: ANNOUNCEMENT_LIMIT })
    : null;
  const noticePromise = canReadNotice.value
    ? pageMyNotices({ page: 1, page_size: NOTICE_LIMIT })
    : null;
  const unreadPromise = canReadNotice.value ? fetchUnreadCount() : null;

  const [annRes, noticeRes, unreadRes] = await Promise.allSettled([
    annPromise,
    noticePromise,
    unreadPromise,
  ]);

  if (annRes.status === 'fulfilled' && annRes.value) {
    const page = annRes.value as Awaited<ReturnType<typeof pageVisibleAnnouncements>>;
    // 接口已按发布时间倒序，这里再截一刀兜底「最多 5 条」的硬约束
    announcements.value = page.list.slice(0, ANNOUNCEMENT_LIMIT);
    announcementTotal.value = page.total;
  } else {
    announcements.value = [];
    announcementTotal.value = 0;
  }

  if (noticeRes.status === 'fulfilled' && noticeRes.value) {
    const page = noticeRes.value as Awaited<ReturnType<typeof pageMyNotices>>;
    notices.value = page.list.slice(0, NOTICE_LIMIT);
    noticeTotal.value = page.total;
  } else {
    notices.value = [];
    noticeTotal.value = 0;
  }

  if (unreadRes.status === 'fulfilled' && unreadRes.value) {
    const count = unreadRes.value as Awaited<ReturnType<typeof fetchUnreadCount>>;
    noticeStore.setUnreadCount(count.unread_count);
  }

  // 有权限的两路都失败才值得打扰用户；单区失败由空态文案说明，不弹错误条。
  // 没有权限的两区不计入「失败」—— 那不是错误，是权限。
  const failed = [annRes, noticeRes].filter((res) => res.status === 'rejected');
  const expected = (canReadAnnouncement.value ? 1 : 0) + (canReadNotice.value ? 1 : 0);
  if (expected > 0 && failed.length === expected) {
    const reason = (failed[0] as PromiseRejectedResult).reason;
    feedError.value = reason instanceof ApiError ? reason.message : '公告与消息加载失败';
  }

  feedLoaded.value = true;
  feedLoading.value = false;
}

/**
 * 拉「我的任务」：逾期优先、其次今日，合并后取前 10 条。
 *
 * 口径照卡面：逾期本身也属于「今天要处理的」，所以排最前。
 * 只调这一个接口（`GET /business/task/my`），不新增任何接口。
 */
async function loadTasks(): Promise<void> {
  tasksLoading.value = true;
  tasksError.value = '';
  try {
    const res = await listMyTasks();
    taskCounts.value = res.counts;
    const seen = new Set<string>();
    const merged: TaskCard[] = [];
    for (const item of [...res.overdue, ...res.today]) {
      if (seen.has(item.id)) continue;
      seen.add(item.id);
      merged.push(item);
      if (merged.length >= TASK_LIMIT) break;
    }
    taskItems.value = merged;
    tasksDenied.value = false;
  } catch (error) {
    taskItems.value = [];
    taskCounts.value = null;
    // 没有 `business:task:read` 的账号拿 403：卡片对全员可见，按「无权限」空态提示，不弹报错条
    if (error instanceof ApiError && error.status === 403) {
      tasksDenied.value = true;
    } else {
      tasksError.value = error instanceof ApiError ? error.message : '工作任务加载失败';
    }
  } finally {
    tasksLoading.value = false;
    tasksLoaded.value = true;
  }
}

/**
 * 打开任务详情：跳任务列表并带 `?id=` —— 列表页 `onMounted` 认这个 query 自动开详情抽屉，
 * 与通知里旧链接 `/task/{id}` 的兼容落地是同一套，不在这里复制第二份详情逻辑。
 */
function openTask(id: string): void {
  void router.push({ path: TASK_PATH, query: { id } });
}

/** 打开公告详情（点开即读，标已读幂等） */
async function openAnnouncement(item: AnnouncementCard): Promise<void> {
  announcementDrawerVisible.value = true;
  announcementDetail.value = null;
  announcementDetailLoading.value = true;
  try {
    const detail = await getAnnouncementDetail(item.id);
    announcementDetail.value = detail;
    if (!detail.read) {
      try {
        await markAnnouncementRead(detail.id);
        detail.read = true;
        item.read = true;
      } catch (error) {
        // 标已读失败不影响阅读，但要让用户知道红点没消
        void MessagePlugin.warning(error instanceof ApiError ? error.message : '标记已读失败');
      }
    }
  } catch (error) {
    void MessagePlugin.error(error instanceof ApiError ? error.message : '公告详情加载失败');
  } finally {
    announcementDetailLoading.value = false;
  }
}

/** 打开消息详情：列表行本身带正文，直接展示；未读则顺手标已读并同步角标 */
async function openNotice(item: MyNoticeRes): Promise<void> {
  activeNotice.value = item;
  noticeDrawerVisible.value = true;
  if (item.read_flag) return;
  try {
    await markNoticeRead(item.receiver_id);
    item.read_flag = true;
    item.read_at = new Date().toISOString();
    // 顶栏角标与本卡共用 store，改一处即可
    noticeStore.setUnreadCount(noticeStore.unreadCount - 1);
  } catch (error) {
    void MessagePlugin.error(error instanceof ApiError ? error.message : '标记已读失败');
  }
}

/** 消息里的跳转地址：新窗口打开，不打断首页上下文 */
function openNoticeLink(url: string): void {
  window.open(url, '_blank', 'noopener');
}

/** 下载公告附件（代理接口要鉴权头，先取 blob 再保存） */
async function downloadAttachment(file: AnnouncementAttachment): Promise<void> {
  try {
    const blob = await downloadAnnouncementAttachment(file.download_url);
    saveBlob(blob, file.file_name);
  } catch (error) {
    void MessagePlugin.error(error instanceof ApiError ? error.message : '附件下载失败');
  }
}

onMounted(() => {
  void loadTasks();
  void loadFeed();
});
</script>

<style scoped lang="less">
// 骨架屏流光：@keyframes 必须放顶层，嵌在选择器里 LESS 不会提升
@keyframes wb-shimmer {
  from {
    background-position: 100% 0;
  }

  to {
    background-position: 0 0;
  }
}

.wb {
  // ---------- ① 问候语条 ----------
  &__banner {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 24px 28px;
    color: var(--app-on-gradient, #1a2230);
    border-radius: 12px;
    background: var(--app-gradient-brand);
    box-shadow: 0 6px 20px rgb(111 138 184 / 24%);
  }

  &__greeting {
    margin: 0 0 6px;
    font-size: 20px;
    font-weight: 600;
    // 白字在深色品牌段上，加一点深色微阴影提升边缘清晰度
    text-shadow: 0 1px 3px rgb(0 0 0 / 26%);
  }

  &__subtitle {
    margin: 0;
    font-size: 13px;
    // 渐变终点是浅色，副标题用同色文字（略透明以区分层级，主色端仍 ≥4.5）
    color: var(--app-on-gradient, #ffffff);
    opacity: 0.94;
    // 白字在深色段已足够清晰，给一点深色微阴影防止后段浅色区发虚
    text-shadow: 0 1px 2px rgb(0 0 0 / 22%);
  }

  &__banner-icon {
    font-size: 72px;
    // 图标在横幅右侧（浅色段），必须用深色油墨才可见
    color: var(--app-on-gradient-soft, #1a2230);
    opacity: 0.16;
  }

  // t-row 用 gutter 负边距（-8px）抵消列 padding，两栏基准宽度需按内容盒计算，
  // 否则 360px + 自适应列会超出容器而换行。这里给 body 显式 nowrap + 负边距归零。
  &__body {
    margin-top: 16px;
    flex-wrap: nowrap;

    @media (max-width: 1199px) {
      flex-wrap: wrap;
    }
  }

  // 左右两栏：右栏固定 360px，左栏自适应。
  // 宽度写在样式里而不是 t-col 的 flex prop —— prop 会被框架写成 inline style，
  // 媒体查询压不住。min-width:0 必需：flex item 默认 min-width:auto 会被内部宽内容撑开。
  &__main {
    flex: 1 1 auto;
    width: 0;
    min-width: 0;
  }

  // 左栏两张卡（工作任务 / 快捷入口）竖排，间距与右栏一致
  &__stack {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }

  &__aside {
    flex: 0 0 360px;
    width: 360px;
    min-width: 0;
    // 右栏按自身内容高度排列：左栏通常更高，若跟着 stretch 拉伸，卡片底部会拖出空白
    align-self: flex-start;
    display: flex;
    flex-direction: column;
    gap: 16px;
  }

  // 窄屏（<1200px）：右栏落到左栏下方通栏
  @media (max-width: 1199px) {
    &__body {
      flex-wrap: wrap;

      .wb__main,
      .wb__aside {
        flex: 1 1 100%;
        width: 100%;
      }
    }
  }

  &__card {
    height: 100%;
  }

  // ---------- ③ 公告与消息卡 ----------
  // 高度由内容撑开：5 条公告 + 10 条消息 ≈ 560px，正好占满右栏首屏。
  // 卡片自身不设固定高度 —— 数据少时短、多时到上限截断，不会留死白。
  &__feed {
    height: auto;

    // t-card 默认内边距 24px；条目行自身已有 5px 留白，收紧到 6px 12px 12px
    :deep(.t-card__body) {
      padding: 6px 12px 12px;
    }

    // 头部右上角两个入口：公告全部 / 消息全部
    &-actions {
      display: flex;
      align-items: center;
      gap: 8px;
      font-size: 13px;
    }

    &-actions-sep {
      color: var(--td-text-color-placeholder);
    }

    // 首屏骨架：12 行灰条，避免「先白屏再跳内容」
    &-skeleton {
      display: flex;
      flex-direction: column;
      gap: 10px;
      padding: 8px 4px;
    }

    &-skeleton-row {
      height: 13px;
      background: linear-gradient(
        90deg,
        var(--td-bg-color-container-hover) 25%,
        var(--td-bg-color-secondarycontainer) 37%,
        var(--td-bg-color-container-hover) 63%
      );
      background-size: 400% 100%;
      border-radius: var(--td-radius-default);
      animation: wb-shimmer 1.4s ease infinite;

      &:nth-child(2n) {
        width: 72%;
      }

      &:nth-child(3n) {
        width: 86%;
      }
    }

    // 上下两区：公告区与消息区之间用细线分隔
    // 行高按「15 行 + 2 个区头 + 统计条」压到 ~600px（5+10 条正好一屏，不多留白）
    &-section {
      padding: 6px 0 2px;

      & + & {
        border-top: 1px solid var(--td-component-stroke);
      }
    }

    &-head {
      display: flex;
      align-items: center;
      gap: 6px;
      padding: 0 4px 2px;
    }

    &-head-icon {
      font-size: 15px;

      &--ann {
        color: var(--td-brand-color);
      }

      &--msg {
        color: var(--td-warning-color);
      }
    }

    &-head-title {
      font-size: 13px;
      font-weight: 600;
      color: var(--td-text-color-primary);
    }

    &-head-count {
      margin-left: auto;
      font-size: 12px;
      color: var(--td-text-color-placeholder);
      font-variant-numeric: tabular-nums;
    }

    &-list {
      margin: 0;
      padding: 0;
      list-style: none;
    }

    // 整行可点：button 重置成文本行，键盘 Tab 也能到达（focus-visible 保留焦点环）
    &-item {
      display: flex;
      gap: 10px;
      align-items: baseline;
      width: 100%;
      padding: 3px 6px;
      font: inherit;
      color: inherit;
      text-align: left;
      cursor: pointer;
      background: none;
      border: 0;
      border-radius: var(--td-radius-default);
      transition: background-color 0.15s ease;

      &:hover {
        background-color: var(--td-bg-color-container-hover);
      }

      &:focus-visible {
        outline: 2px solid var(--td-brand-color);
        outline-offset: -2px;
      }

      // 未读：标题加深加粗 + 前置红点，扫一眼就能区分
      &--unread {
        .wb__feed-item-title {
          font-weight: 600;
          color: var(--td-text-color-primary);
        }
      }
    }

    &-item-title {
      display: flex;
      flex: 1 1 auto;
      gap: 6px;
      align-items: center;
      min-width: 0;
      overflow: hidden;
      font-size: 13px;
      color: var(--td-text-color-secondary);
      text-overflow: ellipsis;
      white-space: nowrap;
    }

    &-item-time {
      flex: none;
      font-size: 12px;
      color: var(--td-text-color-placeholder);
      font-variant-numeric: tabular-nums;
    }

    &-dot {
      flex: none;
      width: 6px;
      height: 6px;
      background-color: var(--td-error-color);
      border-radius: 50%;
    }

    &-empty {
      display: flex;
      flex-direction: column;
      gap: 6px;
      align-items: center;
      padding: 18px 0;
      font-size: 12px;
      color: var(--td-text-color-placeholder);
    }

    &-foot {
      display: flex;
      gap: 6px;
      align-items: center;
      margin-top: 8px;
      padding: 8px 4px 0;
      font-size: 12px;
      color: var(--td-text-color-placeholder);
      border-top: 1px solid var(--td-component-stroke);
      font-variant-numeric: tabular-nums;
    }

    &-foot-sep {
      color: var(--td-text-color-placeholder);
    }

    &-foot-unread {
      color: var(--td-error-color);
    }

    &-error {
      display: flex;
      gap: 6px;
      align-items: center;
      margin-top: 8px;
      padding: 6px 8px;
      font-size: 12px;
      color: var(--td-error-color);
      background-color: var(--td-error-color-1);
      border-radius: var(--td-radius-default);
    }
  }

  // ---------- ② 工作任务卡 ----------
  // 10 行 × 约 40px + 卡头/统计条 ≈ 470px，正好一屏；行数少时不设固定高度，随内容缩短。
  &__tasks {
    height: auto;

    // 与公告与消息卡同一套内边距
    :deep(.t-card__body) {
      padding: 6px 12px 12px;
    }

    &-skeleton {
      display: flex;
      flex-direction: column;
      gap: 10px;
      padding: 8px 4px;
    }

    &-skeleton-row {
      height: 13px;
      background: linear-gradient(
        90deg,
        var(--td-bg-color-container-hover) 25%,
        var(--td-bg-color-secondarycontainer) 37%,
        var(--td-bg-color-container-hover) 63%
      );
      background-size: 400% 100%;
      border-radius: var(--td-radius-default);
      animation: wb-shimmer 1.4s ease infinite;

      &:nth-child(2n) {
        width: 72%;
      }
    }

    &-list {
      margin: 0;
      padding: 0;
      list-style: none;
    }

    // 整行可点：button 重置成文本行，键盘 Tab 也能到达
    &-item {
      display: flex;
      gap: 8px;
      align-items: flex-start;
      width: 100%;
      padding: 6px;
      font: inherit;
      color: inherit;
      text-align: left;
      cursor: pointer;
      background: none;
      border: 0;
      border-radius: var(--td-radius-default);
      transition: background-color 0.15s ease;

      &:hover {
        background-color: var(--td-bg-color-container-hover);
      }

      &:focus-visible {
        outline: 2px solid var(--td-brand-color);
        outline-offset: -2px;
      }
    }

    // 状态图标：待办空心圆 / 进行中实心主题色 / 待验收橙色空心 / 已完成勾选 / 已取消虚线灰
    &-state {
      display: inline-flex;
      flex: none;
      align-items: center;
      justify-content: center;
      box-sizing: border-box;
      width: 14px;
      height: 14px;
      margin-top: 3px;
      font-size: 10px;
      line-height: 1;
      color: var(--td-text-color-white, #fff);
      border: 1.5px solid var(--td-text-color-placeholder);
      border-radius: 50%;

      &--doing {
        background-color: var(--td-brand-color);
        border-color: var(--td-brand-color);
      }

      &--review {
        border-color: var(--td-warning-color);
      }

      &--done {
        background-color: var(--td-success-color);
        border-color: var(--td-success-color);
      }

      &--cancelled {
        border-style: dashed;
        border-color: var(--td-text-color-disabled);
      }
    }

    &-main {
      display: flex;
      flex: 1 1 auto;
      flex-direction: column;
      gap: 1px;
      min-width: 0;
    }

    &-title {
      overflow: hidden;
      font-size: 13px;
      line-height: 1.45;
      color: var(--td-text-color-primary);
      text-overflow: ellipsis;
      white-space: nowrap;
    }

    &-meta {
      display: flex;
      gap: 6px;
      align-items: center;
      min-width: 0;
      font-size: 12px;
      line-height: 1.4;
      color: var(--td-text-color-placeholder);
      font-variant-numeric: tabular-nums;
    }

    &-due {
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;

      // 逾期标红：扫一眼就知道哪几条已经欠着
      &--overdue {
        font-weight: 600;
        color: var(--td-error-color);
      }
    }

    // 优先级标签推到行尾，与截止时间左右分栏
    &-tag {
      flex: none;
      margin-left: auto;
      font-variant-numeric: tabular-nums;
    }

    &-tag-dot {
      display: inline-block;
      width: 6px;
      height: 6px;
      margin-right: 3px;
      border-radius: 50%;
      vertical-align: middle;
    }

    // 已完成：整行置灰 + 标题删除线（勾选图标由 state--done 提供）
    &-item--done {
      .wb__tasks-title {
        color: var(--td-text-color-placeholder);
        text-decoration: line-through;
      }
    }

    &-empty {
      display: flex;
      flex-direction: column;
      gap: 8px;
      align-items: center;
      padding: 22px 0 18px;
      font-size: 12px;
      color: var(--td-text-color-placeholder);
    }

    &-foot {
      display: flex;
      gap: 6px;
      align-items: center;
      margin-top: 8px;
      padding: 8px 4px 0;
      font-size: 12px;
      color: var(--td-text-color-placeholder);
      border-top: 1px solid var(--td-component-stroke);
      font-variant-numeric: tabular-nums;
    }

    &-foot-sep {
      color: var(--td-text-color-placeholder);
    }

    &-error {
      display: flex;
      gap: 6px;
      align-items: center;
      margin-top: 8px;
      padding: 6px 8px;
      font-size: 12px;
      color: var(--td-error-color);
      background-color: var(--td-error-color-1);
      border-radius: var(--td-radius-default);
    }
  }

  // ---------- 公告 / 消息详情抽屉 ----------
  &__detail-title {
    display: flex;
    gap: 6px;
    align-items: center;
    margin: 0 0 10px;
    font-size: 17px;
    font-weight: 600;
    line-height: 1.4;
    color: var(--td-text-color-primary);
  }

  &__detail-pin {
    flex: none;
    color: var(--td-brand-color);
  }

  &__detail-meta {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
    align-items: center;
    padding-bottom: 12px;
    margin-bottom: 14px;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
    border-bottom: 1px solid var(--td-component-stroke);
  }

  &__detail-read {
    color: var(--td-success-color);
  }

  &__detail-body {
    font-size: 14px;
    line-height: 1.75;
    color: var(--td-text-color-primary);
    word-break: break-word;

    :deep(img) {
      max-width: 100%;
    }
  }

  &__detail-link {
    margin-top: 16px;
    font-size: 13px;
  }

  &__detail-files {
    margin-top: 20px;
    padding-top: 12px;
    border-top: 1px solid var(--td-component-stroke);
  }

  &__detail-files-title {
    margin-bottom: 8px;
    font-size: 13px;
    color: var(--td-text-color-secondary);
  }

  &__detail-file {
    display: flex;
    gap: 8px;
    align-items: center;
    width: 100%;
    padding: 8px 10px;
    margin-bottom: 6px;
    font: inherit;
    color: var(--td-text-color-primary);
    text-align: left;
    cursor: pointer;
    background-color: var(--td-bg-color-container-hover);
    border: 0;
    border-radius: var(--td-radius-default);
    transition: background-color 0.15s ease;

    &:hover {
      background-color: var(--td-bg-color-secondarycontainer);
    }
  }

  &__detail-file-name {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    font-size: 13px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  &__detail-file-size {
    flex: none;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
    font-variant-numeric: tabular-nums;
  }

  // ---------- ④ 快捷入口 ----------
  // CSS Grid：列数由内联变量 `--wb-entry-cols` 给（= min(5, 可用入口数)），
  // 窄屏在媒体查询里直接改 grid-template-columns（不能改内联变量 —— 内联优先级更高）。
  &__entries {
    display: grid;
    grid-template-columns: repeat(var(--wb-entry-cols, 5), minmax(0, 1fr));
    gap: 12px;

    @media (max-width: 1199px) {
      grid-template-columns: repeat(3, minmax(0, 1fr));
    }

    @media (max-width: 767px) {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }

  &__entry {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
    min-width: 0;
    padding: 16px 12px;
    cursor: pointer;
    background-color: var(--td-bg-color-container-hover);
    border: 1px solid transparent;
    border-radius: 8px;
    transition: all 0.2s ease;

    &:hover {
      color: var(--td-brand-color);
      background-color: var(--td-brand-color-light);
      border-color: var(--td-brand-color-light-active);
      transform: translateY(-2px);
    }

    &:focus-visible {
      outline: 2px solid var(--td-brand-color);
      outline-offset: -2px;
    }
  }

  &__entry-icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 38px;
    height: 38px;
    font-size: 20px;
    color: var(--td-brand-color);
    background-color: var(--td-bg-color-container);
    border-radius: 10px;
  }

  &__entry-title {
    font-size: 14px;
    color: var(--td-text-color-primary);
    text-align: center;
  }
}
</style>
