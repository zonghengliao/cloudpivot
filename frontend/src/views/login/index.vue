<template>
  <div class="login">
    <div class="login__panel">
      <!-- 左侧品牌区：柔和渐变底 + 一句话定位 -->
      <section
        ref="brandEl"
        class="login__brand"
        @mousemove="fx.onMouseMove"
        @mouseleave="fx.onMouseLeave"
        @touchmove="fx.onTouchMove"
      >
        <div class="login__brand-glow" aria-hidden="true"></div>
        <!-- 品牌装饰图（脑 + 电路手绘稿）：低透明度衬底，只做氛围不抢内容。
             原图是横向米白网格稿，已做「背景渐变透明 + 边缘羽化」处理（public/brand-brain.webp） -->
        <img class="login__brand-deco" src="/brand-brain.webp" alt="" aria-hidden="true" />
        <!-- 星座连线特效层：点与线都跟随鼠标（见 use-constellation-fx.ts）。
             pointer-events: none，不挡表单；prefers-reduced-motion 时不启动 -->
        <canvas ref="fxCanvas" class="login__brand-fx" aria-hidden="true"></canvas>

        <div class="login__brand-inner">
          <!-- 主内容贴顶：品牌识别（logo + 名称）居上，页脚保持沉底 -->
          <div class="login__brand-main">
            <div class="login__brand-head">
              <span class="login__badge">
                <img src="/logo.png" alt="云枢" />
              </span>
              <p class="login__brand-name">云枢 CloudPivot</p>
            </div>

            <h2 class="login__brand-title">「可验证、可定制」的业务中台</h2>
            <p class="login__brand-desc">
              以 ERP 业务域（采购 / 销售 / 库存 / 财务）为落地场景，<br>
              把流程、权限、审计、验证四件事做成底座能力
            </p>
          </div>

          <p class="login__brand-footer">Rust + nik · Vue 3 + TDesign</p>
        </div>
      </section>

      <!-- 右侧表单区 -->
      <section class="login__form-side">
        <!-- 右侧 Logo 仅小屏出现：大屏由左侧品牌区承载，两边同时出现会重复 -->
        <span class="login__badge login__badge--form">
          <img src="/logo.png" alt="云枢" />
        </span>

        <header class="login__form-head">
          <h1 class="login__form-title">账号登录</h1>
          <p class="login__form-subtitle">请使用后台分配的用户名与密码登录</p>
        </header>

        <t-alert v-if="errorMessage" class="login__alert" theme="error" :message="errorMessage" />
        <!-- 空闲超时被踢回：登录页必须说清原因，否则用户以为是系统故障（卡面验收 ⑦） -->
        <t-alert v-else-if="idleNotice" class="login__alert" theme="warning" :message="idleNoticeText" />


        <t-form :data="formData" :rules="formRules" label-width="0" @submit="handleSubmit">
          <t-form-item name="username">
            <t-input
              v-model="formData.username"
              size="large"
              clearable
              placeholder="请输入用户名"
              autocomplete="username"
            >
              <template #prefix-icon><t-icon name="user" /></template>
            </t-input>
          </t-form-item>
          <t-form-item name="password">
            <t-input
              v-model="formData.password"
              type="password"
              size="large"
              clearable
              placeholder="请输入密码"
              autocomplete="current-password"
            >
              <template #prefix-icon><t-icon name="lock-on" /></template>
            </t-input>
          </t-form-item>
          <t-form-item>
            <t-button block theme="primary" size="large" type="submit" :loading="submitting">
              登 录
            </t-button>
          </t-form-item>
        </t-form>

        <p class="login__hint">首次登录请使用运维注入的初始口令，并立即修改</p>
        <p class="login__signup">还没有账号？<b>联系管理员开通</b></p>
      </section>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, reactive, ref } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import type { FormRules } from 'tdesign-vue-next';
import type { LoginParams } from '@/api/system/auth-api';
import { ApiError } from '@/api/request';
import { useUserStore } from '@/stores/user';
import { clearLogoutBroadcast, resetActivity, takeIdleRedirect } from '@/utils/idle-activity';
import { safeRedirect } from '@/utils/safe-redirect';
import { useConstellationFx } from '@/composables/use-constellation-fx';

const route = useRoute();
const router = useRouter();
const userStore = useUserStore();

// 左侧品牌区的星座连线特效：点与线跟随鼠标（不动鼠标时几乎静止）
// glow: false —— 用户要求「鼠标移上去不要发光」，只保留点与线
const brandEl = ref<HTMLElement | null>(null);
const fxCanvas = ref<HTMLCanvasElement | null>(null);
const fx = useConstellationFx(fxCanvas, brandEl, { glow: false });

/** 登录表单数据 */
const formData = reactive<LoginParams>({ username: '', password: '' });
/** 登录表单校验规则 */
const formRules: FormRules<LoginParams> = {
  username: [{ required: true, message: '请输入用户名' }],
  password: [{ required: true, message: '请输入密码' }],
};
/** 提交中标记 */
const submitting = ref(false);
/** 登录失败提示 */
const errorMessage = ref('');

/**
 * 是否被送回了登录页（`?reason=idle|manual|changed`）。
 *
 * 三条退出路径都会带 `reason`：
 * - `idle` —— 空闲超时（前端计时器 / 服务端 401 `20009` / 其它标签页的广播）；
 * - `manual` —— 自己点了退出；
 * - `changed` —— 权限刚被管理员调整过（服务端 401 `20008`，见 `api/request.ts::isPermissionChanged`）。
 * 提示语写人话，不出现 "idle timeout" 这种字眼。
 */
const idleNotice = computed<boolean>(
  () => route.query.reason === 'idle' || route.query.reason === 'manual' || route.query.reason === 'changed',
);
/** 提示文案 */
const idleNoticeText = computed<string>(() => {
  if (route.query.reason === 'manual') {
    return '您已退出登录';
  }
  if (route.query.reason === 'changed') {
    return '您的权限刚被管理员调整，请重新登录以加载最新权限';
  }
  return '因长时间未操作已自动退出，请重新登录';
});

/**
 * 提交登录：失败提示留在表单内，成功后回到「空闲退出前所在页面」。
 *
 * 回跳优先级：`sessionStorage.idle_logout_redirect`（空闲退出时记下）> URL `?redirect=`
 * > 工作台。两者都是可篡改输入，统一过 {@link safeRedirect} 防开放重定向
 * （卡面验收 ⑰⑱：`//evil.com` / `http://evil.com` 必须回首页，不能出站）。
 */
async function handleSubmit(): Promise<void> {
  // 账号密码去掉首尾空白（复制粘贴常带上不可见空格），后端登录接口同样会 trim 兜底
  const username = formData.username.trim();
  const password = formData.password.trim();
  submitting.value = true;
  errorMessage.value = '';
  try {
    await userStore.login({ username, password });
    const savedRedirect = takeIdleRedirect();
    const queryRedirect = typeof route.query.redirect === 'string' ? route.query.redirect : '';
    // 新一轮会话：重置空闲计时起点，并清掉上一轮的退出广播
    resetActivity();
    clearLogoutBroadcast();
    await router.replace(safeRedirect(savedRedirect || queryRedirect));
  } catch (error) {
    errorMessage.value = error instanceof ApiError ? error.message : '登录失败，请稍后重试';
  } finally {
    submitting.value = false;
  }
}
</script>

<style scoped lang="less">
.login {
  position: relative;
  display: flex;
  align-items: center;
  justify-content: center;
  width: 100%;
  // 用 vh 而不是 100%：<router-view> 的包裹层不保证撑满，百分比会塌成内容高度
  min-height: 100vh;
  padding: 32px 24px;
  overflow: hidden;
  // 雾面纸主底（深色主题走 --app-login-bg）
  background: var(--app-login-bg);

  // ① 两团配色光晕：左上走主色、右下走强调色，随配色切换整体换色。
  //    透明度压得很低（12~14%），只负责让白底"有呼吸"，不抢卡片。
  &::before {
    position: absolute;
    inset: -15%;
    content: '';
    pointer-events: none;
    background:
      radial-gradient(
        circle at 14% 16%,
        color-mix(in srgb, var(--app-brand) 14%, transparent) 0%,
        transparent 44%
      ),
      radial-gradient(
        circle at 86% 84%,
        color-mix(in srgb, var(--app-brand-accent) 16%, transparent) 0%,
        transparent 48%
      );
  }

  // ② 大图标水印：右下角半出画，极淡（5%），只做肌理不做主体。
  //    用品牌装饰图本身，和左侧品牌区呼应；遮罩从中心往外化开，避免出现硬边。
  &::after {
    position: absolute;
    right: -9%;
    bottom: -14%;
    width: min(760px, 62vw);
    aspect-ratio: 1;
    content: '';
    pointer-events: none;
    background: url('/brand-brain.webp') center / contain no-repeat;
    opacity: 0.055;
    -webkit-mask-image: radial-gradient(circle at 48% 48%, #000 28%, transparent 70%);
    mask-image: radial-gradient(circle at 48% 48%, #000 28%, transparent 70%);
  }

  // 深色主题：光晕在深底上会发灰，提高一点饱和度并压低水印
  :root[theme-mode='dark'] &::before {
    filter: saturate(1.25);
    opacity: 0.85;
  }

  :root[theme-mode='dark'] &::after {
    opacity: 0.035;
  }
}

.login__panel {
  position: relative;
  z-index: 1; // 压在 .login 的装饰伪元素（光晕 / 水印）之上
  display: flex;
  width: 100%;
  max-width: 960px;
  // 给卡片一个最小高度，左右两栏才有共同的体量，右侧内容垂直居中才有意义
  min-height: 520px;
  overflow: hidden;
  background-color: var(--td-bg-color-container);
  border: 1px solid var(--td-component-stroke);
  border-radius: 16px;
  box-shadow: 0 18px 48px rgb(111 138 184 / 14%);
}

// ---------- 左侧品牌区 ----------
.login__brand {
  position: relative;
  flex: 1 1 auto;
  min-width: 0;
  padding: 48px 44px;
  overflow: hidden;
  // 柔和渐变底：霜月白 → 雾蓝灰 → 漂木棕微暖收尾（深色主题走 --app-login-brand）
  background: var(--app-login-brand);
}

// 柔光装饰：不抢内容，只负责让渐变不死板（强调色随配色走，不再写死流金）
.login__brand-glow {
  position: absolute;
  top: -120px;
  right: -140px;
  width: 320px;
  height: 320px;
  background: radial-gradient(
    circle,
    color-mix(in srgb, var(--app-brand-accent) 30%, transparent) 0%,
    transparent 70%
  );
  pointer-events: none;
}

// 品牌装饰图（脑 + 电路手绘稿）：衬在渐变底上做氛围，不抢文字
// 低透明度 + 径向羽化遮罩，让它从背景里「长出来」而不是贴上去
.login__brand-deco {
  position: absolute;
  right: -76px;
  bottom: -56px;
  width: 430px;
  max-width: 92%;
  opacity: 0.17;
  pointer-events: none;
  user-select: none;
  filter: saturate(0.55) contrast(1.06);
  -webkit-mask-image: radial-gradient(ellipse at 42% 44%, #000 36%, transparent 76%);
  mask-image: radial-gradient(ellipse at 42% 44%, #000 36%, transparent 76%);
}

// 星座连线特效层：z-index 1（在装饰图之上、文字之下），不接收鼠标事件
// 尺寸交给 CSS 100%，像素密度由 composable 里的 DPR 处理
.login__brand-fx {
  position: absolute;
  inset: 0;
  z-index: 1;
  width: 100%;
  height: 100%;
  pointer-events: none;
}

.login__brand-inner {
  position: relative;
  // 3：压在 gl-flow（1）和装饰图（0）之上，保证文字永远可读
  z-index: 3;
  display: flex;
  flex-direction: column;
  height: 100%;
}

// 主内容组贴顶展示：品牌识别（logo + 名称）在视觉重心上方，标题与描述紧随其后
.login__brand-main {
  margin: 0;
}

.login__brand-head {
  display: flex;
  align-items: center;
  gap: 12px;
}

// 方形圆角徽章：圆角矩形裁切 + 外阴影，与整体卡片风格呼应
.login__badge {
  display: inline-flex;
  flex: none;
  align-items: center;
  justify-content: center;
  width: 40px;
  height: 40px;
  overflow: hidden;
  background-color: rgb(255 255 255 / 92%);
  border-radius: 12px;
  box-shadow:
    inset 0 0 0 1px rgb(255 255 255 / 80%),
    0 6px 16px rgb(111 138 184 / 22%);

  > img {
    width: 70%;
    height: 70%;
    object-fit: contain;
  }
}

// 右侧表单区 Logo：与左侧品牌区徽章**分开控制尺寸**——左侧 40px，这里 52px
// （右侧是登录后进入系统的唯一品牌标识，需要更醒目；同为方形圆角 + 阴影）
.login__badge--form {
  display: none;
  width: 68px;
  height: 68px;
  border-radius: 14px;

  > img {
    width: 80%;
    height: 80%;
  }
}

.login__brand-name {
  margin: 0;
  font-size: 24px;
  font-weight: 700;
  line-height: 1.3;
  letter-spacing: 1px;
  color: var(--td-text-color-primary);
}

.login__brand-title {
  margin: 128px 0 12px;
  font-size: 24px;
  font-weight: 600;
  line-height: 1.4;
  color: var(--td-text-color-primary);
}

.login__brand-desc {
  margin: 0;
  font-size: 14px;
  line-height: 1.7;
  color: var(--td-text-color-secondary);
}

// margin-top auto：主内容贴顶后，页脚由弹性布局压到品牌区底部
.login__brand-footer {
  margin-top: auto;
  padding-top: 24px;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

// ---------- 右侧表单区 ----------
.login__form-side {
  display: flex;
  flex-direction: column;
  justify-content: center;
  flex: 0 0 400px;
  padding: 48px 40px;
  background-color: var(--td-bg-color-container);
}

.login__form-head {

  margin-bottom: 28px;
}

.login__form-title {
  margin: 0 0 6px;
  font-size: 22px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.login__form-subtitle {
  margin: 0;
  font-size: 13px;
  color: var(--td-text-color-placeholder);
}

.login__alert {
  margin-bottom: 16px;
}

.login__hint {
  margin: 20px 0 0;
  font-size: 12px;
  text-align: center;
  color: var(--td-text-color-placeholder);
}

.login__signup {
  margin: 10px 0 0;
  font-size: 12px;
  text-align: center;
  color: var(--td-text-color-placeholder);

  b {
    font-weight: 500;
    color: var(--td-brand-color);
  }
}

// ---------- 响应式 ----------
// 中等屏：品牌区收紧留白，避免文字被挤到换行
@media (max-width: 1024px) {
  .login__panel {
    max-width: 840px;
    min-height: 480px;
  }

  .login__form-side {
    flex-basis: 360px;
    padding: 40px 32px;
  }

  .login__brand {
    padding: 40px 32px;

    .login__brand-title {
      margin-top: 128px;
      font-size: 21px;
    }
  }
}

// 小屏：左侧品牌区整体隐藏，整页只保留表单，不做上下堆叠
@media (max-width: 768px) {
  .login {
    padding: 0;
  }

  .login__panel {
    display: block;
    // 左侧隐藏后卡片不再需要最小高度，改由表单区占满一屏
    min-height: 0;
    max-width: none;
    border: 0;
    border-radius: 0;
    box-shadow: none;
  }

  .login__brand {
    display: none;
  }

  .login__form-side {
    display: flex;
    flex-direction: column;
    justify-content: center;
    min-height: 100vh;
    padding: 32px 24px;
  }

  // 左侧品牌区隐藏后，右侧补同款方形圆角徽章承载品牌识别
  .login__badge--form {
    display: inline-flex;
    margin: 0 auto 22px;

  }
}
</style>
