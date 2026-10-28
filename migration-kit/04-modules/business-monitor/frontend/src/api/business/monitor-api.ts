import { get } from '@/api/request';

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
}

/**
 * 查询服务器运行状态（CPU / 内存 / 磁盘 / 数据库 / Redis）。
 *
 * 权限码 `business:monitor:server-status` 只授予超级管理员，非超管调用返回 403；
 * 调用方应在渲染前先判断角色，避免无谓的拒绝请求。
 */
export function fetchServerStatus(): Promise<ServerStatusRes> {
  return get<ServerStatusRes>('/api/business/monitor/server-status');
}
