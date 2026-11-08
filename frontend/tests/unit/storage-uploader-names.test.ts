/**
 * 文件管理「上传人」列译名单测（P2-2B 缺陷②）。
 *
 * 运行：`pnpm test:unit`（`tsx --test`，无 DOM）。
 *
 * 钉的是三条回退规则，错了会让「谁上传的」这条审计线索断掉：
 *
 * 1. 姓名优先（`sys_user.real_name`），空则用户名（`username`）；
 * 2. 解析不到 → **原 UUID**（不是「未知」，也不是空串）：用户至少能拿它去对照；
 * 3. 绝不把手机号当显示名（`sys_user.mobile` 已配脱敏，上传人不是联系方式）。
 *
 * 另外钉「只解析没解析过的 id」：翻页不该把整张用户表再拉一遍。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  UPLOADER_PAGE_SIZE,
  mergeUserNames,
  pendingUploaderIds,
  uploaderDisplayName,
  uploaderLabelOf,
} from '@/views/business/storage/file/uploader-names';

const UID = '01a19012-7573-7602-9b64-6b2e74502477';

test('② 上传人显示名：姓名 → 用户名 → UUID，空值给破折号', () => {
  const names = new Map([
    ['u1', '张三'],
    ['u2', 'lisi'],
  ]);
  assert.equal(uploaderDisplayName('u1', names), '张三');
  assert.equal(uploaderDisplayName('u2', names), 'lisi');
  // 解析不到（用户被删 / 超出译名页范围 / 没有 system:user:list 权限）→ 原 UUID
  assert.equal(uploaderDisplayName(UID, names), UID);
  // 空 / null → 破折号，不留白
  assert.equal(uploaderDisplayName('', names), '—');
  assert.equal(uploaderDisplayName(null, names), '—');
  assert.equal(uploaderDisplayName(undefined, names), '—');
  assert.equal(uploaderDisplayName('   ', names), '—');
  // 表里有空串（脏数据）也走回退，不显示空白
  assert.equal(uploaderDisplayName('u3', new Map([['u3', '  ']])), 'u3');
});

test('② 只请求没解析过的 id：本页去重 + 跨页复用，翻页不重复拉用户表', () => {
  const resolved = new Map([['u1', '张三']]);
  const ids = pendingUploaderIds(
    [
      { uploaded_by: 'u1' }, // 已解析
      { uploaded_by: 'u2' },
      { uploaded_by: 'u2' }, // 本页重复
      { uploaded_by: '' }, // 历史文件没有上传人
      { uploaded_by: null },
      { uploaded_by: ' u3 ' }, // 首尾空白要去掉
    ],
    resolved,
  );
  assert.deepEqual(ids, ['u2', 'u3']);

  // 全部已解析 → 不发请求（返回空数组）
  assert.deepEqual(pendingUploaderIds([{ uploaded_by: 'u1' }], resolved), []);
  // 空列表
  assert.deepEqual(pendingUploaderIds([], resolved), []);
});

test('② 合并用户表：real_name 空则用 username，两者都空不写（继续回退 UUID）', () => {
  const base = new Map([['old', '老数据']]);
  const merged = mergeUserNames(base, [
    { id: 'u1', username: 'zhangsan', real_name: '张三' },
    { id: 'u2', username: 'lisi', real_name: '' },
    { id: 'u3', username: 'wangwu', real_name: null },
    { id: 'u4', username: '', real_name: '  ' },
    { id: '', username: 'ghost', real_name: '幽灵' },
  ]);

  assert.equal(merged.get('u1'), '张三');
  assert.equal(merged.get('u2'), 'lisi');
  assert.equal(merged.get('u3'), 'wangwu');
  assert.equal(merged.has('u4'), false); // 脏数据：不写空名，交给 UUID 回退
  assert.equal(merged.has(''), false);
  assert.equal(merged.get('old'), '老数据'); // 已有条目保留（跨页累积）

  // 返回新 Map（Vue 靠换引用触发渲染），不改入参
  assert.notEqual(merged, base);
  assert.equal(base.has('u1'), false);
  assert.equal(base.size, 1);
});

test('② 译名页大小与其它 ERP 译名页同口径（200）', () => {
  assert.equal(UPLOADER_PAGE_SIZE, 200);
});

test('AUD-02 列表上传人列：后端姓名优先，兜底前端译名，绝不再渲染 UUID', () => {
  const names = new Map([
    ['u1', '张三'],
    ['u2', 'lisi'],
  ]);
  // ① 后端给了姓名（AUD-02 之后的主路径）：直接用，哪怕前端表里有别的名字
  assert.equal(uploaderLabelOf({ uploaded_by: 'u1', uploader_name: '张三丰' }, names), '张三丰');
  // ② 后端没给（老后端）→ 前端译名
  assert.equal(uploaderLabelOf({ uploaded_by: 'u1' }, names), '张三');
  assert.equal(uploaderLabelOf({ uploaded_by: 'u2', uploader_name: '' }, names), 'lisi');
  // ③ 后端给了空串（账号已删）且前端也查不到 → 破折号，**不是 UUID**
  assert.equal(uploaderLabelOf({ uploaded_by: UID, uploader_name: '' }, names), '—');
  assert.equal(uploaderLabelOf({ uploaded_by: UID }, names), '—');
  // ④ 历史文件没有上传人
  assert.equal(uploaderLabelOf({ uploaded_by: '', uploader_name: '' }, names), '—');
  assert.equal(uploaderLabelOf({}, names), '—');
  // ⑤ 空白姓名不算数（脏数据），继续往下回退
  assert.equal(uploaderLabelOf({ uploaded_by: 'u1', uploader_name: '   ' }, names), '张三');
});
