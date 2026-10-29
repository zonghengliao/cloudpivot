<template>
  <div class="cl-detail-page">
    <!-- 头部：返回 + 元信息 -->
    <t-card :bordered="false" class="head-card">
      <div class="head">
        <div class="head__left">
          <t-button variant="text" size="small" @click="goBack">
            <template #icon><t-icon name="chevron-left" /></template>
            返回列表
          </t-button>
          <div class="head__title">
            <t-tag size="small" variant="light" :theme="actionTheme(detail?.action)">
              {{ detail?.action_label || detail?.action || '变更' }}
            </t-tag>
            <span>{{ detail?.resource || '—' }}</span>
            <span class="head__pk">{{ detail?.resource_id || '—' }}</span>
          </div>
        </div>
        <t-space size="small">
          <t-button size="small" variant="outline" :loading="loading" @click="loadDetail">
            <template #icon><t-icon name="refresh" /></template>
            刷新
          </t-button>
        </t-space>
      </div>

      <t-loading :loading="loading" size="small">
        <div v-if="detail" class="meta-grid">
          <div class="meta-grid__item">
            <span class="meta-grid__label">发生时间</span>
            <span class="meta-grid__value">{{ formatDateTime(detail.created_at) }}</span>
          </div>
          <div class="meta-grid__item">
            <span class="meta-grid__label">模块 / 资源</span>
            <span class="meta-grid__value">{{ detail.module }} / {{ detail.resource }}</span>
          </div>
          <div class="meta-grid__item">
            <span class="meta-grid__label">操作人</span>
            <span class="meta-grid__value">{{ detail.operator_name || '（系统）' }}</span>
          </div>
          <div class="meta-grid__item">
            <span class="meta-grid__label">来源</span>
            <span class="meta-grid__value">{{ detail.trace_source }}</span>
          </div>
          <div class="meta-grid__item">
            <span class="meta-grid__label">客户端 IP</span>
            <span class="meta-grid__value">{{ detail.client_ip || '—' }}</span>
          </div>
          <div class="meta-grid__item">
            <span class="meta-grid__label">请求 ID</span>
            <span class="meta-grid__value meta-grid__value--mono">{{ detail.request_id || '—' }}</span>
          </div>
          <div class="meta-grid__item">
            <span class="meta-grid__label">diff 模式</span>
            <span class="meta-grid__value">{{ detail.diff_mode }}</span>
          </div>
          <div class="meta-grid__item">
            <span class="meta-grid__label">耗时</span>
            <span class="meta-grid__value">{{ detail.duration_ms === null ? '—' : `${detail.duration_ms} ms` }}</span>
          </div>
          <div class="meta-grid__item">
            <span class="meta-grid__label">脱敏</span>
            <span class="meta-grid__value">
              <t-tag v-if="detail.masked" size="small" variant="light" theme="warning">含脱敏值</t-tag>
              <span v-else>无</span>
            </span>
          </div>
          <div class="meta-grid__item">
            <span class="meta-grid__label">变化字段</span>
            <span class="meta-grid__value">{{ detail.changed_fields.join('、') || '—' }}</span>
          </div>
        </div>
      </t-loading>
    </t-card>

    <!-- diff 对比表：字段 / 旧值 / 新值 -->
    <t-card :bordered="false" class="diff-card">
      <div class="section-head">
        <div class="section-head__title">
          <t-icon name="swap" />
          <span>字段变更对比</span>
          <t-tag size="small" variant="light">{{ detail?.fields.length ?? 0 }} 个字段</t-tag>
        </div>
        <t-checkbox v-model="hideUnchanged" size="small">隐藏前后相同的字段</t-checkbox>
      </div>

      <t-loading :loading="loading" size="small">
        <div v-if="visibleFields.length > 0" class="diff-table">
          <div class="diff-table__head">
            <div class="diff-table__cell diff-table__cell--field">字段</div>
            <div class="diff-table__cell diff-table__cell--old">旧值（b）</div>
            <div class="diff-table__cell diff-table__cell--new">新值（a）</div>
          </div>
          <div v-for="row in visibleFields" :key="row.field" class="diff-table__row">
            <div class="diff-table__cell diff-table__cell--field">
              <span class="diff-table__name">{{ row.field }}</span>
              <t-tag v-if="row.redacted" size="small" variant="light" theme="danger">整列遮蔽</t-tag>
              <t-tag v-else-if="row.masked" size="small" variant="light" theme="warning">脱敏</t-tag>
            </div>
            <div class="diff-table__cell diff-table__cell--old">
              <pre class="diff-table__value">{{ formatValue(row.before) }}</pre>
            </div>
            <div class="diff-table__cell diff-table__cell--new">
              <pre class="diff-table__value">{{ formatValue(row.after) }}</pre>
            </div>
          </div>
        </div>
        <div v-else-if="!loading" class="empty-state">
          <t-icon class="empty-state__icon" name="swap" />
          <p class="empty-state__title">没有可逐字段展示的变更</p>
          <p class="empty-state__desc">大字段（&gt; 2 KiB）会走下面的摘要行，不在这里展开原值</p>
        </div>
      </t-loading>
    </t-card>

    <!-- 大字段摘要：已截断，共 N 字节 -->
    <t-card v-if="(detail?.big.length ?? 0) > 0" :bordered="false" class="big-card">
      <div class="section-head">
        <div class="section-head__title">
          <t-icon name="file-copy" />
          <span>大字段摘要（不落原值）</span>
          <t-tag size="small" variant="light" theme="warning">{{ detail?.big.length }} 个字段</t-tag>
        </div>
        <span class="section-head__hint">超过 2 KiB 的字段只记长度 / sha256 / 预览，避免日志表被正文撑爆</span>
      </div>

      <div v-for="item in detail?.big ?? []" :key="item.field" class="big-item">
        <div class="big-item__field">{{ item.field }}</div>
        <div class="big-item__cols">
          <div class="big-item__col">
            <div class="big-item__label">旧值（b）</div>
            <div class="big-item__truncated">已截断，共 {{ item.before.len }} 字节</div>
            <div class="big-item__sha">sha256: {{ shortHash(item.before.sha256) }}</div>
            <pre class="big-item__preview">{{ item.before.preview || '（无预览）' }}</pre>
            <t-tag v-if="item.before.redacted" size="small" variant="light" theme="danger">已脱敏</t-tag>
          </div>
          <div class="big-item__col">
            <div class="big-item__label">新值（a）</div>
            <div class="big-item__truncated">已截断，共 {{ item.after.len }} 字节</div>
            <div class="big-item__sha">sha256: {{ shortHash(item.after.sha256) }}</div>
            <pre class="big-item__preview">{{ item.after.preview || '（无预览）' }}</pre>
            <t-tag v-if="item.after.redacted" size="small" variant="light" theme="danger">已脱敏</t-tag>
          </div>
        </div>
      </div>
    </t-card>

    <!-- 原始 diff（调试用，折叠） -->
    <t-card :bordered="false" class="raw-card">
      <t-collapse :default-expanded="[]">
        <t-collapse-panel header="原始 diff JSON（排障用）" value="raw">
          <pre class="raw-json">{{ rawJson }}</pre>
        </t-collapse-panel>
      </t-collapse>
    </t-card>
  </div>
</template>

<script setup lang="ts">
/**
 * 变更日志详情（`system:change-log:list`）。
 *
 * # 这个页面在回答什么
 *
 * 「**谁**在**什么时候**把**哪条数据的哪个字段**从**什么**改成了**什么**」——
 * 所以正文只有两块：逐字段 diff 表（`{b,a}` 成对）与大字段摘要。
 *
 * # 三个显示口径（不要「优化」掉）
 *
 * 1. `null` 与「空字符串」**必须能区分**：`null` 显示 `null`（灰字），空串显示 `""`；
 * 2. 大字段**不展示原值**：只显示「已截断，共 N 字节」+ sha256 前 16 位 + 预览，
 *    假装它是完整内容就是误导审计；
 * 3. `masked` / `redacted` 只加标记、不再二次打码 —— 库内存的**本来就是**脱敏值。
 */
import { computed, onMounted, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import { useRoute, useRouter } from 'vue-router';
import { ApiError } from '@/api/request';
import { getChangeLog, type ChangeLogDetail } from '@/api/business/changelog-api';
import { formatDateTime } from '@/utils/format';

const route = useRoute();
const router = useRouter();

const detail = ref<ChangeLogDetail | null>(null);
const loading = ref(false);
/** 默认隐藏「前后相同」的字段（白名单里登记但值没变的行不该占版面） */
const hideUnchanged = ref(true);

/** 当前日志 id（路由参数） */
const logId = computed(() => String(route.params['id'] ?? ''));
/** 业务发生时刻（query 传了能让后端只扫一个分区） */
const createdAt = computed(() => {
  const value = route.query['created_at'];
  return typeof value === 'string' ? value : undefined;
});

/** 逐字段 diff 过滤 */
const visibleFields = computed(() => {
  const fields = detail.value?.fields ?? [];
  if (!hideUnchanged.value) {
    return fields;
  }
  return fields.filter((row) => !isSameValue(row.before, row.after));
});

/** 原始 diff JSON（美化输出；解析失败就原样展示） */
const rawJson = computed(() => {
  const raw = detail.value?.diff;
  if (raw === undefined || raw === null) {
    return '—';
  }
  try {
    return JSON.stringify(raw, null, 2);
  } catch {
    return String(raw);
  }
});

/** 两个值是否相等（对象走 JSON 比较，够用：后端 diff 值都是可序列化的标量/小对象） */
function isSameValue(before: unknown, after: unknown): boolean {
  try {
    return JSON.stringify(before) === JSON.stringify(after);
  } catch {
    return false;
  }
}

/** 值 → 展示文本（`null` 与空串要能区分） */
function formatValue(value: unknown): string {
  if (value === null) {
    return 'null';
  }
  if (value === undefined) {
    return '—';
  }
  if (typeof value === 'string') {
    return value === '' ? '""（空串）' : value;
  }
  if (typeof value === 'number' || typeof value === 'boolean') {
    return String(value);
  }
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

/** sha256 只显示前 16 位（足够人工比对，又不会把表格撑爆） */
function shortHash(hash: string): string {
  if (!hash) {
    return '—';
  }
  return `${hash.slice(0, 16)}…（${hash.length} 字符）`;
}

function actionTheme(action?: string): 'primary' | 'success' | 'warning' | 'danger' | 'default' {
  switch ((action || '').toUpperCase()) {
    case 'CREATE':
      return 'success';
    case 'DELETE':
      return 'danger';
    case 'PUBLISH':
    case 'OFFLINE':
      return 'warning';
    case 'UPDATE':
      return 'primary';
    default:
      return 'default';
  }
}

async function loadDetail(): Promise<void> {
  if (!logId.value) {
    return;
  }
  loading.value = true;
  try {
    detail.value = await getChangeLog(logId.value, createdAt.value);
  } catch (error) {
    detail.value = null;
    void MessagePlugin.error(error instanceof ApiError ? error.message : '加载变更详情失败');
  } finally {
    loading.value = false;
  }
}

function goBack(): void {
  void router.push('/system/change-log');
}

onMounted(() => {
  void loadDetail();
});
</script>

<style scoped lang="less">
.cl-detail-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 12px;

  &__title {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 8px;
    font-size: 17px;
    font-weight: 600;
    color: var(--td-text-color-primary);
  }

  &__pk {
    font-family: var(--app-font-mono, ui-monospace, SFMono-Regular, Menlo, Consolas, monospace);
    font-size: 13px;
    font-weight: 400;
    color: var(--td-text-color-secondary);
  }
}

.meta-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(240px, 1fr));
  gap: 10px 20px;

  &__item {
    display: flex;
    align-items: baseline;
    gap: 8px;
    font-size: 13px;
  }

  &__label {
    flex: 0 0 72px;
    color: var(--td-text-color-placeholder);
  }

  &__value {
    color: var(--td-text-color-primary);
    word-break: break-all;

    &--mono {
      font-family: var(--app-font-mono, ui-monospace, SFMono-Regular, Menlo, Consolas, monospace);
      font-size: 12px;
    }
  }
}

.section-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 12px;
  flex-wrap: wrap;

  &__title {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 15px;
    font-weight: 600;
    color: var(--td-text-color-primary);
  }

  &__hint {
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }
}

.diff-table {
  border: 1px solid var(--td-component-stroke);
  border-radius: 8px;
  overflow: hidden;

  &__head {
    display: flex;
    background: var(--app-table-head-bg);
    font-size: 13px;
    font-weight: 600;
    color: var(--td-text-color-secondary);
  }

  &__row {
    display: flex;
    border-top: 1px solid var(--td-component-stroke);

    &:nth-child(even) {
      background: var(--td-bg-color-container-hover);
    }
  }

  &__cell {
    padding: 10px 12px;
    min-width: 0;
    font-size: 13px;

    &--field {
      flex: 0 0 190px;
      display: flex;
      align-items: flex-start;
      gap: 6px;
      flex-wrap: wrap;
      border-right: 1px solid var(--td-component-stroke);
    }

    &--old {
      flex: 1;
      border-right: 1px solid var(--td-component-stroke);
    }

    &--new {
      flex: 1;
    }
  }

  &__name {
    font-family: var(--app-font-mono, ui-monospace, SFMono-Regular, Menlo, Consolas, monospace);
    color: var(--td-text-color-primary);
    word-break: break-all;
  }

  &__value {
    margin: 0;
    max-height: 220px;
    overflow: auto;
    font-family: var(--app-font-mono, ui-monospace, SFMono-Regular, Menlo, Consolas, monospace);
    font-size: 12px;
    line-height: 1.6;
    white-space: pre-wrap;
    word-break: break-all;
    color: var(--td-text-color-primary);
  }
}

.big-item {
  padding: 12px 0;
  border-top: 1px solid var(--td-component-stroke);

  &:first-of-type {
    border-top: 0;
  }

  &__field {
    margin-bottom: 8px;
    font-family: var(--app-font-mono, ui-monospace, SFMono-Regular, Menlo, Consolas, monospace);
    font-size: 13px;
    font-weight: 600;
    color: var(--td-text-color-primary);
  }

  &__cols {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
  }

  &__col {
    padding: 10px 12px;
    border-radius: 8px;
    background: var(--td-bg-color-container-hover);
  }

  &__label {
    margin-bottom: 6px;
    font-size: 12px;
    font-weight: 600;
    color: var(--td-text-color-secondary);
  }

  &__truncated {
    font-size: 13px;
    font-weight: 600;
    color: var(--td-warning-color);
  }

  &__sha {
    margin: 4px 0 8px;
    font-family: var(--app-font-mono, ui-monospace, SFMono-Regular, Menlo, Consolas, monospace);
    font-size: 11px;
    color: var(--td-text-color-placeholder);
    word-break: break-all;
  }

  &__preview {
    margin: 0;
    max-height: 140px;
    overflow: auto;
    padding: 8px;
    border-radius: 6px;
    background: var(--td-bg-color-container);
    font-family: var(--app-font-mono, ui-monospace, SFMono-Regular, Menlo, Consolas, monospace);
    font-size: 12px;
    line-height: 1.6;
    white-space: pre-wrap;
    word-break: break-all;
    color: var(--td-text-color-primary);
  }
}

.raw-json {
  margin: 0;
  max-height: 320px;
  overflow: auto;
  padding: 10px;
  border-radius: 6px;
  background: var(--td-bg-color-container-hover);
  font-family: var(--app-font-mono, ui-monospace, SFMono-Regular, Menlo, Consolas, monospace);
  font-size: 12px;
  line-height: 1.6;
  color: var(--td-text-color-primary);
}

.empty-state {
  padding: 48px 0;
  text-align: center;

  &__icon {
    font-size: 44px;
    color: var(--td-text-color-placeholder);
  }

  &__title {
    margin: 10px 0 4px;
    font-size: 15px;
    color: var(--td-text-color-primary);
  }

  &__desc {
    font-size: 13px;
    color: var(--td-text-color-secondary);
  }
}
</style>
