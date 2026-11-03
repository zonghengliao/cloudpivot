/**
 * 站内信 SSE 的**带认证头**读取器（`fetch` + `ReadableStream`）。
 *
 * # 为什么需要它（2026-11-03 实测）
 *
 * 浏览器原生 `EventSource` 不能自定义请求头，本仓的降级姿势是 `/api/business/notice/stream?token=…`，
 * 但认证层只在 `NIK_AUTH_QUERY_TOKEN_PATHS` 白名单里采信查询令牌，而该环境变量默认**为空**：
 * 实测 `GET /api/business/notice/stream?token=<有效令牌>` 返回 `401 未授权访问，请先登录`
 * ⇒ EventSource 永远连不上，只剩 30 秒轮询，新通知要等到下一次轮询才「自动弹」。
 *
 * 后端 handler 的文档里明确写了另一条路：「前端也可用 `fetch` + `ReadableStream` 带头读流，
 * 那条路不受白名单约束，是更推荐的姿势」。本模块就是把那条路做出来，作为 EventSource
 * 失败后的**追加通道**（EventSource 本身一行没改，失败语义仍是「降级轮询」）。
 *
 * # 帧格式
 *
 * | 帧 | 内容 |
 * |----|------|
 * | 数据帧 | `event: notice` + `data: {notice_id, notice_no, notice_type, level, title, link_url, biz_type, biz_id, receiver_id, created_at}` |
 * | 心跳帧 | `: keep-alive`（注释帧，`parser` 直接丢弃） |
 */

/** 解析出来的一条 SSE 帧 */
export interface SseFrame {
  /** 事件名；无 `event:` 行时为 `message`（与 `EventSource.onmessage` 对齐） */
  event: string;
  /** `data:` 行拼接结果（多行按 `\n` 连接） */
  data: string;
}

/** 解析器状态：跨 chunk 的半截帧要留着 */
export interface SseParseState {
  /** 尚未遇到空行的残留缓冲 */
  buffer: string;
}

/** 新建解析状态 */
export function createSseState(): SseParseState {
  return { buffer: '' };
}

/** 解析一个完整的 SSE 事件块（块内不含空行） */
function parseBlock(block: string): SseFrame | null {
  let event = 'message';
  const dataLines: string[] = [];
  for (const rawLine of block.split(/\r?\n/)) {
    // 注释帧（心跳 `: keep-alive`）整行以冒号开头，忽略
    if (!rawLine || rawLine.startsWith(':')) {
      continue;
    }
    const colon = rawLine.indexOf(':');
    const field = colon === -1 ? rawLine : rawLine.slice(0, colon);
    // 规范：冒号后紧跟的一个空格要去掉（不是所有字符）
    let value = colon === -1 ? '' : rawLine.slice(colon + 1);
    if (value.startsWith(' ')) {
      value = value.slice(1);
    }
    if (field === 'event') {
      event = value || 'message';
    } else if (field === 'data') {
      dataLines.push(value);
    }
    // id / retry 字段本仓用不到，直接忽略
  }
  if (dataLines.length === 0) {
    return null;
  }
  return { event, data: dataLines.join('\n') };
}

/**
 * 喂一段文本，返回本次能凑出的完整帧，并把半截帧留在 `state.buffer`。
 *
 * 网络 chunk 边界与 SSE 事件边界**无关**（一条 JSON 可能被切成三段），所以必须缓冲。
 */
export function parseSseChunk(state: SseParseState, chunk: string): SseFrame[] {
  state.buffer += chunk;
  const frames: SseFrame[] = [];
  let separator = state.buffer.search(/\r?\n\r?\n/);
  while (separator !== -1) {
    const block = state.buffer.slice(0, separator);
    // `\r\n\r\n` 是 4 个字符，`\n\n` 是 2 个
    const match = /\r?\n\r?\n/.exec(state.buffer.slice(separator));
    const cut = separator + (match ? match[0].length : 2);
    state.buffer = state.buffer.slice(cut);
    const frame = parseBlock(block);
    if (frame) {
      frames.push(frame);
    }
    separator = state.buffer.search(/\r?\n\r?\n/);
  }
  return frames;
}

/** 订阅参数 */
export interface NoticeStreamOptions {
  /** 完整地址（不含令牌；令牌走请求头） */
  url: string;
  /** 访问令牌（`localStorage.x_access_token`） */
  token: string;
  /** 收到一帧时回调 */
  onFrame: (frame: SseFrame) => void;
  /** 连接建立（HTTP 200 且首包到手）时回调 */
  onOpen?: () => void;
  /** 中断信号：`abort()` 后 `fetch` 与 reader 一起退出 */
  signal: AbortSignal;
}

/**
 * 建立带认证头的 SSE 读取循环（长驻，直到 `signal` 中断或连接结束）。
 *
 * 抛错情形（调用方据此回到轮询）：非 2xx 响应、`response.body` 为空、读流出错。
 */
export async function openNoticeStream(options: NoticeStreamOptions): Promise<void> {
  const response = await fetch(options.url, {
    method: 'GET',
    headers: {
      Authorization: `Bearer ${options.token}`,
      Accept: 'text/event-stream',
    },
    cache: 'no-store',
    signal: options.signal,
  });
  if (!response.ok || !response.body) {
    throw new Error(`SSE 订阅失败：HTTP ${response.status}`);
  }
  const reader = response.body.getReader();
  const decoder = new TextDecoder('utf-8');
  const state = createSseState();
  let opened = false;
  try {
    for (;;) {
      const { value, done } = await reader.read();
      if (done) {
        return;
      }
      if (!opened) {
        opened = true;
        options.onOpen?.();
      }
      const text = decoder.decode(value, { stream: true });
      for (const frame of parseSseChunk(state, text)) {
        options.onFrame(frame);
      }
    }
  } finally {
    // 主动取消时不要等下一次 read 才释放，浏览器要能立刻看到连接结束
    reader.cancel().catch(() => undefined);
  }
}
