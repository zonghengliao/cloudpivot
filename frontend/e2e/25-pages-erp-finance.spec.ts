import { definePageChecks } from './_pagerunner';
import { pages } from './_catalog';

/**
 * ERP 财务 4 页：记账凭证 / 总账 / 会计科目 / 会计期间。
 * 「会计科目 404」是老板报过的原话，这页在这里真开真点。
 */
definePageChecks(pages(['记账凭证', '总账', '会计科目', '会计期间']));
