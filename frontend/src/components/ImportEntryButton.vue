<!--
  业务页「导入」入口（物料 / 供应商 / 单位 / 科目 / 仓库 / 库位 / 字典项）。

  # 解决什么

  修复前导入模板只能从「导入向导」页下载，而向导本身埋在菜单里：用户要批量建主数据
  得先知道有个导入功能 → 找菜单 → 选业务 → 才能下载模板。生产 `sys_import_task`
  长期 0 行就是这条路太长。本组件把三件事搬到用户眼前：

  1. **一键下载模板**（直接落盘，不跳页）；
  2. **字段说明表**（与模板「字段说明」sheet 同源，来自 `def/columns` 接口）；
  3. 需要真正上传时再给一个「打开导入向导」入口（带 `?biz_type=`，向导会预选业务）。

  # 权限

  - 下载模板：`business:impexp:import:template`
  - 字段说明：`business:impexp:def:list`
  两个按钮各自做 `v-privilege` 门禁，没有权限时按钮不渲染（而不是点了才 403）。
-->
<template>
  <span class="import-entry">
    <t-button
      v-privilege="[PERM.impexpImportTemplate, PERM.impexpDefList]"
      variant="outline"
      :data-testid="testid"
      @click="openDialog"
    >
      <template #icon><t-icon name="upload" /></template>
      导入
    </t-button>

    <t-dialog
      v-model:visible="visible"
      :header="`导入${bizLabel}`"
      :footer="false"
      :close-on-overlay-click="false"
      width="1080px"
      :data-testid="`${testid}-dialog`"
    >
      <t-alert theme="info" class="import-entry__tip">
        支持 Excel 批量导入：先下载模板，按「表头（模板第 1 行）」逐列填写（必填列带
        <b>*</b>，第 2 行是示例，导入时系统会自动跳过），再上传校验。字段规则见下方说明表。
      </t-alert>

      <div class="import-entry__actions">
        <t-button
          v-privilege="[PERM.impexpImportTemplate]"
          theme="primary"
          :loading="downloading"
          :data-testid="`${testid}-download`"
          @click="handleDownload"
        >
          <template #icon><t-icon name="download" /></template>
          下载导入模板
        </t-button>
        <t-button variant="outline" :data-testid="`${testid}-goto-wizard`" @click="goWizard">
          <template #icon><t-icon name="upload" /></template>
          打开导入向导（上传文件）
        </t-button>
        <span class="import-entry__meta" v-if="meta">
          模板 v{{ meta.template_version }} · 共 {{ meta.columns.length }} 列 · 单次最多
          {{ meta.max_rows.toLocaleString() }} 行
        </span>
      </div>

      <t-alert v-if="loadError" theme="error" :message="loadError" class="import-entry__tip" />

      <div class="import-entry__title">
        字段说明
        <span class="import-entry__hint">（与下载到的模板内「字段说明」sheet 完全一致）</span>
      </div>
      <ImportFieldTable
        :columns="columns"
        :loading="loading"
        :max-height="380"
        :testid="`${testid}-fields`"
      />
    </t-dialog>
  </span>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue';
import { useRouter } from 'vue-router';
import { MessagePlugin } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import {
  downloadImportTemplate,
  getImportColumns,
  type ImportColumnsRes,
} from '@/api/business/impexp';
import { saveBlob } from '@/utils/download';
import { PERM } from '@/constants/permissions';
import ImportFieldTable from '@/components/ImportFieldTable.vue';

const props = withDefaults(
  defineProps<{
    /** 业务编码（后端注册名，如 `business.erp.material`） */
    bizType: string;
    /** 业务中文名（按钮与弹窗标题用） */
    bizLabel: string;
    /** data-testid 前缀 */
    testid?: string;
  }>(),
  { testid: 'btn-import-entry' },
);

const router = useRouter();
const visible = ref(false);
const loading = ref(false);
const downloading = ref(false);
const loadError = ref('');
const meta = ref<ImportColumnsRes | null>(null);

const columns = computed(() => meta.value?.columns ?? []);

/** 打开弹窗并按需拉字段说明（一次会话内只拉一次，切业务由外页决定） */
async function openDialog(): Promise<void> {
  visible.value = true;
  if (meta.value || loading.value) {
    return;
  }
  await loadColumns();
}

async function loadColumns(): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    meta.value = await getImportColumns(props.bizType);
  } catch (error) {
    loadError.value = error instanceof ApiError ? error.message : '字段说明加载失败';
  } finally {
    loading.value = false;
  }
}

/** 一键下载模板（不跳页）：文件名与模板接口给的口径一致 */
async function handleDownload(): Promise<void> {
  downloading.value = true;
  try {
    const blob = await downloadImportTemplate(props.bizType);
    const name = meta.value?.biz_name || props.bizLabel;
    const version = meta.value?.template_version;
    saveBlob(blob, version ? `${name}导入模板_v${version}.xlsx` : `${name}导入模板.xlsx`);
    void MessagePlugin.success('模板已开始下载');
  } catch (error) {
    void MessagePlugin.error(error instanceof ApiError ? error.message : '模板下载失败');
  } finally {
    downloading.value = false;
  }
}

/** 去向导页并预选当前业务（向导第一步会直接显示模板下载与字段说明） */
function goWizard(): void {
  visible.value = false;
  void router.push({ path: '/business/impexp/import', query: { biz_type: props.bizType } });
}
</script>

<style lang="less" scoped>
.import-entry {
  display: inline-flex;

  &__tip {
    margin-bottom: 12px;
  }

  &__actions {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-bottom: 16px;
  }

  &__meta {
    font-size: 12px;
    color: var(--td-text-color-secondary);
  }

  &__title {
    margin-bottom: 8px;
    font-weight: 600;
  }

  &__hint {
    font-weight: 400;
    font-size: 12px;
    color: var(--td-text-color-secondary);
  }
}
</style>
