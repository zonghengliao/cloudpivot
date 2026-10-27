<!-- 首登强制改密页：must_change_password = true 的用户登录后被路由守卫钉在此页 -->
<template>
  <div class="force-pwd">
    <div class="force-pwd__panel">
      <header class="force-pwd__head">
        <h1 class="force-pwd__title">修改初始密码</h1>
        <p class="force-pwd__desc">
          当前账号使用的是初始密码，必须先修改后才能进入工作台。新密码请勿与原密码相同。
        </p>
      </header>

      <t-alert v-if="errorMessage" class="force-pwd__alert" theme="error" :message="errorMessage" />

      <t-form
        ref="formRef"
        :data="form"
        :rules="rules"
        label-align="top"
        @submit="onSubmit"
      >
        <t-form-item label="原密码（初始密码）" name="old_password">
          <t-input v-model="form.old_password" type="password" placeholder="请输入初始密码" />
        </t-form-item>
        <t-form-item label="新密码" name="new_password">
          <t-input
            v-model="form.new_password"
            type="password"
            placeholder="至少 6 位，建议包含大小写字母、数字与符号"
          />
        </t-form-item>
        <t-form-item label="确认新密码" name="confirm_password">
          <t-input v-model="form.confirm_password" type="password" placeholder="请再次输入新密码" />
        </t-form-item>
        <t-form-item>
          <t-button theme="primary" type="submit" block :loading="submitting">确认修改并重新登录</t-button>
        </t-form-item>
      </t-form>

      <t-button variant="text" block :disabled="submitting" @click="onLogout">退出登录</t-button>
    </div>
  </div>
</template>

<script setup lang="ts">
import { onMounted, reactive, ref } from 'vue';
import { useRouter } from 'vue-router';
import { MessagePlugin, type FormInstanceFunctions, type FormRules } from 'tdesign-vue-next';
import { changePassword } from '@/api/system/auth-api';
import { useUserStore } from '@/stores/user';

/** 改密表单 */
interface PasswordForm {
  /** 原密码 */
  old_password: string;
  /** 新密码 */
  new_password: string;
  /** 确认新密码 */
  confirm_password: string;
}

const router = useRouter();
const userStore = useUserStore();
const formRef = ref<FormInstanceFunctions | null>(null);
const submitting = ref(false);
const errorMessage = ref('');
const form = reactive<PasswordForm>({ old_password: '', new_password: '', confirm_password: '' });

const rules: FormRules<PasswordForm> = {
  old_password: [{ required: true, message: '请输入原密码' }],
  new_password: [
    { required: true, message: '请输入新密码' },
    { min: 6, message: '新密码至少 6 位' },
    {
      validator: (value: string) => value !== form.old_password,
      message: '新密码不能与原密码相同',
    },
  ],
  confirm_password: [
    { required: true, message: '请再次输入新密码' },
    {
      validator: (value: string) => value === form.new_password,
      message: '两次输入的新密码不一致',
    },
  ],
};

onMounted(() => {
  // 直接访问改密页但无需改密时，交给路由守卫处理（这里只兜底清错误）
  errorMessage.value = '';
});

async function onSubmit(): Promise<void> {
  const valid = await formRef.value?.validate();
  if (valid !== true) {
    return;
  }
  submitting.value = true;
  errorMessage.value = '';
  try {
    await changePassword({ old_password: form.old_password, new_password: form.new_password });
    // 后端改密后递增 token_version，既有令牌全部失效：必须用新密码重新登录
    MessagePlugin.success('密码已修改，请用新密码重新登录');
    userStore.reset();
    await router.replace('/login');
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : '修改密码失败，请稍后重试';
  } finally {
    submitting.value = false;
  }
}

async function onLogout(): Promise<void> {
  await userStore.logout();
  await router.replace('/login');
}
</script>

<style scoped lang="less">
.force-pwd {
  display: flex;
  align-items: center;
  justify-content: center;
  min-height: 100vh;
  background: var(--app-page-bg, #f2f2ee);
  padding: 24px;

  &__panel {
    width: 100%;
    max-width: 460px;
    padding: 32px;
    background: var(--app-card-bg, #ffffff);
    border-radius: 12px;
    box-shadow: 0 8px 24px rgb(81 105 124 / 12%);
  }

  &__head {
    margin-bottom: 20px;
  }

  &__title {
    margin: 0 0 8px;
    font-size: 20px;
    font-weight: 600;
    color: var(--app-text-primary, #2a3843);
  }

  &__desc {
    margin: 0;
    font-size: 13px;
    line-height: 20px;
    color: var(--app-text-secondary, #5f6e7a);
  }

  &__alert {
    margin-bottom: 16px;
  }
}
</style>
