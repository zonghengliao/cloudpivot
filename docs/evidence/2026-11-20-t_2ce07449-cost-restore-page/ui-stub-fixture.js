/**
 * 成本还原页 UI 冒烟用的 **接口桩**（只在浏览器里跑，用于验证渲染；不改任何代码）。
 *
 * 用途：空库首启的实例里没有成本数据，为了验证「有数据时」的渲染
 * （汇总卡片 / 明细行 / 展开的逐层台账与半成品明细 / 报表分组），
 * 在 document 起点把打向 cost-restore-rollup / cost-restore-report 的 XHR 改写成本文件的夹具。
 * 其它请求照旧走真实后端。
 */
(function () {
  // ---------- 错误收集（用于冒烟时检查前端运行时异常）----------
  window.__errs = [];
  window.addEventListener('error', function (e) {
    window.__errs.push(String((e && (e.message || e.error)) || 'error'));
  });
  window.addEventListener('unhandledrejection', function (e) {
    var r = e && e.reason;
    window.__errs.push('rejection: ' + String((r && (r.message || r)) || 'unknown'));
  });

  // ---------- 夹具（与后端 DTO 字段一一对应）----------
  var ROLLUP = {
    period_code: '2026-11',
    batch: { id: 'b1', batch_no: 'CS-202611-0001', status: 'SETTLED' },
    cost_ready: true,
    state: 'all',
    keyword: null,
    max_depth: 6,
    formula_restore:
      '还原 = 材料里的自制半成品按其所产批次的完工成本结构（料/工/费）递归拆开，拆出来的「料」若仍是半成品就继续拆，直到全是外购材料；每一步 材料 + 人工 + 制造费用 = 流入金额（尾差进制造费用），总额不变',
    formula_unresolved:
      '还原不彻底 = 停在半成品形态、无法继续拆的金额（产出批次没计算 / 已红冲 / 结构闭环 / 到层数上限 / 没有下一层投入）；它既不算进外购材料，也不丢掉：还原后合计 + 还原不彻底 = 还原前合计',
    totals: {
      row_count: 2,
      restored_row_count: 1,
      partial_row_count: 1,
      no_semi_row_count: 0,
      before_material: '1360',
      before_labor: '140',
      before_overhead: '140',
      before_total: '1640',
      after_material: '516',
      after_labor: '412',
      after_overhead: '512',
      after_total: '1440',
      unresolved: '200',
      semi_in_material: '960',
      check: '0',
      max_depth: 3,
      node_count: 11,
      after_material_share: '35.83',
      after_labor_share: '28.61',
      after_overhead_share: '35.56',
      note: '合计是整张表的（不受分页影响）；「还原后合计 + 还原不彻底 = 还原前合计」恒成立（check 应为 0）。',
    },
    list: [
      {
        object_id: 'obj-f',
        object_no: 'CB-M530-F-0001',
        object_type: 'BATCH',
        lot_no: 'LOT-M530-0001',
        product_id: 'prod-f',
        product_code: 'F',
        product_name: '成品 F',
        workshop_dept_id: 'dept-1',
        workshop_dept_name: '总装车间',
        item: {
          status: 'SETTLED',
          completed_qty: '40',
          finished_cost: '1400',
          unit_finished_cost: '35',
        },
        finished_cost: '1400',
        before: { material: '1000', labor: '100', overhead: '100', total: '1200' },
        semi_in_material: '600',
        purchased_in_material: '400',
        semi_share: '60',
        after: { material: '300', labor: '300', overhead: '400', total: '1000' },
        after_material_share: '30',
        after_labor_share: '30',
        after_overhead_share: '40',
        delta: { material: '-700', labor: '200', overhead: '300', total: '0', note: '还原只是把钱换成形态，总额一分不动' },
        restored_labor: '200',
        restored_overhead: '300',
        unresolved: '200',
        state: 'partial',
        state_label: '还原不彻底',
        depth: 2,
        node_count: 6,
        flows: [
          {
            level: 1,
            node_count: 4,
            imputed: '1000',
            purchased: '400',
            labor: '200',
            overhead: '200',
            carried_down: '200',
            unresolved: '0',
            note: '第 1 层 = 本期材料投入的去向：外购料确认、半成品按结构拆出工/费，其余继续往下',
          },
          {
            level: 2,
            node_count: 1,
            imputed: '200',
            purchased: '0',
            labor: '60',
            overhead: '60',
            carried_down: '0',
            unresolved: '80',
            note: '第 2 层 = 上一层「材料部分」往下拨的 200（不是新增投入，不能与上层相加）',
          },
        ],
        semis: [
          {
            level: 1,
            material_id: 'mat-s2',
            material_code: 'S2',
            material_name: '半成品 S2',
            lot_no: 'LOT-S2-0001',
            imputed: '600',
            source: 'batch',
            source_label: '按批次',
            producer_nos: 'CB-M530-S2-0001',
            elements: { material: '360', labor: '96', overhead: '144', total: '600' },
            unresolved: '0',
            stopped: false,
          },
          {
            level: 2,
            material_id: 'mat-s3',
            material_code: 'S3',
            material_name: '二级半成品 S3',
            lot_no: 'LOT-S3-0001',
            imputed: '200',
            source: null,
            source_label: null,
            producer_nos: null,
            elements: { material: '0', labor: '60', overhead: '60', total: '120' },
            unresolved: '80',
            stopped: true,
          },
        ],
        issues: [
          {
            code: 'NO_STRUCTURE',
            message: '半成品 S3（批号 LOT-S3-0001）取不到完工成本结构：产出批次本期没算成本',
            fix: '到「成本核算」页把产出批次算出来，或核对领料批号与产出批号',
          },
        ],
        warnings: [],
      },
      {
        object_id: 'obj-s2',
        object_no: 'CB-M530-S2-0001',
        object_type: 'BATCH',
        lot_no: 'LOT-S2-0001',
        product_id: 'prod-s2',
        product_code: 'S2',
        product_name: '半成品 S2',
        workshop_dept_id: 'dept-2',
        workshop_dept_name: '冲压车间',
        item: null,
        finished_cost: null,
        before: { material: '360', labor: '40', overhead: '40', total: '440' },
        semi_in_material: '360',
        purchased_in_material: '0',
        semi_share: '100',
        after: { material: '216', labor: '112', overhead: '112', total: '440' },
        after_material_share: '49.09',
        after_labor_share: '25.45',
        after_overhead_share: '25.45',
        delta: { material: '-144', labor: '72', overhead: '72', total: '0', note: '还原只是把钱换成形态，总额一分不动' },
        restored_labor: '72',
        restored_overhead: '72',
        unresolved: '0',
        state: 'restored',
        state_label: '已还原',
        depth: 3,
        node_count: 5,
        flows: [
          {
            level: 1,
            node_count: 2,
            imputed: '360',
            purchased: '0',
            labor: '144',
            overhead: '144',
            carried_down: '72',
            unresolved: '0',
            note: '第 1 层 = 本期材料投入的去向',
          },
        ],
        semis: [
          {
            level: 2,
            material_id: 'mat-s4',
            material_code: 'S4',
            material_name: '三级半成品 S4',
            lot_no: 'LOT-S4-0001',
            imputed: '360',
            source: 'batch',
            source_label: '按批次',
            producer_nos: 'CB-M530-S4-0001',
            elements: { material: '216', labor: '72', overhead: '72', total: '360' },
            unresolved: '0',
            stopped: false,
          },
        ],
        issues: [],
        warnings: ['本行的半成品链到 3 层已到底（下面全是外购料）'],
      },
    ],
    total: 2,
    page: 1,
    page_size: 20,
    state_options: [
      { value: 'all', label: '全部' },
      { value: 'restored', label: '已还原' },
      { value: 'partial', label: '不彻底' },
      { value: 'no_semi', label: '无需还原' },
    ],
    note: '还原不彻底的行请展开看逐层台账与半成品明细。',
  };

  function group(key, label, hint, objectCount, before, restored, shift, shareSum, extra) {
    return Object.assign(
      {
        key: key,
        label: label,
        hint: hint,
        object_count: objectCount,
        before: before,
        restored: restored,
        shift: shift,
        restored_labor: '200',
        restored_overhead: '300',
        semi_in_material: '960',
        semi_share: '70.59',
        restored_rows: 1,
        partial_rows: 1,
        no_semi_rows: 0,
        dominant: 'material',
        dominant_label: '材料最高',
        share_sum: shareSum,
        check: '0',
      },
      extra || {},
    );
  }

  var REPORT = {
    period_code: '2026-11',
    batch: { id: 'b1', batch_no: 'CS-202611-0001', status: 'SETTLED' },
    cost_ready: true,
    group_by: 'dept',
    group_by_label: '按车间',
    group_options: [
      { value: 'dept', label: '按车间' },
      { value: 'product', label: '按产品' },
      { value: 'object', label: '按对象' },
      { value: 'all', label: '整表' },
    ],
    state: 'all',
    state_label: '全部',
    keyword: null,
    max_depth: 6,
    basis: '占比分母 = 还原前合计（= 还原后三要素合计 + 还原不彻底）：材料 + 人工 + 制造费用 + 还原不彻底 四项合计 100.00%',
    formula_restore: '还原 = 材料里的自制半成品按结构递归拆开',
    formula_unresolved: '还原不彻底 = 停在半成品形态的金额',
    formula_share:
      '占比 = 该要素金额 ÷ 还原前合计 × 100（2 位）；还原后三要素 + 还原不彻底 = 还原前合计 ⇒ 四项占比合计 100.00%（各自四舍五入，允许 ±0.01 舍入差，见 share_sum）',
    formula_shift: '占比变化（pp）= 还原后占比 − 还原前占比；材料减少的百分点 = 人工 + 制造费用增加的百分点（+ 不彻底的百分点），三项合计 = − 不彻底占比',
    totals: group(
      'ALL',
      '合计',
      null,
      2,
      { material: '1360', labor: '140', overhead: '140', total: '1640', material_share: '82.93', labor_share: '8.54', overhead_share: '8.54' },
      {
        material: '516',
        labor: '412',
        overhead: '512',
        total: '1440',
        unresolved: '200',
        denominator: '1640',
        material_share: '31.46',
        labor_share: '25.12',
        overhead_share: '31.22',
        unresolved_share: '12.2',
      },
      { material_pp: '-51.47', labor_pp: '16.58', overhead_pp: '22.68', note: '占比变化（pp）= 还原后 − 还原前' },
      '100.00',
    ),
    groups: [
      group(
        'dept-1',
        '总装车间',
        null,
        1,
        { material: '1000', labor: '100', overhead: '100', total: '1200', material_share: '83.33', labor_share: '8.33', overhead_share: '8.33' },
        {
          material: '300',
          labor: '300',
          overhead: '400',
          total: '1000',
          unresolved: '200',
          denominator: '1200',
          material_share: '25',
          labor_share: '25',
          overhead_share: '33.33',
          unresolved_share: '16.68',
        },
        { material_pp: '-58.33', labor_pp: '16.67', overhead_pp: '25', note: '占比变化（pp）= 还原后 − 还原前' },
        '100.01',
      ),
      group(
        'dept-2',
        '冲压车间',
        null,
        1,
        { material: '360', labor: '40', overhead: '40', total: '440', material_share: '81.82', labor_share: '9.09', overhead_share: '9.09' },
        {
          material: '216',
          labor: '112',
          overhead: '112',
          total: '440',
          unresolved: '0',
          denominator: '440',
          material_share: '49.09',
          labor_share: '25.45',
          overhead_share: '25.45',
          unresolved_share: '0',
        },
        { material_pp: '-32.73', labor_pp: '16.36', overhead_pp: '16.36', note: '占比变化（pp）= 还原后 − 还原前' },
        '100.00',
        { partial_rows: 0, restored_rows: 1, dominant: 'material', dominant_label: '材料最高' },
      ),
    ],
    total: 2,
    page: 1,
    page_size: 50,
    state_options: [
      { value: 'all', label: '全部' },
      { value: 'restored', label: '已还原' },
      { value: 'partial', label: '不彻底' },
      { value: 'no_semi', label: '无需还原' },
    ],
    note: '报表是纯汇总：每一组金额 = 明细表对应行金额之和。',
  };

  // ---------- XHR 拦截 ----------
  var origOpen = XMLHttpRequest.prototype.open;
  var origSend = XMLHttpRequest.prototype.send;

  XMLHttpRequest.prototype.open = function (method, url) {
    this.__cr_url = String(url || '');
    return origOpen.apply(this, arguments);
  };

  XMLHttpRequest.prototype.send = function () {
    var url = this.__cr_url || '';
    var hit = /cost-restore-(rollup|report)/.test(url);
    if (!hit) {
      return origSend.apply(this, arguments);
    }
    var payload = /cost-restore-report/.test(url) ? REPORT : ROLLUP;
    var body = JSON.stringify({ code: 0, msg: 'ok', data: payload });
    var self = this;
    setTimeout(function () {
      Object.defineProperty(self, 'readyState', { value: 4, configurable: true });
      Object.defineProperty(self, 'status', { value: 200, configurable: true });
      Object.defineProperty(self, 'statusText', { value: 'OK', configurable: true });
      Object.defineProperty(self, 'responseText', { value: body, configurable: true });
      Object.defineProperty(self, 'response', { value: body, configurable: true });
      self.getAllResponseHeaders = function () {
        return 'content-type: application/json\r\n';
      };
      self.getResponseHeader = function (name) {
        return String(name).toLowerCase() === 'content-type' ? 'application/json' : null;
      };
      if (typeof self.onreadystatechange === 'function') {
        self.onreadystatechange();
      }
      if (typeof self.onload === 'function') {
        self.onload();
      }
      if (typeof self.onloadend === 'function') {
        self.onloadend();
      }
      try {
        console.log('[cr-stub] 已拦截 ' + url.replace(location.origin, ''));
      } catch (e) {}
    }, 60);
  };
})();
