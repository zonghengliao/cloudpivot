/// <reference types="node" />
/**
 * markTitle / markWarehouse / markFileName 的运行时自测（不连库、不起实例）
 * 跑：cd frontend && ./node_modules/.bin/tsx e2e/e2e-marker.selftest.ts
 */
import { E2E_MARK, markTitle, markWarehouse, markFileName, isMarkedTitle } from './e2e-marker';

let bad = 0;
function eq(got: unknown, want: unknown, what: string) {
  const ok = got === want;
  if (!ok) bad += 1;
  console.log(`[${ok ? 'ok  ' : 'FAIL'}] ${what}  got=${JSON.stringify(got)} want=${JSON.stringify(want)}`);
}

// 幂等：已带标记的不重复加（用例手写的 [E2E]xxx 与忘记前缀的标题走同一条路）
eq(markTitle('[E2E]详情-e2emvvvdf01'), '[E2E]详情-e2emvvvdf01', '已带标记原样返回');
eq(markTitle('拖拽-正常-e2e123'), `${E2E_MARK}拖拽-正常-e2e123`, '没带标记自动加前缀');
eq(markTitle('  [E2E]前后空格  '), '[E2E]前后空格', '去首尾空格后不重复加');
eq(markTitle(''), E2E_MARK, '空标题也有标记（后端会拒，但不会留下未标记行）');
eq(isMarkedTitle(markTitle('详情')), true, 'isMarkedTitle(markTitle(x)) 恒真');

// 仓库 / 文件名同口径
eq(markWarehouse('E2E-W4A-1'), 'E2E-W4A-1', '仓库已带前缀原样返回');
eq(markWarehouse('W4A-1'), 'E2E-W4A-1', '仓库自动加前缀');
eq(markFileName('e2e-a.txt'), 'e2e-a.txt', '文件名已带前缀原样返回');
eq(markFileName('a.txt'), 'e2e-a.txt', '文件名自动加前缀');

// 与 python 侧判据同口径：这些标题 python 侧必须命中
const must = ['[E2E]详情-e2emvvvdf01', markTitle('随便什么')];
for (const t of must) {
  eq(t.startsWith('[E2E]'), true, `python 侧谓词 title like '[E2E]%' 命中：${t}`);
}

console.log(`---- e2e-marker selftest：${bad === 0 ? 'PASS' : `FAIL ${bad} 项`} ----`);
process.exit(bad === 0 ? 0 : 1);
