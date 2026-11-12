// 引擎改动等价性验证：HEAD 版 palettes.ts 与当前版，对现有 22 套 × 明暗 2 模式
// 逐变量比对（用于证明 ensureContrast / 渐变终点油墨两处兜底修复对既有方案零影响）。
// 依赖 Node >= 22.18 的原生 TS 类型擦除（云枢 22.23 满足；不再需要 typescript 依赖）。
import { execSync } from 'node:child_process';
import { mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const repo = '/opt/cloudpivot';
const head = execSync(`git -C ${repo} show HEAD:frontend/src/style/palettes.ts`, { encoding: 'utf8' });
const current = readFileSync(`${repo}/frontend/src/style/palettes.ts`, 'utf8');

async function loadFromSource(source, tag) {
  const dir = mkdtempSync(join(tmpdir(), `pal-${tag}-`));
  const out = join(dir, `palettes-${tag}.ts`);
  writeFileSync(out, source, 'utf8');
  return import(pathToFileURL(out).href);
}

const before = await loadFromSource(head, 'head');
const after = await loadFromSource(current, 'cur');

const ids = before.PALETTES.map((p) => p.id);
const diffs = [];
for (const id of ids) {
  for (const mode of ['light', 'dark']) {
    const a = before.buildPalette(id, mode);
    const b = after.buildPalette(id, mode);
    for (const key of Object.keys(a.vars)) {
      if (a.vars[key] !== b.vars[key]) {
        diffs.push(`${id}/${mode} ${key}: ${a.vars[key]} → ${b.vars[key]}`);
      }
    }
    if (JSON.stringify(a.checks) !== JSON.stringify(b.checks)) diffs.push(`${id}/${mode} checks 差异`);
  }
}

console.log(`HEAD 方案数 ${ids.length}；对比变量 ${ids.length * 2} 组 × ${Object.keys(before.buildPalette(ids[0], 'light').vars).length} 个变量`);
console.log(diffs.length === 0 ? '✓ 既有 22 套 × 明暗 2 模式的全部变量与对比度检查逐字节一致' : `✗ 差异 ${diffs.length} 处：\n${diffs.slice(0, 20).join('\n')}`);
process.exit(diffs.length === 0 ? 0 : 1);
