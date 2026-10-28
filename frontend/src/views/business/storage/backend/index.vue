<template>
  <div class="storage-config-page">
    <t-card :bordered="false" class="tabs-card">
      <t-tabs v-model="activeTab" @change="onTabChange">
        <t-tab-panel value="backend" label="存储后端" />
        <t-tab-panel value="policy" label="上传策略" />
      </t-tabs>
    </t-card>

    <!-- ==================== 存储后端 ==================== -->
    <template v-if="activeTab === 'backend'">
      <t-card :bordered="false" class="filter-card">
        <t-row :gutter="16">
          <t-col :span="3">
            <t-input v-model="query.keyword" placeholder="配置名称 / 编码" clearable @enter="handleSearch" />
          </t-col>
          <t-col :span="3">
            <t-select v-model="query.backend_type" placeholder="存储类型" clearable>
              <t-option value="local" label="本地存储" />
              <t-option value="minio" label="MinIO / S3 兼容" />
            </t-select>
          </t-col>
          <t-col :span="3">
            <t-select v-model="query.status" placeholder="状态" clearable>
              <t-option :value="1" label="启用" />
              <t-option :value="0" label="停用" />
            </t-select>
          </t-col>
          <t-col :span="3">
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

      <t-card :bordered="false" class="table-card">
        <div class="toolbar">
          <t-space>
            <t-button v-privilege="[PERM.storageConfigCreate]" theme="primary" @click="openCreate">
              <template #icon><t-icon name="add" /></template>
              新增存储配置
            </t-button>
            <t-button theme="default" :loading="loading" @click="loadConfigs">
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
          :stripe="true"
          @page-change="handlePageChange"
        >
          <template #config_name="{ row }">
            <span class="cell-name">{{ row.config_name }}</span>
            <t-tag v-if="row.is_default" class="cell-name__tag" theme="primary" size="small">默认</t-tag>
          </template>
          <template #backend_type="{ row }">{{ backendTypeLabel(row) }}</template>
          <template #endpoint="{ row }">
            <t-tooltip v-if="row.endpoint" :content="`${row.endpoint}${row.bucket ? ` / ${row.bucket}` : ''}`">
              <span class="cell-ellipsis">{{ row.endpoint }}<span v-if="row.bucket"> / {{ row.bucket }}</span></span>
            </t-tooltip>
            <span v-else class="cell-empty">—</span>
          </template>
          <template #ak_masked="{ row }">
            <span v-if="row.has_secret">{{ row.ak_masked || '已配置' }}</span>
            <span v-else class="cell-empty">未配置</span>
          </template>
          <template #health="{ row }">
            <span class="health" :class="`health--${row.health_status}`">
              <i class="health__dot" />
              {{ healthLabel(row) }}
            </span>
          </template>
          <template #status="{ row }">
            <t-switch
              :value="row.status === 1"
              :loading="statusLoadingId === row.id"
              :disabled="!hasPermission(PERM.storageConfigUpdate)"
              @change="() => toggleStatus(row)"
            />
          </template>
          <template #updated_at="{ row }">{{ formatDateTime(row.updated_at) }}</template>
          <template #op="{ row }">
            <t-space size="small">
              <t-link v-privilege="[PERM.storageConfigTest]" theme="primary" @click="handleTest(row)">
                {{ testingId === row.id ? '测试中…' : '测试' }}
              </t-link>
              <t-link v-privilege="[PERM.storageConfigUpdate]" theme="primary" @click="openEdit(row)">
                编辑
              </t-link>
              <t-link
                v-privilege="[PERM.storageConfigUpdate]"
                :theme="row.is_default ? 'default' : 'primary'"
                :disabled="row.is_default"
                @click="confirmSetDefault(row)"
              >
                设为默认
              </t-link>
              <t-link v-privilege="[PERM.storageConfigDelete]" theme="danger" @click="confirmRemove(row)">
                删除
              </t-link>
            </t-space>
          </template>
          <template #empty>
            <div class="empty-state">
              <t-icon class="empty-state__icon" name="object-storage" />
              <p class="empty-state__title">还没有配置存储后端</p>
              <p class="empty-state__desc">系统将使用本地磁盘（uploads 目录）作为默认存储</p>
              <t-button v-privilege="[PERM.storageConfigCreate]" theme="primary" @click="openCreate">
                新增存储配置
              </t-button>
            </div>
          </template>
        </t-table>
      </t-card>
    </template>

    <!-- ==================== 上传策略 ==================== -->
    <template v-else>
      <t-card :bordered="false" class="table-card">
        <div class="toolbar">
          <t-space>
            <t-input v-model="policyKeyword" class="toolbar__search" placeholder="分类名称" clearable @enter="loadPolicies" />
            <t-button theme="primary" @click="loadPolicies">
              <template #icon><t-icon name="search" /></template>
              查询
            </t-button>
            <t-button theme="default" :loading="policyLoading" @click="loadPolicies">
              <template #icon><t-icon name="refresh" /></template>
              刷新
            </t-button>
          </t-space>
          <span class="toolbar__count">共 {{ policyPagination.total }} 条</span>
        </div>

        <t-table
          row-key="id"
          :data="policyRows"
          :columns="policyColumns"
          :loading="policyLoading"
          :pagination="policyPagination"
          :stripe="true"
          @page-change="handlePolicyPageChange"
        >
          <template #category="{ row }">
            <span class="cell-name">{{ row.category_name }}</span>
            <span class="cell-sub">{{ row.category }}</span>
          </template>
          <template #max_size_mb="{ row }">{{ row.max_size_mb }} MB</template>
          <template #allowed_extensions="{ row }">
            <span v-if="row.allowed_extensions">{{ extensionSummary(row.allowed_extensions) }}</span>
            <span v-else class="cell-empty">不限</span>
          </template>
          <template #direct_upload="{ row }">
            <t-tag :theme="row.direct_upload ? 'success' : 'default'" size="small" variant="light">
              {{ row.direct_upload ? '允许' : '关闭' }}
            </t-tag>
          </template>
          <template #status="{ row }">
            <t-tag :theme="row.status === 1 ? 'success' : 'default'" size="small" variant="light">
              {{ row.status === 1 ? '启用' : '停用' }}
            </t-tag>
          </template>
          <template #op="{ row }">
            <t-link v-privilege="[PERM.storagePolicyUpdate]" theme="primary" @click="openPolicyEdit(row)">编辑</t-link>
          </template>
        </t-table>
      </t-card>
    </template>

    <!-- ==================== 新增 / 编辑抽屉 ==================== -->
    <t-drawer
      v-model:visible="formVisible"
      :header="isEdit ? '编辑存储配置' : '新增存储配置'"
      size="560px"
      :footer="false"
      destroy-on-close
    >
      <t-form ref="formRef" :data="form" :rules="formRules" label-width="96px" class="config-form">
        <t-form-item label="配置名称" name="config_name">
          <t-input v-model="form.config_name" placeholder="如：生产 MinIO" :maxlength="64" />
        </t-form-item>
        <t-form-item label="配置编码" name="config_code">
          <t-input v-model="form.config_code" :disabled="isEdit" placeholder="如：prod-minio" />
        </t-form-item>
        <t-form-item label="存储类型" name="backend_type">
          <t-radio-group v-model="form.backend_type" :disabled="isEdit" @change="onBackendTypeChange">
            <t-radio value="local">本地存储</t-radio>
            <t-radio value="minio">MinIO / S3 兼容</t-radio>
          </t-radio-group>
        </t-form-item>
        <t-alert
          v-if="form.backend_type === 'minio'"
          class="config-form__alert"
          theme="info"
          :message="`提示：阿里 OSS / S3 兼容存储使用本类型即可。若访问异常，请确认端点与 TLS 配置（Path-Style 判定规则见 E-STG-04）`"
        />
        <t-form-item :label="form.backend_type === 'local' ? '存储目录' : '端点'" name="endpoint">
          <t-input
            v-model="form.endpoint"
            :disabled="isEdit && form.backend_type !== 'local'"
            :placeholder="endpointPlaceholder"
          />
        </t-form-item>
        <t-form-item v-if="form.backend_type !== 'local'" label="桶名称" name="bucket">
          <t-input v-model="form.bucket" :disabled="isEdit" placeholder="3-63 位小写字母、数字与 -" />
        </t-form-item>
        <t-form-item v-if="form.backend_type !== 'local'" label="Region" name="region">
          <t-input v-model="form.region" :disabled="isEdit" placeholder="us-east-1" />
        </t-form-item>
        <t-form-item v-if="form.backend_type !== 'local'" label="Access Key" name="ak">
          <t-input v-model="form.ak" :placeholder="isEdit ? '留空则不修改' : '请输入 Access Key'" />
        </t-form-item>
        <t-form-item v-if="form.backend_type !== 'local'" label="Secret Key" name="sk">
          <t-input v-model="form.sk" type="password" :placeholder="isEdit ? '留空则不修改' : '请输入 Secret Key'" />
        </t-form-item>
        <t-form-item label="使用 TLS">
          <t-switch v-model="form.use_tls" :disabled="isEdit && form.backend_type !== 'local'" />
        </t-form-item>
        <t-form-item label="对象前缀">
          <t-input v-model="form.base_path" placeholder="如：cloudpivot/" @blur="normalizeBasePath" />
        </t-form-item>
        <t-form-item label="设为默认">
          <t-switch v-model="form.is_default" />
        </t-form-item>
        <t-form-item label="状态">
          <t-radio-group v-model="form.status">
            <t-radio :value="1">启用</t-radio>
            <t-radio :value="0">停用</t-radio>
          </t-radio-group>
        </t-form-item>
        <t-form-item label="备注">
          <t-textarea v-model="form.remark" :maxlength="255" placeholder="选填" />
        </t-form-item>

        <t-form-item v-if="isEdit && form.backend_type !== 'local'" label=" ">
          <span class="config-form__hint">
            地址与桶不可修改（修改会导致已有文件无法访问）。如需更换，请新增一个配置并切换默认存储。
          </span>
        </t-form-item>

        <!-- 连通性测试结果区：成功绿条 / 桶自动创建黄条 / 失败红条 -->
        <div v-if="testResult" class="test-result" :class="`test-result--${testState}`">
          <t-icon :name="testIcon" />
          <span>{{ testResult }}</span>
        </div>
      </t-form>

      <div class="config-form__footer">
        <t-button
          v-privilege="[PERM.storageConfigTest]"
          variant="outline"
          :loading="testingDrawer"
          :disabled="!canTest"
          @click="handleTestInDrawer"
        >
          测试连接
        </t-button>
        <span class="config-form__footer-spacer" />
        <t-button theme="default" @click="formVisible = false">取消</t-button>
        <t-button theme="primary" :loading="submitting" @click="submitForm">保存</t-button>
      </div>
    </t-drawer>

    <!-- ==================== 策略编辑抽屉 ==================== -->
    <t-drawer v-model:visible="policyVisible" header="编辑上传策略" size="520px" :footer="false" destroy-on-close>
      <t-form :data="policyForm" label-width="110px">
        <t-form-item label="分类">
          <span>{{ policyForm.category_name }}（{{ policyForm.category }}）</span>
        </t-form-item>
        <t-form-item label="单文件上限">
          <t-space>
            <t-input-number v-model="policyForm.max_size_mb" :min="1" :max="2048" theme="normal" />
            <span class="cell-sub">MB</span>
          </t-space>
          <div class="policy-quick">
            <t-link v-for="mb in [5, 20, 50, 100, 500, 2048]" :key="mb" theme="primary" @click="policyForm.max_size_mb = mb">
              {{ mb }}
            </t-link>
          </div>
        </t-form-item>
        <t-form-item label="扩展名白名单">
          <t-input v-model="policyForm.allowed_extensions" placeholder="逗号分隔，如：pdf,docx,xlsx" />
          <div class="config-form__hint">不在白名单的扩展名将被拒绝上传（E-STG-12 / 60004）。</div>
        </t-form-item>
        <t-form-item label="允许私有">
          <t-switch v-model="policyForm.allow_overwrite" />
        </t-form-item>
        <t-form-item label="允许直传">
          <t-switch v-model="policyForm.direct_upload" />
          <div class="config-form__hint">开启后大文件由浏览器直接上传到对象存储（仅对象存储后端有效）。</div>
        </t-form-item>
        <t-form-item label="预签名有效期">
          <t-space>
            <t-input-number v-model="policyForm.presign_ttl_secs" :min="60" :max="86400" theme="normal" />
            <span class="cell-sub">秒</span>
          </t-space>
        </t-form-item>
        <t-form-item label="状态">
          <t-radio-group v-model="policyForm.status">
            <t-radio :value="1">启用</t-radio>
            <t-radio :value="0">停用</t-radio>
          </t-radio-group>
        </t-form-item>
      </t-form>
      <div class="config-form__footer">
        <span class="config-form__footer-spacer" />
        <t-button theme="default" @click="policyVisible = false">取消</t-button>
        <t-button theme="primary" :loading="policySubmitting" @click="submitPolicy">保存</t-button>
      </div>
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin, type FormInstanceFunctions, type FormRules, type PrimaryTableCol } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import {
  createStorageConfig,
  deleteStorageConfig,
  pageStorageConfigs,
  pageStoragePolicies,
  setDefaultStorageConfig,
  testStorageConfig,
  updateStorageConfig,
  updateStoragePolicy,
  type StorageConfigRes,
  type StorageConfigSaveReq,
  type StoragePolicyRes,
} from '@/api/business/storage';
import { useUserStore } from '@/stores/user';
import { formatDateTime } from '@/utils/format';
import { PERM } from '@/constants/permissions';

/** 表格分页事件载荷 */
interface TablePageInfo {
  /** 当前页码 */
  current: number;
  /** 每页条数 */
  pageSize: number;
}

/** 连通性探测结果的界面状态 */
type TestState = 'ok' | 'warn' | 'error';

const userStore = useUserStore();
const hasPermission = (code: string): boolean => userStore.hasPermission(code);

const activeTab = ref<'backend' | 'policy'>('backend');

// ==================== 存储后端列表 ====================
const query = reactive({ keyword: '', backend_type: '' as string, status: undefined as number | undefined });
const rows = ref<StorageConfigRes[]>([]);
const loading = ref(false);
const testingId = ref('');
const statusLoadingId = ref('');
const pagination = reactive({ current: 1, pageSize: 10, total: 0 });

const columns: PrimaryTableCol[] = [
  { colKey: 'config_name', title: '配置名称', width: 180 },
  { colKey: 'backend_type', title: '类型', width: 130 },
  { colKey: 'endpoint', title: '端点 / 桶', width: 260, ellipsis: true },
  { colKey: 'ak_masked', title: '密钥', width: 140 },
  { colKey: 'health', title: '连通性', width: 120 },
  { colKey: 'status', title: '状态', width: 80 },
  { colKey: 'updated_at', title: '更新时间', width: 170 },
  { colKey: 'op', title: '操作', width: 240, fixed: 'right' },
];

/** 后端类型展示名：MinIO 类型同时承载 S3 兼容 / 阿里 OSS（PRD D2 口径） */
function backendTypeLabel(row: StorageConfigRes): string {
  if (row.backend_type === 'local') {
    return '本地存储';
  }
  if (row.backend_type === 'minio') {
    const endpoint = (row.endpoint || '').toLowerCase();
    if (endpoint.includes('aliyuncs')) {
      return '阿里 OSS';
    }
    if (row.region && row.region !== 'us-east-1') {
      return 'S3 兼容';
    }
    return 'MinIO';
  }
  return row.backend_type;
}

/** 连通性文案：绿点正常 / 红点失败 / 灰点未测试 */
function healthLabel(row: StorageConfigRes): string {
  if (row.health_status === 'ok') {
    return '正常';
  }
  if (row.health_status === 'fail') {
    return row.last_error ? '失败' : '失败';
  }
  return '未测试';
}

async function loadConfigs(): Promise<void> {
  loading.value = true;
  try {
    const data = await pageStorageConfigs({
      page: pagination.current,
      page_size: pagination.pageSize,
      keyword: query.keyword || undefined,
      backend_type: query.backend_type || undefined,
      status: query.status,
    });
    rows.value = data.list;
    pagination.total = data.total;
  } catch (error) {
    showError(error);
  } finally {
    loading.value = false;
  }
}

function handleSearch(): void {
  pagination.current = 1;
  void loadConfigs();
}

function handleReset(): void {
  query.keyword = '';
  query.backend_type = '';
  query.status = undefined;
  handleSearch();
}

function handlePageChange(pageInfo: TablePageInfo): void {
  pagination.current = pageInfo.current;
  pagination.pageSize = pageInfo.pageSize;
  void loadConfigs();
}

// ==================== 新增 / 编辑 ====================
const formVisible = ref(false);
const isEdit = ref(false);
const editingId = ref('');
const submitting = ref(false);
const formRef = ref<FormInstanceFunctions>();
const testResult = ref('');
const testState = ref<TestState>('ok');
const testingDrawer = ref(false);

/** 表单模型 */
interface ConfigFormModel {
  /** 配置编码 */
  config_code: string;
  /** 配置名称 */
  config_name: string;
  /** 后端类型 */
  backend_type: string;
  /** 端点 / 目录 */
  endpoint: string;
  /** 桶 */
  bucket: string;
  /** 区域 */
  region: string;
  /** 对象前缀 */
  base_path: string;
  /** TLS */
  use_tls: boolean;
  /** AK */
  ak: string;
  /** SK */
  sk: string;
  /** 默认 */
  is_default: boolean;
  /** 状态 */
  status: number;
  /** 备注 */
  remark: string;
}

function emptyForm(): ConfigFormModel {
  return {
    config_code: '',
    config_name: '',
    backend_type: 'minio',
    endpoint: '',
    bucket: '',
    region: 'us-east-1',
    base_path: '',
    use_tls: true,
    ak: '',
    sk: '',
    is_default: false,
    status: 1,
    remark: '',
  };
}

const form = reactive<ConfigFormModel>(emptyForm());

/** 端点 placeholder 随类型联动（S1-09） */
const endpointPlaceholder = computed(() => {
  if (form.backend_type === 'local') {
    return '本地目录，默认 uploads';
  }
  return 'https://minio.corp.local:9000 或 https://oss-cn-hangzhou.aliyuncs.com';
});

/** 端点必须以 http(s):// 开头（E-STG-07） */
function validateEndpoint(value: unknown): boolean | { result: boolean; message: string } {
  if (!value || form.backend_type === 'local') {
    return true;
  }
  const text = String(value);
  if (!/^https?:\/\//i.test(text)) {
    return { result: false, message: '端点需以 http:// 或 https:// 开头' };
  }
  return true;
}

/** 必填校验（本地存储不需要桶 / AK / SK） */
function requiredWhenRemote(value: unknown): boolean | { result: boolean; message: string } {
  if (form.backend_type === 'local') {
    return true;
  }
  return value !== undefined && value !== null && String(value).trim() !== ''
    ? true
    : { result: false, message: '请填写该项' };
}

const formRules: FormRules<ConfigFormModel> = {
  config_name: [{ required: true, message: '请输入配置名称', type: 'error' }],
  config_code: [{ required: true, message: '请输入配置编码', type: 'error' }],
  endpoint: [{ validator: validateEndpoint, type: 'error' }],
  bucket: [{ validator: requiredWhenRemote, type: 'error' }],
};

/** 测试按钮前置条件：端点、桶、AK、SK 齐备（编辑态已配置过密钥则只校验端点/桶） */
const canTest = computed(() => {
  if (!form.endpoint.trim()) {
    return false;
  }
  if (form.backend_type === 'local') {
    return true;
  }
  const hasKey = (form.ak.trim() && form.sk.trim()) || (isEdit.value && !!editingId.value);
  return !!form.bucket.trim() && !!hasKey;
});

function openCreate(): void {
  isEdit.value = false;
  editingId.value = '';
  Object.assign(form, emptyForm());
  testResult.value = '';
  formVisible.value = true;
}

function openEdit(row: StorageConfigRes): void {
  isEdit.value = true;
  editingId.value = row.id;
  Object.assign(form, {
    config_code: row.config_code,
    config_name: row.config_name,
    backend_type: row.backend_type,
    endpoint: row.endpoint,
    bucket: row.bucket,
    region: row.region || 'us-east-1',
    base_path: row.base_path,
    use_tls: row.use_tls,
    ak: '',
    sk: '',
    is_default: row.is_default,
    status: row.status,
    remark: row.remark,
  });
  testResult.value = '';
  formVisible.value = true;
}

/** 切类型时把 Region 重置到该类后端常用值 */
function onBackendTypeChange(value: unknown): void {
  if (value === 'local') {
    form.endpoint = form.endpoint || 'uploads';
    form.use_tls = false;
  } else {
    form.region = form.region || 'us-east-1';
    form.use_tls = true;
  }
  testResult.value = '';
}

/** 对象前缀自动补尾斜杠 */
function normalizeBasePath(): void {
  const value = form.base_path.trim();
  if (value && !value.endsWith('/')) {
    form.base_path = `${value}/`;
  }
}

async function handleTestInDrawer(): Promise<void> {
  testingDrawer.value = true;
  testResult.value = '';
  try {
    if (!isEdit.value || !editingId.value) {
      testResult.value = '新增配置需先保存后才能做连通性测试（后端探针按已落库的凭据发起）。';
      testState.value = 'warn';
      return;
    }
    const res = await testStorageConfig(editingId.value);
    testState.value = res.healthy ? 'ok' : 'error';
    testResult.value = res.healthy
      ? `连接成功 · 桶 ${form.bucket || '（本地）'} ${res.message ? `· ${res.message}` : ''} · 延迟 ${res.latency_ms}ms`
      : res.message;
    await loadConfigs();
  } catch (error) {
    testState.value = 'error';
    testResult.value = error instanceof ApiError ? error.message : '连通性测试失败';
  } finally {
    testingDrawer.value = false;
  }
}

/** 测试结果图标 */
const testIcon = computed(() => {
  if (testState.value === 'ok') {
    return 'check-circle';
  }
  return testState.value === 'warn' ? 'info-circle' : 'error-circle';
});

async function submitForm(): Promise<void> {
  const valid = await formRef.value?.validate();
  if (valid !== true) {
    return;
  }
  submitting.value = true;
  try {
    const payload: StorageConfigSaveReq = {
      config_code: form.config_code.trim(),
      config_name: form.config_name.trim(),
      backend_type: form.backend_type,
      endpoint: form.endpoint.trim(),
      bucket: form.bucket.trim(),
      region: form.region.trim(),
      base_path: form.base_path.trim(),
      use_tls: form.use_tls,
      ak: form.ak.trim() || undefined,
      sk: form.sk.trim() || undefined,
      is_default: form.is_default,
      status: form.status,
      remark: form.remark,
    };
    if (isEdit.value) {
      await updateStorageConfig(editingId.value, payload);
      void MessagePlugin.success('已保存');
    } else {
      await createStorageConfig(payload);
      void MessagePlugin.success('存储配置已新增');
    }
    formVisible.value = false;
    await loadConfigs();
  } catch (error) {
    showError(error);
  } finally {
    submitting.value = false;
  }
}

/** 行内「测试」：只刷新该行结果，不弹窗（8.2.2） */
async function handleTest(row: StorageConfigRes): Promise<void> {
  testingId.value = row.id;
  try {
    const res = await testStorageConfig(row.id);
    if (res.healthy) {
      void MessagePlugin.success(`连接成功（延迟 ${res.latency_ms}ms）`);
    } else {
      void MessagePlugin.error(res.message || '连接失败');
    }
    await loadConfigs();
  } catch (error) {
    showError(error);
  } finally {
    testingId.value = '';
  }
}

async function toggleStatus(row: StorageConfigRes): Promise<void> {
  statusLoadingId.value = row.id;
  try {
    await updateStorageConfig(row.id, {
      config_code: row.config_code,
      config_name: row.config_name,
      backend_type: row.backend_type,
      status: row.status === 1 ? 0 : 1,
    });
    void MessagePlugin.success(row.status === 1 ? '已停用' : '已启用');
    await loadConfigs();
  } catch (error) {
    // 停用默认后端会被后端拒绝，文案原样透出（E-STG-10）
    showError(error);
  } finally {
    statusLoadingId.value = '';
  }
}

function confirmSetDefault(row: StorageConfigRes): void {
  const dialog = DialogPlugin.confirm({
    header: '设为默认存储',
    body: `将默认存储切换为「${row.config_name}」。只影响之后上传的新文件，已有文件仍从各自原存储读取。是否继续？`,
    confirmBtn: { content: '设为默认', theme: 'primary' },
    onConfirm: () => {
      dialog.destroy();
      void (async () => {
        try {
          await setDefaultStorageConfig(row.id);
          void MessagePlugin.success('已设为默认存储');
          await loadConfigs();
        } catch (error) {
          showError(error);
        }
      })();
    },
  });
}

function confirmRemove(row: StorageConfigRes): void {
  const dialog = DialogPlugin.confirm({
    header: '删除存储配置',
    body: `确定删除存储配置「${row.config_name}」？删除后不可恢复。`,
    theme: 'danger',
    confirmBtn: { content: '删除', theme: 'danger' },
    onConfirm: () => {
      dialog.destroy();
      void (async () => {
        try {
          await deleteStorageConfig(row.id);
          void MessagePlugin.success('已删除');
          await loadConfigs();
        } catch (error) {
          showError(error);
        }
      })();
    },
  });
}

// ==================== 上传策略 ====================
const policyRows = ref<StoragePolicyRes[]>([]);
const policyLoading = ref(false);
const policyKeyword = ref('');
const policyPagination = reactive({ current: 1, pageSize: 10, total: 0 });
const policyVisible = ref(false);
const policySubmitting = ref(false);
const policyForm = reactive({
  id: '',
  category: '',
  category_name: '',
  max_size_mb: 20,
  allowed_extensions: '',
  allowed_mimes: '',
  allow_overwrite: false,
  direct_upload: false,
  presign_ttl_secs: 900,
  storage_code: '',
  status: 1,
  sort_order: 0,
  remark: '',
});

const policyColumns: PrimaryTableCol[] = [
  { colKey: 'category', title: '分类', width: 180 },
  { colKey: 'max_size_mb', title: '单文件上限', width: 120 },
  { colKey: 'allowed_extensions', title: '类型白名单', width: 280, ellipsis: true },
  { colKey: 'direct_upload', title: '允许直传', width: 110 },
  { colKey: 'status', title: '状态', width: 100 },
  { colKey: 'op', title: '操作', width: 100, fixed: 'right' },
];

/** 白名单摘要：前 5 个 + 「等 N 种」 */
function extensionSummary(raw: string): string {
  const list = raw.split(',').map((item) => item.trim()).filter(Boolean);
  if (list.length <= 5) {
    return list.join(' / ');
  }
  return `${list.slice(0, 5).join(' / ')} 等 ${list.length} 种`;
}

async function loadPolicies(): Promise<void> {
  policyLoading.value = true;
  try {
    const data = await pageStoragePolicies({
      page: policyPagination.current,
      page_size: policyPagination.pageSize,
      keyword: policyKeyword.value || undefined,
    });
    policyRows.value = data.list;
    policyPagination.total = data.total;
  } catch (error) {
    showError(error);
  } finally {
    policyLoading.value = false;
  }
}

function handlePolicyPageChange(pageInfo: TablePageInfo): void {
  policyPagination.current = pageInfo.current;
  policyPagination.pageSize = pageInfo.pageSize;
  void loadPolicies();
}

function openPolicyEdit(row: StoragePolicyRes): void {
  Object.assign(policyForm, {
    id: row.id,
    category: row.category,
    category_name: row.category_name,
    max_size_mb: row.max_size_mb,
    allowed_extensions: row.allowed_extensions,
    allowed_mimes: row.allowed_mimes,
    allow_overwrite: row.allow_overwrite,
    direct_upload: row.direct_upload,
    presign_ttl_secs: row.presign_ttl_secs,
    storage_code: row.storage_code,
    status: row.status,
    sort_order: row.sort_order,
    remark: row.remark,
  });
  policyVisible.value = true;
}

async function submitPolicy(): Promise<void> {
  policySubmitting.value = true;
  try {
    await updateStoragePolicy(policyForm.id, {
      category_name: policyForm.category_name,
      max_size_mb: policyForm.max_size_mb,
      allowed_extensions: policyForm.allowed_extensions,
      allowed_mimes: policyForm.allowed_mimes,
      allow_overwrite: policyForm.allow_overwrite,
      direct_upload: policyForm.direct_upload,
      presign_ttl_secs: policyForm.presign_ttl_secs,
      storage_code: policyForm.storage_code || undefined,
      status: policyForm.status,
      sort_order: policyForm.sort_order,
      remark: policyForm.remark,
    });
    void MessagePlugin.success('已保存');
    policyVisible.value = false;
    await loadPolicies();
  } catch (error) {
    showError(error);
  } finally {
    policySubmitting.value = false;
  }
}

function onTabChange(value: unknown): void {
  if (value === 'policy' && policyRows.value.length === 0) {
    void loadPolicies();
  }
}

/** 统一错误提示：后端 msg 已经是给人看的话，直接展示（8.0 约定第 7 条） */
function showError(error: unknown): void {
  void MessagePlugin.error(error instanceof ApiError ? error.message : '请求失败，请稍后重试');
}

onMounted(() => {
  void loadConfigs();
});
</script>

<style lang="less" scoped>
.storage-config-page {
  padding: 16px;

  .tabs-card {
    margin-bottom: 12px;

    :deep(.t-card__body) {
      padding: 0 16px;
    }
  }

  .filter-card {
    margin-bottom: 12px;
  }

  .table-card {
    .toolbar {
      display: flex;
      align-items: center;
      justify-content: space-between;
      margin-bottom: 12px;

      &__count {
        font-size: 13px;
        color: var(--td-text-color-secondary);
      }

      &__search {
        width: 200px;
      }
    }
  }

  .cell-name {
    font-weight: 600;

    &__tag {
      margin-left: 8px;
    }
  }

  .cell-sub {
    margin-left: 8px;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  .cell-ellipsis {
    display: inline-block;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .cell-empty {
    color: var(--td-text-color-placeholder);
  }

  .health {
    display: inline-flex;
    align-items: center;
    font-size: 13px;

    &__dot {
      width: 8px;
      height: 8px;
      margin-right: 6px;
      border-radius: 50%;
      background: var(--td-text-color-placeholder);
    }

    &--ok &__dot,
    &--ok {
      color: var(--td-success-color);
    }

    &--ok &__dot {
      background: var(--td-success-color);
    }

    &--fail,
    &--fail &__dot {
      color: var(--td-error-color);
    }

    &--fail &__dot {
      background: var(--td-error-color);
    }
  }

  .empty-state {
    padding: 48px 0;
    text-align: center;

    &__icon {
      font-size: 48px;
      color: var(--td-text-color-placeholder);
    }

    &__title {
      margin: 12px 0 4px;
      font-size: 15px;
      color: var(--td-text-color-primary);
    }

    &__desc {
      margin-bottom: 16px;
      font-size: 13px;
      color: var(--td-text-color-secondary);
    }
  }

  .config-form {
    &__alert {
      margin: 0 0 16px 96px;
    }

    &__hint {
      margin-top: 4px;
      font-size: 12px;
      line-height: 1.6;
      color: var(--td-text-color-secondary);
    }

    &__footer {
      display: flex;
      align-items: center;
      margin-top: 8px;
      padding-top: 16px;
      border-top: 1px solid var(--td-component-stroke);
    }

    &__footer-spacer {
      flex: 1;
    }
  }

  .test-result {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-left: 96px;
    padding: 10px 12px;
    border-radius: 4px;
    font-size: 13px;
    line-height: 1.6;

    &--ok {
      color: var(--td-success-color);
      background: var(--td-success-color-light);
    }

    &--warn {
      color: var(--td-warning-color);
      background: var(--td-warning-color-light);
    }

    &--error {
      color: var(--td-error-color);
      background: var(--td-error-color-light);
    }
  }

  .policy-quick {
    display: flex;
    gap: 12px;
    margin-top: 4px;
    font-size: 12px;
  }
}
</style>
