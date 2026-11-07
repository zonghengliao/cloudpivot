<!--
  FlowAssigneeDialog.vue —— 「指派审批人」引导框（FIX-1 · 新环境开箱即用）

  # 它接在哪儿

  `api/request.ts` 的响应拦截器拿到 `65018`（无可用审批人）时，通过
  `utils/flow-assignee-guard.ts` 打开本组件；用户选完人、指派成功后，
  拦截器**自动重试刚才那次提交**（用户不用再点一次「提交审批」）。

  # 为什么不是一个普通弹窗，而要带这么多信息

  新环境的现场是：`admin` 建单 → 提交 → 红字「没有可用审批人」。
  只给这句话，用户不知道：
  - 是**哪个节点**（`so_approve` 有 3 个审批节点）；
  - 缺的是**哪个角色**（`SALES_SUPERVISOR` 还是 `FINANCE_MANAGER`）；
  - 那个角色现在**几个人**（0 人 = 加人；有人但都停用 = 先看成员状态）；
  - 为什么明明有人也不行（「张三：发起人本人」——内控职责分离）。

  所以弹窗把后端 `65018` 载荷（`business::flow::no_assignee`）整份摊开，
  再给三个出口：**选人直接指派 / 去角色管理 / 取消**。
-->
<template>
  <t-dialog
    v-model:visible="visible"
    :header="headerText"
    :width="620"
    :footer="false"
    placement="center"
    :close-on-overlay-click="false"
    data-testid="flow-assignee-dialog"
    @close="onCancel"
  >
    <template v-if="payload">
      <!-- ① 哪个节点、缺什么 -->
      <div class="assignee-dialog__summary" data-testid="assignee-dialog-summary">
        <div class="assignee-dialog__row">
          <span class="assignee-dialog__label">审批节点</span>
          <span class="assignee-dialog__value">
            {{ payload.node_name }}
            <code class="assignee-dialog__code">{{ payload.node_key }}</code>
          </span>
        </div>
        <div class="assignee-dialog__row">
          <span class="assignee-dialog__label">审批人来源</span>
          <span class="assignee-dialog__value">
            {{ payload.assignee_type || '未配置' }}
            <template v-if="payload.role_name">
              · 角色「{{ payload.role_name }}」
              <code class="assignee-dialog__code">{{ payload.role_code }}</code>
            </template>
          </span>
        </div>
        <div class="assignee-dialog__row">
          <span class="assignee-dialog__label">该角色当前</span>
          <span class="assignee-dialog__value" data-testid="assignee-dialog-member-count">
            {{ payload.member_count }} 名成员（可用 {{ payload.usable_member_count }} 人）
          </span>
        </div>
        <div class="assignee-dialog__row">
          <span class="assignee-dialog__label">原因</span>
          <span class="assignee-dialog__value assignee-dialog__reason">
            {{ reasonText(payload) }}
            <code class="assignee-dialog__code">{{ payload.reason }}</code>
          </span>
        </div>
        <div v-if="payload.excluded?.length" class="assignee-dialog__row">
          <span class="assignee-dialog__label">被排除的人</span>
          <span class="assignee-dialog__value">{{ payload.excluded.join('；') }}</span>
        </div>
      </div>

      <!-- ② 加人也解决不了的原因：只给「去改流程」的指引 -->
      <t-alert
        v-if="!fixableByAssigning"
        theme="warning"
        :message="payload.fix_hint"
        data-testid="assignee-dialog-not-fixable"
      />

      <!-- ③ 可自助：选人 → 指派 -->
      <template v-else>
        <p class="assignee-dialog__hint">{{ payload.fix_hint }}</p>
        <t-select
          v-model="selectedUserIds"
          class="assignee-dialog__select"
          multiple
          filterable
          clearable
          :loading="usersLoading"
          :options="userOptions"
          placeholder="选择要加入该角色的成员（可多选，支持搜索）"
          data-testid="assignee-dialog-user-select"
        />
        <p v-if="usersError" class="assignee-dialog__error" data-testid="assignee-dialog-users-error">
          {{ usersError }}
        </p>
        <p class="assignee-dialog__note">
          指派只**增加**成员，不会移除任何现有成员；指派成功后会自动重新提交刚才的操作。
        </p>
      </template>

      <!-- ④ 三个出口 -->
      <div class="assignee-dialog__footer">
        <t-button
          theme="default"
          variant="outline"
          data-testid="assignee-dialog-goto-role"
          @click="goRolePage"
        >
          去角色管理加人
        </t-button>
        <div class="assignee-dialog__footer-right">
          <t-button theme="default" data-testid="assignee-dialog-cancel" @click="onCancel">
            取消
          </t-button>
          <t-button
            v-if="fixableByAssigning"
            theme="primary"
            :loading="submitting"
            :disabled="selectedUserIds.length === 0"
            data-testid="assignee-dialog-submit"
            @click="onAssign"
          >
            指派并重新提交
          </t-button>
        </div>
      </div>
    </template>
  </t-dialog>
</template>

<script setup lang="ts">
/**
 * 「指派审批人」引导框。
 *
 * # 数据来源
 *
 * 后端 `65018` 的 `data` 载荷（`utils/flow-assignee-guard.ts::NoAssigneePayload`）。
 * 载荷里的 `role_code` 直接喂给 `POST /api/system/setup/assign`（跨环境稳定）。
 *
 * # 关闭语义
 *
 * - 指派成功 → `settleAssigneeFix(true)` ⇒ 拦截器重试原请求；
 * - 取消 / 去角色管理 → `settleAssigneeFix(false)` ⇒ 原请求按失败返回。
 */
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { useRouter } from 'vue-router';
import { MessagePlugin } from 'tdesign-vue-next';
import { pageUsers, type UserRes } from '@/api/system/user-api';
import { assignSetupMembers } from '@/api/system/setup-api';
import {
  canFixByAssigningRole,
  currentPendingFix,
  reasonText,
  settleAssigneeFix,
  subscribePendingFix,
  type NoAssigneePayload,
} from '@/utils/flow-assignee-guard';

interface PendingFix {
  payload: NoAssigneePayload;
  resolve: (assigned: boolean) => void;
}

const router = useRouter();

/** 当前要处理的 `65018`（`null` = 对话框关闭） */
const pending = ref<PendingFix | null>(null);
const payload = computed(() => pending.value?.payload ?? null);
const visible = computed({
  get: () => pending.value !== null,
  set: (value: boolean) => {
    if (!value) {
      settleAssigneeFix(false);
    }
  },
});

const headerText = computed(() =>
  payload.value?.role_name ? `指派审批人 · ${payload.value.role_name}` : '指派审批人',
);
const fixableByAssigning = computed(() => (payload.value ? canFixByAssigningRole(payload.value) : false));

/** 选中的用户 id */
const selectedUserIds = ref<string[]>([]);
/** 用户下拉项 */
const userOptions = ref<{ label: string; value: string }[]>([]);
const usersLoading = ref(false);
const usersError = ref('');
const submitting = ref(false);

/** 拉用户列表（`system:user:list`；没有权限时只提示，不影响「去角色管理」这条路） */
async function loadUsers(): Promise<void> {
  usersLoading.value = true;
  usersError.value = '';
  try {
    const page = await pageUsers({ page: 1, page_size: 200, status: 1 });
    userOptions.value = page.list
      .filter((user: UserRes) => user.user_type !== 'ROBOT')
      .map((user: UserRes) => ({
        label: user.real_name ? `${user.real_name}（${user.username}）` : user.username,
        value: user.id,
      }));
  } catch (error) {
    usersError.value = `无法加载用户列表：${(error as Error).message}（可改用「去角色管理加人」）`;
  } finally {
    usersLoading.value = false;
  }
}

watch(pending, (value) => {
  selectedUserIds.value = [];
  usersError.value = '';
  if (value && canFixByAssigningRole(value.payload) && userOptions.value.length === 0) {
    void loadUsers();
  }
});

/** 指派：调 `/system/setup/assign` → 关闭并让拦截器重试 */
async function onAssign(): Promise<void> {
  const current = pending.value;
  if (!current) {
    return;
  }
  const target = current.payload;
  if (!target.role_code) {
    void MessagePlugin.error('缺少角色编码，无法指派：请到角色管理里手工加人');
    return;
  }
  submitting.value = true;
  try {
    const result = await assignSetupMembers({
      role_code: target.role_code,
      user_ids: selectedUserIds.value,
    });
    void MessagePlugin.success(
      `已为角色「${result.role_name}」指派 ${result.added.length} 人（现共 ${result.member_count} 人），正在重新提交…`,
    );
    settleAssigneeFix(true);
  } catch (error) {
    void MessagePlugin.error(`指派失败：${(error as Error).message}`);
  } finally {
    submitting.value = false;
  }
}

/** 去角色管理（放弃自动重试：用户要手工加人，加完自己再提交一次） */
function goRolePage(): void {
  settleAssigneeFix(false);
  void router.push('/system/role');
}

/** 取消 */
function onCancel(): void {
  settleAssigneeFix(false);
}

let unsubscribe: (() => void) | null = null;

onMounted(() => {
  pending.value = currentPendingFix();
  unsubscribe = subscribePendingFix((value) => {
    pending.value = value;
  });
});

onBeforeUnmount(() => {
  unsubscribe?.();
  unsubscribe = null;
});
</script>

<style scoped>
.assignee-dialog__summary {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 12px;
  margin-bottom: 12px;
  background: var(--td-bg-color-container-hover, #f5f7fa);
  border-radius: 6px;
}

.assignee-dialog__row {
  display: flex;
  gap: 8px;
  font-size: 13px;
  line-height: 20px;
}

.assignee-dialog__label {
  flex: 0 0 84px;
  color: var(--td-text-color-secondary, #00000099);
}

.assignee-dialog__value {
  flex: 1;
  color: var(--td-text-color-primary, #000000e6);
  word-break: break-all;
}

.assignee-dialog__reason {
  color: var(--td-warning-color, #ed7b2f);
}

.assignee-dialog__code {
  margin-left: 4px;
  padding: 0 4px;
  font-size: 12px;
  color: var(--td-text-color-secondary, #00000099);
  background: var(--td-bg-color-component, #f0f0f0);
  border-radius: 3px;
}

.assignee-dialog__hint {
  margin: 0 0 8px;
  font-size: 13px;
  color: var(--td-text-color-primary, #000000e6);
}

.assignee-dialog__select {
  width: 100%;
}

.assignee-dialog__note {
  margin: 8px 0 0;
  font-size: 12px;
  color: var(--td-text-color-secondary, #00000099);
}

.assignee-dialog__error {
  margin: 8px 0 0;
  font-size: 12px;
  color: var(--td-error-color, #d54941);
}

.assignee-dialog__footer {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-top: 16px;
}

.assignee-dialog__footer-right {
  display: flex;
  gap: 8px;
}
</style>
