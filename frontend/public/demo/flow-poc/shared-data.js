/**
 * POC-FLOW · 三个画布方案共用的唯一数据源
 * ------------------------------------------------------------
 * 三个 demo（自研 / Vue Flow / LogicFlow）都 import 这一个文件，
 * 节点坐标、连线、标签文字、卡片规格全部从这里取 —— 否则对比没有意义。
 *
 * 数据本身照抄卡片给定的云枢「采购订单审批（标准·金额分级）」真实流程。
 */

export const flowData = {
  name: '采购订单审批（标准·金额分级）',
  nodes: [
    { id: 's',      type: 'START',        name: '开始',           x: 80,   y: 200 },
    { id: 'c_amt',  type: 'CONDITION',    name: '金额分级',       x: 260,  y: 200 },
    { id: 'n_dept', type: 'COUNTERSIGN',  name: '部门主管审批',   x: 480,  y: 80  },
    { id: 'n_fin',  type: 'APPROVAL',     name: '财务复核',       x: 480,  y: 320 },
    { id: 'n_gm',   type: 'APPROVAL',     name: '总经理审批',     x: 700,  y: 320 },
    { id: 'e',      type: 'END',          name: '结束',           x: 920,  y: 200 },
  ],
  edges: [
    { from: 's',     to: 'c_amt' },
    { from: 'c_amt', to: 'n_dept', label: '差异金额 < 10000（单级审批）' },
    { from: 'c_amt', to: 'n_fin',  label: '其他情况' },
    { from: 'n_dept', to: 'e' },
    { from: 'n_fin', to: 'n_gm' },
    { from: 'n_gm',  to: 'e' },
  ],
};

/** 节点卡片规格 —— 与现有 FlowCanvas 完全一致，三个 demo 必须都用这一份 */
export const NODE = { w: 150, h: 64, r: 11, barW: 3.5, iconSize: 30, iconR: 9 };

/** 节点类型 → 图标 / 色 / 副标题（图标用文字符号，避免图标库差异） */
export const NODE_TYPES = {
  START:       { icon: '▶', color: '#2BA471', label: '开始',   sub: '流程起点' },
  CONDITION:   { icon: '◇', color: '#E8A33D', label: '条件',   sub: '条件分支' },
  APPROVAL:    { icon: '✓', color: '#51697C', label: '审批',   sub: '单人审批' },
  COUNTERSIGN: { icon: '⧉', color: '#7B6CC4', label: '会签',   sub: '会签 · 2 人' },
  END:         { icon: '■', color: '#8A8F99', label: '结束',   sub: '流程终点' },
};

/** 云枢品牌色（与 shared-style.css 的 :root 变量同值，SVG 里要直接取值时用） */
export const C = {
  brand: '#51697C',
  brandLight: '#AAB7C2',
  bg: '#F2F2EE',
  card: '#FFFFFF',
  border: '#E0E4E8',
  text: '#1A1A1A',
  textSub: '#6B7785',
  dot: '#DADFE4',
};

export const FONT_STACK =
  '-apple-system, BlinkMacSystemFont, "PingFang SC", "Microsoft YaHei", system-ui, sans-serif';

const byId = Object.fromEntries(flowData.nodes.map((n) => [n.id, n]));
export const nodeById = (id) => byId[id];

/** 节点矩形（左上角坐标，与 flowData 里的 x/y 一致） */
export function nodeRect(id) {
  const n = byId[id];
  return { x: n.x, y: n.y, w: NODE.w, h: NODE.h };
}

/** 出边锚点：右侧边中点 */
export function anchorOut(id) {
  const n = byId[id];
  return { x: n.x + NODE.w, y: n.y + NODE.h / 2 };
}

/** 入边锚点：左侧边中点 */
export function anchorIn(id) {
  const n = byId[id];
  return { x: n.x, y: n.y + NODE.h / 2 };
}

/** 水平三次贝塞尔的水平分量（横向间距的一半，最少 40px，保证回头弯也好看） */
function ctrl(sx, tx) {
  return Math.max(40, (tx - sx) * 0.5);
}

/**
 * 三点贝塞尔路径：给「起止点」出 d。
 * 三个库都用这一个函数生成 path —— 这是三个 demo 连线一模一样的关键。
 */
export function bezierPath(sx, sy, tx, ty) {
  const k = ctrl(sx, tx);
  return `M ${sx} ${sy} C ${sx + k} ${sy}, ${tx - k} ${ty}, ${tx} ${ty}`;
}

/** 曲线中点（t=0.5）—— 标签就锚在这里 */
export function bezierMid(sx, sy, tx, ty) {
  const k = ctrl(sx, tx);
  const p0 = [sx, sy], p1 = [sx + k, sy], p2 = [tx - k, ty], p3 = [tx, ty];
  const f = (a, b, c, d) => (a + 3 * b + 3 * c + d) / 8;
  return { x: f(p0[0], p1[0], p2[0], p3[0]), y: f(p0[1], p1[1], p2[1], p3[1]) };
}

/* ---------- 标签排版：SVG 里没有 text-overflow，自己量宽 + 折行 ---------- */
const SVGNS = 'http://www.w3.org/2000/svg';
let _probeText = null;

/**
 * 量文字宽度：**用 SVG <text> 的 getBBox 量**，不用 canvas.measureText。
 * canvas 的字体回落和 SVG 实际渲染字体不一定一致（实测差值 ~20%，标签框会包不住字），
 * getBBox 量到的就是浏览器真正要画的宽度，三个库也都吃同一份数字。
 */
export function measure(text, fontSize = 11) {
  if (typeof document === 'undefined' || !document.body) return text.length * fontSize;
  if (!_probeText) {
    const svg = document.createElementNS(SVGNS, 'svg');
    svg.setAttribute('width', '0');
    svg.setAttribute('height', '0');
    svg.style.cssText = 'position:absolute;left:-9999px;top:0;opacity:0;pointer-events:none';
    const t = document.createElementNS(SVGNS, 'text');
    t.setAttribute('font-family', FONT_STACK);
    svg.appendChild(t);
    document.body.appendChild(svg);
    _probeText = t;
  }
  _probeText.setAttribute('font-size', String(fontSize));
  _probeText.textContent = text;
  return _probeText.getBBox().width;
}

/**
 * 条件标签折行：优先在「（」处断行（中文标签最自然的断点），否则按字符宽度硬断。
 * 三栏并排时横向空间只有 70px，长标签必须折成两行才不压住节点。
 */
export function wrapLabel(text, maxW = 104) {
  if (measure(text) <= maxW) return [text];
  const paren = text.indexOf('（');
  if (paren > 0) {
    const head = text.slice(0, paren);
    const tail = text.slice(paren);
    if (measure(head) <= maxW + 12 && measure(tail) <= maxW + 12) return [head, tail];
  }
  const lines = [];
  let buf = '';
  for (const ch of text) {
    if (measure(buf + ch) > maxW && buf) {
      lines.push(buf);
      buf = ch;
    } else {
      buf += ch;
    }
  }
  if (buf) lines.push(buf);
  return lines;
}

// 11px 中文字形的 bbox 高 16px（上升 12.8 / 下探 3.2）—— 行高按 16 排，文字才不会贴着标签边框
const LABEL_LINE_H = 16;
const LABEL_PAD_X = 5;
const LABEL_PAD_Y = 4;
const LABEL_BASELINE_LEAD = 12.8;

/**
 * 一条边的完整几何：路径 d + 标签盒（x/y 是盒子左上角）+ 每行文字的基线坐标。
 * native / Vue Flow / LogicFlow 三个库都直接消费这个结果 → 像素级一致。
 */
export function edgeGeometry(edge, rectOf) {
  const box = (id) => (rectOf ? rectOf(id) : nodeRect(id));
  const a = { x: box(edge.from).x + NODE.w, y: box(edge.from).y + NODE.h / 2 };
  const b = { x: box(edge.to).x, y: box(edge.to).y + NODE.h / 2 };
  const d = bezierPath(a.x, a.y, b.x, b.y);
  const geo = { id: `${edge.from}->${edge.to}`, d, sx: a.x, sy: a.y, tx: b.x, ty: b.y, label: null };
  if (!edge.label) return geo;
  geo.label = labelLayout(a.x, a.y, b.x, b.y, edge.label);
  return geo;
}

/**
 * 标签排版：给定曲线起止点 → 盒子尺寸 + 每行基线。
 * Vue Flow / LogicFlow 的边组件只拿得到起止点，也调这一个函数，
 * 所以三个库的标签位置像素级一致。
 */
export function labelLayout(sx, sy, tx, ty, text) {
  const lines = wrapLabel(text);
  const w = Math.max(...lines.map((l) => measure(l))) + LABEL_PAD_X * 2;
  const h = lines.length * LABEL_LINE_H + LABEL_PAD_Y * 2;
  const mid = bezierMid(sx, sy, tx, ty);
  const x = mid.x - w / 2;
  const y = mid.y - h / 2;
  return {
    x, y, w, h, lines,
    // 文字基线：行首留 PAD_Y，之后每行一个行高，再抬 12.8（11px 中文字形的上升高度）
    baselineY: lines.map((_, i) => y + LABEL_PAD_Y + i * LABEL_LINE_H + LABEL_BASELINE_LEAD),
    cx: mid.x,
  };
}

/* ---------- 视口：三个 demo 用同一套 fit 算法 → 同样的画布尺寸下缩放比一致 ---------- */
export function contentBounds() {
  const xs = flowData.nodes.map((n) => n.x);
  const ys = flowData.nodes.map((n) => n.y);
  const x = Math.min(...xs);
  const y = Math.min(...ys);
  return { x, y, w: Math.max(...xs) + NODE.w - x, h: Math.max(...ys) + NODE.h - y };
}

export function fitViewport(viewW, viewH, pad = 22) {
  const b = contentBounds();
  const s = Math.min((viewW - pad * 2) / b.w, (viewH - pad * 2) / b.h, 1);
  return {
    s,
    x: (viewW - b.w * s) / 2 - b.x * s,
    y: (viewH - b.h * s) / 2 - b.y * s,
  };
}
