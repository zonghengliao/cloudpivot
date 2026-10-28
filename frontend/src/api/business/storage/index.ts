/**
 * 对象存储接口（批次 A · A1/A2）。
 *
 * 路径与后端 `backend/src/business/storage/handler.rs` 的 `#[alun::get/post/put/delete]`
 * 逐条对应；权限码与 `#[alun::permission]` 注解、`sys_menu.auth_code` 三者一致。
 *
 * # 约定
 *
 * - 所有路径必须写全 `/api` 前缀（漏写会命中 SPA fallback 返回 HTML，报「响应体格式不符合」）；
 * - 密钥永远只出不进：响应里的 `ak_masked` / `has_secret` 是掩码结果，明文 AK/SK 只在提交时出现；
 * - 直传不走 axios（`axios` 的 `onUploadProgress` 在部分浏览器统计的是请求体发送量而非服务端确认量），
 *   统一用 `XMLHttpRequest` 拿到**真实上传进度**。
 */
import service, { ApiError, del, get, post, put, type ApiResult, type PageData } from '@/api/request';

/** 存储后端类型：`local` 本地磁盘，`minio` 兼容 S3 / OSS / COS */
export type StorageBackendType = 'local' | 'minio';

/** 存储配置记录（对应后端 `StorageConfigRes`） */
export interface StorageConfigRes {
  /** 配置 id */
  id: string;
  /** 配置编码（唯一） */
  config_code: string;
  /** 配置名称 */
  config_name: string;
  /** 后端类型 */
  backend_type: string;
  /** 端点（本地为目录） */
  endpoint: string;
  /** 桶名称 */
  bucket: string;
  /** 区域 */
  region: string;
  /** 对象前缀 */
  base_path: string;
  /** 是否 TLS */
  use_tls: boolean;
  /** 是否 Path-Style */
  path_style: boolean;
  /** 掩码后的 Access Key（如 `AKIA****7ZP2`） */
  ak_masked: string;
  /** 是否已配置 Secret（永不回显明文） */
  has_secret: boolean;
  /** 密钥版本号 */
  key_version: number;
  /** 是否默认后端 */
  is_default: boolean;
  /** 状态：1 启用 / 0 停用 */
  status: number;
  /** 连通性：`ok` / `fail` / `unknown` */
  health_status: string;
  /** 最近一次探测时间 */
  last_check_at?: string | null;
  /** 最近一次探测失败原因 */
  last_error: string;
  /** 排序值 */
  sort_order: number;
  /** 备注 */
  remark: string;
  /** 创建时间 */
  created_at: string;
  /** 更新时间 */
  updated_at: string;
}

/** 存储配置分页查询参数（用 `type` 而非 `interface`：需隐式索引签名才能直接传给 `get()`） */
export type StorageConfigPageQuery = {
  /** 页码，从 1 开始 */
  page: number;
  /** 每页条数 */
  page_size: number;
  /** 名称 / 编码模糊 */
  keyword?: string;
  /** 后端类型 */
  backend_type?: string;
  /** 状态 */
  status?: number;
}

/** 存储配置保存参数（新增 / 修改共用；`ak`/`sk` 留空表示不改密钥） */
export interface StorageConfigSaveReq {
  /** 配置编码 */
  config_code: string;
  /** 配置名称 */
  config_name: string;
  /** 后端类型 */
  backend_type: string;
  /** 端点 */
  endpoint?: string;
  /** 桶 */
  bucket?: string;
  /** 区域 */
  region?: string;
  /** 对象前缀 */
  base_path?: string;
  /** 是否 TLS */
  use_tls?: boolean;
  /** 是否 Path-Style */
  path_style?: boolean;
  /** Access Key（明文，落库前加密） */
  ak?: string;
  /** Secret Key（明文，落库前加密） */
  sk?: string;
  /** 是否设为默认 */
  is_default?: boolean;
  /** 状态 */
  status?: number;
  /** 排序值 */
  sort_order?: number;
  /** 备注 */
  remark?: string;
}

/** 连通性探测结果（对应后端 `StorageTestRes`） */
export interface StorageTestRes {
  /** 配置编码 */
  config_code: string;
  /** 后端类型 */
  backend_type: string;
  /** 是否连通 */
  healthy: boolean;
  /** 延迟（毫秒） */
  latency_ms: number;
  /** 面向用户的说明（成功为 E-STG-01，失败为六类原因之一） */
  message: string;
  /** 探测时间 */
  checked_at: string;
}

/** 上传策略（对应后端 `StoragePolicyRes`） */
export interface StoragePolicyRes {
  /** 策略 id */
  id: string;
  /** 分类短码 */
  category: string;
  /** 分类中文名 */
  category_name: string;
  /** 单文件上限（MB） */
  max_size_mb: number;
  /** 扩展名白名单（逗号分隔） */
  allowed_extensions: string;
  /** MIME 白名单（逗号分隔） */
  allowed_mimes: string;
  /** 是否允许覆盖 */
  allow_overwrite: boolean;
  /** 是否允许直传 */
  direct_upload: boolean;
  /** 预签名有效期（秒） */
  presign_ttl_secs: number;
  /** 指定后端编码（空表示默认后端） */
  storage_code: string;
  /** 状态 */
  status: number;
  /** 排序值 */
  sort_order: number;
  /** 备注 */
  remark: string;
  /** 创建时间 */
  created_at: string;
  /** 更新时间 */
  updated_at: string;
}

/** 上传策略保存参数 */
export interface StoragePolicySaveReq {
  /** 分类中文名 */
  category_name: string;
  /** 单文件上限（MB） */
  max_size_mb: number;
  /** 扩展名白名单 */
  allowed_extensions?: string;
  /** MIME 白名单 */
  allowed_mimes?: string;
  /** 是否允许覆盖 */
  allow_overwrite?: boolean;
  /** 是否允许直传 */
  direct_upload?: boolean;
  /** 预签名有效期（秒） */
  presign_ttl_secs: number;
  /** 指定后端编码 */
  storage_code?: string;
  /** 状态 */
  status?: number;
  /** 排序值 */
  sort_order?: number;
  /** 备注 */
  remark?: string;
}

/** 文件记录（对应后端 `StorageFileRes`） */
export interface StorageFileRes {
  /** 文件 id */
  id: string;
  /** 存储文件名 */
  file_name: string;
  /** 原始文件名 */
  original_name: string;
  /** 存储 key */
  file_key: string;
  /** 大小（字节） */
  file_size: number;
  /** MIME */
  mime_type: string;
  /** 扩展名 */
  extension: string;
  /** 分类 */
  category: string;
  /** 状态 */
  status: number;
  /** 上传人 */
  uploaded_by: string;
  /** 上传时间 */
  created_at: string;
  /** 访问地址 */
  url: string;
  /** 所在后端编码 */
  storage_code: string;
  /** 上传方式：`server` 服务端中转 / `direct` 浏览器直传 */
  upload_mode: string;
}

/** 文件分页查询参数（用 `type` 而非 `interface`：需隐式索引签名才能直接传给 `get()`） */
export type StorageFilePageQuery = {
  /** 页码 */
  page: number;
  /** 每页条数 */
  page_size: number;
  /** 原始文件名模糊 */
  original_name?: string;
  /** 分类 */
  category?: string;
  /** 后端编码 */
  storage_code?: string;
  /** 文件哈希（秒传校验） */
  file_hash?: string;
}

/** 预签名直传初始化参数（对应后端 `StoragePresignReq`） */
export interface StoragePresignReq {
  /** 分类（决定策略） */
  category: string;
  /** 原始文件名 */
  file_name: string;
  /** 文件大小（字节，用于策略与秒传判断） */
  file_size?: number;
  /** 文件哈希（sha256，用于秒传） */
  file_hash?: string;
  /** MIME */
  mime_type?: string;
}

/** 预签名直传响应（对应后端 `StoragePresignRes`） */
export interface StoragePresignRes {
  /** 对象 key */
  file_key: string;
  /** 预签名上传地址 */
  upload_url: string;
  /** HTTP 方法（PUT） */
  method: string;
  /** 必须原样带回的请求头 */
  headers: StorageHeader[];
  /** 链接过期时间 */
  expire_at: string;
  /** 目标后端编码 */
  storage_code: string;
  /** 分类 */
  category: string;
  /** 是否秒传命中（true 时无需真正上传，直接登记） */
  duplicated: boolean;
  /** 秒传命中时直接返回的既有文件记录 */
  file?: StorageFileRes | null;
}

/** 预签名请求头 */
export interface StorageHeader {
  /** 头名称 */
  name: string;
  /** 头值 */
  value: string;
}

/** 直传完成登记参数（对应后端 `StorageCompleteReq`） */
export interface StorageCompleteReq {
  /** 分类 */
  category: string;
  /** 对象 key（来自 init 响应） */
  file_key: string;
  /** 原始文件名 */
  file_name: string;
  /** 文件大小 */
  file_size: number;
  /** 文件哈希 */
  file_hash?: string;
  /** MIME */
  mime_type?: string;
}

/** 分页查询存储配置 */
export function pageStorageConfigs(query: StorageConfigPageQuery): Promise<PageData<StorageConfigRes>> {
  return get<PageData<StorageConfigRes>>('/api/business/storage/config/page', query);
}

/** 查询单个存储配置 */
export function getStorageConfig(id: string): Promise<StorageConfigRes> {
  return get<StorageConfigRes>(`/api/business/storage/config/${id}`);
}

/** 新增存储配置 */
export function createStorageConfig(data: StorageConfigSaveReq): Promise<StorageConfigRes> {
  return post<StorageConfigRes>('/api/business/storage/config', data);
}

/** 修改存储配置（`ak`/`sk` 留空表示不改密钥） */
export function updateStorageConfig(id: string, data: StorageConfigSaveReq): Promise<StorageConfigRes> {
  return put<StorageConfigRes>(`/api/business/storage/config/${id}`, data);
}

/** 删除存储配置 */
export function deleteStorageConfig(id: string): Promise<null> {
  return del<null>(`/api/business/storage/config/${id}`);
}

/** 设为默认后端 */
export function setDefaultStorageConfig(id: string): Promise<null> {
  return post<null>(`/api/business/storage/config/${id}/default`);
}

/** 连通性探测（后端写 1KB 探针对象再删除，最多 10 秒） */
export function testStorageConfig(id: string): Promise<StorageTestRes> {
  return post<StorageTestRes>(`/api/business/storage/config/${id}/test`);
}

/** 分页查询上传策略 */
export function pageStoragePolicies(query: {
  page: number;
  page_size: number;
  keyword?: string;
  status?: number;
}): Promise<PageData<StoragePolicyRes>> {
  return get<PageData<StoragePolicyRes>>('/api/business/storage/policy/page', query);
}

/** 修改上传策略 */
export function updateStoragePolicy(id: string, data: StoragePolicySaveReq): Promise<StoragePolicyRes> {
  return put<StoragePolicyRes>(`/api/business/storage/policy/${id}`, data);
}

/** 服务端中转上传（multipart，字段名固定 `file`） */
export function uploadStorageFile(file: File, category?: string): Promise<StorageFileRes> {
  const formData = new FormData();
  formData.append('file', file);
  const query = category ? `?category=${encodeURIComponent(category)}` : '';
  return service
    .post<ApiResult<StorageFileRes>>(`/api/business/storage/upload${query}`, formData)
    .then((response) => {
      const body = response.data;
      if (!body || typeof body.code !== 'number' || body.code !== 0) {
        throw new ApiError(typeof body?.code === 'number' ? body.code : -1, body?.msg || '上传失败');
      }
      return body.data;
    });
}

/** 预签名直传初始化（含秒传短路） */
export function presignStorageUpload(data: StoragePresignReq): Promise<StoragePresignRes> {
  return post<StoragePresignRes>('/api/business/storage/presign', data);
}

/** 直传完成登记（后端校验大小 / 类型 / 哈希，对象不存在返回 60007） */
export function completeStorageUpload(data: StorageCompleteReq): Promise<StorageFileRes> {
  return post<StorageFileRes>('/api/business/storage/complete', data);
}

/** 分页查询文件 */
export function pageStorageFiles(query: StorageFilePageQuery): Promise<PageData<StorageFileRes>> {
  return get<PageData<StorageFileRes>>('/api/business/storage/file/page', query);
}

/** 删除文件（软删，物理对象由 GC 任务延迟回收） */
export function deleteStorageFile(id: string): Promise<null> {
  return del<null>(`/api/business/storage/${id}`);
}

/** 重新登记文件（同 key 新记录） */
export function rebindStorageFile(id: string, data: { original_name?: string; category?: string }): Promise<StorageFileRes> {
  return post<StorageFileRes>(`/api/business/storage/${id}/rebind`, data);
}

/** 下载文件（携带鉴权头取二进制，由调用方触发保存） */
export async function downloadStorageFile(id: string): Promise<Blob> {
  const response = await service.get<Blob>(`/api/business/storage/download/${id}`, { responseType: 'blob' });
  return response.data;
}

/**
 * 浏览器直传：把文件 PUT 到预签名地址，并回报**真实**上传进度。
 *
 * # 参数
 *
 * - `url`：后端预签名地址；
 * - `headers`：后端要求原样带回的请求头；
 * - `file`：浏览器 `File` 对象；
 * - `onProgress`：进度回调，入参为 0~100 的整数百分比。
 *
 * # 返回值
 *
 * PUT 成功（2xx）时 resolve；失败时 reject 带可读文案的 `Error`。
 *
 * # 为什么用 XHR
 *
 * 直传是「浏览器 → 对象存储」的裸 PUT，必须逐字节发送文件内容；`fetch` 无上传进度，
 * `axios` 在此场景也拿不到服务端确认量，只有 `XMLHttpRequest.upload.onprogress` 能给出
 * `loaded/total` 的真实增长。
 */
export function putFileWithProgress(
  url: string,
  headers: StorageHeader[],
  file: File,
  onProgress?: (percent: number) => void,
): Promise<void> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open('PUT', url, true);
    for (const header of headers) {
      xhr.setRequestHeader(header.name, header.value);
    }
    xhr.upload.onprogress = (event: ProgressEvent) => {
      if (!event.lengthComputable || !onProgress) {
        return;
      }
      onProgress(Math.min(100, Math.round((event.loaded / event.total) * 100)));
    };
    xhr.onload = () => {
      if (xhr.status >= 200 && xhr.status < 300) {
        onProgress?.(100);
        resolve();
        return;
      }
      reject(new Error(`直传失败（HTTP ${xhr.status}）`));
    };
    xhr.onerror = () => reject(new Error('直传失败：网络中断或对象存储拒绝跨域请求'));
    xhr.ontimeout = () => reject(new Error('直传超时，请检查网络后重试'));
    xhr.send(file);
  });
}

/** 存储模块接口集合 */
export const storageApi = {
  pageStorageConfigs,
  getStorageConfig,
  createStorageConfig,
  updateStorageConfig,
  deleteStorageConfig,
  setDefaultStorageConfig,
  testStorageConfig,
  pageStoragePolicies,
  updateStoragePolicy,
  uploadStorageFile,
  presignStorageUpload,
  completeStorageUpload,
  pageStorageFiles,
  deleteStorageFile,
  rebindStorageFile,
  downloadStorageFile,
  putFileWithProgress,
};
