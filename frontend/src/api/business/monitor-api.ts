import { get, default as service } from '@/api/request';

/** CPU 使用情况（对应后端 `business::monitor::dto::CpuRes`） */
export interface CpuRes {
  /** 采样窗口内的使用率，0-100，保留 1 位小数 */
  percent: number;
  /** 逻辑核心数 */
  cores: number;
}

/** 内存 / 磁盘容量信息（对应后端 `UsageRes`） */
export interface UsageRes {
  /** 使用率百分比，0-100 */
  percent: number;
  /** 已用字节数 */
  used_bytes: number;
  /** 总字节数 */
  total_bytes: number;
  /** 已用容量文本（GB 口径） */
  used_text: string;
  /** 总容量文本（GB 口径） */
  total_text: string;
}

/** 数据库状态（对应后端 `DatabaseRes`） */
export interface DatabaseRes {
  /** `up` / `down` */
  state: string;
  /** 当前库名 */
  name: string;
  /** 库大小（字节） */
  size_bytes: number;
  /** 库大小文本（MB / GB） */
  size_text: string;
  /** 数据库连接数 */
  connections: number;
  /** 数据库版本（截断至 40 字符） */
  version: string;
  /** 服务端版本号（如 `18.6`） */
  server_version: string;
  /** 本次查询往返耗时（毫秒）；库不可用时为 null */
  latency_ms: number | null;
  /** 已应用迁移条数（`schema_migrations`） */
  migrations: number;
}

/** Redis 状态（对应后端 `RedisRes`） */
export interface RedisRes {
  /** `up` / `down` */
  state: string;
  /** Redis 版本 */
  version: string;
  /** 已用内存文本（Redis 自报单位） */
  used_memory_text: string;
  /** 已用内存字节数 */
  used_memory_bytes: number;
  /** 客户端连接数 */
  connected_clients: number;
  /** 运行时长（秒） */
  uptime_seconds: number;
  /** 命中次数 */
  keyspace_hits: number;
  /** 未命中次数 */
  keyspace_misses: number;
  /** 本次 `INFO` 往返耗时（毫秒）；不可用时为 null */
  latency_ms: number | null;
}

/** 主机信息（对应后端 `HostRes`） */
export interface HostRes {
  /** 主机名 */
  hostname: string;
  /** 发行版名称 */
  os_name: string;
  /** 内核版本 */
  kernel: string;
  /** CPU 架构 */
  arch: string;
  /** CPU 型号 */
  cpu_model: string;
  /** CPU 主频 MHz（0 = 未知，ARM 常见） */
  cpu_mhz: number;
  /** 1 分钟平均负载 */
  load1: number;
  /** 5 分钟平均负载 */
  load5: number;
  /** 15 分钟平均负载 */
  load15: number;
  /** 主机运行时长（秒） */
  uptime_seconds: number;
  /** 开机时间（RFC 3339，可能为空串） */
  boot_time: string;
}

/** 后端进程信息（对应后端 `AppRes`） */
export interface AppRes {
  /** 应用版本 */
  version: string;
  /** 构建时 commit 短哈希 */
  commit: string;
  /** 构建时 rustc 版本 */
  rust_version: string;
  /** 当前 profile */
  profile: string;
  /** 进程启动时间（RFC 3339） */
  started_at: string;
  /** 进程运行时长（秒） */
  uptime_seconds: number;
  /** 进程号 */
  pid: number;
}

/** 业务量快照（对应后端 `BusinessRes`） */
export interface BusinessRes {
  /** 用户总数 */
  users_total: number;
  /** 在线会话数 */
  online_users: number;
  /** 今日登录次数 */
  login_today: number;
  /** 今日操作次数 */
  op_log_today: number;
}

/** Hermes Agent 探测结果（对应后端 `HermesProbeRes`） */
export interface HermesProbeRes {
  /** `up` / `not_installed` */
  state: string;
  /** 探测地址 */
  address: string;
  /** 连接耗时（毫秒） */
  latency_ms: number | null;
  /** 一句话说明 */
  detail: string;
}

/** 服务器状态响应体（对应后端 `ServerStatusRes`） */
export interface ServerStatusRes {
  /** CPU */
  cpu: CpuRes;
  /** 内存 */
  memory: UsageRes;
  /** 磁盘 */
  disk: UsageRes;
  /** 数据库 */
  database: DatabaseRes;
  /** Redis */
  redis: RedisRes;
  /** 采集时间（RFC 3339） */
  collected_at: string;
  /** 主机信息 */
  host: HostRes;
  /** 进程信息 */
  app: AppRes;
  /** 业务量 */
  business: BusinessRes;
  /** Hermes Agent 探测结果（未安装是正常状态） */
  hermes: HermesProbeRes;
}

/**
 * 查询服务器运行状态（CPU / 内存 / 磁盘 / 数据库 / Redis / 主机 / 进程 / 业务量）。
 *
 * 权限码 `business:monitor:server-status` 只授予超级管理员，非超管调用返回 403；
 * 调用方应在渲染前先判断角色，避免无谓的拒绝请求。
 */
export function fetchServerStatus(): Promise<ServerStatusRes> {
  return get<ServerStatusRes>('/api/business/monitor/server-status');
}

/**
 * 下载诊断包（zip，仅超管）。
 *
 * 返回体是**二进制流**而不是统一 `Res<T>` 信封，所以必须 `responseType: 'blob'`
 * 且带鉴权头（不能用 `<a href>` 直接下）。文件名由后端 `Content-Disposition` 给出，
 * 但 axios 的 blob 分支拿不到响应头，前端按同一规则自己生成（见页面里的
 * `diagnosticFileName()`）。
 *
 * 权限：`system:server-status:view`（只授超管）+ 后端第二层超管判定，非超管 403。
 */
export function downloadDiagnosticBundle(): Promise<Blob> {
  return service
    .get<Blob>('/api/system/server-status/diagnostic', { responseType: 'blob', timeout: 30000 })
    .then((response) => response.data);
}
