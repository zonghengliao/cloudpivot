/**
 * A7 前端验收用的**本地假后端**（仅用于 UI 行为取证，不参与交付运行）。
 *
 * 为什么要它：A7 的验收要求「每页列表能加载 / 新增 / 编辑 / 删除 / 无权限时按钮隐藏」与
 * 「直传进度条真实增长」。真后端在 172.18.10.144 上，本机无法访问（见交付 summary 的环境说明），
 * 因此用一个零依赖的 Node HTTP 服务：
 *   1. 静态托管 `frontend/dist`（SPA fallback）；
 *   2. 按 A1–A6 的接口契约返回固定夹具（路径/字段与后端 handler.rs 一致）；
 *   3. `/upload-target` 接收真实 PUT 并**限速读取**，让浏览器端 XHR 的 `upload.onprogress`
 *      真正分多次触发（这样才能证明进度条不是假动画）。
 *
 * 三种令牌模拟三种权限面：
 *   - `mock-super-token`   → `*:*:*`（超管，全部按钮可见）
 *   - `mock-ops-token`     → 存储上传/列表/下载 + 导入导出主链路 + 消息中心（无配置管理权限）
 *   - `mock-limited-token` → 只有 `business:notice:mine`（只能进消息中心）
 */
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const DIST = path.resolve(__dirname, '../frontend/dist');
const PORT = Number(process.env.MOCK_PORT || 4180);

const SUPER_PERMS = [
  '*:*:*',
];

const OPS_PERMS = [
  'business:storage:upload',
  'business:storage:list',
  'business:storage:download',
  'business:impexp:def:list',
  'business:impexp:import:template',
  'business:impexp:import:upload',
  'business:impexp:import:list',
  'business:impexp:import:confirm',
  'business:impexp:import:cancel',
  'business:impexp:import:retry',
  'business:impexp:import:download',
  'business:impexp:export:create',
  'business:impexp:export:list',
  'business:impexp:export:download',
  'business:impexp:export:cancel',
  'business:notice:mine',
];

const LIMITED_PERMS = ['business:notice:mine'];

/** 令牌 → 用户画像 */
const TOKENS = {
  'mock-super-token': { id: 'u-super', username: 'admin', real_name: '超级管理员', roles: ['SUPER_ADMIN'], permissions: SUPER_PERMS },
  'mock-ops-token': { id: 'u-ops', username: 'ops', real_name: '运维小王', roles: ['OPS_ONLY'], permissions: OPS_PERMS },
  'mock-limited-token': { id: 'u-limited', username: 'staff', real_name: '普通员工', roles: ['STAFF'], permissions: LIMITED_PERMS },
};

function userFromToken(req) {
  const auth = req.headers.authorization || '';
  const token = auth.replace(/^Bearer\s+/i, '');
  return TOKENS[token] || null;
}

/** 完整菜单树（超管视角；其它角色由前端按 auth_code 过滤） */
const MENUS = [
  {
    id: 'm-dash', name: 'dashboard', title: '首页', path: '/dashboard', component: 'dashboard/index',
    icon: 'dashboard', menu_type: 2, auth_code: '', parent_id: null, sort_order: 10, is_visible: true, children: [],
  },
  {
    id: 'm-biz', name: 'business', title: '通用底座', path: '/business', component: null,
    icon: 'app', menu_type: 1, auth_code: '', parent_id: null, sort_order: 70, is_visible: true,
    children: [
      { id: 'm-st-base', name: '存储管理', title: '存储管理', path: '/business/storage/backend', component: 'business/storage/backend/index', icon: 'object-storage', menu_type: 2, auth_code: 'business:storage:config:list', parent_id: 'm-biz', sort_order: 10, is_visible: true, children: [] },
      { id: 'm-st-file', name: '文件管理', title: '文件管理', path: '/business/storage/file', component: 'business/storage/file/index', icon: 'folder', menu_type: 2, auth_code: 'business:storage:list', parent_id: 'm-biz', sort_order: 20, is_visible: true, children: [] },
      { id: 'm-ie-imp', name: '导入向导', title: '导入向导', path: '/business/impexp/import', component: 'business/impexp/import/index', icon: 'upload', menu_type: 2, auth_code: 'business:impexp:import:upload', parent_id: 'm-biz', sort_order: 30, is_visible: true, children: [] },
      { id: 'm-ie-itask', name: '导入任务', title: '导入任务', path: '/business/impexp/import-task', component: 'business/impexp/import-task/index', icon: 'file-import', menu_type: 2, auth_code: 'business:impexp:import:list', parent_id: 'm-biz', sort_order: 40, is_visible: true, children: [] },
      { id: 'm-ie-etask', name: '导出任务', title: '导出任务', path: '/business/impexp/export-task', component: 'business/impexp/export-task/index', icon: 'file-export', menu_type: 2, auth_code: 'business:impexp:export:list', parent_id: 'm-biz', sort_order: 50, is_visible: true, children: [] },
      { id: 'm-nt-tpl', name: '通知模板', title: '通知模板', path: '/business/notice/template', component: 'business/notice/template/index', icon: 'notification', menu_type: 2, auth_code: 'business:notice:template:list', parent_id: 'm-biz', sort_order: 60, is_visible: true, children: [] },
      { id: 'm-nt-msg', name: '消息中心', title: '消息中心', path: '/business/notice/message', component: 'business/notice/message/index', icon: 'notification', menu_type: 2, auth_code: 'business:notice:mine', parent_id: 'm-biz', sort_order: 70, is_visible: false, children: [] },
    ],
  },
];

// ==================== 夹具数据 ====================
const STORAGE_CONFIGS = [
  {
    id: 'sc-1', config_code: 'local-default', config_name: '本地默认', backend_type: 'local',
    endpoint: 'uploads', bucket: '', region: '', base_path: '', use_tls: false, path_style: false,
    ak_masked: '', has_secret: false, key_version: 1, is_default: true, status: 1,
    health_status: 'ok', last_check_at: '2026-10-28T12:00:00Z', last_error: '', sort_order: 10,
    remark: '系统内置', created_at: '2026-10-28T08:00:00Z', updated_at: '2026-10-28T12:00:00Z',
  },
  {
    id: 'sc-2', config_code: 'prod-minio', config_name: '生产 MinIO', backend_type: 'minio',
    endpoint: 'http://minio.corp.local:9000', bucket: 'cloudpivot', region: 'us-east-1',
    base_path: 'cloudpivot/', use_tls: false, path_style: true,
    ak_masked: 'AKIA****7ZP2', has_secret: true, key_version: 1, is_default: false, status: 1,
    health_status: 'fail', last_check_at: '2026-10-28T11:50:00Z',
    last_error: '连接失败：无法访问 minio.corp.local:9000（连接超时 10 秒）', sort_order: 20,
    remark: '', created_at: '2026-10-28T08:10:00Z', updated_at: '2026-10-28T11:50:00Z',
  },
];

const POLICIES = [
  {
    id: 'sp-1', category: 'attachment', category_name: '通用附件', max_size_mb: 20,
    allowed_extensions: 'pdf,docx,xlsx,zip,png,jpg', allowed_mimes: '', allow_overwrite: false,
    direct_upload: true, presign_ttl_secs: 900, storage_code: '', status: 1, sort_order: 10,
    remark: '', created_at: '2026-10-28T08:00:00Z', updated_at: '2026-10-28T08:00:00Z',
  },
  {
    id: 'sp-2', category: 'import', category_name: '导入文件', max_size_mb: 50,
    allowed_extensions: 'xlsx,xls', allowed_mimes: '', allow_overwrite: false,
    direct_upload: true, presign_ttl_secs: 1800, storage_code: '', status: 1, sort_order: 20,
    remark: '', created_at: '2026-10-28T08:00:00Z', updated_at: '2026-10-28T08:00:00Z',
  },
];

const FILES = [
  {
    id: 'f-1', file_name: '2026-10-budget.xlsx', original_name: '2026年10月预算表.xlsx',
    file_key: 'cloudpivot/attachment/2026-10-budget.xlsx', file_size: 2_355_000,
    mime_type: 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
    extension: 'xlsx', category: 'attachment', status: 1, uploaded_by: 'ops',
    created_at: '2026-10-28T09:12:00Z', url: '', storage_code: 'prod-minio', upload_mode: 'direct',
  },
  {
    id: 'f-2', file_name: 'org-chart.png', original_name: '组织架构图.png',
    file_key: 'cloudpivot/attachment/org-chart.png', file_size: 184_320,
    mime_type: 'image/png', extension: 'png', category: 'attachment', status: 1,
    uploaded_by: 'admin', created_at: '2026-10-27T15:30:00Z', url: '', storage_code: 'local-default',
    upload_mode: 'server',
  },
];

const IMPORT_TASKS = [
  {
    id: 'it-0', task_no: 'IMP20261028002', biz_type: 'support.dict_item', biz_name: '字典项',
    file_name: '字典项导入-干净文件.xlsx', file_size: 44_000, mode: 'STRICT', status: 'VALIDATED',
    total_rows: 60, valid_rows: 60, error_rows: 0, imported_rows: 0, progress: 40,
    message: '校验通过，可确认入库', error_summary: null, created_at: '2026-10-28T10:40:00Z', finished_at: null,
  },
  {
    id: 'it-1', task_no: 'IMP20261028001', biz_type: 'support.dict_item', biz_name: '字典项',
    file_name: '字典项导入.xlsx', file_size: 51_200, mode: 'STRICT', status: 'VALIDATED',
    total_rows: 120, valid_rows: 115, error_rows: 5, imported_rows: 0, progress: 40,
    message: '校验完成，5 行失败待修正', error_summary: { error: 5 }, created_at: '2026-10-28T10:20:00Z', finished_at: null,
  },
  {
    id: 'it-2', task_no: 'IMP20261027009', biz_type: 'support.dict_item', biz_name: '字典项',
    file_name: '字典项导入-0927.xlsx', file_size: 49_800, mode: 'STRICT', status: 'SUCCESS',
    total_rows: 88, valid_rows: 88, error_rows: 0, imported_rows: 88, progress: 100,
    message: '成功 88，失败 0', error_summary: null, created_at: '2026-10-27T16:40:00Z', finished_at: '2026-10-27T16:41:10Z',
  },
  {
    id: 'it-3', task_no: 'IMP20261026003', biz_type: 'support.dict_item', biz_name: '字典项',
    file_name: '字典项导入-坏文件.xlsx', file_size: 12_000, mode: 'STRICT', status: 'FAILED',
    total_rows: 30, valid_rows: 0, error_rows: 30, imported_rows: 0, progress: 100,
    message: '表头缺少必需列', error_summary: { error: 30 }, created_at: '2026-10-26T09:00:00Z', finished_at: '2026-10-26T09:00:20Z',
  },
];

const IMPORT_ERRORS = [
  { row_no: 12, column_key: 'dict_label', column_header: '字典标签', raw_value: '启用状态', error_code: 'UNIQUE', error_message: '编码已存在（与第 88 行重复）' },
  { row_no: 33, column_key: 'sort_order', column_header: '排序', raw_value: '第一', error_code: 'TYPE', error_message: '应为数字，实际为「第一」' },
  { row_no: 47, column_key: 'status', column_header: '状态', raw_value: '台套', error_code: 'ENUM', error_message: '值「台套」不在可选范围内（启用 / 停用）' },
  { row_no: 88, column_key: 'dict_code', column_header: '字典编码', raw_value: '', error_code: 'REQUIRED', error_message: '必填项为空' },
  { row_no: 96, column_key: 'dict_label', column_header: '字典标签', raw_value: '很长很长很长很长很长很长很长很长很长很长很长的标签', error_code: 'LENGTH', error_message: '长度超限（最多 32 个字符，实际 45 个）' },
];

const EXPORT_TASKS = [
  {
    id: 'et-1', task_no: 'EXP20261028001', biz_type: 'support.dict_item', biz_name: '字典项',
    status: 'RUNNING', total_rows: 203_481, exported_rows: 85_400, progress: 42,
    file_name: null, file_size: null, expire_at: null, message: '', created_at: '2026-10-28T10:01:00Z', finished_at: null,
  },
  {
    id: 'et-2', task_no: 'EXP20261027004', biz_type: 'support.dict_item', biz_name: '字典项',
    status: 'SUCCESS', total_rows: 4_800, exported_rows: 4_800, progress: 100,
    file_name: '字典项-20261027.xlsx', file_size: 355_000,
    expire_at: new Date(Date.now() + 20 * 3600_000).toISOString(), message: '导出完成',
    created_at: '2026-10-27T18:00:00Z', finished_at: '2026-10-27T18:00:40Z',
  },
  {
    id: 'et-3', task_no: 'EXP20261025002', biz_type: 'support.dict_item', biz_name: '字典项',
    status: 'SUCCESS', total_rows: 1_200, exported_rows: 1_200, progress: 100,
    file_name: '字典项-20261025.xlsx', file_size: 120_000,
    expire_at: new Date(Date.now() - 3 * 3600_000).toISOString(), message: '导出完成',
    created_at: '2026-10-25T10:00:00Z', finished_at: '2026-10-25T10:00:20Z',
  },
];

const DEFS = [
  { biz_type: 'support.dict_item', biz_name: '字典项', supports_import: true, supports_export: true, template_version: 1, max_rows: 100_000, allow_partial: false },
  { biz_type: 'support.dict_type', biz_name: '字典类型', supports_import: true, supports_export: false, template_version: 1, max_rows: 10_000, allow_partial: false },
];

const MY_NOTICES = [
  {
    receiver_id: 'nr-1', notice_id: 'nt-1', notice_no: 'NTC20261028001', notice_type: 'ALERT', level: 'ERROR',
    title: '导入任务失败：字典项', content: '任务 IMP20261026003 校验未通过，30 行失败。\n请前往「导入任务」查看明细后重试。',
    content_format: 'TEXT', link_url: '/business/impexp/import-task', read_flag: false, read_at: null,
    created_at: '2026-10-28T10:30:00Z',
  },
  {
    receiver_id: 'nr-2', notice_id: 'nt-2', notice_no: 'NTC20261028002', notice_type: 'SYSTEM', level: 'INFO',
    title: '导出任务已完成', content: '字典项导出已完成，下载链接 24 小时内有效。', content_format: 'TEXT',
    link_url: '/business/impexp/export-task', read_flag: false, read_at: null, created_at: '2026-10-28T09:10:00Z',
  },
  {
    receiver_id: 'nr-3', notice_id: 'nt-3', notice_no: 'NTC20261027003', notice_type: 'CIRCULAR', level: 'SUCCESS',
    title: '系统维护通知', content: '系统将于本周日 02:00-04:00 维护。', content_format: 'TEXT', link_url: '',
    read_flag: true, read_at: '2026-10-27T12:00:00Z', created_at: '2026-10-27T08:00:00Z',
  },
];

const TEMPLATES = [
  {
    id: 'tpl-1', template_code: 'NOTICE_IMPORT_DONE', template_name: '导入完成通知', notice_type: 'SYSTEM',
    default_level: 'INFO', title_template: '{{userName}} 的导入任务已完成', content_template: '共导入 {{rowCount}} 行，失败 {{failCount}} 行。',
    content_format: 'TEXT', link_template: '/business/impexp/import-task', channels: 'inbox', channel_mask: 1,
    variables: [
      { name: 'userName', label: '接收人姓名', required: true, sample: '张三' },
      { name: 'rowCount', label: '导入行数', required: true, sample: '120' },
      { name: 'failCount', label: '失败行数', required: true, sample: '0' },
    ],
    status: 1, remark: '内置模板', created_at: '2026-10-28T08:00:00Z', updated_at: '2026-10-28T08:00:00Z',
  },
  {
    id: 'tpl-2', template_code: 'NOTICE_EXPORT_DONE', template_name: '导出完成通知', notice_type: 'SYSTEM',
    default_level: 'SUCCESS', title_template: '导出完成：{{bizName}}', content_template: '{{rowCount}} 行已导出，链接 24 小时有效。',
    content_format: 'TEXT', link_template: '/business/impexp/export-task', channels: 'inbox', channel_mask: 1,
    variables: [
      { name: 'bizName', label: '业务名称', required: true, sample: '字典项' },
      { name: 'rowCount', label: '导出行数', required: true, sample: '4800' },
    ],
    status: 1, remark: '', created_at: '2026-10-28T08:00:00Z', updated_at: '2026-10-28T08:00:00Z',
  },
];

// ==================== 工具 ====================
function ok(res, data) {
  sendJson(res, { code: 0, msg: 'success', data });
}
function fail(res, code, msg, status = 200) {
  sendJson(res, { code, msg, data: null }, status);
}
function sendJson(res, body, status = 200) {
  const payload = JSON.stringify(body);
  res.writeHead(status, { 'Content-Type': 'application/json; charset=utf-8', 'Content-Length': Buffer.byteLength(payload) });
  res.end(payload);
}
function pageOf(list, url) {
  const page = Number(url.searchParams.get('page') || 1);
  const pageSize = Number(url.searchParams.get('page_size') || 10);
  const start = (page - 1) * pageSize;
  return { list: list.slice(start, start + pageSize), total: list.length, page, page_size: pageSize };
}
function readBody(req) {
  return new Promise((resolve) => {
    let raw = '';
    req.on('data', (chunk) => {
      raw += chunk;
    });
    req.on('end', () => {
      try {
        resolve(raw ? JSON.parse(raw) : {});
      } catch {
        resolve({});
      }
    });
  });
}

const MIME = {
  '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8', '.png': 'image/png', '.jpg': 'image/jpeg',
  '.svg': 'image/svg+xml', '.ico': 'image/x-icon', '.woff2': 'font/woff2', '.json': 'application/json',
};

function serveStatic(req, res, pathname) {
  let filePath = path.join(DIST, pathname);
  if (!filePath.startsWith(DIST) || !fs.existsSync(filePath) || fs.statSync(filePath).isDirectory()) {
    filePath = path.join(DIST, 'index.html');
  }
  const ext = path.extname(filePath);
  res.writeHead(200, { 'Content-Type': MIME[ext] || 'application/octet-stream' });
  fs.createReadStream(filePath).pipe(res);
}

// ==================== 主服务 ====================
const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, `http://127.0.0.1:${PORT}`);
  const p = url.pathname;

  // ---- 直传目标：真实接收 PUT，按 1MB/40ms 限速读取，制造可观测的进度 ----
  if (p === '/upload-target' && req.method === 'PUT') {
    let received = 0;
    req.on('data', (chunk) => {
      received += chunk.length;
      req.pause();
      setTimeout(() => req.resume(), 12);
    });
    req.on('end', () => {
      res.writeHead(200, { ETag: `"mock-${received}"` });
      res.end('');
    });
    return;
  }

  if (!p.startsWith('/api/')) {
    return serveStatic(req, res, p);
  }

  const body = req.method === 'GET' || req.method === 'DELETE' ? {} : await readBody(req);

  // ---- 认证 ----
  if (p === '/api/system/auth/login' && req.method === 'POST') {
    const token = body.username === 'admin' ? 'mock-super-token' : body.username === 'ops' ? 'mock-ops-token' : 'mock-limited-token';
    const user = TOKENS[token];
    return ok(res, {
      access_token: token, refresh_token: `${token}-refresh`, token_type: 'Bearer', expires_in: 7200,
      must_change_password: false,
      user: { id: user.id, username: user.username, real_name: user.real_name },
    });
  }
  if (p === '/api/system/auth/me') {
    const user = userFromToken(req);
    if (!user) return fail(res, 20001, '登录状态已失效', 401);
    return ok(res, { ...user, email: `${user.username}@example.com`, mobile: '', dept_id: null, must_change_password: false });
  }
  if (p === '/api/system/auth/menus') {
    return ok(res, MENUS);
  }

  // ---- 通知（人人可用，先于鉴权样例放在前面） ----
  if (p === '/api/business/notice/unread-count') {
    return ok(res, { unread_count: MY_NOTICES.filter((n) => !n.read_flag).length });
  }
  if (p === '/api/business/notice/sse') {
    res.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache', Connection: 'keep-alive' });
    res.write(': connected\n\n');
    const timer = setInterval(() => res.write(': ping\n\n'), 15_000);
    req.on('close', () => clearInterval(timer));
    return;
  }
  if (p === '/api/business/notice/mine') {
    const flag = url.searchParams.get('read_flag');
    // read_flag=false → 未读；true → 已读；不传 → 全部
    const filtered =
      flag === null ? MY_NOTICES : MY_NOTICES.filter((n) => n.read_flag === (flag === 'true'));
    return ok(res, pageOf(filtered, url));
  }
  if (p.startsWith('/api/business/notice/read/') && req.method === 'PUT') {
    const id = p.split('/').pop();
    const row = MY_NOTICES.find((n) => n.receiver_id === id);
    if (row) {
      row.read_flag = true;
      row.read_at = new Date().toISOString();
    }
    return ok(res, null);
  }
  if (p === '/api/business/notice/read-all' && req.method === 'PUT') {
    MY_NOTICES.forEach((n) => {
      n.read_flag = true;
    });
    return ok(res, null);
  }
  if (p.startsWith('/api/business/notice/mine/') && req.method === 'DELETE') {
    const id = p.split('/').pop();
    const index = MY_NOTICES.findIndex((n) => n.receiver_id === id);
    if (index >= 0) MY_NOTICES.splice(index, 1);
    return ok(res, null);
  }
  if (p === '/api/business/notice/template/page') {
    return ok(res, pageOf(TEMPLATES, url));
  }
  if (p === '/api/business/notice/channel/config') {
    return ok(res, { inbox: true, email: false, webhook: false, sms: false, webhook_masked: '' });
  }
  if (/^\/api\/business\/notice\/template\/[^/]+\/preview$/.test(p) && req.method === 'POST') {
    const id = p.split('/')[5];
    const tpl = TEMPLATES.find((t) => t.id === id);
    const vars = body.vars || {};
    let title = tpl?.title_template || '';
    let content = tpl?.content_template || '';
    for (const [key, value] of Object.entries(vars)) {
      title = title.split(`{{${key}}}`).join(String(value));
      content = content.split(`{{${key}}}`).join(String(value));
    }
    return ok(res, { title, content });
  }
  if (p === '/api/business/notice/template' && req.method === 'POST') {
    const created = { ...body, id: `tpl-${Date.now()}`, channel_mask: 1, created_at: new Date().toISOString(), updated_at: new Date().toISOString() };
    TEMPLATES.push(created);
    return ok(res, created);
  }
  if (/^\/api\/business\/notice\/template\/[^/]+$/.test(p) && req.method === 'PUT') {
    const id = p.split('/').pop();
    const tpl = TEMPLATES.find((t) => t.id === id);
    if (tpl) Object.assign(tpl, body, { updated_at: new Date().toISOString() });
    return ok(res, tpl);
  }
  if (/^\/api\/business\/notice\/template\/[^/]+$/.test(p) && req.method === 'DELETE') {
    const id = p.split('/').pop();
    const index = TEMPLATES.findIndex((t) => t.id === id);
    if (index >= 0) TEMPLATES.splice(index, 1);
    return ok(res, null);
  }
  if (p === '/api/business/notice' && req.method === 'POST') {
    return ok(res, {
      id: `nt-${Date.now()}`, notice_no: `NTC${Date.now()}`, notice_type: body.notice_type || 'SYSTEM',
      level: body.level || 'INFO', title: body.title || '（模板渲染）', content: body.content || '',
      content_format: 'TEXT', link_url: body.link_url || '', biz_type: '', biz_id: '',
      template_code: body.template_code || '', sender_id: 'u-super', sender_name: '超级管理员',
      target_type: body.target_type, target_ids: body.target_ids || [], channel_mask: 1, channels: ['inbox'],
      receiver_count: body.target_type === 'ALL' ? 42 : (body.target_ids || []).length, read_count: 0, read_rate: 0,
      status: 1, expire_at: null, created_at: new Date().toISOString(),
    });
  }

  // ---- 从这里开始要求有效令牌 ----
  const user = userFromToken(req);
  if (!user) {
    return fail(res, 20001, '登录状态已失效', 401);
  }
  const hasPerm = (code) => user.permissions.includes('*:*:*') || user.permissions.includes(code);
  const guard = (code) => {
    if (!hasPerm(code)) {
      fail(res, 30001, '您没有该操作的权限，请联系管理员');
      return false;
    }
    return true;
  };

  // ---- 存储配置 ----
  if (p === '/api/business/storage/config/page') {
    if (!guard('business:storage:config:list')) return;
    return ok(res, pageOf(STORAGE_CONFIGS, url));
  }
  if (p === '/api/business/storage/config' && req.method === 'POST') {
    if (!guard('business:storage:config:create')) return;
    const created = {
      ...body, id: `sc-${Date.now()}`, ak_masked: body.ak ? `${body.ak.slice(0, 4)}****${body.ak.slice(-4)}` : '',
      has_secret: !!body.sk, key_version: 1, health_status: 'unknown', last_check_at: null, last_error: '',
      created_at: new Date().toISOString(), updated_at: new Date().toISOString(),
    };
    STORAGE_CONFIGS.push(created);
    return ok(res, created);
  }
  if (/^\/api\/business\/storage\/config\/[^/]+$/.test(p)) {
    const id = p.split('/').pop();
    const row = STORAGE_CONFIGS.find((c) => c.id === id);
    if (req.method === 'PUT') {
      if (!guard('business:storage:config:update')) return;
      if (row) Object.assign(row, body, { updated_at: new Date().toISOString(), ak_masked: body.ak ? 'AKIA****MASK' : row.ak_masked });
      return ok(res, row);
    }
    if (req.method === 'DELETE') {
      if (!guard('business:storage:config:delete')) return;
      const index = STORAGE_CONFIGS.findIndex((c) => c.id === id);
      if (index >= 0) STORAGE_CONFIGS.splice(index, 1);
      return ok(res, null);
    }
    return ok(res, row);
  }
  if (/^\/api\/business\/storage\/config\/[^/]+\/default$/.test(p) && req.method === 'POST') {
    if (!guard('business:storage:config:update')) return;
    const id = p.split('/')[5];
    STORAGE_CONFIGS.forEach((c) => {
      c.is_default = c.id === id;
    });
    return ok(res, null);
  }
  if (/^\/api\/business\/storage\/config\/[^/]+\/test$/.test(p) && req.method === 'POST') {
    if (!guard('business:storage:config:test')) return;
    const id = p.split('/')[5];
    const row = STORAGE_CONFIGS.find((c) => c.id === id);
    const healthy = id === 'sc-1';
    if (row) {
      row.health_status = healthy ? 'ok' : 'fail';
      row.last_check_at = new Date().toISOString();
    }
    return ok(res, {
      config_code: row?.config_code || '', backend_type: row?.backend_type || 'local', healthy,
      latency_ms: healthy ? 42 : 10_000,
      message: healthy ? '桶 uploads 已存在' : '连接失败：无法访问 minio.corp.local:9000（连接超时 10 秒）。请检查地址、端口、防火墙与内网互通',
      checked_at: new Date().toISOString(),
    });
  }

  // ---- 上传策略 ----
  if (p === '/api/business/storage/policy/page') {
    if (!guard('business:storage:policy:list')) return;
    return ok(res, pageOf(POLICIES, url));
  }
  if (/^\/api\/business\/storage\/policy\/[^/]+$/.test(p) && req.method === 'PUT') {
    if (!guard('business:storage:policy:update')) return;
    const id = p.split('/').pop();
    const row = POLICIES.find((x) => x.id === id);
    if (row) Object.assign(row, body, { updated_at: new Date().toISOString() });
    return ok(res, row);
  }

  // ---- 直传初始化 / 完成登记 ----
  if (p === '/api/business/storage/presign' && req.method === 'POST') {
    if (!guard('business:storage:upload')) return;
    return ok(res, {
      file_key: `cloudpivot/${body.category || 'attachment'}/${body.file_name}`,
      upload_url: `http://127.0.0.1:${PORT}/upload-target?name=${encodeURIComponent(body.file_name || '')}`,
      method: 'PUT', headers: [], expire_at: new Date(Date.now() + 900_000).toISOString(),
      storage_code: 'prod-minio', category: body.category || 'attachment', duplicated: false, file: null,
    });
  }
  if (p === '/api/business/storage/complete' && req.method === 'POST') {
    if (!guard('business:storage:upload')) return;
    const created = {
      id: `f-${Date.now()}`, file_name: body.file_key.split('/').pop(), original_name: body.file_name,
      file_key: body.file_key, file_size: body.file_size, mime_type: body.mime_type || 'application/octet-stream',
      extension: (body.file_name.split('.').pop() || '').toLowerCase(), category: body.category, status: 1,
      uploaded_by: user.username, created_at: new Date().toISOString(), url: '',
      storage_code: 'prod-minio', upload_mode: 'direct',
    };
    FILES.unshift(created);
    return ok(res, created);
  }

  // ---- 文件 ----
  if (p === '/api/business/storage/file/page') {
    if (!guard('business:storage:list')) return;
    return ok(res, pageOf(FILES, url));
  }
  if (/^\/api\/business\/storage\/[^/]+$/.test(p) && req.method === 'DELETE') {
    if (!guard('business:storage:delete')) return;
    const id = p.split('/').pop();
    const index = FILES.findIndex((f) => f.id === id);
    if (index >= 0) FILES.splice(index, 1);
    return ok(res, null);
  }
  if (p.startsWith('/api/business/storage/download/')) {
    if (!guard('business:storage:download')) return;
    res.writeHead(200, { 'Content-Type': 'application/octet-stream' });
    res.end('mock file body');
    return;
  }

  // ---- 导入导出 ----
  if (p === '/api/business/impexp/def/list') {
    if (!guard('business:impexp:def:list')) return;
    return ok(res, DEFS);
  }
  if (p === '/api/business/impexp/import/page') {
    if (!guard('business:impexp:import:list')) return;
    return ok(res, pageOf(IMPORT_TASKS, url));
  }
  if (/^\/api\/business\/impexp\/import\/[^/]+$/.test(p) && req.method === 'GET') {
    if (!guard('business:impexp:import:list')) return;
    const id = p.split('/').pop();
    return ok(res, IMPORT_TASKS.find((t) => t.id === id));
  }
  if (/^\/api\/business\/impexp\/import\/[^/]+\/errors$/.test(p)) {
    if (!guard('business:impexp:import:list')) return;
    return ok(res, pageOf(IMPORT_ERRORS, url));
  }
  if (/^\/api\/business\/impexp\/import\/[^/]+\/errors\/file$/.test(p)) {
    if (!guard('business:impexp:import:download')) return;
    res.writeHead(200, { 'Content-Type': 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet' });
    res.end('mock xlsx');
    return;
  }
  if (/^\/api\/business\/impexp\/import\/[^/]+\/retry$/.test(p) && req.method === 'POST') {
    if (!guard('business:impexp:import:retry')) return;
    const id = p.split('/')[5];
    const source = IMPORT_TASKS.find((t) => t.id === id);
    const created = {
      ...source, id: `it-retry-${Date.now()}`, task_no: `IMP${Date.now()}`, status: 'PENDING',
      progress: 0, imported_rows: 0, created_at: new Date().toISOString(), finished_at: null,
    };
    IMPORT_TASKS.unshift(created);
    return ok(res, created);
  }
  if (/^\/api\/business\/impexp\/import\/[^/]+\/cancel$/.test(p) && req.method === 'POST') {
    if (!guard('business:impexp:import:cancel')) return;
    const id = p.split('/')[5];
    const task = IMPORT_TASKS.find((t) => t.id === id);
    if (task) task.status = 'CANCELLED';
    return ok(res, null);
  }
  if (/^\/api\/business\/impexp\/export\/[^/]+\/cancel$/.test(p) && req.method === 'POST') {
    if (!guard('business:impexp:export:cancel')) return;
    const id = p.split('/')[5];
    const task = EXPORT_TASKS.find((t) => t.id === id);
    if (task) task.status = 'CANCELLED';
    return ok(res, null);
  }
  if (/^\/api\/business\/impexp\/import\/[^/]+\/confirm$/.test(p) && req.method === 'POST') {
    if (!guard('business:impexp:import:confirm')) return;
    const id = p.split('/')[5];
    const task = IMPORT_TASKS.find((t) => t.id === id);
    if (task) {
      task.status = 'SUCCESS';
      task.imported_rows = task.valid_rows;
      task.progress = 100;
    }
    return ok(res, { task_id: id, status: 'SUCCESS', imported_rows: task?.valid_rows ?? 0, message: `成功 ${task?.valid_rows ?? 0}，失败 0` });
  }
  if (p === '/api/business/impexp/import/upload') {
    if (!guard('business:impexp:import:upload')) return;
    return ok(res, {
      task_id: 'it-mock', task_no: 'IMP20261028999', status: 'VALIDATED',
      report: {
        total: 120, valid: 115, error: 5, skipped_example: 1, skipped_blank: 0,
        warnings: [{ code: 'E-IMP-05', message: '表头顺序与模板不一致，已按列名匹配，请确认数据对位是否正确' }],
        errors: IMPORT_ERRORS, truncated: false, fk_sql_statements: 3, elapsed_ms: 812,
      },
    });
  }
  if (p === '/api/business/impexp/export/page') {
    if (!guard('business:impexp:export:list')) return;
    return ok(res, pageOf(EXPORT_TASKS, url));
  }
  if (p === '/api/business/impexp/export' && req.method === 'POST') {
    if (!guard('business:impexp:export:create')) return;
    const created = {
      id: `et-${Date.now()}`, task_no: `EXP${Date.now()}`, biz_type: body.biz_type, biz_name: '字典项',
      status: 'PENDING', total_rows: 0, exported_rows: 0, progress: 0, file_name: null, file_size: null,
      expire_at: null, message: '', created_at: new Date().toISOString(), finished_at: null,
    };
    EXPORT_TASKS.unshift(created);
    return ok(res, created);
  }
  if (/^\/api\/business\/impexp\/export\/[^/]+$/.test(p) && req.method === 'DELETE') {
    if (!guard('business:impexp:export:delete')) return;
    const id = p.split('/').pop();
    const index = EXPORT_TASKS.findIndex((t) => t.id === id);
    if (index >= 0) EXPORT_TASKS.splice(index, 1);
    return ok(res, null);
  }
  if (/^\/api\/business\/impexp\/export\/[^/]+\/download$/.test(p)) {
    if (!guard('business:impexp:export:download')) return;
    res.writeHead(200, { 'Content-Type': 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet' });
    res.end('mock export xlsx');
    return;
  }

  return fail(res, 10404, `mock 未实现的接口：${req.method} ${p}`, 404);
});

server.listen(PORT, '0.0.0.0', () => {
  console.log(`A7 mock server listening on http://127.0.0.1:${PORT} (static: ${DIST})`);
});
