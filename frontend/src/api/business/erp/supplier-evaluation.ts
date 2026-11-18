/**
 * 供应商评估 API 层（后端 `backend/app/src/business/erp/purchase/supplier_evaluation_{dto,handler}.rs`）。
 *
 * # 接口与权限码（V0457 的 3 行菜单 …4495–…4497）
 *
 * | 方法 | 路径 | 权限码 |
 * |------|------|--------|
 * | GET  | `/page`                | `business:erp:supplier-evaluation:list` |
 * | GET  | `/rank`                | 同上 |
 * | GET  | `/{id}`                | 同上 |
 * | GET  | `/delivery-suggestion` | `business:erp:supplier-evaluation:save` |
 * | POST | ``（打分录入）          | `business:erp:supplier-evaluation:save` |
 *
 * 页面显形码 `business:erp:supplier-evaluation:page` **只在路由 `meta.permission`**，
 * 后端零引用（AGENTS 约定 14），本文件也不拿它判任何东西。
 *
 * # 四条渲染口径（都来自后端，别在页面里重新猜）
 *
 * 1. **总分是库层生成列**：入参里**没有** `total_score`；出参里的 `total_score` 就是库里那一列。
 *    页面要「边填边看」只能自己算预览分（`supplier-evaluation-logic.ts` 里逐位复述同一公式），
 *    且必须标成「预览」—— 保存后以后端返回值为准。
 * 2. **等级由后端按阈值写**（A≥90 / B≥80 / C≥70 / D<70，含下界）；页面不判等级，只显示 `grade`。
 * 3. **一份期间一个结论**：同 `(supplier_id, period_start, period_end)` 再 POST 一次就是
 *    **覆盖**（单号不变），返回 `created=false`。页面文案要说「重新打分」而不是「又建了一张」。
 * 4. **交期参考分只对交期维度有意义**：质量 / 价格 / 服务没有数据源（见 M4-13 README §四），
 *    `suggested_score` 是 `null` 时表示「没有可判准时的入库」，**不是 0 分**。
 *
 * # 数值类型
 *
 * `Decimal` 在 JSON 里是**字符串**（`"89.55"`）：前端一律按字符串接、按字符串显示，
 * 别 `Number()` 之后再 `toFixed` —— 那会把 0.005 的中点舍入变成浮点误差。
 *
 * 前缀 `/api` 必须写全（后端 `#[nik::get]` 注解不含 `/api`，漏了会命中 SPA fallback 返回 HTML）。
 */
import { get, post, type PageData } from '@/api/request';

/** 接口前缀（与 `supplier-access.ts` / `framework-agreement.ts` 同一常量口径） */
const BASE = '/api/business/erp/supplier-evaluation';

/** 等级闭集（与 `supplier_evaluation_model::GRADES` / `ck_biz_sev_grade` 逐字一致） */
export type EvaluationGrade = 'A' | 'B' | 'C' | 'D';

/**
 * 评估单一行（= 后端 `EvaluationRowRes`，列表 / 详情 / 排名**同一个结构**）。
 *
 * 排名行 = 本结构 + `rank`（后端用 `#[serde(flatten)]` 摊平，所以前端看到的是同一层）。
 */
export interface EvaluationRow {
  /** 主键 */
  id: string;
  /** 单号 `SEV-yyyyMMdd-NNNNNN` */
  evaluation_no: string;
  /** 供应商 id */
  supplier_id: string;
  /** 供应商名**快照**（评估结论的历史事实，约定 13） */
  supplier_name: string | null;
  /** 供应商**现名**（快照为空时回退到它） */
  supplier_display_name: string;
  /** 期间起 `YYYY-MM-DD` */
  period_start: string;
  /** 期间止 `YYYY-MM-DD` */
  period_end: string;
  /** 期间展示标签（`2026-Q1` / `2026-03` / `2026-FY` / `起~止`） */
  period_label: string;
  /** 评估日 `YYYY-MM-DD` */
  eval_date: string;
  /** 评估人 id */
  evaluator_id: string | null;
  /** 评估人姓名快照 */
  evaluator_name: string | null;
  /** 质量权重（0–1，字符串小数） */
  quality_weight: string;
  /** 交期权重 */
  delivery_weight: string;
  /** 价格权重 */
  price_weight: string;
  /** 服务权重 */
  service_weight: string;
  /** 质量得分（0–100） */
  quality_score: string;
  /** 交期得分 */
  delivery_score: string;
  /** 价格得分 */
  price_score: string;
  /** 服务得分 */
  service_score: string;
  /** 加权总分（库层生成列，只读） */
  total_score: string;
  /** 等级（`null` = 未评级） */
  grade: EvaluationGrade | null;
  /** 等级中文（未评级时是「未评级」） */
  grade_label: string;
  /** 备注 */
  remark: string | null;
  /** 创建时间（RFC3339） */
  created_at: string;
}

/** 排名行（本结构 + 名次；后端 flatten 后就是一层） */
export type RankRow = EvaluationRow & {
  /** 名次（1 起，按总分降序；总分相同按单号先后，名次不并列） */
  rank: number;
};

/**
 * 分页入参（后端 `EvaluationPageQuery`）。
 *
 * ⚠ 用 `type` 而不是 `interface`：`request()` 的 `params` 是 `Record<string, unknown>`，
 * 类型别名才有隐式索引签名（interface 赋不过去，会逼着页面到处写 as）。
 */
export type EvaluationQuery = {
  /** 页码（1 起） */
  page?: number;
  /** 每页条数（后端 clamp 到 1–200） */
  page_size?: number;
  /** 关键字：单号 / 供应商名 / 备注（后端上限 64 字符） */
  keyword?: string;
  /** 等级（闭集外的值后端 400，不静默忽略） */
  grade?: EvaluationGrade | '';
  /** 供应商 id（uuid 字符串；非法值后端 400） */
  supplier_id?: string;
  /** 期间起（与 `period_end` 成对使用：只给一个按「起点等于」筛，不是区间） */
  period_start?: string;
  /** 期间止 */
  period_end?: string;
};

/** 排名入参（后端 `RankQuery`：期间**必填**） */
export type RankQuery = {
  /** 期间起 `YYYY-MM-DD` */
  period_start: string;
  /** 期间止 `YYYY-MM-DD` */
  period_end: string;
  /** 取前 N 名（默认 100，上限 500） */
  limit?: number;
};

/** 交期参考分入参（后端 `SuggestionQuery`） */
export type SuggestionQuery = {
  /** 供应商 id */
  supplier_id: string;
  /** 期间起 */
  period_start: string;
  /** 期间止 */
  period_end: string;
};

/** 排名出参（后端 `RankRes`） */
export interface RankResult {
  /** 期间起 */
  period_start: string;
  /** 期间止 */
  period_end: string;
  /** 期间展示标签 */
  period_label: string;
  /** 参与排名的评估数 */
  total: number;
  /** 名次列表 */
  rows: RankRow[];
}

/**
 * 交期参考分出参（后端 `DeliverySuggestionRes`）。
 *
 * `suggested_score === null` = 该期间没有可判准时的采购入库 ⇒ 只能人工打分。
 * 页面**不许**把它显示成 0 分（0 分是「全部逾期」的意思）。
 */
export interface DeliverySuggestion {
  /** 供应商 id */
  supplier_id: string;
  /** 期间起 */
  period_start: string;
  /** 期间止 */
  period_end: string;
  /** 参与统计的入库单张数 */
  docs: number;
  /** 其中含逾期行的入库单张数 */
  late_docs: number;
  /** 到货总数量 */
  total_qty: string;
  /** 准时数量 */
  on_time_qty: string;
  /** 逾期数量 */
  late_qty: string;
  /** 准时率（0–100；无数据为 `null`） */
  on_time_rate: string | null;
  /** 建议交期分（= 准时率；无数据为 `null`） */
  suggested_score: string | null;
  /** 一句话口径说明 */
  note: string;
}

/**
 * 打分入参（后端 `SaveEvaluationReq`）。
 *
 * * 四维权重**要么四个都给（之和 = 1），要么都不给**（后端按四等分 0.25 落库）——
 *   给一半后端 400；
 * * 四维分数必填；
 * * `eval_date` 不给 = 北京时间当天（不许早于期间起点）；
 * * **没有** `total_score` / `grade`：前者是库层生成列，后者由后端按阈值写。
 */
export type SaveEvaluationPayload = {
  /** 供应商 id */
  supplier_id: string;
  /** 期间起 */
  period_start: string;
  /** 期间止 */
  period_end: string;
  /** 评估日（空 = 当天） */
  eval_date?: string;
  /** 质量权重（0–1） */
  quality_weight?: string;
  /** 交期权重 */
  delivery_weight?: string;
  /** 价格权重 */
  price_weight?: string;
  /** 服务权重 */
  service_weight?: string;
  /** 质量得分（0–100，必填） */
  quality_score: string;
  /** 交期得分（必填） */
  delivery_score: string;
  /** 价格得分（必填） */
  price_score: string;
  /** 服务得分（必填） */
  service_score: string;
  /** 备注 */
  remark?: string | null;
};

/** 打分结果（后端 `SaveEvaluationRes`） */
export interface SaveEvaluationResult {
  /** 落库后的整行 */
  row: EvaluationRow;
  /** `true` = 本次取号新建；`false` = 覆盖已有评估（重新打分） */
  created: boolean;
}

/** 台账分页（`GET /supplier-evaluation/page`，权限码 `…:list`） */
export function fetchEvaluationPage(query: EvaluationQuery): Promise<PageData<EvaluationRow>> {
  return get<PageData<EvaluationRow>>(`${BASE}/page`, query);
}

/** 详情（`GET /supplier-evaluation/{id}`，权限码 `…:list`；不存在 / 已删 → 404） */
export function fetchEvaluationDetail(id: string): Promise<EvaluationRow> {
  return get<EvaluationRow>(`${BASE}/${id}`);
}

/** 本期排名（`GET /supplier-evaluation/rank`，权限码 `…:list`；期间必填） */
export function fetchEvaluationRank(query: RankQuery): Promise<RankResult> {
  return get<RankResult>(`${BASE}/rank`, query);
}

/**
 * 交期参考分（`GET /supplier-evaluation/delivery-suggestion`，权限码 `…:save`）。
 *
 * 为什么挂在 `save` 码下：它是「打分表单」的一部分（评估人点「取参考分」才调），
 * 不是台账展示项；只读看板不该因为这一条多要一个写码的接口。
 */
export function fetchDeliverySuggestion(query: SuggestionQuery): Promise<DeliverySuggestion> {
  return get<DeliverySuggestion>(`${BASE}/delivery-suggestion`, query);
}

/**
 * 打分录入（`POST /supplier-evaluation`，权限码 `…:save`）。
 *
 * 同一供应商同一期间**再调一次就是覆盖**（`created=false`，单号不变）。
 */
export function saveEvaluation(payload: SaveEvaluationPayload): Promise<SaveEvaluationResult> {
  return post<SaveEvaluationResult>(BASE, payload);
}
