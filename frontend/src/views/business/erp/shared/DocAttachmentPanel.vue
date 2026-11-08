<!--
  单据附件面板（ARCH-3 DC-17 / P1-4；ATT-03 列表增强；**ATT-04 按 file_role 分栏**）

  在五个单据详情抽屉里复用同一个组件：合同 / 采购单 / 收货单 / 应付单（付款相关）/ 报销单。
  存储与策略全部交给后端（`business/storage`）——本组件**只**管「选文件 + 传 + 列 + 删 + 排」。

  # ATT-04：为什么按角色分栏

  所有附件混在一张表里时，「合同扫描件到底传了没有」必须逐行看文件名才能回答。
  分栏之后每个角色一个分组标题（中文名由后端下发），**空分组也画出来**并显示
  「暂无 XX，点击上传」—— 一眼能看出缺哪一份。

  # 分组的三个关键决定

  1. **分组清单来自接口**（`list.roles` / `sort.roles`），不是前端遍历 items 推的：
     空分组恰好不在数据里，遍历推不出来；前端自己维护词表则必然与后端漂移。
     老实例不返回 `roles` 时回落到本地 `FILE_ROLE_OPTIONS`（`resolveRoleCatalog`）。
  2. **分组规则在 `components/attachment-roles-pure.ts`**（纯函数 + 单测）：
     未知 / 兼容角色并入 `DEFAULT`（附件绝不凭空消失），词表顺序 = 渲染顺序。
  3. **组内上移 / 下移换算回整单顺序**再提交（后端 `PUT /attachment/sort` 要求完整
     id 列表）：`moveWithinGroup` 返回「分组顺序 × 组内顺序」的平铺结果，与落库的
     `sort_order` 口径一致 —— 界面上看到什么，库里就存什么。

  # ATT-03 列表口径（不变）

  - 列：类型图标 | 文件名 | 大小（B/KB/MB）| 上传人 | 上传时间 | 备注 | 操作；
  - 默认按上传时间倒序；切到「手动排序」后每行给上移 / 下移，写 `sort_order` 落库；
  - 大小格式化与图标映射在 `components/attachment-panel-pure.ts`（同一份显示口径只有一个来源）。

  # 多角色上传

  每个分组头有「上传」按钮、空分组的引导文案本身也是上传入口、分组头还是拖拽落点，
  上传即绑定**该分组**的角色（不必先去下拉里选）；顶部工具条的下拉 + 拖拽区用于
  「先选角色再拖一批」。一个角色可以一次选多个文件（`multiple`），逐个上传。

  # 删除按钮

  显示与否完全由后端 `can_delete` 决定（上传人本人 ∪ `business:attachment:delete`），
  前端不自己算一遍权限 —— 两处判据必然漂移，最后就是「按钮在、点了 403」。
  删除是**软删**（后端写 `deleted_at` + `operation_logs`），页面只是不再出现这一行。

  # ATT-05：Office 只下载 + 单文件 20 MB

  - **「预览」按钮只在图片 / PDF 出现**（判据 `row.display.previewable`，来自
    `attachment-panel-pure.describeAttachment`，面板不自己再判扩展名）；
    Office（doc/docx/xls/xlsx/ppt/pptx/wps/et/dps）/ 压缩包 / 其他只有「下载」。
  - **下载完弹引导**：Office 提示「已下载到文件夹，请用本地 Office/WPS 打开」
    （`attachment-upload-pure.downloadHintFor`，压缩包 / 其他各有自己的文案；图片 / PDF 不弹）。
  - **单文件 20 MB**：前端在发请求前按 `partitionFilesBySize` 分流，超限的一个都不发，
    弹一条汇总提示「文件超过 20MB，请压缩或分卷上传」；后端 `dto::MAX_FILE_BYTES` 同口径
    （绕过前端直传 → 400 + 同一句话）。
  - **图标配色**：Word 蓝 / Excel 绿 / PPT 橙 / WPS 红（`.wps` / `.et` / `.dps` 走红，
    形状图标沿用对应家族）。
-->
<template>
  <div class="doc-attach">
    <div class="doc-attach__head">
      <span class="doc-attach__title">附件</span>
      <span class="doc-attach__hint">已上传 {{ total }} / {{ maxCount }}</span>
      <span class="doc-attach__grow" />
      <t-radio-group
        v-if="docId && items.length > 0"
        v-model="sortMode"
        size="small"
        variant="default-filled"
        :data-testid="`${testidPrefix}-sort`"
        @change="onSortModeChange"
      >
        <t-radio-button value="time">按上传时间</t-radio-button>
        <t-radio-button value="manual">手动排序</t-radio-button>
      </t-radio-group>
    </div>

    <div v-if="!docId" class="doc-attach__empty">保存单据后可上传附件</div>

    <template v-else>
      <div class="doc-attach__toolbar">
        <t-select
          v-model="role"
          size="small"
          :options="roleOptions"
          class="doc-attach__role"
          :data-testid="`${testidPrefix}-role`"
        />
        <t-input
          v-model="remark"
          size="small"
          placeholder="备注（可空，≤200 字）"
          class="doc-attach__remark-input"
          :maxlength="200"
          :data-testid="`${testidPrefix}-remark`"
        />
      </div>

      <div
        class="doc-attach__drop"
        :class="{ 'doc-attach__drop--over': dragOver }"
        :data-testid="`${testidPrefix}-drop`"
        @dragover.prevent="dragOver = true"
        @dragleave.prevent="dragOver = false"
        @drop.prevent="onDrop"
        @click="pickFiles(role)"
      >
        <t-icon name="upload" size="18px" />
        <span>
          拖拽文件到此处，或点击选择（按上方角色上传；单文件上限 {{ MAX_ATTACHMENT_FILE_MB }} MB，
          超过请压缩或分卷；格式按存储策略）
        </span>
        <input
          ref="fileInputRef"
          type="file"
          multiple
          class="doc-attach__input"
          @change="onFilePicked"
        />
      </div>

      <div v-if="loading" class="doc-attach__empty">加载中…</div>
      <template v-else>
        <!-- 分组（ATT-04）：每个角色一个分组，空分组也显示（卡面验收②） -->
        <div
          v-for="group in groups"
          :key="group.code"
          class="doc-attach__group"
          :data-testid="`${testidPrefix}-group-${group.code}`"
        >
          <div
            class="doc-attach__group-head"
            :class="{ 'doc-attach__group-head--over': dragOverGroup === group.code }"
            @dragover.prevent="dragOverGroup = group.code"
            @dragleave.prevent="dragOverGroup = null"
            @drop.prevent="onGroupDrop($event, group.code)"
          >
            <span class="doc-attach__group-title" :data-testid="`${testidPrefix}-group-title`">
              {{ group.label }}
            </span>
            <t-tag size="small" variant="light" :theme="group.count > 0 ? 'primary' : 'default'">
              {{ group.count }}
            </t-tag>
            <span class="doc-attach__grow" />
            <t-button
              size="small"
              variant="text"
              :disabled="total >= maxCount"
              :data-testid="`${testidPrefix}-upload-${group.code}`"
              @click="pickFiles(group.code)"
            >
              上传
            </t-button>
          </div>

          <!-- 空分组：引导文案本身就是上传入口 -->
          <div
            v-if="group.count === 0"
            class="doc-attach__empty doc-attach__empty--role"
            :data-testid="`${testidPrefix}-empty-${group.code}`"
            @click="pickFiles(group.code)"
          >
            {{ emptyRoleHint(group.label) }}
          </div>

          <div v-else class="doc-attach__table">
            <div class="doc-attach__row doc-attach__row--head">
              <span class="doc-attach__cell doc-attach__cell--icon">类型</span>
              <span class="doc-attach__cell doc-attach__cell--name">文件名</span>
              <span class="doc-attach__cell doc-attach__cell--size">大小</span>
              <span class="doc-attach__cell doc-attach__cell--uploader">上传人</span>
              <span class="doc-attach__cell doc-attach__cell--time">上传时间</span>
              <span class="doc-attach__cell doc-attach__cell--remark">备注</span>
              <span class="doc-attach__cell doc-attach__cell--actions">操作</span>
            </div>

            <div
              v-for="(row, index) in group.items"
              :key="row.item.id"
              class="doc-attach__item doc-attach__row"
              :data-testid="`${testidPrefix}-item`"
            >
              <span
                class="doc-attach__cell doc-attach__cell--icon"
                :title="row.display.kindLabel"
                :data-testid="`${testidPrefix}-icon`"
              >
                <t-icon :name="row.display.icon" :color="iconColor(row.display.theme)" size="18px" />
              </span>
              <span class="doc-attach__cell doc-attach__cell--name" :title="row.item.file_name">
                <span class="doc-attach__name">{{ row.item.file_name }}</span>
              </span>
              <span class="doc-attach__cell doc-attach__cell--size" :data-testid="`${testidPrefix}-size`">
                {{ row.display.sizeLabel }}
              </span>
              <span class="doc-attach__cell doc-attach__cell--uploader">
                {{ row.item.uploader_name || '—' }}
              </span>
              <span class="doc-attach__cell doc-attach__cell--time">
                {{ formatDateTime(row.item.created_at) }}
              </span>
              <span
                class="doc-attach__cell doc-attach__cell--remark"
                :title="row.item.remark || ''"
              >{{ row.item.remark || '—' }}</span>
              <span class="doc-attach__cell doc-attach__cell--actions">
                <template v-if="sortMode === 'manual'">
                  <t-button
                    size="small"
                    variant="text"
                    :disabled="index === 0"
                    :data-testid="`${testidPrefix}-up`"
                    @click="moveItem(group.code, index, -1)"
                  >
                    上移
                  </t-button>
                  <t-button
                    size="small"
                    variant="text"
                    :disabled="index === group.items.length - 1"
                    :data-testid="`${testidPrefix}-down`"
                    @click="moveItem(group.code, index, 1)"
                  >
                    下移
                  </t-button>
                </template>
                <!-- ATT-05：只有图片 / PDF 给「预览」；Office / 压缩包 / 其他一律只下载 -->
                <t-button
                  v-if="row.display.previewable"
                  size="small"
                  variant="text"
                  :data-testid="`${testidPrefix}-preview`"
                  @click="openPreview(row.item)"
                >
                  预览
                </t-button>
                <t-button
                  size="small"
                  variant="text"
                  :data-testid="`${testidPrefix}-download`"
                  @click="download(row.item)"
                >
                  下载
                </t-button>
                <t-button
                  v-if="row.item.can_delete"
                  size="small"
                  variant="text"
                  theme="danger"
                  @click="remove(row.item)"
                >
                  删除
                </t-button>
              </span>
            </div>
          </div>
        </div>
      </template>
    </template>

    <!-- ATT-01：统一附件预览（本单据的全部附件可左右切换；Office/压缩包只给下载） -->
    <attachment-preview
      v-model:visible="previewVisible"
      :items="previewItems"
      :index="previewIndex"
      title="附件预览"
    />
  </div>
</template>

<script setup lang="ts">
/**
 * 单据附件面板（ATT-04：按 `file_role` 分栏；ATT-03：列增强 + 排序 + 软删）。
 *
 * # props
 *
 * - `docType`：单据类型（决定后端校验哪张表是否存在）；
 * - `docId`：单据主键（详情抽屉打开后才有值，空值时不发请求）；
 * - `defaultRole`：首次上传默认选中的角色（各页面按单据类型给，缺省 `DEFAULT`）；
 * - `testidPrefix`：e2e 定位前缀（五个页面各不同）。
 *
 * # 事件
 *
 * - `changed`：附件增删 / 重排后触发（参数为当前条数）。
 *
 * # 排序状态为什么不放在 store
 *
 * 排序口径（`time` / `manual`）是**这张单据的当前视图偏好**，不是全局设置：
 * 用户看合同喜欢按时间、看应付单喜欢手排，混在一个全局开关里反而更烦。
 * 组件卸载即回落默认（时间倒序），刷新后按 `time` 重新拉 —— 后端 `manual`
 * 只影响「顺序」，不影响「有没有删除」。
 *
 * # 上传角色为什么按分组走
 *
 * 卡面要求「支持多个 role 同时上传」：每个分组头 / 空分组引导 / 分组拖拽都能直接往
 * 该角色上传，不必先回顶部改下拉。工具条的下拉只用于「顶部拖拽区」这个批量入口。
 */
import { computed, ref, watch } from 'vue';
import { DialogPlugin, MessagePlugin } from 'tdesign-vue-next';

import {
  deleteDocAttachment,
  downloadDocAttachment,
  listDocAttachments,
  saveBlob,
  sortDocAttachments,
  uploadDocAttachment,
  FILE_ROLE_OPTIONS,
  type DocAttachmentItem,
  type DocAttachmentRoleGroup,
  type DocAttachmentSortMode,
  type DocType,
  type FileRole,
} from '@/api/business/erp/attachment';
import AttachmentPreview from '@/components/AttachmentPreview.vue';
import type { PreviewItem } from '@/components/attachment-preview-pure';
import { describeAttachment } from '@/components/attachment-panel-pure';
import {
  emptyRoleHint,
  flattenGroups,
  groupByRole,
  moveWithinGroup,
  resolveRoleCatalog,
  type RoleOption,
} from '@/components/attachment-roles-pure';
import {
  downloadHintFor,
  MAX_ATTACHMENT_FILE_MB,
  partitionFilesBySize,
  tooLargeMessage,
} from '@/components/attachment-upload-pure';
import { ApiError } from '@/api/request';
import { formatDateTime } from '@/utils/format';

const props = withDefaults(
  defineProps<{
    docType: DocType;
    docId?: string | null;
    defaultRole?: FileRole;
    /** data-testid 前缀（五个页面各不同，便于 e2e 定位） */
    testidPrefix?: string;
  }>(),
  { docId: null, defaultRole: 'DEFAULT', testidPrefix: 'doc-attach' },
);

const emit = defineEmits<{ (event: 'changed', total: number): void }>();

const loading = ref(false);
const dragOver = ref(false);
/** 当前有拖拽悬停的分组（只影响该分组头的高亮） */
const dragOverGroup = ref<string | null>(null);
const items = ref<DocAttachmentItem[]>([]);
const total = ref(0);
const maxCount = ref(50);
const role = ref<FileRole>(props.defaultRole);
const remark = ref('');
const sortMode = ref<DocAttachmentSortMode>('time');
const fileInputRef = ref<HTMLInputElement | null>(null);
/** 本次点击选文件要用的角色（分组上传时由 `pickFiles` 预设） */
const pendingRole = ref<FileRole>(props.defaultRole);
/** 后端下发的分组口径（老实例没给时为空 → 回落本地词表） */
const backendRoles = ref<DocAttachmentRoleGroup[]>([]);

const roleOptions = computed(() =>
  FILE_ROLE_OPTIONS.map((option) => ({ label: option.label, value: option.value })),
);

/**
 * 本地分组词表（`resolveRoleCatalog` 的回落值）。
 *
 * `FILE_ROLE_OPTIONS` 是**下拉选项**（`{ value, label }`，TDesign `t-select` 要 `value`），
 * 分组纯逻辑要的是 `{ code, label }` —— 这里转一次，不在纯函数里塞两套字段名。
 */
const LOCAL_ROLE_CATALOG: RoleOption[] = FILE_ROLE_OPTIONS.map((option) => ({
  code: option.value,
  label: option.label,
}));

/** 渲染用的分组清单：后端口径优先，老实例回落本地词表 */
const catalog = computed(() => resolveRoleCatalog(backendRoles.value, LOCAL_ROLE_CATALOG));

/**
 * 每行的派生展示信息（图标 / 类型名 / 大小文案）+ 分组用的角色字段。
 *
 * 一次算好放进 computed：模板里每行调用 3 次函数会在每次输入框输入（`remark` 变化）
 * 时重算整表 —— 附件不多但这属于白花的重渲染。
 */
const rows = computed(() =>
  items.value.map((item) => ({
    item,
    file_role: item.file_role,
    display: describeAttachment(item.file_name, item.mime_type, item.file_size),
  })),
);

/** 分栏结果（顺序 = 词表顺序；空分组也在） */
const groups = computed(() => groupByRole(rows.value, catalog.value));

/** 图标色：TDesign 的主题色变量，避免写死 hex（换主题时会跟不过去） */
function iconColor(theme: 'danger' | 'primary' | 'success' | 'warning' | 'default'): string {
  switch (theme) {
    case 'danger':
      return 'var(--td-error-color)';
    case 'primary':
      return 'var(--td-brand-color)';
    case 'success':
      return 'var(--td-success-color)';
    case 'warning':
      return 'var(--td-warning-color)';
    default:
      return 'var(--td-text-color-placeholder)';
  }
}

/** 拉列表（单据不存在 / 无权限时后端给 404 / 403，这里只提示不阻断详情页） */
async function load(): Promise<void> {
  if (!props.docId) {
    items.value = [];
    total.value = 0;
    return;
  }
  loading.value = true;
  try {
    const result = await listDocAttachments(props.docType, props.docId, sortMode.value);
    items.value = result.list;
    total.value = result.total;
    maxCount.value = result.max_count;
    backendRoles.value = result.roles ?? [];
  } catch (error) {
    items.value = [];
    total.value = 0;
    MessagePlugin.warning(error instanceof ApiError ? error.message : '附件加载失败');
  } finally {
    loading.value = false;
  }
}

watch(
  () => [props.docType, props.docId] as const,
  () => {
    role.value = props.defaultRole;
    pendingRole.value = props.defaultRole;
    sortMode.value = 'time';
    void load();
  },
  { immediate: true },
);

/** 切排序口径：重新按该口径拉列表（手动口径的顺序只有后端知道） */
function onSortModeChange(): void {
  void load().then(() => emit('changed', total.value));
}

/** 打开文件选择框（`roleCode` 决定这次选的文件落到哪个分组） */
function pickFiles(roleCode: string): void {
  role.value = roleCode as FileRole;
  pendingRole.value = roleCode as FileRole;
  fileInputRef.value?.click();
}

/** 顶部拖拽区：按工具条选中的角色上传 */
function onDrop(event: DragEvent): void {
  dragOver.value = false;
  const files = event.dataTransfer?.files;
  if (files && files.length > 0) {
    void upload([...files], role.value);
  }
}

/** 分组头拖拽：直接落到该分组的角色（支持多个 role 同时上传） */
function onGroupDrop(event: DragEvent, roleCode: string): void {
  dragOverGroup.value = null;
  const files = event.dataTransfer?.files;
  if (files && files.length > 0) {
    void upload([...files], roleCode as FileRole);
  }
}

function onFilePicked(event: Event): void {
  const input = event.target as HTMLInputElement;
  const files = input.files ? [...input.files] : [];
  input.value = '';
  if (files.length > 0) {
    void upload(files, pendingRole.value);
  }
}

/**
 * 上传一批文件到**同一个角色**（策略校验 / 配额都在后端，前端只做「到上限就别再发」的省流量检查）。
 *
 * # ATT-05：单文件 20 MB 前端先拦
 *
 * 超限文件**一个都不发**（不是发出去等 400）：提示文案与后端逐字一致
 * （`FILE_TOO_LARGE_MESSAGE`），一次批量选 10 个只弹一条汇总，不刷屏。
 * 后端同样拦（`dto::MAX_FILE_BYTES`），绕过前端 curl 直传会拿到 400 + 同一句话 ——
 * 前端拦截是体验，后端拦截才是规则。
 *
 * 逐个串行上传：并发多文件时后端每个请求都要校验单据 + 配额，串行更容易看清失败的是哪一个，
 * 也不会让「内容秒传」的判据在并发下互相竞争。
 */
async function upload(files: File[], roleCode: FileRole): Promise<void> {
  if (!props.docId || files.length === 0) {
    return;
  }
  // ATT-05：先按 20 MB 分流（超限的连请求都不发）
  const { accepted: withinSize, rejected } = partitionFilesBySize(files);
  const tooLarge = tooLargeMessage(rejected);
  if (tooLarge) {
    MessagePlugin.warning(tooLarge);
  }
  if (withinSize.length === 0) {
    return;
  }

  const room = maxCount.value - total.value;
  if (room <= 0) {
    MessagePlugin.warning(`单张单据最多 ${maxCount.value} 个附件，请先删除不再需要的`);
    return;
  }
  const accepted = withinSize.slice(0, room);
  if (accepted.length < withinSize.length) {
    MessagePlugin.warning(`一次最多还能上传 ${room} 个（单张单据上限 ${maxCount.value}）`);
  }

  loading.value = true;
  let uploaded = 0;
  const failures: string[] = [];
  try {
    for (const file of accepted) {
      try {
        await uploadDocAttachment(props.docType, props.docId, file, roleCode, remark.value);
        uploaded += 1;
      } catch (error) {
        failures.push(`${file.name}：${error instanceof ApiError ? error.message : '上传失败'}`);
      }
    }
  } finally {
    remark.value = '';
    await load();
    loading.value = false;
    emit('changed', total.value);
  }

  if (uploaded > 0) {
    const label =
      FILE_ROLE_OPTIONS.find((option) => option.value === roleCode)?.label ??
      (roleCode === 'SIGN_RECEIPT' ? '签收单' : roleCode);
    MessagePlugin.success(`已上传 ${uploaded} 个文件到「${label}」`);
  }
  if (failures.length > 0) {
    MessagePlugin.error(failures.join('；'));
  }
}

/**
 * 下载（ATT-05：Office / 压缩包 / 其他下载完提示「用本地软件打开」）。
 *
 * # 为什么提示是「下载完才弹」
 *
 * 用户拍板：**不做在线预览**，Office 文档只能落到本地再用 Office/WPS 打开。
 * 提示必须出现在「点完之后」——否则用户会以为点了没反应（浏览器下载不一定抢焦点），
 * 或者还在等页面里打开。图片 / PDF 可预览，下载只是另存一份，不弹提示（
 * `downloadHintFor` 返回 `null`）。
 */
async function download(item: DocAttachmentItem): Promise<void> {
  try {
    const blob = await downloadDocAttachment(item.id);
    saveBlob(blob, item.file_name);
    const hint = downloadHintFor(item.file_name, item.mime_type);
    if (hint) {
      // 用 info（不是 success）：这是一句「接下来怎么打开」的引导，不是「操作成功」；
      // 5 秒（比默认 3 秒长一点）：用户可能正在浏览器的下载栏里找文件，太短会看漏。
      MessagePlugin.info(hint, 5000);
    }
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '下载失败');
  }
}

/**
 * 组内上移 / 下移一行（`delta` = -1 / +1）。
 *
 * 顺序是**整单提交**（后端要求完整 id 列表）：先把「组内换位」换算成整单平铺顺序
 * （`moveWithinGroup`：分组顺序 × 组内顺序），本地乐观换位让点按立即有反馈，
 * 请求成功后用后端回读的列表覆盖（序号已被重排成 0..n-1）；失败则回滚到服务端顺序。
 */
async function moveItem(code: string, index: number, delta: number): Promise<void> {
  if (!props.docId) {
    return;
  }
  const next = moveWithinGroup(rows.value, catalog.value, code, index, index + delta).map(
    (row) => row.item,
  );
  const ids = next.map((row) => row.id);
  if (ids.join(',') === items.value.map((row) => row.id).join(',')) {
    return;
  }
  items.value = next;
  try {
    const result = await sortDocAttachments(props.docType, props.docId, ids);
    items.value = result.list;
    total.value = result.total;
    backendRoles.value = result.roles ?? backendRoles.value;
    sortMode.value = 'manual';
    emit('changed', total.value);
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '保存排序失败');
    await load();
  }
}

/** 预览弹窗是否显示 */
const previewVisible = ref(false);
/** 预览初始定位（点哪个附件就从哪个开始，左右切换走同一张单据内的分组顺序） */
const previewIndex = ref(0);

/**
 * 预览列表（顺序 = 界面上的分组顺序 × 组内顺序）。
 *
 * 注意下载入口是**绑定行 id**（`item.id`），不是 `file_id` —— 同一份字节被多个单据
 * 秒传复用时 `file_id` 相同，只有绑定行才知道「当前用户看的是哪张单据的附件」，
 * 权限判据也在绑定行上。
 */
const previewItems = computed<PreviewItem[]>(() =>
  flattenGroups(groups.value).map((row) => ({
    key: row.item.id,
    fileId: row.item.file_id,
    name: row.item.file_name,
    size: row.item.file_size,
    mimeType: row.item.mime_type,
    uploader: row.item.uploader_name,
    uploadedAt: row.item.created_at,
    remark: row.item.remark,
    tag: row.item.file_role_label,
    load: () => downloadDocAttachment(row.item.id),
  })),
);

function openPreview(item: DocAttachmentItem): void {
  const index = flattenGroups(groups.value).findIndex((row) => row.item.id === item.id);
  previewIndex.value = index >= 0 ? index : 0;
  previewVisible.value = true;
}

function remove(item: DocAttachmentItem): void {
  const dialog = DialogPlugin.confirm({
    header: '删除附件',
    body: `确认删除「${item.file_name}」？删除后列表不再显示，文件记录标记删除（物理对象延迟回收），操作会记入审计日志。`,
    theme: 'warning',
    confirmBtn: { content: '删除', theme: 'danger' },
    onConfirm: async () => {
      try {
        await deleteDocAttachment(item.id);
        dialog.destroy();
        await load();
        emit('changed', total.value);
        MessagePlugin.success('附件已删除');
      } catch (error) {
        MessagePlugin.error(error instanceof ApiError ? error.message : '删除失败');
      }
    },
  });
}

defineExpose({ reload: load });
</script>

<style scoped>
.doc-attach {
  padding: 4px 0;
}

.doc-attach__head {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 6px;
}

.doc-attach__grow {
  flex: 1;
}

.doc-attach__title {
  font-weight: 600;
  font-size: 14px;
}

.doc-attach__hint {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.doc-attach__toolbar {
  display: flex;
  gap: 8px;
  margin-bottom: 8px;
}

.doc-attach__role {
  width: 150px;
}

.doc-attach__remark-input {
  flex: 1;
  max-width: 320px;
}

.doc-attach__drop {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 12px 14px;
  border: 1px dashed var(--td-component-border);
  border-radius: 6px;
  color: var(--td-text-color-secondary);
  cursor: pointer;
  transition: border-color 0.2s;
}

.doc-attach__drop--over {
  border-color: var(--td-brand-color);
  color: var(--td-brand-color);
}

.doc-attach__input {
  display: none;
}

.doc-attach__empty {
  padding: 10px 2px;
  color: var(--td-text-color-placeholder);
  font-size: 13px;
}

/* 空分组的上传引导：整块可点，视觉上像虚线放置区而不是「没数据」 */
.doc-attach__empty--role {
  padding: 10px 12px;
  margin: 2px 0 6px;
  border: 1px dashed var(--td-component-border);
  border-radius: 4px;
  cursor: pointer;
  transition: color 0.2s, border-color 0.2s;
}

.doc-attach__empty--role:hover {
  color: var(--td-brand-color);
  border-color: var(--td-brand-color);
}

.doc-attach__group {
  margin-top: 10px;
}

.doc-attach__group-head {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 4px 8px;
  border-radius: 4px;
  background: var(--td-bg-color-container-hover);
  outline: 1px dashed transparent;
  transition: outline-color 0.2s;
}

.doc-attach__group-head--over {
  outline-color: var(--td-brand-color);
}

.doc-attach__group-title {
  font-weight: 600;
  font-size: 13px;
}

.doc-attach__table {
  margin-top: 4px;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

/* 列宽与表头同源：grid-template-columns 只写一处，表头 / 数据行共用一个 class */
.doc-attach__row {
  display: grid;
  grid-template-columns:
    34px minmax(0, 1fr) 76px 88px 136px minmax(0, 140px) auto;
  align-items: center;
  gap: 8px;
  padding: 6px 8px;
  border-radius: 4px;
  font-size: 13px;
}

.doc-attach__row--head {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
  padding-bottom: 2px;
}

.doc-attach__item {
  background: var(--td-bg-color-container-hover);
}

.doc-attach__cell {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.doc-attach__cell--icon {
  display: flex;
  justify-content: center;
}

.doc-attach__cell--name {
  display: flex;
  align-items: center;
  gap: 6px;
}

.doc-attach__cell--size,
.doc-attach__cell--time,
.doc-attach__cell--uploader {
  color: var(--td-text-color-placeholder);
}

.doc-attach__cell--remark {
  color: var(--td-text-color-secondary);
}

.doc-attach__cell--actions {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  white-space: nowrap;
}

.doc-attach__name {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
</style>
