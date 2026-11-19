/// calc 第 5 段「分摊计算」的纯逻辑（守恒断言 / 未覆盖池诊断 / 落库后回查），B2-10。
pub mod cost_alloc_calc_dto;
/// 分摊结果数据层（`biz_cost_alloc_result` + 分摊转入的 `ALLOC` 归集行），B2-10。
pub mod cost_alloc_result_model;
/// B2-5 的真库用例（`ERP_DB_URL` 开关）：四件套 + 生效期选版本 + 四种标准试算。只在 `cargo test` 时编译。
#[cfg(test)]
mod cost_alloc_rule_db_tests;
/// 分摊规则（ARCH-9 §3.1 ⑦ / §4.4 / §5.2 #20–#24）：四件套 + 生效期版本选择 + 试算（不落库）。
pub mod cost_alloc_rule_dto;
pub mod cost_alloc_rule_handler;
pub mod cost_alloc_rule_model;
/// 分批法「料工费归集到批次」**编排**（M5-03）：三路「扫描 + 取差」+ 单事务写入
/// （先软删过期、再插入、再回查）+ 预览（同一套取差，不落库）。
pub mod cost_batch_collect;
/// M5-03 的真库用例（`ERP_DB_URL` 开关）：三路归集 / 幂等重跑 / 改挂批次 / 过期清理 /
/// 状态闸 409 / 数据范围 404 / 与期间 calc 不打架 / 清场零残留。
#[cfg(test)]
mod cost_batch_collect_db_tests;
/// 分批法「料工费归集到批次」的**契约层与纯逻辑**（M5-03）：三路取差 / 行状态 /
/// 状态闸（期间批次冻结 · 批次关批 · 已作废）/ 旧行取舍。零 DB 依赖，可纯单测。
pub mod cost_batch_collect_dto;
/// 分批法「料工费归集到批次」**接口层**（M5-03）：
/// `GET …/cost-collect-sheet/{id}/sources`（三路来源预览，复用 `cost-object:list`）+
/// `POST …/cost-collect-sheet/{id}/collect`（执行归集，复用 `cost-period:calc`）。
pub mod cost_batch_collect_handler;
/// 分批法「料工费归集到批次」**取数层**（M5-03）：本期活跃成本批次（闸门）/
/// 本期三要素已有归集额 / 物料名批量补 / 批次抬头（复用 M5-02 的范围探针）。
/// 三路扫描与写入原语**直接复用** calc 各段的函数，不抄 SQL。
pub mod cost_batch_collect_model;
/// 分批法「完工批次结转（完工转出）」**编排**（M5-04）：锁批次行 → 重算
/// `累计归集 − 已结转` → 判定闸门 → 写结转单 → 回查。一个事务，幂等（差额 0 不写空单）。
pub mod cost_batch_finish;
/// M5-04 的真库用例（`ERP_DB_URL` 开关）：整批全额结转 / 差额补结转 / 幂等重放 /
/// 在制与已结算拦截 / 数据范围 404 / 并发第二次空转 / 清场零残留。
#[cfg(test)]
mod cost_batch_finish_db_tests;
/// 分批法「完工批次结转（完工转出）」的**契约层与纯逻辑**（M5-04）：三要素值对象 /
/// 结转判定（作废·已结算·在制·无产出·无成本·卫生四条·期间冻结）/ 结转计划与单位成本。
/// 零 DB 依赖，可纯单测。
pub mod cost_batch_finish_dto;
/// 分批法「完工批次结转（完工转出）」**接口层**（M5-04）：
/// `GET …/cost-collect-sheet/{id}/finish-preview`（试算，复用 `cost-object:list`）+
/// `POST …/cost-collect-sheet/{id}/finish`（执行结转，复用 `cost-period:calc`）。
pub mod cost_batch_finish_handler;
/// 分批法「完工批次结转（完工转出）」**取数层**（M5-04）：累计归集（全期间）/ 已结转与
/// 历史结转单 / 已结算计算单 / 成本对象行锁（**串行点**）。批次抬头与卫生四条复用既有实现。
pub mod cost_batch_finish_model;
/// M5-06 的**全链路单测**（`ERP_DB_URL` 开关跑真库部分）：M5-02 归集表 → M5-03 归集 →
/// M5-04 结转 → M5-05 余额/台账，**走真实接口**串起来；核心是「部分完工」的口径
/// （批没完一分不转、余额不缩水、单位成本按累计完工数）。只在 `cargo test` 时编译。
#[cfg(test)]
mod cost_batch_method_tests;
/// M5-05 的真库用例（`ERP_DB_URL` 开关）：截止期间口径 / 跨期滚动 / 结转后余额归零 /
/// 部分完工留余额 / 负余额警告 / 数据范围 404 / 列表与台账同数 / 清场零残留。
#[cfg(test)]
mod cost_batch_wip_db_tests;
/// 分批法「在制批次余额」**契约层与纯逻辑**（M5-05）：余额 = 累计归集 − 累计结转（逐要素、
/// 不夹零）/ 逐期滚动台账（期初 → 投入 → 转出 → 期末）/ 六类警告。零 DB 依赖，可纯单测。
pub mod cost_batch_wip_dto;
/// 分批法「在制批次余额」**接口层**（M5-05）：`GET …/cost-collect-sheet/wip-balance`
/// （截至某期的一批一行余额表）+ `GET …/cost-collect-sheet/{id}/wip-ledger`（单批逐期台账）。
/// 两条都是只读，复用 `cost-object:list`。
pub mod cost_batch_wip_handler;
/// 分批法「在制批次余额」**取数层**（M5-05）：四个派生表（累计归集 / 累计结转 / 本期投入 /
/// 本期转出）+ 逐期流量 + 截止期后发生额 + 已结算计算单。批次抬头与数据范围探针复用 M5-02。
pub mod cost_batch_wip_model;
/// calc 第 5 段：分摊计算（ARCH-9 §4.4.3 · 四标准基数 + 最大余数法 + 守恒断言 + 整删重灌）。
pub mod cost_calc_alloc;
/// B2-10 的真库用例（`ERP_DB_URL` 开关）：四标准逐对象核数 / 尾差只落一行 / 池 0 与基数 0 边界 / 重算幂等。
#[cfg(test)]
mod cost_calc_alloc_db_tests;
/// calc 第 6 段：约当产量与成本计算单（ARCH-9 §4.6 · 公式 + 差额倒挤 + 期初在制结转）。
pub mod cost_calc_item;
/// B2-11 的真库用例（`ERP_DB_URL` 开关）：算例 A / 恒等式 / 跨月期初结转 / 重算幂等 / 五类 409。
#[cfg(test)]
mod cost_calc_item_db_tests;
/// calc 第 2 段：工归集（ARCH-9 §4.3 · 扫当期已确认工时 → 幂等写 `biz_cost_collect`）。
pub mod cost_calc_labor;
/// B2-7 的真库用例（`ERP_DB_URL` 开关）：只取 CONFIRMED + 金额取台账 + 缺费率进未归集清单。
#[cfg(test)]
mod cost_calc_labor_db_tests;
/// calc 第 1 段：料归集（ARCH-9 §4.3 · 扫当期已过账领料 → 幂等写 `biz_cost_collect`）。
pub mod cost_calc_material;
/// B2-6 的真库用例（`ERP_DB_URL` 开关）：只扫已过账+挂对象的领料 + 幂等 + 金额取账上真实值。
#[cfg(test)]
mod cost_calc_material_db_tests;
/// calc 第 3 段：费归集（ARCH-9 §4.3 费三路 · 折旧/报销/手工 → 池行 + 直接行）。
pub mod cost_calc_overhead;
/// B2-8 的真库用例（`ERP_DB_URL` 开关）：三路来源口径 + 池按 (车间, 科目) 汇总 + 直接行。
#[cfg(test)]
mod cost_calc_overhead_db_tests;
/// calc **编排**（ARCH-9 §5.2 #27 · COST-B2-15）：定批次 → 料→工→费→对账→分摊→计算单，一次事务。
pub mod cost_calc_period;
/// B2-15 的真库用例（`ERP_DB_URL` 开关）：首次建 DRAFT 并落六段 / 重复调用幂等 / 非 DRAFT 409 / 红冲后重建。
#[cfg(test)]
mod cost_calc_period_db_tests;
/// calc 第 4 段：池与 `4101.*` 对账（ARCH-9 §4.11 C-COST-1 · 逐 (车间, 科目) 求差，差额列凭证号）。
pub mod cost_calc_reconcile;
/// B2-9 的真库用例（`ERP_DB_URL` 开关）：逐组对账 / 旁路记账点凭证 / 红冲与红字不计 / 阶段口径。
#[cfg(test)]
mod cost_calc_reconcile_db_tests;
/// 成本归集明细的纯逻辑（幂等键 / 行校验 / 增量计划），B2-6 起用，后续 calc 各段共用。
pub mod cost_collect_dto;
/// 成本归集明细（`biz_cost_collect`）的数据层：候选扫描 / 已归集行 / 幂等写入 / 软删。
pub mod cost_collect_model;
/// M5-02 的真库用例（`ERP_DB_URL` 开关）：一行一批 / 期间隔离 / 池行不进表 / 筛选 /
/// 翻页合计恒定 / 排序 / 数据范围 / 明细与表行对得上 / 清场零残留。
#[cfg(test)]
mod cost_collect_sheet_db_tests;
/// 按生产订单/批次归集表（M5-02 · 分批法 ①）的契约层：期间必填 / 状态与排序值域 /
/// 出参（行 = 一个批次，列 = 料工费合计）。**复用** `cost-object:list` 权限码。
pub mod cost_collect_sheet_dto;
/// ① 归集表接口层（M5-02）：`GET /business/erp/cost-collect-sheet`（按期间分页 +
/// 整表合计）+ `GET …/{object_id}`（逐笔归集明细）。无写接口：归集只能由单据产生。
pub mod cost_collect_sheet_handler;
/// ① 归集表数据层（M5-02）：按批次聚合本期料工费（派生表 + `FILTER`）、整表合计、
/// 明细分笔。数据范围复用 `biz_cost_object` 锚点；参数顺序手工构造并逐次校验。
pub mod cost_collect_sheet_model;
/// calc 第 2 段「工归集」的纯逻辑（幂等键 / 行校验 / 增量计划 / 未归集清单），B2-7。
pub mod cost_labor_collect_dto;
/// B2-1 的真库用例（`ERP_DB_URL` 开关）：CRUD + 确认幂等 + 金额 CHECK 映射。只在 `cargo test` 时编译。
#[cfg(test)]
mod cost_labor_db_tests;
/// 工时台账（ARCH-9 §3.1 ④ / §5.2 #10–#14）：`biz_cost_labor` CRUD + 确认（幂等）。
pub mod cost_labor_dto;
pub mod cost_labor_handler;
pub mod cost_labor_import;
pub mod cost_labor_model;
/// M5-01 的真库用例（`ERP_DB_URL` 开关）：默认口径 / 对象级覆盖 / 重叠拦截 / 生效期两端 /
/// 删改纪律 / 计算入口方法闸。
#[cfg(test)]
mod cost_method_db_tests;
/// 成本方法配置（M5-01 · 品种法 / 分批法 / 标准成本）：值域 / 判定（对象级 > 全局级 > 默认）/
/// 生效期重叠 / 五个业务码，纯逻辑与契约层。
pub mod cost_method_dto;
/// 成本方法配置接口层（M5-01）：分页 / 生效方法查询 / 增 / 改 / 删 + 成本计算入口的方法闸
/// （`ensure_methods_runnable_in_tx`，未实现方法一律 409 + 66101，绝不静默按分批法算）。
pub mod cost_method_handler;
/// 成本方法配置数据层（M5-01 · `biz_cost_method_config`）：全量取数 / 分页 / 写入 /
/// 本期拦截清单（与期间相交的非分批法配置）。
pub mod cost_method_model;
/// B1-8 的真库用例（`ERP_DB_URL` 开关）：5 个接口 + 关批 5 条前置。只在 `cargo test` 时编译。
#[cfg(test)]
mod cost_object_db_tests;
pub mod cost_object_dto;
pub mod cost_object_handler;
pub mod cost_object_model;
/// calc 第 3 段「费归集」的纯逻辑（幂等键 / 行校验 / 增量计划 / 池汇总 / 未归集清单），B2-8。
pub mod cost_overhead_collect_dto;
/// 费归集（`biz_cost_collect` 的 OVERHEAD 段）数据层：三路取数 + 幂等写入 + 池汇总。
pub mod cost_overhead_collect_model;
/// B2-4 的真库用例（`ERP_DB_URL` 开关）：科目合法性 + 金额 > 0 + 归集后不许删。只在 `cargo test` 时编译。
#[cfg(test)]
mod cost_overhead_db_tests;
/// 手工费用台账（ARCH-9 §3.1 ⑤ / §5.2 #17–#19）：`biz_cost_overhead` 列表 / 新增 / 软删。
pub mod cost_overhead_dto;
pub mod cost_overhead_handler;
pub mod cost_overhead_model;
/// B3-3 的真库用例（`ERP_DB_URL` 开关）：提交 → 起实例 / 驳回 → 回草稿清痕迹 / 撤回 → 终止实例。
#[cfg(test)]
mod cost_period_approval_db_tests;
/// 成本结算审批的业务回调（`COST_SETTLE`）：结论 → 批次状态 + 自审闸 + 内控排除，B3-3。
pub mod cost_period_bridge;
/// B3-1 的真库用例（`ERP_DB_URL` 开关）：取号 / 唯一键 / 状态机全链路 / 非法跃迁。只在 `cargo test` 时编译。
#[cfg(test)]
mod cost_period_db_tests;
/// 成本核算批次（`biz_cost_period`）契约层：状态值域 / 批次号 / 入参归一化，B3-1。
pub mod cost_period_dto;
/// 成本核算批次接口层：列表 / 详情 / 建 / 改 / 作废 / 提交 / 撤回（状态机 + 审批引擎），B3-1 / B3-3。
pub mod cost_period_handler;
/// calc 第 6 段「约当产量与计算单」的纯逻辑（约当产量 / 单位成本 / 尾差倒挤 / 期初要素还原），B2-11。
pub mod cost_period_item_dto;
/// 成本计算单（`biz_cost_period_item`）数据层：取对象与投入 / 期初结转 / 整删重灌 / 回查，B2-11。
pub mod cost_period_item_model;
/// 成本核算批次数据层：CRUD + 活跃批次 + 取号 + 明细与合计，B3-1。
pub mod cost_period_model;
/// 成本计算单导出（ARCH-9 §5.2 #32）：主表 20 列 + 分摊明细 + HTTP 文件流，B4-1。
pub mod cost_period_sheet;
/// 成本计算单导出的分摊取数（`biz_cost_alloc_result` 按期间聚合），B4-1。
pub mod cost_period_sheet_model;
/// B2-3 费率取数（ARCH-9 §4.5.4）：`biz_cost_rate` 三级优先级 + 缺费率报错（无 HTTP 入口，供工时台账复用）。
pub mod cost_rate_dto;
pub mod cost_rate_handler;
pub mod cost_rate_model;
/// calc 第 4 段「池与 4101 对账」的纯逻辑（分组 / 求差 / 差额凭证号报文），B2-9。
pub mod cost_reconcile_dto;
/// 对账取数（`biz_cost_collect` 池行按来源反查科目 + `biz_voucher_entry` 的 4101.* 借方），B2-9。
pub mod cost_reconcile_model;
/// 红冲（ARCH-9 §4.8 反向路径 · B3-7）：逐凭证 `reverse_posted` + 库存回冲（`PRODUCE_CANCEL`）
/// + 生产入库单作废 + 计算单回 `CALC` + 批次 → `REVERSED`。对外是设计 §5.2 #30。
pub mod cost_reverse_run;
/// B3-7 的真库用例（`ERP_DB_URL` 开关）：红冲端到端 / 库存不足整笔回滚 / 红冲后再算一次不撞唯一键。
#[cfg(test)]
mod cost_reverse_run_db_tests;
/// 结算 ①「完工入库」（ARCH-9 §4.7.4/§4.7.6/§4.8 ③ · B3-4）：`stock_in(PRODUCTION)` +
/// 明细 + `inventory_writer` 三本账 + 计算单回写 + 生产批登记。无 HTTP 入口，供 B3-6 结算事务调用。
pub mod cost_settle_finish;
/// B3-4 的真库用例（`ERP_DB_URL` 开关）：完工入库单 + 三本账 + 计算单回写 + 两道幂等闸。
#[cfg(test)]
mod cost_settle_finish_db_tests;
/// 结算编排（ARCH-9 §4.8 ①–⑥ · B3-6）：锁批次 → 完工入库 → 三张凭证 → 状态回写 + 合计回写
/// → 事务内 6 条断言（C-COST-1~4 的事务内版，任一不平整笔回滚）。对外是设计 §5.2 #29。
pub mod cost_settle_run;
/// B3-6 的真库用例（`ERP_DB_URL` 开关）：一次事务的端到端结算 + 6 条断言 + 「第 6 条失败 ⇒ 全回滚」。
#[cfg(test)]
mod cost_settle_run_db_tests;
/// 结算 ②「三张凭证」（ARCH-9 §4.7.2–§4.7.5 · B3-5）：人工计提 / 手工台账 / 制造费用结转 /
/// 完工入库结转，全部经 `voucher::post_voucher`。无 HTTP 入口，供 B3-6 结算事务调用。
pub mod cost_settle_voucher;
/// B3-5 的真库用例（`ERP_DB_URL` 开关）：三张凭证的借贷平衡 / 科目 / 金额与计算单一致 / 四道闸。
#[cfg(test)]
mod cost_settle_voucher_db_tests;
/// M5-11 的真库用例（`ERP_DB_URL` 开关）：可比行逐项核数 / 缺标准 / 缺实际 / 换版提示 /
/// 未发布不参与 / 四档筛选 / 合计与逐行对拍 / 无批次时只出标准侧 / 清场零残留。
#[cfg(test)]
mod cost_standard_compare_db_tests;
/// 标准成本 × 实际成本 **逐物料对比** 契约层（M5-11）：三态判定 / 差异额与差异率 / 单位与总额双口径 /
/// 尾差对平 / 三要素拆分 / 整表合计 / 两处「缺一侧」的具体动作。纯逻辑，零 DB。
pub mod cost_standard_compare_dto;
/// 标准成本 × 实际成本 **对比接口层**（M5-11）：`GET /business/erp/report/cost-variance`
/// （期间必填 + 物料关键字 + 四档状态筛选 + 分页 + 整表合计）。复用 `cost-method:list`，零新增码。
pub mod cost_standard_compare_handler;
/// 标准成本 × 实际成本 **对比取数层**（M5-11）：`act` / `std` / `draft` 三个 CTE 按物料 FULL OUTER JOIN，
/// 分页走 `scope::paginate_unscoped`，合计走同模板聚合（与 Rust 逐行算数对拍）。
pub mod cost_standard_compare_model;
/// M5-08 的真库用例（`ERP_DB_URL` 开关）：建版算数 / 生效期重叠 / 期间取数 / 换版提示 /
/// 已生效锁改删 / 生效失效 / 库层区间护栏。只在 `cargo test` 时编译。
#[cfg(test)]
mod cost_standard_db_tests;
/// 标准成本卡 **版本表（物料 × 期间）** 契约层（M5-08）：期间解析 / 三要素算数（与库层
/// 四条 CHECK 同口径）/ 生效区间重叠与覆盖判定 / 已生效锁改判据 / 三个业务码。纯逻辑。
pub mod cost_standard_dto;
/// 标准成本卡 **接口层**（M5-08）：版本列表 / 详情 / 期间生效版本 / 期间矩阵 / 新建 / 修改 /
/// 生效失效 / 软删（8 条路由，复用 `cost-method` 四个权限码：零新增死码）。
pub mod cost_standard_handler;
/// 标准成本卡 **xlsx 批量导入**（M5-09）：9 列模板（一组「物料编码 + 期间」= 一版）/
/// 逐行纯校验 / 引用一次性批量查库 / **单事务**逐组写入（全有或全无）。
pub mod cost_standard_import;
/// M5-09 的真库用例（`ERP_DB_URL` 开关）：一组 = 一版 / 全有或全无 / 期间已占用 /
/// 相邻两期 / 服务类拒绝 / 事务回滚。只在 `cargo test` 时编译。
#[cfg(test)]
mod cost_standard_import_db_tests;
/// 标准成本卡 **取数层**（M5-08）：卡头 + 物料实时 join + 明细行数、标准 BOM、按物料/日期/期间
/// 取版本、advisory lock、卡头与明细写入原语。
pub mod cost_standard_model;
/// 标准成本卡 **发布审批**（M5-10「审核后生效」）：`DRAFT → PENDING → PUBLISHED` 状态机 +
/// `cost_standard_publish_approve` 流程接线（提交 / 撤回 2 条路由，复用 `cost-method:update`）+ 自审闸 + 内控排除。
pub mod cost_standard_publish;
/// M5-10 的真库用例（`ERP_DB_URL` 开关）：提交建实例 / 通过即发布并参与取数 / 驳回回草稿 /
/// 撤回同事务 / 状态闸 66111 / 自审 403 / 审批中不许改删。只在 `cargo test` 时编译。
#[cfg(test)]
mod cost_standard_publish_db_tests;
/// 成本核算模块开关探针（ARCH-9 §7.4 灰度 · COST-B4-4）：`GET /business/erp/cost-switch`。
pub mod cost_switch_handler;
/// B2-12 的真库用例（`ERP_DB_URL` 开关）：四块缺口的真库断言。只在 `cargo test` 时编译。
#[cfg(test)]
mod cost_uncollected_db_tests;
/// 「未归集清单」的纯逻辑与出参（四块缺口 / 原因判定 / 怎么修文案 / 跳转），B2-12。
pub mod cost_uncollected_dto;
/// 「未归集清单」接口层（设计 §5.2 #33 + 按期间自查入口），B2-12。
pub mod cost_uncollected_handler;
/// 「未归集清单」取数层（领料未挂对象 / 工时行数 / 池余额 / 批次），B2-12。
pub mod cost_uncollected_model;
