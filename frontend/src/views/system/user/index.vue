<template>
  <div class="user-page">
    <div class="user-layout">
      <!-- 左侧：部门树形列表，选中部门后按部门筛选用户 -->
      <div class="user-left">
        <t-card :bordered="false" class="dept-card">
          <div class="dept-toolbar">
            <span class="dept-title">组织架构</span>
            <t-button size="small" variant="text" @click="loadDeptTree">
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

      <!-- 右侧：筛选栏 + 列表 -->
      <div class="user-right">
        <!-- 筛选栏 -->
        <t-card :bordered="false" class="filter-card">
          <t-row :gutter="16">
            <t-col :span="2.4">
              <t-input v-model="query.username" placeholder="用户名" clearable @enter="handleSearch" />
            </t-col>
            <t-col :span="2.4">
              <t-input v-model="query.real_name" placeholder="姓名" clearable @enter="handleSearch" />
            </t-col>
            <t-col :span="2.4">
              <t-select v-model="query.status" placeholder="状态" clearable>
                <t-option :value="1" label="正常" />
                <t-option :value="0" label="停用" />
              </t-select>
            </t-col>
            <t-col :span="2.4">
              <t-space>
                <t-button theme="primary" @click="handleSearch">
                  <template #icon><t-icon name="search" /></template>
                  查询
                </t-button>
                <t-button theme="default" @click="handleReset">重置</t-button>
              </t-space>
            </t-col>
          </t-row>
        </t-card>

        <!-- 表格 -->
        <t-card :bordered="false" class="table-card">
          <div class="toolbar">
            <t-space>
              <t-button v-privilege="['system:user:create']" theme="primary" @click="openCreate">
                <template #icon><t-icon name="add" /></template>
                新增用户
              </t-button>
              <t-button theme="default" @click="loadList">
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
            @page-change="handlePageChange"
          >
            <template #username="{ row }">
              <t-space size="4" align="center">
                <t-icon v-if="row.user_type === 'ROBOT'" name="robot" class="cell-robot" />
                <span>{{ row.username }}</span>
              </t-space>
            </template>
            <template #user_type="{ row }">
              <t-tag v-if="row.user_type === 'ROBOT'" size="small" theme="primary" variant="light">
                机器人
              </t-tag>
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
                <t-tag
                  v-for="role in row.roles"
                  :key="role.role_id"
                  size="small"
                  theme="primary"
                  variant="light"
                >
                  {{ role.role_name }}
                </t-tag>
              </t-space>
              <span v-else class="cell-empty">—</span>
            </template>
            <template #op="{ row }">
              <t-space size="small">
                <t-link v-privilege="['system:user:update']" theme="primary" @click="openEdit(row)">
                  编辑
                </t-link>
                <t-link v-privilege="['system:user:assign']" theme="primary" @click="openAssignRoles(row)">
                  分配角色
                </t-link>
                <t-link v-privilege="['system:user:reset']" v-if="row.user_type !== 'ROBOT'" theme="warning" @click="openResetPassword(row)">
                  重置密码
                </t-link>
                <t-link v-privilege="['system:user:delete']" theme="danger" @click="confirmRemove(row)">
                  删除
                </t-link>
              </t-space>
            </template>
          </t-table>
        </t-card>
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
import { DialogPlugin, MessagePlugin, type FormInstanceFunctions, type FormRules, type PrimaryTableCol } from 'tdesign-vue-next';
import DeptTreeSelect from '@/components/DeptTreeSelect.vue';
import AiUserForm from './components/AiUserForm.vue';
import {
  createAiUser,
  deleteAiUser,
  getAiUser,
  getAiUserStatus,
  updateAiUser,
} from '@/api/system/ai-user-api';
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

/** 表格分页事件载荷 */
interface TablePageInfo {
  /** 当前页码 */
  current: number;
  /** 每页条数 */
  pageSize: number;
}

/** 查询条件 */
interface UserQuery {
  /** 用户名 */
  username: string;
  /** 姓名 */
  real_name: string;
  /** 部门 id（query 参数为空串时不下发） */
  dept_id: string | undefined;
  /** 状态 */
  status: StatusValue | undefined;
}

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

const userStore = useUserStore();
const { hasPermission } = userStore;

const query = reactive<UserQuery>({
  username: '',
  real_name: '',
  dept_id: undefined,
  status: undefined,
});
const rows = ref<UserRes[]>([]);
const loading = ref(false);
const pagination = reactive({ current: 1, pageSize: 10, total: 0 });

// ===== 左侧部门树 =====
const deptTree = ref<DeptNode[]>([]);
const deptLoading = ref(false);
/// 当前激活的部门节点 key（受控，重置筛选时同步清空）
const activeDeptKeys = ref<Array<string | number>>([]);

const columns: PrimaryTableCol[] = [
  { colKey: 'username', title: '用户名', width: 150, cell: 'username' },
  { colKey: 'user_type', title: '类型', width: 90, cell: 'user_type' },
  { colKey: 'real_name', title: '姓名', width: 120 },
  { colKey: 'dept_name', title: '部门', width: 160 },
  { colKey: 'roles', title: '角色', width: 180, cell: 'roles' },
  { colKey: 'mobile', title: '手机号', width: 140 },
  { colKey: 'email', title: '邮箱', ellipsis: true },
  { colKey: 'status', title: '状态', width: 100, cell: 'status' },
  { colKey: 'last_login_at', title: '最近登录', width: 180, cell: 'last_login_at' },
  { colKey: 'created_at', title: '创建时间', width: 180, cell: 'created_at' },
  { colKey: 'op', title: '操作', width: 240, fixed: 'right' },
];

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

/** 加载用户列表 */
async function loadList(): Promise<void> {
  loading.value = true;
  try {
    const data = await pageUsers({
      page: pagination.current,
      page_size: pagination.pageSize,
      username: query.username || undefined,
      real_name: query.real_name || undefined,
      dept_id: query.dept_id || undefined,
      // 点击部门树默认查询本部门及其全部子部门的用户
      include_children: query.dept_id ? true : undefined,
      status: query.status,
    });
    rows.value = data.list;
    pagination.total = data.total;
  } catch (error) {
    showError(error instanceof ApiError ? error.message : '用户列表加载失败');
  } finally {
    loading.value = false;
  }
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

/** 部门树选中：按所选部门筛选用户列表 */
function onDeptActive(value: Array<string | number>): void {
  activeDeptKeys.value = value;
  query.dept_id = value.length > 0 ? String(value[0]) : undefined;
  handleSearch();
}

function handleSearch(): void {
  pagination.current = 1;
  void loadList();
}

function handleReset(): void {
  query.username = '';
  query.real_name = '';
  query.dept_id = undefined;
  query.status = undefined;
  activeDeptKeys.value = [];
  handleSearch();
}

function handlePageChange(pageInfo: TablePageInfo): void {
  pagination.current = pageInfo.current;
  pagination.pageSize = pageInfo.pageSize;
  void loadList();
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
    await loadList();
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
          await loadList();
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
            void MessagePlugin.success(
              `AI 用户已删除${res.soul_backup_path ? '（SOUL.md 已备份）' : ''}`,
            );
          } else {
            await deleteUser(row.id);
            void MessagePlugin.success('用户已删除');
          }
          await loadList();
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
  void loadList();
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

  .filter-card {
    margin-bottom: 12px;
  }

  .toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 12px;

    &__count {
      font-size: 13px;
      color: var(--td-text-color-secondary);
    }
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
