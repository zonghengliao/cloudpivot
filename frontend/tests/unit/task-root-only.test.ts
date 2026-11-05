/**
 * T8-C 单测：任务列表 `root_only`（只返回顶层任务）—— 前端侧。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`）。
 *
 * # 为什么是「读源码断言」而不是挂组件
 *
 * 本卡前端的改动只有两行语义：
 * ① `TaskPageQuery` 多一个可选布尔（类型契约）；
 * ② **列表视图**的 `pageTasks` 调用多一个 `root_only: true`（默认只取顶层）。
 * 这两件事的正确性判据是「谁传了、谁没传」——挂载 index.vue 会连带拉路由 / 权限 /
 * 看板组件，断言反而变成测别人；直接读源码能把「只有列表视图传」这条钉死
 * （看板 / 甘特 / 燃尽图 / 父任务选择器一律不传，否则它们会只剩顶层任务）。
 *
 * 同时读后端三个文件做**跨层对齐**：参数名 / 片段 / 归一化三处任一处改名，这里先红。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join, resolve } from 'node:path';

const FRONTEND = resolve(process.cwd());
const REPO = resolve(FRONTEND, '..');
const TASK_API = join(FRONTEND, 'src/api/business/task-api.ts');
const TASK_VIEW = join(FRONTEND, 'src/views/business/task/index.vue');
const TASK_VIEWS_DIR = join(FRONTEND, 'src/views/business/task');

/** 取源码里某个区间（`[start, end)`）—— 用来把断言限制在接口 / 函数内部 */
function slice(text: string, startMarker: string, endMarker: string): string {
  const start = text.indexOf(startMarker);
  assert.notEqual(start, -1, `找不到起始标记：${startMarker}`);
  const end = text.indexOf(endMarker, start + startMarker.length);
  assert.notEqual(end, -1, `找不到结束标记：${endMarker}`);
  return text.slice(start, end);
}

/** 递归收集目录下的 .vue / .ts 文件 */
function walk(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    if (statSync(full).isDirectory()) {
      walk(full, out);
    } else if (name.endsWith('.vue') || name.endsWith('.ts')) {
      out.push(full);
    }
  }
  return out;
}

// ---------------------------------------------------------------- ① 类型契约

test('TaskPageQuery 包含可选布尔 root_only（与后端 TaskPageQuery.root_only 同名）', () => {
  const source = readFileSync(TASK_API, 'utf8');
  const block = slice(source, 'export interface TaskPageQuery {', '\n}\n');
  assert.match(block, /root_only\?:\s*boolean;/, `TaskPageQuery 里必须有 root_only?: boolean：\n${block}`);
  // 位置：在 keyword 之后、接口体内（防止被写到别的接口里）
  assert.ok(block.indexOf('keyword?: string;') < block.indexOf('root_only?: boolean;'), 'root_only 应排在既有筛选字段之后');
  // pageTasks 的入参就是它（调用方无需第二次声明类型）
  assert.match(source, /export function pageTasks\(query: TaskPageQuery\)/, 'pageTasks 必须直接吃 TaskPageQuery');
});

// ---------------------------------------------------------------- ② 列表视图默认启用

test('列表视图 loadList 的 pageTasks 调用带 root_only: true，且全页只出现一次', () => {
  const source = readFileSync(TASK_VIEW, 'utf8');
  const loadList = slice(source, 'async function loadList', '\nfunction handleSearch');
  const call = slice(loadList, 'const data = await pageTasks({', '});');
  assert.match(call, /root_only:\s*true,/, `列表视图必须只取顶层任务：\n${call}`);
  // 「我的任务」分支走 listMyTasks，不该被这段影响（同一函数里两个分支）
  assert.match(loadList, /listMyTasks\(/, '我的任务分支应保持原样（它不分页、不传 root_only）');
  const occurrences = source.match(/root_only/g) ?? [];
  assert.equal(occurrences.length, 1, `index.vue 里 root_only 只该出现 1 处（列表视图），实际 ${occurrences.length}`);
});

// ---------------------------------------------------------------- ③ 其它视图不受影响

test('看板 / 燃尽图 / 甘特图 / 父任务选择器都不传 root_only（它们需要全部任务）', () => {
  const files = walk(TASK_VIEWS_DIR).filter((file) => !file.endsWith(join('task', 'index.vue')) && !file.endsWith('task-api.ts'));
  const offenders = files.filter((file) => /root_only/.test(readFileSync(file, 'utf8')));
  assert.deepEqual(offenders, [], `这些文件不该出现 root_only：${JSON.stringify(offenders)}`);
  // 反向对照：这些文件里确实有 pageTasks 调用（否则上面的断言是真空通过）
  const board = readFileSync(join(TASK_VIEWS_DIR, 'components/TaskBoard.vue'), 'utf8');
  assert.match(board, /pageTasks\(/, 'TaskBoard 仍应调用 pageTasks（只是不传 root_only）');
  assert.ok(readdirSync(TASK_VIEWS_DIR).includes('gantt.vue'), '甘特图文件应存在');
});

// ---------------------------------------------------------------- ④ 跨层对齐（参数名 / 片段 / 归一化）

test('后端 dto / model / handler 三处与前端契约一致', () => {
  const dto = readFileSync(join(REPO, 'backend/src/business/task/dto.rs'), 'utf8');
  const dtoBlock = slice(dto, 'pub struct TaskPageQuery {', '\n}\n');
  assert.match(dtoBlock, /pub root_only: Option<bool>,/, 'dto.rs 的 TaskPageQuery 必须有 root_only');

  // TD-2 拆分后 `TaskFilter` 在 `task_model_*.rs` 部件里（`model.rs` 只剩 facade 重导出），
  // 所以这里按「model.rs + 所有 task_model_*.rs 部件」拼起来读 —— 再拆一次也不会假红。
  const modelPath = join(REPO, 'backend/src/business/task/model.rs');
  const taskDir = join(REPO, 'backend/src/business/task');
  const model = [
    readFileSync(modelPath, 'utf8'),
    ...readdirSync(taskDir)
      .filter((name: string) => name.startsWith('task_model_') && name.endsWith('.rs'))
      .sort()
      .map((name: string) => readFileSync(join(taskDir, name), 'utf8')),
  ].join('\n');
  assert.match(model, /pub root_only: bool,/, 'model.rs / task_model_*.rs 的 TaskFilter 必须有归一化后的 root_only');
  assert.match(model, /if filter\.root_only \{[\s\S]{0,400}?AND t\.parent_id IS NULL/, 'root_only 必须落成无参数片段 AND t.parent_id IS NULL');

  // P2-1A 拆分后 handler 散了：`handler.rs` 只剩 facade（127 行重导出），接口体与 DTO 归一化
  // 都搬进 `task_handler_*.rs` 部件（本条断言的目标在 `task_handler_crud.rs`）。
  // 所以和上面 model 段同一口径：按「handler.rs + 所有 task_handler_*.rs 部件」拼起来读 ——
  // 再拆一次也不会假红，且没有删断言 / 放宽成模糊匹配。
  const handler = [
    readFileSync(join(taskDir, 'handler.rs'), 'utf8'),
    ...readdirSync(taskDir)
      .filter((name: string) => name.startsWith('task_handler_') && name.endsWith('.rs'))
      .sort()
      .map((name: string) => readFileSync(join(taskDir, name), 'utf8')),
  ].join('\n');
  assert.match(handler, /root_only: query\.root_only\.unwrap_or\(false\)/, 'handler 必须把 DTO 归一化成 bool（缺省 false）');
});
