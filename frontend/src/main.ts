import { createApp } from 'vue';
import { createPinia } from 'pinia';
import 'tdesign-vue-next/es/style/index.css';

import App from '@/App.vue';
import router from '@/router';
import { setupDirectives } from '@/directives';
import { setupGlobalErrorHandler } from '@/utils/global-error';
import { useSettingStore } from '@/stores/setting';
import { initPalette } from '@/composables/usePalette';
import '@/style/index.less';

// 注册全局错误兜底：捕获未被页面处理的异常，避免静默失败
setupGlobalErrorHandler();

const app = createApp(App);

// 指令内部依赖 Pinia 的 store，需在 use(createPinia()) 之后再创建使用
const pinia = createPinia();
app.use(pinia);

// 主题与配色必须在 mount 之前落地，否则首屏会先闪一帧默认蓝再跳色：
//   1) initTheme() 把明暗模式写到 <html theme-mode>
//   2) initPalette() 由色板推导 6×2 套 CSS 变量，写进 <html> 的内联 style
// theme.less 里的变量仍是兜底（JS 未执行/异常时不至于白屏）
useSettingStore(pinia).initTheme();
initPalette();

setupDirectives(app);
app.use(router);
app.mount('#app');
