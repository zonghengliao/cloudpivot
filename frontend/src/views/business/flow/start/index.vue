<template>
  <div class="flow-start-page">
    <t-card :bordered="false" class="card">
      <div class="card__head">
        <span class="card__title">发起审批</span>
        <span class="card__hint">只列出已发布（PUBLISHED）的流程</span>
      </div>

      <t-alert v-if="errorMessage" class="card__alert" theme="error" :message="errorMessage">
        <template #operation>
          <t-button size="small" variant="outline" @click="loadFlows">重试</t-button>
        </template>
      </t-alert>

      <t-row :gutter="[20, 0]">
        <!-- 左：表单 -->
        <t-col :span="14">
          <t-form label-align="top" class="start-form">
            <t-form-item label="选择流程" required>
              <t-select
                v-model="form.defId"
                :options="flowOptions"
                :loading="flowLoading"
                filterable
                placeholder="选择要发起的流程"
                @change="onFlowChange"
              />
              <p v-if="flowOptions.length === 0 && !flowLoading" class="form-tip">
                没有可发起的流程。请先在「流程定义」里发布一个流程（状态 PUBLISHED 才会出现在这里）。
              </p>
            </t-form-item>

            <t-form-item label="标题" required>
              <t-input v-model="form.title" placeholder="一句话说明这次要审什么" :maxlength="120" show-limit-number />
            </t-form-item>

            <t-form-item label="内容">
              <t-textarea
                v-model="form.content"
                placeholder="事由 / 金额 / 依据…（P0 只做意见 + 附件）"
                :autosize="{ minRows: 5, maxRows: 12 }"
                :maxlength="2000"
              />
            </t-form-item>

            <t-form-item label="附件">
              <div class="uploader">
                <FileUploader category="flow" @uploaded="onUploaded" @error="onUploadError" />
                <ul v-if="attachments.length > 0" class="uploader__list">
                  <li v-for="(file, index) in attachments" :key="file.id">
                    <t-link theme="primary" :href="file.url" target="_blank">{{ file.original_name }}</t-link>
                    <span class="uploader__size">{{ formatSize(file.file_size) }}</span>
                    <t-link theme="danger" @click="attachments.splice(index, 1)">移除</t-link>
                  </li>
                </ul>
              </div>
            </t-form-item>
          </t-form>

          <t-alert v-if="submitError" class="card__alert" theme="warning" :message="submitError" />

          <div class="start-form__actions">
            <t-button theme="primary" :loading="submitting" :disabled="!canSubmit" @click="onSubmit">提交审批</t-button>
            <t-button variant="outline" @click="resetForm">重置</t-button>
          </div>
        </t-col>

        <!-- 右：流程预览 -->
        <t-col :span="10">
          <div class="preview">
            <div class="preview__head">
              <span class="preview__title">流程预览</span>
              <span v-if="previewVersion" class="preview__version">v{{ previewVersion.version_no }}</span>
            </div>

            <div v-if="previewLoading" class="preview__state">加载流程…</div>
            <FlowPreview
              v-else-if="previewVersion"
              :nodes="previewVersion.nodes"
              :edges="previewVersion.edges"
              height="300px"
            />
            <div v-else class="preview__state">
              <t-icon name="flowchart" size="24px" />
              <p>选择流程后，这里显示它会经过哪些节点</p>
            </div>

            <div v-if="previewNodes.length > 0" class="preview__steps">
              <template v-for="(node, index) in previewNodes" :key="node.key">
                <span class="preview__step">
                  <span class="preview__step-index">{{ index + 1 }}</span>
                  {{ node.name }}
                </span>
                <t-icon v-if="index < previewNodes.length - 1" name="chevron-right" class="preview__arrow" />
              </template>
            </div>
          </div>
        </t-col>
      </t-row>
    </t-card>
  </div>
</template>

<script setup lang="ts">
/**
 * 发起审批（卡面页面 3 / `/flow/start`）。
 *
 * # 数据来源
 *
 * - 可发起流程：`GET /business/flow/def/page?status=PUBLISHED`（**只列已发布**，草稿不能发起）；
 * - 流程预览：该流程「当前发布版本」的整图（`GET /version/{vid}`），复用只读画布渲染；
 * - 提交：`POST /business/flow/instance` —— **实例侧接口尚未交付（D2）**，
 *   这里如实把后端错误提示出来（`flow-api.ts` 里该函数带 `TODO(D2)`），
 *   **不伪造实例 id、不本地造一条假审批单**（红线）。
 */
import { computed, onMounted, ref } from 'vue';
import { useRouter } from 'vue-router';
import { MessagePlugin } from 'tdesign-vue-next';
import {
  createFlowInstance,
  getFlowVersion,
  pageFlowDefs,
  type FlowDef,
  type FlowVersion,
} from '@/api/business/flow-api';
import { ApiError } from '@/api/request';
import type { FileRes } from '@/api/support/file-api';
import FileUploader from '@/components/FileUploader.vue';
import FlowPreview from '@/views/business/flow/components/FlowPreview.vue';

const router = useRouter();

/** 已发布流程列表 */
const flows = ref<FlowDef[]>([]);
/** 加载中 */
const flowLoading = ref(false);
/** 加载错误 */
const errorMessage = ref('');

/** 表单 */
const form = ref({ defId: '', title: '', content: '' });

/** 附件 */
const attachments = ref<FileRes[]>([]);

/** 预览版本（当前发布版本的整图） */
const previewVersion = ref<FlowVersion | null>(null);
/** 预览加载中 */
const previewLoading = ref(false);

/** 提交中 */
const submitting = ref(false);
/** 提交失败提示 */
const submitError = ref('');

/** 流程下拉选项 */
const flowOptions = computed(() =>
  flows.value.map((flow) => ({ label: `${flow.flow_name}（${flow.flow_code}）`, value: flow.id })),
);

/** 预览用的节点顺序（按 x 排序，读起来是「从左到右」） */
const previewNodes = computed(() =>
  [...(previewVersion.value?.nodes ?? [])].sort((a, b) => (a.x === b.x ? a.y - b.y : a.x - b.x)),
);

/** 是否可以提交 */
const canSubmit = computed(() => !!form.value.defId && !!form.value.title.trim() && !submitting.value);

/** 载入已发布流程 */
async function loadFlows(): Promise<void> {
  flowLoading.value = true;
  errorMessage.value = '';
  try {
    const data = await pageFlowDefs({ page: 1, page_size: 100, status: 'PUBLISHED' });
    flows.value = data.list ?? [];
    if (flows.value.length === 1) {
      form.value.defId = flows.value[0].id;
      void loadPreview();
    }
  } catch (error) {
    flows.value = [];
    errorMessage.value = error instanceof ApiError ? error.message : '流程列表加载失败';
  } finally {
    flowLoading.value = false;
  }
}

/** 切换流程：加载该流程当前发布版本的整图做预览 */
function onFlowChange(): void {
  previewVersion.value = null;
  void loadPreview();
}

/** 加载预览图 */
async function loadPreview(): Promise<void> {
  const def = flows.value.find((item) => item.id === form.value.defId);
  if (!def || !def.current_version_id) {
    previewVersion.value = null;
    return;
  }
  previewLoading.value = true;
  try {
    previewVersion.value = await getFlowVersion(def.current_version_id);
  } catch (error) {
    previewVersion.value = null;
    MessagePlugin.error(error instanceof ApiError ? error.message : '流程预览加载失败');
  } finally {
    previewLoading.value = false;
  }
}

/** 上传成功 */
function onUploaded(file: FileRes): void {
  attachments.value.push(file);
}

/** 上传失败 */
function onUploadError(message: string): void {
  MessagePlugin.error(message);
}

/** 文件大小格式化 */
function formatSize(size: number): string {
  if (!size) {
    return '0 B';
  }
  if (size < 1024) {
    return `${size} B`;
  }
  if (size < 1024 * 1024) {
    return `${(size / 1024).toFixed(1)} KB`;
  }
  return `${(size / 1024 / 1024).toFixed(1)} MB`;
}

/** 提交：调实例侧接口（D2 已交付） */
async function onSubmit(): Promise<void> {
  submitError.value = '';
  if (!form.value.defId) {
    MessagePlugin.warning('请先选择流程');
    return;
  }
  if (!form.value.title.trim()) {
    MessagePlugin.warning('请填写标题');
    return;
  }
  submitting.value = true;
  try {
    // 出参是 `ActionRes`（没有 instance_no，只有 instance_id），单据号到详情页再取
    const result = await createFlowInstance({
      def_id: form.value.defId,
      title: form.value.title.trim(),
      form_data: {
        content: form.value.content,
        attachments: attachments.value.map((file) => file.id),
      },
    });
    MessagePlugin.success('已提交，正在跳转审批详情');
    void router.push(`/flow/instance/${result.instance_id}`);
  } catch (error) {
    const message = error instanceof ApiError ? error.message : '提交失败';
    submitError.value = `提交未成功：${message}`;
    MessagePlugin.error(message);
  } finally {
    submitting.value = false;
  }
}

/** 重置表单 */
function resetForm(): void {
  form.value = { defId: '', title: '', content: '' };
  attachments.value = [];
  previewVersion.value = null;
  submitError.value = '';
}

onMounted(() => {
  void loadFlows();
});
</script>

<style scoped lang="less">
.flow-start-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.card {
  border-radius: 8px;

  &__head {
    display: flex;
    gap: 10px;
    align-items: baseline;
    margin-bottom: 16px;
  }

  &__title {
    font-size: 15px;
    font-weight: 600;
    color: var(--td-text-color-primary);
  }

  &__hint {
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__alert {
    margin-bottom: 12px;
  }
}

.start-form {
  &__actions {
    display: flex;
    gap: 10px;
    padding-top: 4px;
  }
}

.form-tip {
  margin-top: 6px;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.uploader {
  width: 100%;

  &__list {
    padding: 0;
    margin: 8px 0 0;
    list-style: none;

    li {
      display: flex;
      gap: 10px;
      align-items: center;
      padding: 4px 0;
      font-size: 13px;
    }
  }

  &__size {
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }
}

.preview {
  padding: 12px;
  background: var(--td-bg-color-secondarycontainer);
  border: 1px solid var(--td-component-stroke);
  border-radius: 8px;

  &__head {
    display: flex;
    gap: 8px;
    align-items: baseline;
    margin-bottom: 10px;
  }

  &__title {
    font-size: 14px;
    font-weight: 600;
    color: var(--td-text-color-primary);
  }

  &__version {
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__state {
    display: flex;
    flex-direction: column;
    gap: 8px;
    align-items: center;
    justify-content: center;
    height: 300px;
    font-size: 13px;
    color: var(--td-text-color-placeholder);
    text-align: center;
  }

  &__steps {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
    margin-top: 10px;
  }

  &__step {
    display: inline-flex;
    gap: 4px;
    align-items: center;
    padding: 2px 8px;
    font-size: 12px;
    color: var(--td-text-color-secondary);
    background: var(--td-bg-color-container);
    border: 1px solid var(--td-component-stroke);
    border-radius: 12px;
  }

  &__step-index {
    font-size: 10px;
    color: var(--td-text-color-placeholder);
  }

  &__arrow {
    color: var(--td-text-color-placeholder);
  }
}
</style>
