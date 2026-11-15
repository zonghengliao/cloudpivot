<!--
  用户管理（system/user）。

  ```
  左：部门树（选中 → 按部门子树筛用户）
  右：筛选行（用户名 / 姓名 / 状态）+ 用户列表 + 行操作（编辑 / 分配角色 / 重置密码 / 删除）
  ```

  # 列表部分（P3-11）
  右侧列表的筛选 / 分页 / 空态 / 行操作 / 加载失败全部交给 `components/ListPage`。
  本页只声明「列 + 筛选项 + 怎么拉一页 + 入参怎么拼」。

  # 部门树不是筛选项，是「第二套入口」
  左树选中写进**同一份筛选状态**（`filter.dept_id`），列表请求带上
  `include_children=true`（点部门要连子部门的人一起看）。所以：

  - `dept_id` 刻意**不**声明成 `FilterDef`：它没有筛选行控件（控件在左树），
    声明了反而会多出一个空槽位；
  - 「重置」时组件清的是声明的三个键，`dept_id` 与树的选中态由页面自己一并清
    （见 `handleReset`），否则会出现「列表回到全量、树还高亮着某个部门」。

  # AI 用户（云枢 × Hermes）
  `user_type='ROBOT'` 的账号：列表带机器人图标 + 类型标签；删除必须走专用接口
  （通用删除只软删 `sys_user`，会留下孤立 profile）；编辑态的职责 / SOUL.md / 模型
  走 AI 专用接口与探活。
-->
<template>
  <div class="user-page">
    <div class="user-layout">
      <!-- 左侧：部门树形列表，选中部门后按部门筛选用户 -->
      <div class="user-left">
        <t-card :bordered="false" class="dept-card">
          <div class="dept-toolbar">
            <span class="dept-title">组织架构</span>
            <t-button size="small" variant="text" data-testid="user-dept-refresh" @click="loadDeptTree">
              <template #icon><t-icon name="refresh" /></template>
            </t-button>
          </div>
          <div class="dept-body">
            <t-loading :loading="deptLoading" size="small" text="加载中...">
              <t-tree
                :data="deptTree"
                :keys="{ value: 'id', label: 'dept_name', children: 'children' }"
                :active="activeDeptKeys"
                :expand-all="true"
                activable
                hover
                @active="onDeptActive"
              />
              <div v-if="!deptLoading && deptTree.length === 0" class="dept-empty">暂无部门</div>
            </t-loading>
          </div>
        </t-card>
      </div>

      <!-- 右侧：筛选 + 列表（P3-11：全部交给 ListPage） -->
      <div class="user-right">
        <ListPage
          ref="listRef"
          title="用户"
          testid="system-user"
          :columns="columns"
          :filters="filters"
          :filter-state="filter"
          :fetcher="fetcher"
          :query-builder="queryBuilder"
          :page-size="PAGE_SIZE"
          @reset="handleReset"
        >
          <template #toolbar>
            <t-button
              v-privilege="['system:user:create']"
              theme="primary"
              data-testid="system-user-create"
              @click="openCreate"
            >
              <template #icon><t-icon name="add" /></template>
              新增用户
            </t-button>
          </template>

          <!-- 按部门筛选的入口在左树：把「当前在看哪个部门」显示在筛选行里，避免用户忘了自己选过 -->
          <template #filter-footer>
            <div v-if="activeDeptName" class="dept-chip" data-testid="system-user-dept-chip">
              <t-tag theme="primary" variant="light" size="small">部门：{{ activeDeptName }}（含子部门）</t-tag>
              <t-link theme="primary" size="small" data-testid="system-user-dept-clear" @click="clearDept">清除</t-link>
            </div>
          </template>

          <template #username="{ row }">
            <t-space size="4" align="center">
              <t-icon v-if="row.user_type === 'ROBOT'" name="robot" class="cell-robot" />
              <span>{{ row.username }}</span>
            </t-space>
          </template>
          <template #user_type="{ row }">
            <t-tag v-if="row.user_type === 'ROBOT'" size="small" theme="primary" variant="light">机器人</t-tag>
            <t-tag v-else size="small" variant="light">人类</t-tag>
          </template>
          <template #status="{ row }">
            <t-switch
              :value="row.status === 1"
              :disabled="!hasPermission('system:user:update')"
              @change="() => toggleStatus(row)"
            />
          </template>
          <template #last_login_at="{ row }">{{ formatDateTime(row.last_login_at) }}</template>
          <template #created_at="{ row }">{{ formatDateTime(row.created_at) }}</template>
          <template #roles="{ row }">
            <t-space v-if="row.roles && row.roles.length" size="4" break-line>
              <t-tag v-for="role in row.roles" :key="role.role_id" size="small" theme="primary" variant="light">
                {{ role.role_name }}
              </t-tag>
            </t-space>
            <span v-else class="cell-empty">—</span>
          </template>

          <!-- 行操作按状态 / 权限出现 → 页面自绘（AI 用户没有「重置密码」） -->
          <template #op="{ row }">
            <t-space size="small">
              <t-link v-privilege="['system:user:update']" theme="primary" data-testid="system-user-edit" @click="openEdit(row)">
                编辑
              </t-link>
              <t-link v-privilege="['system:user:assign']" theme="primary" data-testid="system-user-assign" @click="openAssignRoles(row)">
                分配角色
              </t-link>
              <t-link
                v-if="row.user_type !== 'ROBOT'"
                v-privilege="['system:user:reset']"
                theme="warning"
                data-testid="system-user-reset-pwd"
                @click="openResetPassword(row)"
              >
                重置密码
              </t-link>
              <t-link v-privilege="['system:user:delete']" theme="danger" data-testid="system-user-delete" @click="confirmRemove(row)">
                删除
              </t-link>
            </t-space>
          </template>
        </ListPage>
      </div>
    </div>

    <t-dialog
      v-model:visible="formVisible"
      :header="dialogTitle"
      :confirm-btn="{ content: '确定', theme: 'primary', loading: submitting }"
      :width="isAiForm ? '980px' : '560px'"
      destroy-on-close
      @confirm="submitForm"
    >
      <t-form ref="formRef" :data="form" :rules="formRules" label-width="90px">
        <!-- 账号类型：只有新建时可选；AI 用户会连带创建 Hermes profile -->
        <t-form-item v-if="!isEdit" label="账号类型" name="account_type">
          <t-radio-group v-model="form.account_type">
            <t-radio value="HUMAN">人类用户</t-radio>
            <t-radio value="ROBOT">AI 用户（自动创建 Hermes profile）</t-radio>
          </t-radio-group>
        </t-form-item>
        <t-form-item v-if="!isEdit" label="用户名" name="username">
          <div class="form-field">
            <t-input
              v-model="form.username"
              :placeholder="isAiForm ? '小写字母开头，只含小写字母/数字/连字符' : '请输入用户名'"
            />
            <div v-if="isAiForm" class="form-field__hint">
              该用户名同时是 Hermes profile 名（<code>^[a-z][a-z0-9-]&#123;1,30&#125;$</code>），创建后不可修改。
            </div>
          </div>
        </t-form-item>
        <t-form-item v-if="!isEdit && !isAiForm" label="密码" name="password">
          <t-input v-model="form.password" type="password" placeholder="请输入初始密码" />
        </t-form-item>
        <t-form-item label="姓名" name="real_name">
          <t-input v-model="form.real_name" placeholder="请输入姓名" />
        </t-form-item>

        <!-- 人类用户：沿用原有字段 -->
        <template v-if="!isAiForm">
          <t-form-item label="部门" name="dept_id">
            <dept-tree-select v-model="form.dept_id" @error="showError" />
          </t-form-item>
          <t-form-item label="角色" name="role_ids">
            <t-select
              v-model="form.role_ids"
              :options="roleOptions"
              multiple
              clearable
              filterable
              :loading="roleOptionsLoading"
              placeholder="请选择角色（可多选）"
            />
          </t-form-item>
          <t-form-item label="手机号" name="mobile">
            <t-input v-model="form.mobile" placeholder="请输入手机号" />
          </t-form-item>
          <t-form-item label="邮箱" name="email">
            <t-input v-model="form.email" placeholder="请输入邮箱" />
          </t-form-item>
          <t-form-item label="状态" name="status">
            <t-radio-group v-model="form.status">
              <t-radio :value="1">正常</t-radio>
              <t-radio :value="0">停用</t-radio>
            </t-radio-group>
          </t-form-item>
          <t-form-item label="备注" name="remark">
            <t-textarea v-model="form.remark" placeholder="请输入备注" :maxlength="200" />
          </t-form-item>
        </template>

        <!-- AI 用户：职责 + 模型 + SOUL.md（+ 编辑态 profile 状态徽标） -->
        <template v-else>
          <AiUserForm
            :mode="isEdit ? 'edit' : 'create'"
            :show-username="false"
            :show-status="isEdit"
            :username="form.username"
            :description="form.agent_description"
            :model="form.model"
            :soul="form.soul_md"
            :soul-path="aiSoulPath"
            :profile="aiProfile"
            :healthy="aiHealthy"
            :profile-exists="aiProfileExists"
            :gateway="aiGateway"
            :status-detail="aiStatusDetail"
            :status-loading="aiStatusLoading"
            @update:description="form.agent_description = $event"
            @update:model="form.model = $event"
            @update:soul="form.soul_md = $event"
            @refresh-status="refreshAiStatus"
          />
        </template>
      </t-form>
    </t-dialog>

    <t-dialog
      v-model:visible="roleVisible"
      header="分配角色"
      :confirm-btn="{ content: '确定', theme: 'primary', loading: roleSubmitting }"
      width="480px"
      destroy-on-close
      @confirm="submitRoles"
    >
      <t-form :data="roleForm" label-width="90px">
        <t-form-item label="用户">
          <span>{{ currentRow?.username }}</span>
        </t-form-item>
        <t-form-item label="角色">
          <t-select v-model="roleForm.role_ids" :options="roleOptions" multiple placeholder="请选择角色" />
        </t-form-item>
      </t-form>
    </t-dialog>

    <t-dialog
      v-model:visible="passwordVisible"
      header="重置密码"
      :confirm-btn="{ content: '确定', theme: 'primary', loading: passwordSubmitting }"
      width="460px"
      destroy-on-close
      @confirm="submitPassword"
    >
      <t-form :data="passwordForm" :rules="passwordRules" label-width="90px">
        <t-form-item label="用户">
          <span>{{ currentRow?.username }}</span>
        </t-form-item>
        <t-form-item label="新密码" name="password">
          <t-input v-model="passwordForm.password" type="password" placeholder="请输入新密码" />
        </t-form-item>
      </t-form>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin, type FormInstanceFunctions, type FormRules } from 'tdesign-vue-next';
import DeptTreeSelect from '@/components/DeptTreeSelect.vue';
import ListPage from '@/components/ListPage/index.vue';
import type { ColumnDef, FilterDef, ListFetcher, ListFilter, ListQueryBuilder } from '@/components/ListPage/types';
import AiUserForm from './components/AiUserForm.vue';
import { createAiUser, deleteAiUser, getAiUser, getAiUserStatus, updateAiUser } from '@/api/system/ai-user-api';
import {
  changeUserStatus,
  createUser,
  deleteUser,
  getUser,
  getUserRoles,
  pageUsers,
  resetUserPassword,
  updateUser,
  updateUserRoles,
  type StatusValue,
  type UserRes,
} from '@/api/system/user-api';
import { listRoles, type RoleRes } from '@/api/system/role-api';
import { getDeptTree, type DeptNode } from '@/api/system/dept-api';
import { ApiError } from '@/api/request';
import { useUserStore } from '@/stores/user';
import { formatDateTime } from '@/utils/format';

/** 新增/编辑表单 */
interface UserForm {
  /** 用户 id，新增时为空 */
  id: string;
  /** 用户名（AI 用户时同时是 Hermes profile 名） */
  username: string;
  /** 密码（AI 用户不需要） */
  password: string;
  /** 姓名 / 显示名 */
  real_name: string;
  /** 手机号 */
  mobile: string;
  /** 邮箱 */
  email: string;
  /** 部门 id */
  dept_id: string | undefined;
  /** 状态 */
  status: StatusValue;
  /** 备注 */
  remark: string;
  /** 角色 id 列表（新增随创建一并分配；编辑时保存后单独提交） */
  role_ids: string[];
  /** 账号类型：人类 / AI（仅新建可选，创建后不可改） */
  account_type: 'HUMAN' | 'ROBOT';
  /** AI 用户职责 */
  agent_description: string;
  /** AI 用户模型名（映射为 model_override.model） */
  model: string;
  /** AI 用户 SOUL.md 全文 */
  soul_md: string;
}

/** 每页条数（与改造前一致） */
const PAGE_SIZE = 10;

const userStore = useUserStore();
const { hasPermission } = userStore;

// ==================== 列表（P3-11） ====================

/** 筛选状态（`dept_id` 由左树写入，不声明成筛选项，见文件头注释） */
const filter = reactive<ListFilter>({ username: '', real_name: '', status: '', dept_id: undefined });

const filters: FilterDef[] = [
  { key: 'username', type: 'keyword', placeholder: '用户名', width: 200, testid: 'system-user-username' },
  { key: 'real_name', type: 'keyword', placeholder: '姓名', width: 200, testid: 'system-user-real-name' },
  {
    key: 'status',
    type: 'select',
    placeholder: '状态',
    width: 140,
    options: [
      { label: '正常', value: 1 },
      { label: '停用', value: 0 },
    ],
    testid: 'system-user-status',
  },
];

const columns: ColumnDef[] = [
  { colKey: 'username', title: '用户名', width: 150 },
  { colKey: 'user_type', title: '类型', width: 90, exportable: false },
  { colKey: 'real_name', title: '姓名', width: 120 },
  { colKey: 'dept_name', title: '部门', width: 160 },
  { colKey: 'roles', title: '角色', width: 180, exportable: false },
  { colKey: 'mobile', title: '手机号', width: 140 },
  { colKey: 'email', title: '邮箱', ellipsis: true },
  { colKey: 'status', title: '状态', width: 100, exportable: false },
  { colKey: 'last_login_at', title: '最近登录', width: 180 },
  { colKey: 'created_at', title: '创建时间', width: 180 },
  { colKey: 'op', title: '操作', width: 240, fixed: 'right', exportable: false },
];

const fetcher: ListFetcher<UserRes> = (query) => pageUsers(query as Parameters<typeof pageUsers>[0]);

/**
 * 入参拼装。
 *
 * `include_children` 只在「选了部门」时才发（点部门树默认查本部门及其全部子部门）；
 * `status` 是数字枚举，`0`（停用）是真条件，不能按假值丢掉。
 */
const queryBuilder: ListQueryBuilder = (_state, page, size) => {
  const status = filter.status;
  const deptId = filter.dept_id ? String(filter.dept_id) : undefined;
  return {
    page,
    page_size: size,
    username: textOf(filter.username),
    real_name: textOf(filter.real_name),
    dept_id: deptId,
    include_children: deptId ? true : undefined,
    status: status === '' || status === undefined || status === null ? undefined : Number(status),
  };
};

/** 去空格，空值返回 undefined */
function textOf(value: unknown): string | undefined {
  const text = String(value ?? '').trim();
  return text || undefined;
}

const listRef = ref<InstanceType<typeof ListPage> | null>(null);

/** 重拉当前页（增删改 / 启停后） */
function reloadList(): void {
  listRef.value?.reload();
}

// ===== 左侧部门树 =====
const deptTree = ref<DeptNode[]>([]);
const deptLoading = ref(false);
/** 当前激活的部门节点 key（受控，重置筛选时同步清空） */
const activeDeptKeys = ref<Array<string | number>>([]);

/** 当前选中部门名（筛选行里的提示条用；树里翻不到名字时退回「已选部门」） */
const activeDeptName = computed(() => {
  const id = filter.dept_id ? String(filter.dept_id) : '';
  if (!id) {
    return '';
  }
  const found = findDept(deptTree.value, id);
  return found?.dept_name ?? '已选部门';
});

/** 在部门树里按 id 找节点 */
function findDept(nodes: DeptNode[], id: string): DeptNode | null {
  for (const node of nodes) {
    if (node.id === id) {
      return node;
    }
    const hit = findDept(node.children ?? [], id);
    if (hit) {
      return hit;
    }
  }
  return null;
}

/** 加载左侧部门树 */
async function loadDeptTree(): Promise<void> {
  deptLoading.value = true;
  try {
    deptTree.value = await getDeptTree();
  } catch (error) {
    deptTree.value = [];
    showError(error instanceof ApiError ? error.message : '部门树加载失败');
  } finally {
    deptLoading.value = false;
  }
}

/** 部门树选中：按所选部门（含子部门）筛选用户列表 */
function onDeptActive(value: Array<string | number>): void {
  activeDeptKeys.value = value;
  filter.dept_id = value.length > 0 ? String(value[0]) : undefined;
  listRef.value?.search();
}

/** 清掉部门筛选（筛选行里的「清除」） */
function clearDept(): void {
  activeDeptKeys.value = [];
  filter.dept_id = undefined;
  listRef.value?.search();
}

/**
 * 「重置」：组件已把声明的三个筛选键清空并回第 1 页，这里补两件它管不到的事 ——
 * ① 清掉左树的部门筛选（`dept_id` 不是声明的筛选项）；② 取消树节点高亮。
 */
function handleReset(): void {
  filter.dept_id = undefined;
  activeDeptKeys.value = [];
}

// ==================== 新增 / 编辑表单 ====================

const formVisible = ref(false);
const isEdit = ref(false);
const submitting = ref(false);
const formRef = ref<FormInstanceFunctions | null>(null);
const form = reactive<UserForm>({
  id: '',
  username: '',
  password: '',
  real_name: '',
  mobile: '',
  email: '',
  dept_id: undefined,
  status: 1,
  remark: '',
  role_ids: [],
  account_type: 'HUMAN',
  agent_description: '',
  model: '',
  soul_md: '',
});
const formRules = computed<FormRules<UserForm>>(() => {
  if (form.account_type === 'ROBOT') {
    // AI 用户：用户名是 profile 名（正则与后端逐字一致），密码不需要，姓名为选填
    return {
      username: isEdit.value
        ? []
        : [
            { required: true, message: '请输入用户名（= Hermes profile 名）' },
            {
              pattern: /^[a-z][a-z0-9-]{1,30}$/,
              message: '必须以小写字母开头，只含小写字母/数字/连字符，长度 2–31',
            },
          ],
    };
  }
  return {
    username: isEdit.value ? [] : [{ required: true, message: '请输入用户名' }],
    password: isEdit.value ? [] : [{ required: true, message: '请输入初始密码' }],
    real_name: [{ required: true, message: '请输入姓名' }],
    email: [{ email: true, message: '邮箱格式不正确' }],
  };
});

const roleVisible = ref(false);
const roleSubmitting = ref(false);
const roleForm = reactive<{ role_ids: string[] }>({ role_ids: [] });
const roleOptions = ref<Array<{ label: string; value: string }>>([]);
/** 角色下拉加载中（新增 / 编辑弹窗的选择器复用） */
const roleOptionsLoading = ref(false);
const currentRow = ref<UserRes | null>(null);

const passwordVisible = ref(false);
const passwordSubmitting = ref(false);
const passwordForm = reactive({ password: '' });
const passwordRules: FormRules<{ password: string }> = {
  password: [
    { required: true, message: '请输入新密码' },
    { min: 6, message: '密码长度不少于 6 位' },
  ],
};

/** 统一展示接口错误 */
function showError(message: string): void {
  void MessagePlugin.error(message);
}

/** 加载角色下拉数据，供新增 / 编辑弹窗与分配角色弹窗共用 */
async function loadRoleOptions(): Promise<void> {
  roleOptionsLoading.value = true;
  try {
    const list: RoleRes[] = await listRoles();
    roleOptions.value = list.map((role) => ({ label: role.role_name, value: role.id }));
  } catch {
    roleOptions.value = [];
  } finally {
    roleOptionsLoading.value = false;
  }
}

// ===== AI 用户（云枢 × Hermes）=====

/** 当前表单是否是 AI 用户（新建看选择，编辑看行数据带入的 account_type） */
const isAiForm = computed(() => form.account_type === 'ROBOT');

const dialogTitle = computed(() => {
  if (!isEdit.value) {
    return isAiForm.value ? '新增 AI 用户' : '新增用户';
  }
  return isAiForm.value ? '编辑 AI 用户' : '编辑用户';
});

/** SOUL.md 服务器绝对路径（编辑态由后端下发，保存前提示要用） */
const aiSoulPath = ref('');
/** profile 名（编辑态展示） */
const aiProfile = ref('');
const aiProfileExists = ref(false);
const aiHealthy = ref(false);
const aiGateway = ref('unknown');
const aiStatusDetail = ref<string | null>(null);
const aiStatusLoading = ref(false);

function resetAiStatus(): void {
  aiSoulPath.value = '';
  aiProfile.value = '';
  aiProfileExists.value = false;
  aiHealthy.value = false;
  aiGateway.value = 'unknown';
  aiStatusDetail.value = null;
  aiStatusLoading.value = false;
}

/** `model_override`（任意 JSON）→ 表单里的模型名（只认 `{ model: string }` 这一种形态） */
function modelNameOf(model: Record<string, unknown> | null): string {
  const value = model?.model;
  return typeof value === 'string' ? value : '';
}

/** 表单里的模型名 → `model_override`（空串 = 不设覆盖） */
function toModelOverride(name: string): Record<string, unknown> | null {
  const trimmed = name.trim();
  return trimmed ? { model: trimmed } : null;
}

/** 探活：拿到 profile 目录 / SOUL.md / workspace / CLI / gateway 的真实状态 */
async function refreshAiStatus(): Promise<void> {
  if (!form.id) {
    return;
  }
  aiStatusLoading.value = true;
  try {
    const status = await getAiUserStatus(form.id);
    aiProfileExists.value = status.profile_dir_exists;
    aiHealthy.value = status.healthy;
    aiGateway.value = status.gateway;
    aiStatusDetail.value = status.detail;
    aiProfile.value = status.hermes_profile;
  } catch (error) {
    aiStatusDetail.value = error instanceof ApiError ? error.message : '探活失败';
  } finally {
    aiStatusLoading.value = false;
  }
}

/** 重置表单为初始值 */
function resetForm(): void {
  Object.assign(form, {
    id: '',
    username: '',
    password: '',
    real_name: '',
    mobile: '',
    email: '',
    dept_id: undefined,
    status: 1,
    remark: '',
    role_ids: [],
    account_type: 'HUMAN',
    agent_description: '',
    model: '',
    soul_md: '',
  });
  resetAiStatus();
}

function openCreate(): void {
  isEdit.value = false;
  resetForm();
  formVisible.value = true;
}

async function openEdit(row: UserRes): Promise<void> {
  try {
    const detail = await getUser(row.id);
    isEdit.value = true;
    Object.assign(form, {
      id: detail.id,
      username: detail.username,
      password: '',
      real_name: detail.real_name,
      mobile: detail.mobile,
      email: detail.email,
      dept_id: detail.dept_id ?? undefined,
      status: detail.status,
      remark: detail.remark,
      // 角色 id 走独立接口：详情里的 roles 只有名称，选择器需要 id
      role_ids: detail.user_type === 'ROBOT' ? [] : await getUserRoles(detail.id),
      account_type: detail.user_type === 'ROBOT' ? 'ROBOT' : 'HUMAN',
      agent_description: '',
      model: '',
      soul_md: '',
    });
    if (detail.user_type === 'ROBOT') {
      // AI 用户：补拉职责 / SOUL.md / 模型 + 探活（两次调用，编辑页才需要）
      const ai = await getAiUser(detail.id);
      form.agent_description = ai.agent_description;
      form.soul_md = ai.soul_md;
      form.model = modelNameOf(ai.model_override);
      aiSoulPath.value = ai.soul_path;
      aiProfile.value = ai.hermes_profile;
      void refreshAiStatus();
    }
    formVisible.value = true;
  } catch (error) {
    showError(error instanceof ApiError ? error.message : '用户详情加载失败');
  }
}

async function submitForm(): Promise<void> {
  const valid = await formRef.value?.validate();
  if (valid !== true) {
    return;
  }
  submitting.value = true;
  try {
    if (isEdit.value) {
      if (isAiForm.value) {
        // AI 用户：职责 / SOUL.md / 模型走专用接口（会写磁盘 + 同步 profile 描述）
        await updateAiUser(form.id, {
          agent_description: form.agent_description,
          soul_md: form.soul_md,
          model_override: toModelOverride(form.model),
        });
        void MessagePlugin.success('AI 用户已更新（SOUL.md 已写入服务器）');
      } else {
        await updateUser(form.id, {
          real_name: form.real_name,
          mobile: form.mobile,
          email: form.email,
          dept_id: form.dept_id,
          status: form.status,
          remark: form.remark,
        });
        // 后端编辑接口不含角色字段，角色单独提交；两者都成功才算保存完成
        await updateUserRoles(form.id, form.role_ids);
        void MessagePlugin.success('用户已更新');
      }
    } else if (isAiForm.value) {
      // 新建 AI 用户：建号 + Hermes profile + SOUL.md + workspace（后端一个事务 + 补偿）
      await createAiUser({
        username: form.username,
        real_name: form.real_name,
        agent_description: form.agent_description,
        soul_md: form.soul_md,
        model_override: toModelOverride(form.model),
      });
      void MessagePlugin.success('AI 用户已创建（Hermes profile 与 SOUL.md 已就绪）');
    } else {
      await createUser({
        username: form.username,
        password: form.password,
        real_name: form.real_name,
        mobile: form.mobile,
        email: form.email,
        dept_id: form.dept_id,
        status: form.status,
        remark: form.remark,
        role_ids: form.role_ids,
      });
      void MessagePlugin.success('用户已创建');
    }
    formVisible.value = false;
    reloadList();
  } catch (error) {
    showError(error instanceof ApiError ? error.message : '保存失败');
  } finally {
    submitting.value = false;
  }
}

/** 切换用户状态，失败时列表不刷新以保持与后端一致 */
function toggleStatus(row: UserRes): void {
  const next: StatusValue = row.status === 1 ? 0 : 1;
  const action = next === 1 ? '启用' : '停用';
  const dialog = DialogPlugin.confirm({
    header: `${action}用户`,
    body: `确认${action}用户「${row.username}」？`,
    onConfirm: () => {
      dialog.destroy();
      void (async () => {
        try {
          await changeUserStatus(row.id, next);
          void MessagePlugin.success(`已${action}`);
          reloadList();
        } catch (error) {
          showError(error instanceof ApiError ? error.message : `${action}失败`);
        }
      })();
    },
  });
}

function confirmRemove(row: UserRes): void {
  const isRobot = row.user_type === 'ROBOT';
  const dialog = DialogPlugin.confirm({
    header: isRobot ? '删除 AI 用户' : '删除用户',
    body: isRobot
      ? `确认删除 AI 用户「${row.username}」？将先备份 SOUL.md，再删除 Hermes profile 与 workspace 目录，最后软删账号。`
      : `确认删除用户「${row.username}」？该操作不可恢复。`,
    theme: 'danger',
    confirmBtn: { content: '删除', theme: 'danger' },
    onConfirm: () => {
      dialog.destroy();
      void (async () => {
        try {
          if (isRobot) {
            // AI 用户必须走专用删除接口：通用删除只软删 sys_user，会留下孤立 profile
            const res = await deleteAiUser(row.id);
            void MessagePlugin.success(`AI 用户已删除${res.soul_backup_path ? '（SOUL.md 已备份）' : ''}`);
          } else {
            await deleteUser(row.id);
            void MessagePlugin.success('用户已删除');
          }
          reloadList();
        } catch (error) {
          showError(error instanceof ApiError ? error.message : '删除失败');
        }
      })();
    },
  });
}

async function openAssignRoles(row: UserRes): Promise<void> {
  currentRow.value = row;
  roleForm.role_ids = [];
  roleVisible.value = true;
  try {
    roleForm.role_ids = await getUserRoles(row.id);
  } catch (error) {
    showError(error instanceof ApiError ? error.message : '角色加载失败');
  }
}

async function submitRoles(): Promise<void> {
  if (!currentRow.value) {
    return;
  }
  roleSubmitting.value = true;
  try {
    await updateUserRoles(currentRow.value.id, roleForm.role_ids);
    void MessagePlugin.success('角色已更新');
    roleVisible.value = false;
    // 角色列在列表里 → 改完刷一下当前页
    reloadList();
  } catch (error) {
    showError(error instanceof ApiError ? error.message : '角色保存失败');
  } finally {
    roleSubmitting.value = false;
  }
}

function openResetPassword(row: UserRes): void {
  currentRow.value = row;
  passwordForm.password = '';
  passwordVisible.value = true;
}

async function submitPassword(): Promise<void> {
  if (!currentRow.value) {
    return;
  }
  passwordSubmitting.value = true;
  try {
    await resetUserPassword(currentRow.value.id, passwordForm.password);
    void MessagePlugin.success('密码已重置');
    passwordVisible.value = false;
  } catch (error) {
    showError(error instanceof ApiError ? error.message : '密码重置失败');
  } finally {
    passwordSubmitting.value = false;
  }
}

onMounted(() => {
  void loadDeptTree();
  void loadRoleOptions();
});
</script>

<style lang="less" scoped>
// 空单元格占位符（如用户未分配任何角色）
.cell-empty {
  color: var(--td-text-color-placeholder);
}

.user-page {
  // 与布局内容区高度对齐，左侧部门树撑满并独立滚动
  .user-layout {
    display: flex;
    height: calc(100vh - 88px);
  }

  .user-left {
    width: 260px;
    min-width: 260px;
    overflow: hidden;
    margin-right: 16px;
  }

  .dept-card {
    height: 100%;
    display: flex;
    flex-direction: column;

    :deep(.t-card__body) {
      flex: 1;
      display: flex;
      flex-direction: column;
      overflow: hidden;
      padding: 0;
    }
  }

  .dept-toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 16px;
    border-bottom: 1px solid var(--td-component-stroke);

    .dept-title {
      font-size: 15px;
      font-weight: 600;
    }
  }

  .dept-body {
    flex: 1;
    overflow-y: auto;
    padding: 8px 8px;

    :deep(.t-tree__item) {
      height: 36px;
    }

    :deep(.t-tree__label) {
      line-height: 36px;
    }
  }

  .dept-empty {
    padding: 32px;
    text-align: center;
    color: var(--td-text-color-placeholder);
    font-size: 13px;
  }

  .user-right {
    flex: 1;
    min-width: 0;
    overflow-y: auto;
  }

  /* 筛选卡底部的「当前在看哪个部门」提示条 */
  .dept-chip {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 12px;
  }

  /* AI 用户：列表里的机器人图标（与「类型」列的标签配合使用） */
  .cell-robot {
    color: var(--td-brand-color);
  }

  /* 表单内的辅助说明（如「用户名即 profile 名」） */
  .form-field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    width: 100%;

    &__hint {
      font-size: 12px;
      line-height: 1.5;
      color: var(--td-text-color-secondary);

      code {
        font-family: var(--td-font-family-mono, Menlo, Consolas, monospace);
        background: rgba(0, 0, 0, 0.04);
        padding: 0 4px;
        border-radius: 2px;
      }
    }
  }
}
</style>
