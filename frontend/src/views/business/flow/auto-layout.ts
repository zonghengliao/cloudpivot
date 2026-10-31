/**
 * dagre 自动布局（P12）。
 *
 * 纯算法层：**只算坐标，不碰任何节点数据**。调用方（流程编辑器）决定怎么应用 ——
 * 目前是包成一条 `layout` 命令压进撤销栈，因此「自动布局」天然可撤销。
 *
 * # 为什么另起一个文件，不写进 `graph.ts`
 *
 * `graph.ts` 是坐标口径的唯一来源（节点尺寸 / 锚点 / 正交折线），630 行、已被
 * 29 个单测与页面共同依赖；dagre 属于「布局策略」，换策略不该动坐标口径。
 * 这里只 import 尺寸常量的调用方传进来，保持零耦合。
 *
 * # 方向为什么默认 LR
 *
 * 本画布的锚点是**固定左进右出**（`inPort` 在左、`outPort` 在右），
 * 树状布局若用 TB（上→下），连线的起终点仍会从右侧出来再绕回下一个节点的左侧，
 * 观感是「横着走的树」。所以编辑器里调用时传 `'LR'`；函数默认值仍按接口约定给 `'TB'`。
 */
import dagre from 'dagre';

/** 参与布局的节点（只需要 id 和尺寸） */
export interface LayoutNode {
  /** 节点 key */
  id: string;
  /** 宽（画布逻辑像素） */
  width: number;
  /** 高 */
  height: number;
}

/** 参与布局的边 */
export interface LayoutEdge {
  /** 起点 key */
  source: string;
  /** 终点 key */
  target: string;
}

/** 布局方向 */
export type LayoutDirection = 'TB' | 'LR';

/** 一层内的节点间距 / 层间距（与画布 10 px 网格对齐的观感值） */
const NODE_SEP = 46;
const RANK_SEP = 72;
const MARGIN = 40;

/**
 * 用 dagre 算出一张整齐的树状布局。
 *
 * 只返回**左上角坐标**（dagre 给的是中心点，这里减掉半个宽高并取整），
 * 不改任何节点数据 —— 调用方决定怎么应用。
 *
 * # 参数
 * - `nodes`：节点（id + 宽高）
 * - `edges`：边（两端 key 必须都在 `nodes` 里，否则忽略该边）
 * - `dir`：布局方向，默认 `TB`
 *
 * # 返回值
 * `key → { x, y }`；空输入返回空 Map。
 */
export function computeAutoLayout(
  nodes: LayoutNode[],
  edges: LayoutEdge[],
  dir: LayoutDirection = 'TB',
): Map<string, { x: number; y: number }> {
  const out = new Map<string, { x: number; y: number }>();
  if (nodes.length === 0) {
    return out;
  }

  const g = new dagre.graphlib.Graph({ multigraph: true });
  g.setGraph({
    rankdir: dir,
    nodesep: NODE_SEP,
    ranksep: RANK_SEP,
    marginx: MARGIN,
    marginy: MARGIN,
  });
  g.setDefaultEdgeLabel(() => ({}));

  for (const n of nodes) {
    g.setNode(n.id, { width: n.width, height: n.height });
  }
  let edgeIndex = 0;
  for (const e of edges) {
    // 端点缺失的脏边直接跳过：dagre 会为不存在的端点凭空建节点，把布局算歪
    if (!g.hasNode(e.source) || !g.hasNode(e.target)) {
      continue;
    }
    // 自环边（A→A）在层次布局里没有意义，还给 dagre 反而会撑出一整层
    if (e.source === e.target) {
      continue;
    }
    // multigraph + 唯一 name：允许 A→B 多条边（工作流里的条件分支可能同向多条）
    g.setEdge(e.source, e.target, {}, `e${edgeIndex}`);
    edgeIndex += 1;
  }

  dagre.layout(g);

  for (const n of nodes) {
    const gn = g.node(n.id);
    if (!gn) {
      continue;
    }
    out.set(n.id, {
      x: Math.round(gn.x - n.width / 2),
      y: Math.round(gn.y - n.height / 2),
    });
  }
  return out;
}
