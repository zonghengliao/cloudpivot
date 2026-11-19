//! 成本还原「半成品成本结构表」的**接口层**（成本核算 · M5-26）。
//!
//! 前缀 `/api` 由 `[router].prefix` 注入，宏内写 `/business/erp/cost-restore-structure`（AGENTS 约定 8）。
//!
//! | # | 方法 | 路径 | 权限码 | 说明 |
//! |---|---|---|---|---|
//! | 1 | GET | `/business/erp/cost-restore-structure` | `business:erp:cost-object:list` | 期间内逐成本对象的**逐层结构**（成品 → 半成品 → 二级半成品 → 外购材料）+ 逐层汇总 |
//!
//! # 一条路由，为什么没有第二个
//!
//! 「结构」与「还原」是两件事，本卡只做前者：
//!
//! * **M5-26（本卡）**：一层的钱从哪来 —— 材料投入里哪些是外购料、哪些是自制半成品，
//!   自制半成品再按其所产批次的完工成本结构展开下一层。**不动钱的总额**。
//! * **M5-27**：把这些结构按比例卷积下去，得出「还原后的料 / 工 / 费」。
//!
//! 所以这里只有读，没有写：结构是算出来的，不是填出来的。
//!
//! # 权限码：复用 `cost-object:list`（零新增码、零迁移、零菜单）
//!
//! 与 `cost-wip-equiv`（M5-20）/ `cost-wip-alloc`（M5-22）/ `cost-wip-balance`（M5-23）同码：
//! 它们读的是同一批 `biz_cost_object`。新开 `cost-restore:*` 会带来两个问题：
//! ① 没有菜单行承载 ⇒ 死码（`permission_audit_tests::n14` 判红）；
//! ② 先插一条指向不存在组件的菜单行 ⇒ `n08` 判红。页面码 / 接口码解耦（PERM-01）只在 HR 试点。
//!
//! # 数据权限
//!
//! 行由 `biz_cost_object.workshop_dept_id` 裁剪（与其它成本表同一锚点）。
//! 看不见的对象**不出现在表里**，也不报 403。半成品产出批次不再二次裁剪，理由见取数层模块头。
//!
//! # 为什么业务逻辑是普通 `async fn`
//!
//! 与 `cost_wip_balance_handler` / `cost_wip_alloc_handler` 同款：axum handler 要 `AuthClaims`
//! 才能跑，单测调不到；把「归一化 + 逐层展开 + 筛选 + 分页 + 装配」收进 [`structure_table_of`]
//! 之后，真库用例走**同一条代码路径**（只跳过认证）。

use std::collections::{HashMap, HashSet};

use axum::extract::Query;
use nik::prelude::rust_decimal::Decimal;
use nik::prelude::*;
use nik::uuid::Uuid;

use crate::business::erp::cost::cost_calc_overhead;
use crate::business::erp::cost::cost_labor_dto;
use crate::business::erp::cost::cost_restore_structure_dto::{
    self as dto, ElementSplit, IssueView, ItemView, NodeView, ProducerChoice, ProducerView, RestoreFilter,
    RestoreIssue, RestoreQuery, RestoreRes, Row, RowState, StateFilter, Totals,
};
use crate::business::erp::cost::cost_restore_structure_model as model;
use crate::business::erp::cost::cost_wip_balance_model::batches_of;
use crate::business::erp::model_base::normalize_page;
use cloudpivot_kernel::business::shared::principal::ScopedPrincipal;
use cloudpivot_kernel::utils;

// ==================== 权限码 ====================

/// 表读取（**复用** `cost-object:list`，理由见模块头）。
pub const PERM_LIST: &str = dto::PERM_LIST;

/// 本模块全部权限码（与其它模块 `all_permissions()` 同形）。
pub const RESTORE_PERMISSIONS: [&str; 1] = dto::PERMISSIONS;

// ==================== 入参归一化 ====================

/// 归一化入参（纯逻辑，单测直接喂 [`RestoreQuery`]）。
///
/// 期间必须显式给：结构是「某一期投了什么料」的快照，选错期间等于把别的月份的结构当本期。
pub fn filter_of(query: &RestoreQuery) -> std::result::Result<RestoreFilter, ApiError> {
    let period_code = cost_labor_dto::clean_optional(query.period_code.clone())
        .ok_or_else(|| dto::bad_request("period_code（yyyy-MM）不能为空：结构是某一期投料的快照，例如 2026-11"))?;
    cost_calc_overhead::ensure_period_code(&period_code)?;
    let state = StateFilter::parse(query.state.as_deref())?;
    let (page, page_size) = normalize_page(query.page.unwrap_or(1), query.page_size.unwrap_or(0));
    // 层数：0 / 未给 ⇒ 默认；超过上限 ⇒ 夹到上限（与 page_size 同风格，回显在响应里）
    let max_depth = match query.max_depth.unwrap_or(dto::MAX_DEPTH_DEFAULT) {
        0 => dto::MAX_DEPTH_DEFAULT,
        value => value.min(dto::MAX_DEPTH_LIMIT),
    };
    Ok(RestoreFilter {
        period_code,
        keyword: cost_labor_dto::clean_optional(query.keyword.clone()),
        state,
        page,
        page_size,
        max_depth,
    })
}

// ==================== 逐层展开 ====================

/// 一个结构节点在装配前的草稿（父子用下标相连）。
#[derive(Debug, Clone)]
struct NodeDraft {
    /// 属于哪一行。
    row: usize,
    /// 层级（1 起）。
    level: u32,
    /// 父节点下标（`None` = 根层的节点）。
    parent: Option<usize>,
    /// 物料 id（料行没有物料 ⇒ `None`）。
    material_id: Option<Uuid>,
    /// 物料编码。
    material_code: Option<String>,
    /// 物料名称。
    material_name: Option<String>,
    /// 批号。
    lot_no: Option<String>,
    /// 数量。
    qty: Decimal,
    /// 金额。
    amount: Decimal,
    /// 是否自制半成品（= 有成本对象产出过该物料）。
    is_semi: bool,
    /// 结构来源（外购材料 ⇒ `None`）。
    choice: Option<ProducerChoice>,
    /// 本节点金额的三要素（取不到 ⇒ `None`）。
    elements: Option<ElementSplit>,
    /// 取不到 / 被截断的原因。
    issues: Vec<RestoreIssue>,
    /// 只提示不拦的提醒。
    warnings: Vec<String>,
    /// 是否被截断。
    truncated: bool,
}

/// 一个待展开的父节点（根层 = 成本对象本身）。
#[derive(Debug, Clone)]
struct Pending {
    /// 行号。
    row: usize,
    /// 草稿下标（`None` = 根层）。
    draft: Option<usize>,
    /// 父成本对象 id。
    object_id: Uuid,
    /// 父对象期间。
    period_code: String,
    /// 从根到这里的成本对象链（闭环检测用）。
    path: Vec<Uuid>,
}

/// 结构表（一条只读报表：逐层展开 → 筛 → 分页 → 整表合计）。
pub async fn structure_table_of(
    principal: &ScopedPrincipal,
    filter: &RestoreFilter,
) -> std::result::Result<RestoreRes, ApiError> {
    // ① 本期批次（只影响行头能不能看到本期计算单，不影响结构本身）
    let batches = batches_of(&filter.period_code)
        .await
        .map_err(|error| utils::db_error(error, "取本期成本核算批次"))?;

    // ② 根行（数据范围 + 关键字已在 SQL 里收窄）
    let roots = model::roots_of(
        principal,
        &filter.period_code,
        filter.keyword.as_deref(),
        batches.active_id(),
    )
    .await
    .map_err(|error| utils::db_error(error, "查询本期有材料投入的成本对象"))?;

    // ③ 逐层展开（BFS；每层两条查询）
    let mut drafts: Vec<NodeDraft> = Vec::new();
    let mut overflow_rows: HashSet<usize> = HashSet::new();
    let mut overflow = false;
    let mut pending: Vec<Pending> = roots
        .iter()
        .enumerate()
        .map(|(index, root)| Pending {
            row: index,
            draft: None,
            object_id: root.object_id,
            period_code: filter.period_code.clone(),
            path: vec![root.object_id],
        })
        .collect();

    let mut level: u32 = 1;
    while level <= filter.max_depth && !pending.is_empty() && !overflow {
        let pairs: Vec<(Uuid, String)> = pending
            .iter()
            .map(|item| (item.object_id, item.period_code.clone()))
            .collect();
        let inputs = model::material_inputs_of(&pairs)
            .await
            .map_err(|error| utils::db_error(error, "查询逐层材料投入"))?;

        // 只保留「允许的 (对象, 期间) 配对」：跨对象 / 跨期间的串行结果不参与
        let allowed: HashSet<(Uuid, String)> = pairs.iter().cloned().collect();
        let mut by_parent: HashMap<(Uuid, String), Vec<model::MaterialInput>> = HashMap::new();
        for input in inputs {
            if allowed.contains(&(input.cost_object_id, input.period_code.clone())) {
                by_parent
                    .entry((input.cost_object_id, input.period_code.clone()))
                    .or_default()
                    .push(input);
            }
        }

        // 生产者候选：一次取回本层全部物料（分片在取数层里做）
        let material_ids: Vec<Uuid> = by_parent
            .values()
            .flatten()
            .filter_map(|input| input.material_id)
            .collect();
        // 「谁产过这个物料」与「能不能取到结构」是两件事：前者定 is_semi，后者定 elements
        let self_made = model::produced_materials(&material_ids)
            .await
            .map_err(|error| utils::db_error(error, "查询自制半成品的物料"))?;
        let producers = model::producers_of(&material_ids, &filter.period_code)
            .await
            .map_err(|error| utils::db_error(error, "查询半成品的产出批次"))?;

        let mut next: Vec<Pending> = Vec::new();
        for parent in pending.drain(..) {
            let key = (parent.object_id, parent.period_code.clone());
            let Some(children) = by_parent.get(&key) else {
                continue;
            };
            for input in children {
                if drafts.len() >= dto::NODE_GUARD {
                    overflow = true;
                    overflow_rows.insert(parent.row);
                    break;
                }
                let candidates = input.material_id.and_then(|id| producers.get(&id));
                let is_semi = input.material_id.map(|id| self_made.contains(&id)).unwrap_or(false);
                let choice = match (is_semi, candidates) {
                    (true, Some(list)) => dto::resolve_producer(list, input.lot_no.as_deref()),
                    _ => None,
                };

                let mut issues: Vec<RestoreIssue> = Vec::new();
                let mut warnings: Vec<String> = Vec::new();
                let mut truncated = false;

                if input.material_id.is_none() {
                    issues.push(RestoreIssue {
                        code: dto::CODE_MATERIAL_MISSING,
                        message: format!(
                            "料行没有物料（金额 {}，数量 {}）：结构无从判断这是外购料还是半成品",
                            dto::money_text(input.amount),
                            dto::qty_text(input.qty)
                        ),
                        fix: "补物料的领料行或作废该归集行后重新归集（库层 ck_biz_cost_collect_mat 本该拦住）"
                            .to_string(),
                    });
                }

                let elements = if !is_semi {
                    // 外购材料：金额全额进材料
                    Some(ElementSplit::material_only(input.amount))
                } else {
                    match choice
                        .as_ref()
                        .and_then(|item| dto::scale_split(input.amount, &item.structure))
                    {
                        Some(split) => Some(split),
                        None => {
                            issues.push(RestoreIssue {
                                code: dto::CODE_NO_STRUCTURE,
                                message: format!(
                                    "自制半成品 {}（批号 {}，金额 {}）取不到完工成本结构：\
                                     产出它的成本对象在本期及以前没有可用计算单（未计算 / 已红冲 / 完工成本为 0）",
                                    material_label(input),
                                    input.lot_no.as_deref().unwrap_or("无"),
                                    dto::money_text(input.amount)
                                ),
                                fix: "先给产出该半成品的成本对象执行成本计算（POST /business/erp/cost-period/calc）；\
                                      若该批计算单已红冲，重算后再看"
                                    .to_string(),
                            });
                            None
                        }
                    }
                };

                if let Some(choice) = choice.as_ref() {
                    match choice.source {
                        dto::ProduceSource::PeriodAvg => {
                            warnings.push(match input.lot_no.as_deref() {
                                Some(lot) => format!(
                                    "批号 {lot} 没对上任何产出批次：按 {} 同月所产平均结构拆分，未继续下钻（下钻需要批号）",
                                    choice.period_code
                                ),
                                None => format!(
                                    "领料行没有批号：按 {} 同月所产平均结构拆分，未继续下钻（下钻需要批号）",
                                    choice.period_code
                                ),
                            });
                        }
                        dto::ProduceSource::Batch => {
                            let producer_id = Uuid::parse_str(&choice.object_ids[0])
                                .map_err(|error| dto::internal_error(&format!("成本对象 id 不是合法 uuid：{error}")))?;
                            if parent.path.contains(&producer_id) {
                                issues.push(RestoreIssue {
                                    code: dto::CODE_CYCLE,
                                    message: format!(
                                        "结构出现闭环：{} 绕回了自身（自己消耗自己产出的半成品），在此截断",
                                        choice.object_nos.join(" / ")
                                    ),
                                    fix: "核对 BOM / 领料单是否挂错了成本对象；同一批的产出不能又是自己的投入"
                                        .to_string(),
                                });
                                truncated = true;
                            } else if level < filter.max_depth {
                                let mut path = parent.path.clone();
                                path.push(producer_id);
                                next.push(Pending {
                                    row: parent.row,
                                    draft: Some(drafts.len()),
                                    object_id: producer_id,
                                    period_code: choice.period_code.clone(),
                                    path,
                                });
                            }
                            // `level == max_depth`：先不展开，交给 ④ 的探针判断「到底了没有」
                        }
                    }
                }

                drafts.push(NodeDraft {
                    row: parent.row,
                    level,
                    parent: parent.draft,
                    material_id: input.material_id,
                    material_code: input.material_code.clone(),
                    material_name: input.material_name.clone(),
                    lot_no: input.lot_no.clone(),
                    qty: input.qty,
                    amount: input.amount,
                    is_semi,
                    choice,
                    elements,
                    issues,
                    warnings,
                    truncated,
                });
            }
            if overflow {
                break;
            }
        }
        // 本层的「可按批下钻」父节点就是下一层的输入（漏了这一行 = 永远只有第 1 层）
        pending = next;
        level += 1;
    }

    // ④ 层数上限的截断探针：最后一层的半成品「再往下还有没有半成品」
    probe_depth_cut(&mut drafts, filter.max_depth).await?;

    // ⑤ 逐行装配 + 筛 + 合计（整表，不是当前页）
    let children_of = children_index(&drafts);
    let mut totals = Totals::default();
    let mut kept: Vec<Row> = Vec::with_capacity(roots.len());
    for (index, root) in roots.iter().enumerate() {
        let row = row_of(index, root, &drafts, &children_of, &overflow_rows);
        if !filter.state.matches(state_of(&row)) {
            continue;
        }
        totals.add(&row);
        kept.push(row);
    }

    // ⑥ 分页（升序来自 SQL：`ORDER BY t.object_no`；页码越界给空页，不报错）
    let total_rows = kept.len();
    let offset = filter
        .page
        .saturating_sub(1)
        .saturating_mul(filter.page_size)
        .min(u64::try_from(total_rows).unwrap_or(u64::MAX));
    let start = usize::try_from(offset).unwrap_or(total_rows);
    let end = start
        .saturating_add(usize::try_from(filter.page_size).unwrap_or(0))
        .min(total_rows);
    let list: Vec<Row> = kept.get(start..end).unwrap_or(&[]).to_vec();

    // ⑦ 页脚说明
    let mut note = String::new();
    if batches.active.is_none() {
        note.push_str(&format!(
            "{} 还没有成本核算批次：行头的「本期完工成本」会是空的，但结构照出 —— \
             结构只依赖归集行与半成品的产出批次，不依赖本期算没算过。",
            filter.period_code
        ));
    }
    if totals.no_structure_row_count > 0 {
        note.push_str(&format!(
            "有 {} 行里的半成品取不到完工成本结构（未计算 / 已红冲 / 完工成本为 0）：\
             这些节点的三要素给空值而不是 0，按行内 hint 处理后重算。",
            totals.no_structure_row_count
        ));
    }
    if totals.cycle_row_count > 0 {
        note.push_str(&format!(
            "有 {} 行出现结构闭环（自己消耗自己产出的半成品）：在闭环处截断，先核对领料单挂的成本对象。",
            totals.cycle_row_count
        ));
    }
    if totals.truncated_row_count > 0 {
        note.push_str(&format!(
            "有 {} 行到 {} 层还没钻到底：调大 max_depth（上限 {}）可继续下钻。",
            totals.truncated_row_count,
            filter.max_depth,
            dto::MAX_DEPTH_LIMIT
        ));
    }
    if overflow {
        note.push_str(&format!(
            "本次展开的节点数到了上限（{}）：后面的层没展开完，请用 keyword 收窄对象或调小 max_depth 分段看。",
            dto::NODE_GUARD
        ));
    }
    note.push_str(dto::FORMULA_LAYER);
    note.push(' ');
    note.push_str(dto::FORMULA_ELEMENT);

    Ok(RestoreRes {
        period_code: filter.period_code.clone(),
        batch: batches.active.as_ref().map(|(_, batch)| dto::BatchView {
            id: batch.id.clone(),
            batch_no: batch.batch_no.clone(),
            status: batch.status.clone(),
        }),
        cost_ready: batches.active.is_some(),
        state: filter.state.as_str().to_string(),
        keyword: filter.keyword.clone(),
        max_depth: filter.max_depth,
        formula_layer: dto::FORMULA_LAYER.to_string(),
        formula_element: dto::FORMULA_ELEMENT.to_string(),
        totals: dto::totals_view(&totals),
        list,
        total: u64::try_from(total_rows).unwrap_or(0),
        page: filter.page,
        page_size: filter.page_size,
        state_options: dto::state_options(),
        note,
    })
}

/// 到层数上限时探一次「再往下还有没有半成品」。
///
/// 为什么不在 BFS 里直接标截断：`max_depth` 那一层的半成品，其材料可能全是外购料
/// （结构本来就到底了）—— 直接标「未钻到底」会让每一行都挂一条假问题。
/// 这里只对边界层的 `batch` 节点多探两条查询：它的材料里只要有一个是自制半成品，
/// 才算真的被层数上限截断。
async fn probe_depth_cut(drafts: &mut [NodeDraft], max_depth: u32) -> std::result::Result<(), ApiError> {
    let mut targets: Vec<(usize, Uuid, String)> = Vec::new();
    for (index, draft) in drafts.iter().enumerate() {
        if draft.level != max_depth || draft.truncated {
            continue;
        }
        let Some(choice) = draft.choice.as_ref() else {
            continue;
        };
        if choice.source != dto::ProduceSource::Batch {
            continue;
        }
        let Some(first) = choice.object_ids.first() else {
            continue;
        };
        let producer_id = Uuid::parse_str(first)
            .map_err(|error| dto::internal_error(&format!("成本对象 id 不是合法 uuid：{error}")))?;
        targets.push((index, producer_id, choice.period_code.clone()));
    }
    if targets.is_empty() {
        return Ok(());
    }

    let pairs: Vec<(Uuid, String)> = targets
        .iter()
        .map(|(_, object_id, period)| (*object_id, period.clone()))
        .collect();
    let inputs = model::material_inputs_of(&pairs)
        .await
        .map_err(|error| utils::db_error(error, "探查下一层材料投入"))?;
    let material_ids: Vec<Uuid> = inputs.iter().filter_map(|input| input.material_id).collect();
    // 判据与主流程一致：只要下一层有「自制半成品」，就说明没钻到底（不管它算没算过成本）
    let self_made = model::produced_materials(&material_ids)
        .await
        .map_err(|error| utils::db_error(error, "探查下一层自制半成品"))?;

    let mut has_semi: HashSet<(Uuid, String)> = HashSet::new();
    for input in &inputs {
        let Some(material_id) = input.material_id else {
            continue;
        };
        if self_made.contains(&material_id) {
            has_semi.insert((input.cost_object_id, input.period_code.clone()));
        }
    }

    for (index, object_id, period) in targets {
        if !has_semi.contains(&(object_id, period)) {
            continue;
        }
        if let Some(draft) = drafts.get_mut(index) {
            draft.truncated = true;
            draft.issues.push(RestoreIssue {
                code: dto::CODE_DEPTH_CUT,
                message: format!(
                    "已展开到第 {max_depth} 层，这批半成品下面还有自制半成品没展开（父批 {}，批号 {}）",
                    draft
                        .choice
                        .as_ref()
                        .map(|choice| choice.object_nos.join(" / "))
                        .unwrap_or_default(),
                    draft.lot_no.as_deref().unwrap_or("无")
                ),
                fix: format!(
                    "调大 max_depth（上限 {}）继续下钻；层数很深通常意味着半成品层级设计过细或 BOM 有环",
                    dto::MAX_DEPTH_LIMIT
                ),
            });
        }
    }
    Ok(())
}

/// 一行的草稿 → 出参（纯装配，不连库）。
fn row_of(
    index: usize,
    root: &model::RootRow,
    drafts: &[NodeDraft],
    children_of: &HashMap<Option<usize>, Vec<usize>>,
    overflow_rows: &HashSet<usize>,
) -> Row {
    let tree = build_siblings(drafts, children_of, None, index, root.material_cost);

    // 第 1 层的半成品金额：这一层才等于材料投入（更深层是半成品自己的投入）
    let mut semi_cost = Decimal::ZERO;
    for node in &tree {
        if node.is_semi {
            semi_cost += node.amount.parse::<Decimal>().unwrap_or(Decimal::ZERO);
        }
    }

    // 问题 / 提醒：整棵树（含深层）都要收
    let mut issues: Vec<IssueView> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut has_no_structure = false;
    let mut has_cycle = false;
    let mut has_truncated = overflow_rows.contains(&index);
    collect_signals(
        &tree,
        &mut issues,
        &mut warnings,
        &mut has_no_structure,
        &mut has_cycle,
        &mut has_truncated,
    );
    if overflow_rows.contains(&index) {
        issues.push(
            RestoreIssue {
                code: dto::CODE_NODE_GUARD,
                message: format!("本次请求展开的节点数到了上限（{}），这一行没展开完", dto::NODE_GUARD),
                fix: "用 keyword 收窄对象，或调小 max_depth 分段看".to_string(),
            }
            .view(),
        );
    }

    let state = dto::row_state(semi_cost, has_no_structure, has_cycle, has_truncated);
    Row {
        object_id: root.object_id.to_string(),
        object_no: root.object_no.clone(),
        object_type: root.object_type.clone(),
        lot_no: root.lot_no.clone(),
        product_id: root.product_id.to_string(),
        product_code: root.product_code.clone(),
        product_name: root.product_name.clone(),
        workshop_dept_id: root.workshop_dept_id.to_string(),
        workshop_dept_name: root.workshop_dept_name.clone(),
        item: root.item.as_ref().map(|item| item_view(item, root)),
        finished_cost: root.item.as_ref().map(|item| dto::money_text(item.finished_cost)),
        material_cost: dto::money_text(root.material_cost),
        semi_cost: dto::money_text(semi_cost),
        purchased_cost: dto::money_text(root.material_cost - semi_cost),
        semi_share: dto::share_pct(semi_cost, root.material_cost).map(dto::pct_text),
        state: state.as_str().to_string(),
        state_label: state.label().to_string(),
        depth: dto::max_level(&tree),
        node_count: dto::count_nodes(&tree),
        layers: dto::layer_views(&tree),
        tree,
        issues,
        warnings,
    }
}

/// 递归收问题 / 提醒 / 三档标志（同级去重，避免同一句提示刷屏）。
fn collect_signals(
    nodes: &[NodeView],
    issues: &mut Vec<IssueView>,
    warnings: &mut Vec<String>,
    has_no_structure: &mut bool,
    has_cycle: &mut bool,
    has_truncated: &mut bool,
) {
    for node in nodes {
        if node.truncated {
            *has_truncated = true;
        }
        for issue in &node.issues {
            match issue.code.as_str() {
                dto::CODE_NO_STRUCTURE | dto::CODE_STRUCTURE_INVALID | dto::CODE_MATERIAL_MISSING => {
                    *has_no_structure = true
                }
                dto::CODE_CYCLE => *has_cycle = true,
                dto::CODE_DEPTH_CUT | dto::CODE_NODE_GUARD => *has_truncated = true,
                _ => {}
            }
            if !issues
                .iter()
                .any(|seen| seen.code == issue.code && seen.message == issue.message)
            {
                issues.push(issue.clone());
            }
        }
        for warning in &node.warnings {
            if !warnings.iter().any(|seen| seen == warning) {
                warnings.push(warning.clone());
            }
        }
        collect_signals(
            &node.children,
            issues,
            warnings,
            has_no_structure,
            has_cycle,
            has_truncated,
        );
    }
}

/// 行状态（从出参回读，避免两处各算一遍）。
fn state_of(row: &Row) -> RowState {
    match row.state.as_str() {
        "semi" => RowState::Semi,
        "no_semi" => RowState::NoSemi,
        "no_structure" => RowState::NoStructure,
        "cycle" => RowState::Cycle,
        _ => RowState::Truncated,
    }
}

/// 父 → 子下标索引。
fn children_index(drafts: &[NodeDraft]) -> HashMap<Option<usize>, Vec<usize>> {
    let mut out: HashMap<Option<usize>, Vec<usize>> = HashMap::new();
    for (index, draft) in drafts.iter().enumerate() {
        out.entry(draft.parent).or_default().push(index);
    }
    out
}

/// 装配同一父下、属于指定行的所有子节点（份额按兄弟合计算）。
fn build_siblings(
    drafts: &[NodeDraft],
    children_of: &HashMap<Option<usize>, Vec<usize>>,
    parent: Option<usize>,
    row: usize,
    fallback_parent_amount: Decimal,
) -> Vec<NodeView> {
    let ids: Vec<usize> = children_of
        .get(&parent)
        .map(|ids| ids.iter().copied().filter(|id| drafts[*id].row == row).collect())
        .unwrap_or_default();
    let total = if ids.is_empty() {
        fallback_parent_amount
    } else {
        ids.iter()
            .map(|id| drafts[*id].amount)
            .fold(Decimal::ZERO, |acc, amount| acc + amount)
    };
    ids.into_iter()
        .map(|id| build_node(drafts, children_of, id, total))
        .collect()
}

/// 装配一个节点（递归；层数受 `max_depth` 限制，递归深度与之同界）。
fn build_node(
    drafts: &[NodeDraft],
    children_of: &HashMap<Option<usize>, Vec<usize>>,
    index: usize,
    parent_amount: Decimal,
) -> NodeView {
    let draft = &drafts[index];
    let children = build_siblings(drafts, children_of, Some(index), draft.row, draft.amount);
    let unit_cost = if draft.qty > Decimal::ZERO {
        Some(dto::rate_text(draft.amount / draft.qty))
    } else {
        None
    };
    NodeView {
        level: draft.level,
        material_id: draft.material_id.map(|id| id.to_string()).unwrap_or_default(),
        material_code: draft.material_code.clone(),
        material_name: draft.material_name.clone(),
        lot_no: draft.lot_no.clone(),
        qty: dto::qty_text(draft.qty),
        unit_cost,
        amount: dto::money_text(draft.amount),
        share_pct: dto::share_pct(draft.amount, parent_amount).map(dto::pct_text),
        is_semi: draft.is_semi,
        source: draft.choice.as_ref().map(|choice| choice.source.as_str().to_string()),
        source_label: draft.choice.as_ref().map(|choice| choice.source.label().to_string()),
        producer: draft.choice.as_ref().map(producer_view),
        elements: draft.elements.as_ref().map(ElementSplit::view),
        children,
        truncated: draft.truncated,
        issues: draft.issues.iter().map(RestoreIssue::view).collect(),
        warnings: draft.warnings.clone(),
    }
}

/// 结构来源 → 出参。
fn producer_view(choice: &ProducerChoice) -> ProducerView {
    let unit = if choice.completed_qty > Decimal::ZERO {
        Some(dto::rate_text(choice.finished_cost / choice.completed_qty))
    } else {
        None
    };
    ProducerView {
        object_id: match choice.source {
            dto::ProduceSource::Batch => choice.object_ids.first().cloned(),
            dto::ProduceSource::PeriodAvg => None,
        },
        object_nos: choice.object_nos.join(" / "),
        lot_no: choice.lot_no.clone(),
        period_code: choice.period_code.clone(),
        item_status: choice.item_status.clone(),
        batch_count: choice.batch_count,
        completed_qty: dto::qty_text(choice.completed_qty),
        finished_cost: dto::money_text(choice.finished_cost),
        unit_finished_cost: unit,
        elements: choice.structure.view(),
    }
}

/// 计算单 → 出参（本期投入取计算单落库的那三列，不由单位成本现算）。
fn item_view(item: &dto::ItemFacts, root: &model::RootRow) -> ItemView {
    ItemView {
        status: item.status.clone(),
        completed_qty: dto::qty_text(item.completed_qty),
        finished_cost: dto::money_text(item.finished_cost),
        unit_finished_cost: dto::rate_text(item.unit_finished_cost),
        mat_cur: dto::money_text(root.mat_cur),
        labor_cur: dto::money_text(root.labor_cur),
        oh_cur: dto::money_text(root.oh_cur),
    }
}

/// 物料标签（`编码 名称`，缺一个就只显示另一个）。
fn material_label(input: &model::MaterialInput) -> String {
    match (input.material_code.as_deref(), input.material_name.as_deref()) {
        (Some(code), Some(name)) => format!("{code} {name}"),
        (Some(code), None) => code.to_string(),
        (None, Some(name)) => name.to_string(),
        (None, None) => "（无物料主数据）".to_string(),
    }
}

// ==================== HTTP 入口 ====================

/// 半成品成本结构表（逐成本对象：本期材料投入 → 自制半成品 / 外购材料 → 逐层结构）。
#[nik::permission(
    path = "/business/erp/cost-restore-structure",
    method = "GET",
    permission = "business:erp:cost-object:list"
)]
#[nik::get("/business/erp/cost-restore-structure")]
pub async fn cost_restore_structure_api(
    Extension(AuthClaims(claims)): Extension<AuthClaims>,
    Query(query): Query<RestoreQuery>,
) -> ResResult<RestoreRes> {
    let principal = cloudpivot_kernel::business::shared::principal::resolve_for_claims(&claims).await?;
    let filter = filter_of(&query)?;
    Ok(Res::ok(structure_table_of(&principal, &filter).await?))
}

/// 权限码自检出口（与其它 ERP 模块同形）。
pub fn all_permissions() -> Vec<&'static str> {
    RESTORE_PERMISSIONS.to_vec()
}

// ==================== 单测（纯逻辑，不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn query() -> RestoreQuery {
        RestoreQuery {
            period_code: Some("2026-11".to_string()),
            keyword: None,
            state: None,
            page: None,
            page_size: None,
            max_depth: None,
        }
    }

    fn draft(
        level: u32,
        parent: Option<usize>,
        amount: i64,
        is_semi: bool,
        choice: Option<ProducerChoice>,
    ) -> NodeDraft {
        NodeDraft {
            row: 0,
            level,
            parent,
            material_id: None,
            material_code: None,
            material_name: None,
            lot_no: None,
            qty: Decimal::from(1),
            amount: Decimal::from(amount),
            is_semi,
            choice,
            elements: Some(ElementSplit::material_only(Decimal::from(amount))),
            issues: Vec::new(),
            warnings: Vec::new(),
            truncated: false,
        }
    }

    #[test]
    fn permission_reuses_cost_object_list() {
        assert_eq!(PERM_LIST, "business:erp:cost-object:list");
        assert_eq!(all_permissions(), vec!["business:erp:cost-object:list"]);
    }

    #[test]
    fn period_is_required() {
        let mut q = query();
        q.period_code = None;
        let error = filter_of(&q).expect_err("没期间必须 400");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("period_code"));

        q.period_code = Some("   ".to_string());
        assert!(filter_of(&q).is_err(), "空白串也算没给");
    }

    #[test]
    fn period_shape_is_validated() {
        let mut q = query();
        q.period_code = Some("2026-13".to_string());
        assert_eq!(filter_of(&q).expect_err("13 月必须 400").status, 400);
        q.period_code = Some("202611".to_string());
        assert!(filter_of(&q).is_err(), "缺连字符必须 400");
    }

    #[test]
    fn state_typo_is_rejected() {
        let mut q = query();
        q.state = Some("semis".to_string());
        let error = filter_of(&q).expect_err("拼错必须 400");
        assert_eq!(error.status, 400);
        assert!(error.msg.contains("semis"));
    }

    #[test]
    fn defaults_are_all_and_depth_six() {
        let filter = filter_of(&query()).expect("缺省可解析");
        assert_eq!(filter.period_code, "2026-11");
        assert_eq!(filter.page, 1);
        assert_eq!(filter.page_size, 20);
        assert_eq!(filter.state, StateFilter::All);
        assert_eq!(filter.max_depth, dto::MAX_DEPTH_DEFAULT);
    }

    #[test]
    fn page_size_and_depth_are_capped() {
        let mut q = query();
        q.page = Some(0);
        q.page_size = Some(9999);
        q.max_depth = Some(99);
        let filter = filter_of(&q).expect("合法入参");
        assert_eq!(filter.page, 1, "页码从 1 起");
        assert_eq!(filter.page_size, 200, "上限 200");
        assert_eq!(filter.max_depth, dto::MAX_DEPTH_LIMIT, "层数夹到上限");

        q.max_depth = Some(0);
        assert_eq!(filter_of(&q).expect("0 层按默认").max_depth, dto::MAX_DEPTH_DEFAULT);
    }

    #[test]
    fn tree_assembles_children_and_shares() {
        // 一行：材料 100 = 半成品 A 60 + 外购料 40；A 下面再挂一个外购料 20
        let drafts = vec![
            draft(1, None, 60, true, None),
            draft(1, None, 40, false, None),
            draft(2, Some(0), 20, false, None),
        ];
        let index = children_index(&drafts);
        let tree = build_siblings(&drafts, &index, None, 0, Decimal::from(100));
        assert_eq!(tree.len(), 2);
        assert_eq!(tree[0].children.len(), 1, "半成品 A 下面挂着二级料");
        assert_eq!(tree[0].share_pct.as_deref(), Some("60.00"));
        assert_eq!(tree[1].share_pct.as_deref(), Some("40.00"));
        assert_eq!(tree[0].children[0].share_pct.as_deref(), Some("100.00"), "独子是 100%");
        assert_eq!(dto::count_nodes(&tree), 3);
        assert_eq!(dto::max_level(&tree), 2);
    }

    #[test]
    fn siblings_of_other_rows_never_leak_in() {
        // 两行的根节点在同一个 `children_of[None]` 里：按行过滤，别把别人的料算进来
        let mut first = draft(1, None, 60, false, None);
        first.row = 0;
        let mut second = draft(1, None, 40, false, None);
        second.row = 1;
        let drafts = vec![first, second];
        let index = children_index(&drafts);
        let tree = build_siblings(&drafts, &index, None, 1, Decimal::from(40));
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].share_pct.as_deref(), Some("100.00"));
    }

    #[test]
    fn signals_are_collected_from_deep_levels() {
        // 深层节点的闭环 / 截断必须冒到行级，否则行状态会把有问题的结构报成「可还原」
        let deep = NodeView {
            level: 2,
            material_id: String::new(),
            material_code: None,
            material_name: None,
            lot_no: None,
            qty: "1.0000".to_string(),
            unit_cost: None,
            amount: "10".to_string(),
            share_pct: None,
            is_semi: true,
            source: Some("batch".to_string()),
            source_label: Some("按批".to_string()),
            producer: None,
            elements: None,
            children: Vec::new(),
            truncated: true,
            issues: vec![IssueView {
                code: dto::CODE_CYCLE.to_string(),
                message: "绕回自身".to_string(),
                fix: "核对领料单".to_string(),
            }],
            warnings: vec!["提示".to_string()],
        };
        let root = NodeView {
            children: vec![deep],
            ..NodeView {
                level: 1,
                material_id: String::new(),
                material_code: None,
                material_name: None,
                lot_no: None,
                qty: "1.0000".to_string(),
                unit_cost: None,
                amount: "10".to_string(),
                share_pct: None,
                is_semi: true,
                source: Some("batch".to_string()),
                source_label: Some("按批".to_string()),
                producer: None,
                elements: None,
                children: Vec::new(),
                truncated: false,
                issues: Vec::new(),
                warnings: Vec::new(),
            }
        };
        let mut issues = Vec::new();
        let mut warnings = Vec::new();
        let mut no_structure = false;
        let mut cycle = false;
        let mut truncated = false;
        collect_signals(
            std::slice::from_ref(&root),
            &mut issues,
            &mut warnings,
            &mut no_structure,
            &mut cycle,
            &mut truncated,
        );
        assert!(cycle, "深层闭环要冒到行级");
        assert!(truncated, "深层截断要冒到行级");
        assert_eq!(issues.len(), 1);
        assert_eq!(warnings, vec!["提示".to_string()]);
    }
}
