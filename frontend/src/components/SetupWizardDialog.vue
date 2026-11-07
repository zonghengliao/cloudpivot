<!--
  SetupWizardDialog.vue —— 首次启动向导（FIX-1 · 新环境开箱即用）

  # 触发条件（`BasicLayout` 在挂载后调 `openIfNeeded()`）

  登录后 `GET /api/system/setup/check` 发现「关键角色无成员」且当前用户有
  `system:setup:read` 权限 ⇒ 弹本框。**可跳过**：跳过会写一条
  `localStorage` 标记，同一次登录不再骚扰（换个账号 / 清缓存会重新提示）。

  # 为什么要「关键角色」这一层

  `SALES_SUPERVISOR` / `PURCHASE_MANAGER` / `FINANCE_MANAGER` / `DEPT_LEADER` /
  `WAREHOUSE_MANAGER` / `GENERAL_MANAGER` 是审批快照里真正绑定的 6 个角色。
  其中任何一个 0 人，对应那条业务链一提交就 `65018`（内控不许自审）。
  向导把它们一次性摊在用户面前，比让用户「提交 → 报错 → 回来配」高效得多。

  # 两条出路

  1. **逐角色选人**（生产）：从用户列表里挑在职成员 → `POST /system/setup/assign`；
  2. **一键建演示账号**（开发 / 测试）：`POST /system/setup/demo-users`，
     需要后端 `NIK_SEED_DEMO_USERS=true`，否则 409 并提示怎么开。
-->
<template>
  <t-dialog
    v-model:visible="visible"
    header="首次启动向导 · 给审批岗配人"
    :width="720"
    :footer="false"
    placement="center"
    :close-on-overlay-click="false"
    data-testid="setup-wizard-dialog"
    @close="skip"
  >
    <div class="setup-wizard">
      <t-alert
        v-if="demoUsersEnabled"
        theme="info"
        class="setup-wizard__demo-alert"
        message="开发 / 测试环境可以一键创建演示账号（demo_sales / demo_sales_mgr / demo_purchase … 共 9 个），六条业务链立刻能跑。"
      />

      <p class="setup-wizard__intro">
        下面这些角色是审批流的必经岗位。只要有一个角色没有任何在职成员，对应的单据一提交就会被
        内控拦下（「没有可用审批人」）。勾选成员后保存即可，**只增不减**。
      </p>

      <t-loading :loading="loading" size="small">
        <div v-if="missing.length === 0" class="setup-wizard__done" data-testid="setup-wizard-done">
          <t-icon name="check-circle" size="18px" />
          关键角色都有人了，可以关闭本向导。
        </div>

        <div
          v-for="role in missing"
          :key="role.role_code"
          class="setup-wizard__role"
          :data-testid="`setup-wizard-role-${role.role_code}`"
        >
          <div class="setup-wizard__role-head">
            <span class="setup-wizard__role-name">{{ role.role_name }}</span>
            <code class="setup-wizard__code">{{ role.role_code }}</code>
            <span class="setup-wizard__role-meta">
              当前 {{ role.member_count }} 人 · 影响：{{ role.why }}
            </span>
          </div>
          <t-select
            v-model="picked[role.role_code]"
            multiple
            filterable
            clearable
            class="setup-wizard__select"
            :options="userOptions"
            :loading="usersLoading"
            :placeholder="`从用户列表里选择「${role.role_name}」成员`"
            :data-testid="`setup-wizard-select-${role.role_code}`"
          />
        </div>
      </t-loading>

      <p v-if="usersError" class="setup-wizard__error">{{ usersError }}</p>

      <div class="setup-wizard__footer">
        <t-button
          v-if="demoUsersEnabled"
          theme="default"
          variant="outline"
          :loading="creatingDemo"
          data-testid="setup-wizard-demo-users"
          @click="createDemo"
        >
          一键创建演示账号
        </t-button>
        <div class="setup-wizard__footer-right">
          <t-button theme="default" data-testid="setup-wizard-skip" @click="skip">稍后再说</t-button>
          <t-button
            theme="primary"
            :loading="saving"
            :disabled="assignCount === 0"
            data-testid="setup-wizard-save"
            @click="save"
          >
            保存（{{ assignCount }} 个角色）
          </t-button>
        </div>
      </div>
    </div>
  </t-dialog>
</template>

<script setup lang="ts">
/**
 * 首次启动向导。
 *
 * # 打开时机
 *
 * `BasicLayout.onMounted()` 调 `openIfNeeded()`：有 `system:setup:read` 权限、
 * 本次登录没跳过过、且后端 `check` 有缺人角色 ⇒ 打开。
 *
 * # 与 `FlowAssigneeDialog` 的分工
 *
 * 本组件是**事前**（登录就先配好）；`FlowAssigneeDialog` 是**事后**（提交失败时救火）。
 * 两者共用同一个后端接口 `/system/setup/assign`，口径不会分叉。
 */
import { computed, reactive, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import { pageUsers } from '@/api/system/user-api';
import {
  assignSetupMembers,
  checkSetup,
  createDemoUsers,
  type SetupRoleStatus,
} from '@/api/system/setup-api';
import { SETUP_READ_PERMISSION, hasPermission } from '@/utils/flow-assignee-guard';

/** 「本次登录别再弹」的 localStorage 键（按用户隔离，换账号会重新提示） */
const SKIP_KEY_PREFIX = 'cloudpivot.setup-wizard.skipped:';

const visible = ref(false);
const loading = ref(false);
const saving = ref(false);
const creatingDemo = ref(false);
const usersLoading = ref(false);
const usersError = ref('');
const missing = ref<SetupRoleStatus[]>([]);
const demoUsersEnabled = ref(false);
/** 每个角色选中的用户 id */
const picked = reactive<Record<string, string[]>>({});
const userOptions = ref<{ label: string; value: string }[]>([]);

/** 有几个角色选了人（决定「保存」按钮的可用性与文案） */
const assignCount = computed(
  () => Object.values(picked).filter((ids) => Array.isArray(ids) && ids.length > 0).length,
);

function skipKey(username: string): string {
  return `${SKIP_KEY_PREFIX}${username}`;
}

/** 拉一次用户列表（向导里所有角色共用一份，避免 N 次请求） */
async function loadUsers(): Promise<void> {
  if (userOptions.value.length > 0) {
    return;
  }
  usersLoading.value = true;
  usersError.value = '';
  try {
    const page = await pageUsers({ page: 1, page_size: 200, status: 1 });
    userOptions.value = page.list
      .filter((user) => user.user_type !== 'ROBOT')
      .map((user) => ({
        label: user.real_name ? `${user.real_name}（${user.username}）` : user.username,
        value: user.id,
      }));
  } catch (error) {
    usersError.value = `无法加载用户列表：${(error as Error).message}`;
  } finally {
    usersLoading.value = false;
  }
}

/**
 * 需要时打开（`BasicLayout` 挂载后调用）。
 *
 * # 参数
 *
 * `username`：当前登录名（用于「跳过」标记按人隔离）。
 *
 * # 静默失败
 *
 * `check` 403（普通用户）或网络失败时**不弹框、不报错**：向导是锦上添花，
 * 不该在每次登录时往控制台刷红字。
 */
async function openIfNeeded(username: string): Promise<void> {
  if (!username) {
    return;
  }
  if (!hasPermission(SETUP_READ_PERMISSION)) {
    return;
  }
  if (localStorage.getItem(skipKey(username)) === '1') {
    return;
  }
  activeUsername = username;
  loading.value = true;
  try {
    const result = await checkSetup();
    demoUsersEnabled.value = result.demo_users_enabled;
    missing.value = result.missing;
    if (result.all_ready) {
      return;
    }
    visible.value = true;
    void loadUsers();
  } catch {
    // 权限不足 / 接口异常：静默（见函数头注释）
  } finally {
    loading.value = false;
  }
}

/** 逐角色保存 */
async function save(): Promise<void> {
  const tasks = Object.entries(picked).filter(([, ids]) => Array.isArray(ids) && ids.length > 0);
  if (tasks.length === 0) {
    return;
  }
  saving.value = true;
  let savedRoles = 0;
  try {
    for (const [roleCode, userIds] of tasks) {
      const result = await assignSetupMembers({ role_code: roleCode, user_ids: userIds });
      savedRoles += 1;
      void MessagePlugin.success(
        `「${result.role_name}」已指派 ${result.added.length} 人（现共 ${result.member_count} 人）`,
      );
    }
    // 重新自检：还有缺的就留着，全好了就关
    const after = await checkSetup();
    missing.value = after.missing;
    for (const key of Object.keys(picked)) {
      picked[key] = [];
    }
    if (after.all_ready) {
      visible.value = false;
      void MessagePlugin.success('关键角色都配好了，六条业务链可以跑了');
    }
  } catch (error) {
    void MessagePlugin.error(`保存失败：${(error as Error).message}`);
  } finally {
    saving.value = false;
    if (savedRoles === 0) {
      // 一个都没成功：保持弹窗，让用户重试
      visible.value = true;
    }
  }
}

/** 一键建演示账号（开发 / 测试） */
async function createDemo(): Promise<void> {
  creatingDemo.value = true;
  try {
    const result = await createDemoUsers();
    void MessagePlugin.success(
      `演示账号：新建 ${result.created.length} 个、已存在 ${result.existed.length} 个；口令见环境变量 ${result.password_env}（首次登录强制改密）`,
    );
    const after = await checkSetup();
    missing.value = after.missing;
    if (after.all_ready) {
      visible.value = false;
    }
  } catch (error) {
    void MessagePlugin.error(`创建演示账号失败：${(error as Error).message}`);
  } finally {
    creatingDemo.value = false;
  }
}

/** 跳过的按用户隔离标识（`openIfNeeded` 里记下来，`skip()` 用） */
let activeUsername = '';

/** 跳过（本次登录不再提示） */
function skip(): void {
  if (activeUsername) {
    localStorage.setItem(skipKey(activeUsername), '1');
  }
  visible.value = false;
}

defineExpose({ openIfNeeded });
</script>

<style scoped>
.setup-wizard__demo-alert {
  margin-bottom: 12px;
}

.setup-wizard__intro {
  margin: 0 0 12px;
  font-size: 13px;
  line-height: 20px;
  color: var(--td-text-color-secondary, #00000099);
}

.setup-wizard__done {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 12px 0;
  color: var(--td-success-color, #2ba471);
}

.setup-wizard__role {
  padding: 10px 0;
  border-top: 1px solid var(--td-component-stroke, #e7e7e7);
}

.setup-wizard__role-head {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: 6px;
  margin-bottom: 6px;
}

.setup-wizard__role-name {
  font-weight: 600;
}

.setup-wizard__code {
  padding: 0 4px;
  font-size: 12px;
  color: var(--td-text-color-secondary, #00000099);
  background: var(--td-bg-color-component, #f0f0f0);
  border-radius: 3px;
}

.setup-wizard__role-meta {
  font-size: 12px;
  color: var(--td-text-color-secondary, #00000099);
}

.setup-wizard__select {
  width: 100%;
}

.setup-wizard__error {
  margin: 8px 0 0;
  font-size: 12px;
  color: var(--td-error-color, #d54941);
}

.setup-wizard__footer {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-top: 16px;
}

.setup-wizard__footer-right {
  display: flex;
  gap: 8px;
}
</style>
