//! 触发点接线状态（`spec::TRIGGER_SPECS`）的**漂移守卫**（P1-FIX-5C）。
//!
//! # 为什么需要这一组测试
//!
//! P1-FIX-5B 的真机实测暴露了一件事：**白名单 ≠ 接线**。`spec::TRIGGERS` 里有 15 个触发点，
//! 但代码里真正调联动引擎的只有 2 个，其余 13 个上配规则「能保存、能启用、永远不触发」。
//! 光加一个 `wired: bool` 字段并不能解决问题 —— 手写的 `true` 会随着重构漂移：把调用删掉
//! 没人发现，或者偷偷多接一个触发点却没更新 `wired`，配置页就会继续骗人。
//!
//! 所以这组测试做**双向**核对：
//!
//! 1. **正向**：每个 `wired = true` 的触发点，其 `proofs` 点名的文件必须存在且必须含该调用片段
//!    （删了调用 → 红）；
//! 2. **反向**：扫描 `src/business` 下全部生产代码，找出所有提到联动引擎的文件，
//!    这个集合必须**恰好等于** wired 触发点点名的文件集合（多接一个没登记 → 红）。
//!
//! 反向扫描用运行时 `std::fs`（`CARGO_MANIFEST_DIR` 在测试运行时可用），不引入新的构建期依赖。
//!
//! # 为什么扫描的是「文件里出现 `linkage::engine::<入口>`」而不是语法树
//!
//! 本仓库没有把 `syn` 拉进 dev-dependencies，硬加一个解析器只为一条守卫不值得；而「引擎入口
//! 的名字」在这份代码里是稳定契约（`engine::run` / `run_excluding` / `run_deferred` /
//! `status_notice` / `stockin_voucher`）。宁可偶尔因为**注释里提到引擎**而让人来更新这张表
//! （fail-closed），也不要漏掉真实调用。

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::spec;

/// 引擎的调用入口（出现任意一个 = 这个文件在调联动引擎）。
///
/// 加新入口时**必须**同步这里，否则新入口的调用点不会被反向扫描发现。
const ENGINE_ENTRIES: &[&str] = &[
    "linkage::engine::run(",
    "linkage::engine::run_excluding",
    "linkage::engine::run_deferred",
    "linkage::engine::status_notice",
    "linkage::engine::stockin_voucher",
];

/// `backend/` 根目录（测试运行时可用）。
fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// 这个路径是不是「联动模块自身的代码」（自己不算调用方）。
fn is_linkage_module(relative: &Path) -> bool {
    let text = relative.to_string_lossy().replace('\\', "/");
    text == "flow/linkage.rs" || text.starts_with("flow/linkage/")
}

/// 测试文件名（`*_tests.rs` / `tests*.rs`）：测试里调引擎不算生产接线。
fn is_test_file(relative: &Path) -> bool {
    let name = relative
        .file_name()
        .map(|item| item.to_string_lossy().to_string())
        .unwrap_or_default();
    name.ends_with("_tests.rs") || name.starts_with("tests")
}

/// 递归收集 `src/business` 下的 `.rs` 文件（相对 `src/business` 的路径）。
fn collect_rs_files(dir: &Path, root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, root, out);
            continue;
        }
        if path.extension().map(|ext| ext == "rs") != Some(true) {
            continue;
        }
        let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        out.push(relative);
    }
}

/// 扫描生产代码里所有调联动引擎的文件（相对 `src/business`）。
fn engine_call_sites() -> BTreeSet<String> {
    let root = manifest_dir().join("src").join("business");
    let mut files = Vec::new();
    collect_rs_files(&root, &root, &mut files);
    let mut sites = BTreeSet::new();
    for relative in files {
        if is_linkage_module(&relative) || is_test_file(&relative) {
            continue;
        }
        let Ok(text) = fs::read_to_string(root.join(&relative)) else {
            continue;
        };
        if ENGINE_ENTRIES.iter().any(|entry| text.contains(entry)) {
            sites.insert(format!(
                "src/business/{}",
                relative.to_string_lossy().replace('\\', "/")
            ));
        }
    }
    sites
}

/// 接线表必须与白名单**逐字、同序**一致（漏一个 = 前端下拉/写入口的判据缺一条）。
#[test]
fn trigger_specs_cover_whitelist_in_order() {
    let entities: Vec<&str> = spec::TRIGGER_SPECS.iter().map(|item| item.entity).collect();
    assert_eq!(
        entities,
        spec::TRIGGERS.to_vec(),
        "TRIGGER_SPECS 必须与 TRIGGERS 逐字同序（新增触发点必须同时声明接线状态）"
    );
    assert_eq!(entities.len(), 16, "当前白名单是 16 个触发点");
    assert_eq!(
        spec::wired_triggers(),
        vec![
            "STOCK_IN",
            "STOCK_OUT",
            "STOCK_TRANSFER",
            "PURCHASE_ORDER",
            "PAYABLE",
            "VOUCHER",
            "ACCOUNTING_PERIOD",
            "ACCOUNTING_YEAR",
            "CONTRACT",
            "STOCK_TAKE",
            "FIXED_ASSET",
            "DEPRECIATION",
            "EXPENSE_REPORT",
            "PURCHASE_RETURN",
            "TASK",
            "FLOW_INSTANCE"
        ],
        "已接线的触发点：STOCK_IN（收货过账）、STOCK_OUT（出库过账，P1-FIX-5C 新接）、\
         STOCK_TRANSFER（库存调拨状态变更，LNK-01 新接）、\
         PURCHASE_ORDER（采购单状态变更 + 部分/全部收货，LNK-02 新接）、\
         PAYABLE（应付单付款 / 部分付款 / 结清，LNK-03 新接）、\
         VOUCHER（凭证手工落草稿 / 审核 / 反审核 / 记账 / 红冲，LNK-06 新接）、\
         ACCOUNTING_PERIOD（会计期间结账 / 反结账，LNK-08 新接）、\
         ACCOUNTING_YEAR（年度结转 / 反年结，LNK-11 新接）、\
         CONTRACT（合同提交审批 / 审批通过 / 驳回 / 生效 / 完成 / 终止，LNK-07 新接）、\
         STOCK_TAKE（盘点单过账 / 作废，LNK-04 新接）、\
         FIXED_ASSET（资产处置 / 作废处置，LNK-09 新接）、\
         DEPRECIATION（折旧批次过账 / 红冲，LNK-10 新接）、\
         EXPENSE_REPORT（报销单提交 / 通过 / 付清 / 作废 / 打回，LNK-05 新接）、\
         PURCHASE_RETURN（采购退货确认 = 唯一动账点，P2-03 新接）、\
         TASK（任务状态变更）、\
         FLOW_INSTANCE（审批结论 APPROVED / REJECTED / WITHDRAWN，LNK-12 新接）；\
         接线状态变化时这份断言要跟着改，改之前先想清楚「真的有人调引擎了吗」"
    );
    assert_eq!(
        spec::unwired_triggers().len(),
        0,
        "15 个白名单触发点全部接线完成（FLOW_INSTANCE 由 LNK-12 接上）；\
         新增白名单项时这里会重新变成 1+，请同时补 wired = true + proofs"
    );
}

/// 每条声明都要自洽：已接线的必须有证据对，未接线的必须给出原因且不能有证据对。
#[test]
fn every_trigger_declares_wired_and_reason() {
    for item in spec::TRIGGER_SPECS {
        assert!(
            !item.wiring.trim().is_empty(),
            "触发点 {} 必须写清「接线位置」或「为什么没接」",
            item.entity
        );
        if item.wired {
            assert!(
                !item.proofs.is_empty(),
                "触发点 {} 声明 wired = true 但没有 proofs：拿什么证明有人调引擎",
                item.entity
            );
        } else {
            assert!(
                item.proofs.is_empty(),
                "触发点 {} 声明 wired = false 却给了 proofs",
                item.entity
            );
        }
    }
    assert!(
        !spec::is_wired_trigger("NOT_A_DOC"),
        "未知触发点必须 fail-closed"
    );
}

/// 正向守卫：wired 触发点点名的文件必须存在且含该调用片段。
#[test]
fn wired_trigger_proofs_exist_in_source() {
    let root = manifest_dir();
    for item in spec::TRIGGER_SPECS.iter().filter(|item| item.wired) {
        for (file, needle) in item.proofs {
            let path = root.join(file);
            let text = fs::read_to_string(&path).unwrap_or_else(|error| {
                panic!("触发点 {} 的接线文件 {file} 读不到：{error}", item.entity)
            });
            assert!(
                text.contains(needle),
                "触发点 {} 声明接线在 {file}，但文件里找不到 `{needle}` —— 调用被删/改名了，\
                 要么改回来，要么把 spec::TRIGGER_SPECS 的 wired 改成 false 并在配置页承认它不生效",
                item.entity
            );
        }
    }
}

/// 反向守卫：生产代码里调引擎的文件集合必须恰好等于 wired 触发点的证据文件集合。
#[test]
fn wired_triggers_match_call_sites() {
    let declared: BTreeSet<String> = spec::TRIGGER_SPECS
        .iter()
        .filter(|item| item.wired)
        .flat_map(|item| item.proofs.iter().map(|(file, _)| file.to_string()))
        .collect();
    let actual = engine_call_sites();
    if declared != actual {
        let undeclared: Vec<&String> = actual.difference(&declared).collect();
        let missing: Vec<&String> = declared.difference(&actual).collect();
        panic!(
            "联动引擎的调用点与 spec::TRIGGER_SPECS 不一致：\n\
             - 调了引擎但没登记（多接了触发点却没写 wired = true）：{undeclared:?}\n\
             - 登记了但没调（调用被删 / 路径改了）：{missing:?}\n\
             实际扫描到：{actual:?}\n\
             已登记：{declared:?}"
        );
    }
    assert_eq!(
        actual.len(),
        17,
        "当前真实调用点 17 个文件（收货过账 / 出库过账 handler + 出库联动 hook + 调拨联动 hook + \
         采购联动 hook + 应付联动 hook + 盘点联动 hook + 报销联动 hook + 凭证联动 hook + \
         合同联动 hook + 会计期间联动 hook + 会计年度联动 hook + 固定资产联动 hook + \
         折旧批次联动 hook + 任务状态通知 + 审批实例联动 hook + \
         采购退货联动 hook（purchase_return_handler.rs::confirm_return，P2-03 新接））：{actual:?}\n\
         ⚠️ 本计数器是**文件级去重 + 只认 `linkage::engine::*` 直调**：\n\
         固定资产新增的「停用 / 启用」写在 `erp/asset_handler.rs` 里、只间接调\
         `asset_linkage::run_in_tx`（引擎在 hook 里被调），所以**不会**也不该让这个数字 +1；\n\
         那条触发点的守卫在 `asset_wires_generic_engine_and_excludes_voucher` 的函数内计数上。"
    );
}

/// 出库过账的接线细节：必须**真的**走 `run_excluding` 且排除专用路径动作。
///
/// 这条防的是「接线了但排除清单丢了」：出库过账自己已经扣了库存、生成了成本结转凭证，
/// 通用引擎再跑一次 `WRITE_INVENTORY` / `GENERATE_VOUCHER` 就是**重复落账**
/// （同一行明细扣两次库存、一张单两张凭证）。e2e 能抓到，但改一行代码就静默发生，
/// 所以在这里把源码原文与排除清单一起钉住。
#[test]
fn stockout_post_wires_generic_engine_and_excludes_dedicated_actions() {
    let handler = include_str!("../../erp/stockout_handler.rs");
    assert!(
        handler.contains("linkage::engine::run_excluding("),
        "出库过账必须调用通用引擎（run_excluding）：否则 STOCK_OUT/POSTED 的联动规则配了也不触发"
    );
    assert!(
        handler.contains("linkage::spec::STOCKOUT_EXCLUDED_ACTIONS"),
        "出库过账调通用引擎时必须传 spec::STOCKOUT_EXCLUDED_ACTIONS（不许改成写死的空清单）"
    );
    for code in [
        "WRITE_INVENTORY",
        "WRITE_INVENTORY_TXN",
        "UPDATE_COST",
        spec::ACTION_GENERATE_VOUCHER,
    ] {
        assert!(
            spec::STOCKOUT_EXCLUDED_ACTIONS.contains(&code),
            "出库过账的排除清单漏了 {code}：它在 post_out_tx ③⑤ 有专用落账，通用引擎再跑一次就是重复落账"
        );
    }
    assert!(
        !spec::STOCKOUT_EXCLUDED_ACTIONS.contains(&spec::ACTION_SEND_NOTICE),
        "SEND_NOTICE 不能被出库排除：出库过账的通知就靠通用引擎的 AfterCommit 动作"
    );
    let hook = include_str!("../../erp/stockout_linkage.rs");
    assert!(
        hook.contains("linkage::engine::run_deferred"),
        "出库过账的提交后动作必须交给 run_deferred（commit 之后跑），否则通知 / 建任务永远不执行"
    );
}

/// 库存调拨（`STOCK_TRANSFER`）的接线细节（LNK-01）：**两条状态变更路都要调引擎**，
/// 且必须排除会与 `settle_transfer` 重复落账的动作。
///
/// 这条防的三种漂移：
///
/// 1. 只在 `spec.rs` 把 `wired` 改成 true、handler 里没调用（P1-FIX-5C 的原罪）；
/// 2. 排除清单丢了 → 调拨收货时同一行明细扣两次库存、一张单四条流水（e2e 抓得到，
///    但改一行代码就静默发生）；
/// 3. 只接人工流转、漏掉审批回调 → 「配了已发布流程」的生产环境里
///    `STOCK_TRANSFER/APPROVED` 的规则永远不触发（最坏的一种：能保存、能启用、永远不响）。
#[test]
fn transfer_status_wires_generic_engine_and_excludes_inventory() {
    let hook = include_str!("../../erp/transfer_linkage.rs");
    assert!(
        hook.contains("linkage::engine::run_excluding("),
        "调拨状态变更必须调用通用引擎（run_excluding）：否则 STOCK_TRANSFER 的联动规则配了不触发"
    );
    assert!(
        hook.contains("linkage::spec::TRANSFER_EXCLUDED_ACTIONS"),
        "调拨调通用引擎时必须传 spec::TRANSFER_EXCLUDED_ACTIONS（不许改成写死的空清单）"
    );
    assert!(
        hook.contains("linkage::engine::run_deferred"),
        "调拨的提交后动作必须交给 run_deferred（commit 之后跑），否则通知 / 建任务永远不执行"
    );
    for code in [
        "WRITE_INVENTORY",
        "WRITE_INVENTORY_TXN",
        spec::ACTION_GENERATE_VOUCHER,
    ] {
        assert!(
            spec::TRANSFER_EXCLUDED_ACTIONS.contains(&code),
            "调拨的排除清单漏了 {code}：它由 settle_transfer 专用落账，通用引擎再跑一次就是重复落账"
        );
    }
    for code in [spec::ACTION_SEND_NOTICE, spec::ACTION_CREATE_TASK] {
        assert!(
            !spec::TRANSFER_EXCLUDED_ACTIONS.contains(&code),
            "{code} 不能被调拨排除：通知 / 建任务就靠通用引擎的 AfterCommit 动作"
        );
    }

    // 人工流转：状态事务内接线 + commit 后延迟接线
    let handler = include_str!("../../erp/transfer_handler_status.rs");
    assert!(
        handler.contains("transfer_linkage::run_in_tx("),
        "transfer_handler_status::apply_transition 必须真的调 transfer_linkage::run_in_tx\
         （只改 spec.rs 的 wired 标记不算接线）"
    );
    assert!(
        handler.contains("transfer_linkage::run_deferred_after_commit("),
        "change_transfer_status 必须在 commit 之后调 run_deferred_after_commit\
         （在事务里发通知，一旦回滚就是「用户看到通知、单子没动」）"
    );

    // 审批回调：PENDING → APPROVED / DRAFT 走的是 task_bridge，不经过 change_transfer_status
    let bridge = include_str!("../../flow/task_bridge.rs");
    assert!(
        bridge.contains("transfer_linkage::run_in_tx("),
        "审批回调（flow::task_bridge::settle_transfer）也必须调 transfer_linkage::run_in_tx：\
         否则配了已发布流程的环境里 STOCK_TRANSFER/APPROVED 的规则永远不触发"
    );
    assert!(
        bridge.contains("transfer_linkage::run_deferred_after_commit("),
        "审批回调的提交后动作同样必须在 commit 之后跑（run_deferred_after_commit）"
    );
}

/// 调拨的排除清单在**语义上**真的会把那三类动作拦下来（不是只有字符串写对）。
///
/// `engine::is_routed_elsewhere` 是引擎判定「这条规则交给专用路径」的唯一判据：
/// 命中的规则**不产生 `RuleRun`**（`run_excluding` 里 `continue`）。
#[test]
fn transfer_excluded_actions_are_routed_elsewhere() {
    use crate::business::flow::linkage::engine;
    for code in spec::TRANSFER_EXCLUDED_ACTIONS {
        assert!(
            engine::is_routed_elsewhere(spec::TRANSFER_EXCLUDED_ACTIONS, code),
            "{code} 在排除清单里却没被判成「专用路由」：清单传下去也拦不住它"
        );
    }
    for code in [spec::ACTION_SEND_NOTICE, spec::ACTION_CREATE_TASK] {
        assert!(
            !engine::is_routed_elsewhere(spec::TRANSFER_EXCLUDED_ACTIONS, code),
            "{code} 被误判成专用路由：调拨的通知 / 建任务会静默不执行"
        );
    }
}

/// 采购订单（`PURCHASE_ORDER`）的接线细节（LNK-02）：**两处状态变更路都要调引擎**，
/// 且必须排除会与收货过账重复落账的动作。
///
/// 这条防的四种漂移：
///
/// 1. 只在 `spec.rs` 把 `wired` 改成 true、handler 里没调用（P1-FIX-5C 的原罪）；
/// 2. 只接 `apply_status`、漏掉 `sync_purchase_status` → 「部分收货 → 全部收货」这条边
///    由 `state_machine::transition_if_changed` 推进，**完全不经过 apply_status**，
///    漏了这一处，`PURCHASE_ORDER/RECEIVED` 的规则永远不响；
/// 3. 排除清单丢了 → 通用引擎在采购单状态变更时再写一次库存 / 流水 / 凭证
///    （同一物料加两次库存、一张收货单两张进项凭证），或把派生口径的已收数量再推一次；
/// 4. 排除清单把通知 / 建任务也排掉 → 联动配了等于没配（最坏的一种：不报错、不落账）。
#[test]
fn purchase_order_wires_generic_engine_and_excludes_stock_actions() {
    let hook = include_str!("../../erp/purchase_linkage.rs");
    assert!(
        hook.contains("linkage::engine::run_excluding("),
        "采购单状态变更必须调用通用引擎（run_excluding）：否则 PURCHASE_ORDER 的联动规则配了不触发"
    );
    assert!(
        hook.contains("linkage::spec::PURCHASE_EXCLUDED_ACTIONS"),
        "采购单调通用引擎时必须传 spec::PURCHASE_EXCLUDED_ACTIONS（不许改成写死的空清单）"
    );
    assert!(
        hook.contains("linkage::engine::run_deferred"),
        "采购单的提交后动作必须交给 run_deferred（commit 之后跑），否则通知 / 建任务永远不执行"
    );
    for code in [
        "WRITE_INVENTORY",
        "WRITE_INVENTORY_TXN",
        spec::ACTION_GENERATE_VOUCHER,
        spec::ACTION_UPDATE_RECEIVED_QTY,
    ] {
        assert!(
            spec::PURCHASE_EXCLUDED_ACTIONS.contains(&code),
            "采购单的排除清单漏了 {code}：它在收货过账里有专用落账（已收数量是派生值），\
             通用引擎再跑一次就是重复落账"
        );
    }
    for code in [spec::ACTION_SEND_NOTICE, spec::ACTION_CREATE_TASK] {
        assert!(
            !spec::PURCHASE_EXCLUDED_ACTIONS.contains(&code),
            "{code} 不能被采购单排除：通知 / 建任务就靠通用引擎的 AfterCommit 动作"
        );
    }

    // 两处状态写入点：① `apply_status`（提交 / 审批 / 关闭 / 作废）② 收货推进
    let model = include_str!("../../erp/purchase_model.rs");
    assert!(
        model.contains("purchase_linkage::run_in_tx("),
        "purchase_model::apply_status 必须真的调 purchase_linkage::run_in_tx\
         （只改 spec.rs 的 wired 标记不算接线）"
    );
    let receipt = include_str!("../../erp/stockin_purchase.rs");
    assert!(
        receipt.contains("purchase_linkage::run_in_tx("),
        "stockin_purchase::sync_purchase_status 也必须调 purchase_linkage::run_in_tx：\
         「部分收货 → 全部收货」这条边由 transition_if_changed 推进、不经过 apply_status，\
         不接这一处这条边永远走不到联动"
    );
}

/// 采购单的排除清单在**语义上**真的会把那四类动作拦下来（不是只有字符串写对）。
///
/// `engine::is_routed_elsewhere` 是引擎判定「这条规则交给专用路径」的唯一判据：
/// 命中的规则**不产生 `RuleRun`**（`run_excluding` 里 `continue`）。
#[test]
fn purchase_excluded_actions_are_routed_elsewhere() {
    use crate::business::flow::linkage::engine;
    for code in spec::PURCHASE_EXCLUDED_ACTIONS {
        assert!(
            engine::is_routed_elsewhere(spec::PURCHASE_EXCLUDED_ACTIONS, code),
            "{code} 在排除清单里却没被判成「专用路由」：清单传下去也拦不住它"
        );
    }
    for code in [spec::ACTION_SEND_NOTICE, spec::ACTION_CREATE_TASK] {
        assert!(
            !engine::is_routed_elsewhere(spec::PURCHASE_EXCLUDED_ACTIONS, code),
            "{code} 被误判成专用路由：采购单的通知 / 建任务会静默不执行"
        );
    }
}

/// 应付单（`PAYABLE`）的接线细节（LNK-03）：**付款事务内**跑通用引擎，且必须排除
/// 「余额」与「凭证」两类会与付款事务重复落账的动作。
///
/// 这条防的四种漂移：
///
/// 1. 只在 `spec.rs` 把 `wired` 改成 true、付款路径里没调用（P1-FIX-5C 的原罪：
///    规则能保存、能启用、永远不触发）；
/// 2. 排除清单丢了 → 同一笔钱被扣两次（`paid_amount` 翻倍 / `unpaid_amount` 变负），
///    或一笔付款生成两张付款凭证；
/// 3. 排除清单把通知 / 建任务也排掉 → 联动配了等于没配（不报错、不落账）；
/// 4. 只在直付路径 flush、审批路径不 flush（或反之）→ 配了已发布付款流程的生产环境里
///    `PAYABLE/SETTLED` 的通知永远发不出来。
#[test]
fn payable_status_wires_generic_engine_and_excludes_balance_and_voucher() {
    let hook = include_str!("../../erp/payable_linkage.rs");
    assert!(
        hook.contains("linkage::engine::run_excluding("),
        "应付单状态变更必须调用通用引擎（run_excluding）：否则 PAYABLE 的联动规则配了不触发"
    );
    assert!(
        hook.contains("linkage::spec::PAYABLE_EXCLUDED_ACTIONS"),
        "应付单调通用引擎时必须传 spec::PAYABLE_EXCLUDED_ACTIONS（不许改成写死的空清单）"
    );
    assert!(
        hook.contains("linkage::engine::run_deferred"),
        "应付单的提交后动作必须交给 run_deferred（commit 之后跑），否则通知 / 建任务永远不执行"
    );
    for code in ["UPDATE_PAYABLE_BALANCE", spec::ACTION_GENERATE_VOUCHER] {
        assert!(
            spec::PAYABLE_EXCLUDED_ACTIONS.contains(&code),
            "应付单的排除清单漏了 {code}：付款事务里已经维护过余额 / 生成过付款凭证，\
             通用引擎再跑一次就是重复落账"
        );
    }
    for code in [spec::ACTION_SEND_NOTICE, spec::ACTION_CREATE_TASK] {
        assert!(
            !spec::PAYABLE_EXCLUDED_ACTIONS.contains(&code),
            "{code} 不能被应付单排除：通知 / 建任务就靠通用引擎的 AfterCommit 动作"
        );
    }

    // 唯一记账实现 `settle_payment_in_tx` 里必须真的调 hook（三条状态推进共用这一处）
    let payment = include_str!("../../erp/payable_payment.rs");
    assert!(
        payment.contains("payable_linkage::run_in_tx("),
        "payable_payment::settle_payment_in_tx 必须真的调 payable_linkage::run_in_tx\
         （只改 spec.rs 的 wired 标记不算接线）"
    );
    assert_eq!(
        payment
            .matches("payable_linkage::run_deferred_after_commit(")
            .count(),
        2,
        "两条入口都要在 commit 之后 flush：直付 pay_payable_with_reason + \
         审批通过 settle_approved_payment（少一处，那条路径的通知永远不发）"
    );
}

/// 应付单的排除清单在**语义上**真的会把那两类动作拦下来（不是只有字符串写对）。
///
/// `engine::is_routed_elsewhere` 是引擎判定「这条规则交给专用路径」的唯一判据：
/// 命中的规则**不产生 `RuleRun`**（`run_excluding` 里 `continue`）。
#[test]
fn payable_excluded_actions_are_routed_elsewhere() {
    use crate::business::flow::linkage::engine;
    for code in spec::PAYABLE_EXCLUDED_ACTIONS {
        assert!(
            engine::is_routed_elsewhere(spec::PAYABLE_EXCLUDED_ACTIONS, code),
            "{code} 在排除清单里却没被判成「专用路由」：清单传下去也拦不住它"
        );
    }
    for code in [spec::ACTION_SEND_NOTICE, spec::ACTION_CREATE_TASK] {
        assert!(
            !engine::is_routed_elsewhere(spec::PAYABLE_EXCLUDED_ACTIONS, code),
            "{code} 被误判成专用路由：应付单的通知 / 建任务会静默不执行"
        );
    }
}

/// 盘点单（`STOCK_TAKE`）的接线细节（LNK-04）：**过账与作废两条路都要调引擎**，
/// 且必须排除会与 `post_tx` / `cancel_tx` 专用落账重复的动作。
///
/// 这条防的四种漂移：
///
/// 1. 只在 `spec.rs` 把 `wired` 改成 true、handler 里没调用（P1-FIX-5C 的原罪：
///    规则能保存、能启用、永远不触发）；
/// 2. 只接过账、漏掉作废 → `STOCK_TAKE/CANCELLED` 的规则永远不响（作废后要通知
///    制单人 / 冲销待办这类需求全部失效）；
/// 3. 排除清单丢了 → 通用引擎在过账时再写一次库存 / 流水 / 差异凭证
///    （盘盈盘亏落两次账、「盘点后库存 = 实盘」直接失效）；
/// 4. 排除清单把通知 / 建任务也排掉 → 联动配了等于没配（不报错、不落账）。
#[test]
fn stocktake_wires_generic_engine_and_excludes_inventory() {
    let hook = include_str!("../../erp/stocktake_linkage.rs");
    assert!(
        hook.contains("linkage::engine::run_excluding("),
        "盘点单状态变更必须调用通用引擎（run_excluding）：否则 STOCK_TAKE 的联动规则配了不触发"
    );
    assert!(
        hook.contains("linkage::spec::STOCKTAKE_EXCLUDED_ACTIONS"),
        "盘点单调通用引擎时必须传 spec::STOCKTAKE_EXCLUDED_ACTIONS（不许改成写死的空清单）"
    );
    assert!(
        hook.contains("linkage::engine::run_deferred"),
        "盘点单的提交后动作必须交给 run_deferred（commit 之后跑），否则通知 / 建任务永远不执行"
    );
    for code in [
        "WRITE_INVENTORY",
        "WRITE_INVENTORY_TXN",
        spec::ACTION_GENERATE_VOUCHER,
    ] {
        assert!(
            spec::STOCKTAKE_EXCLUDED_ACTIONS.contains(&code),
            "盘点单的排除清单漏了 {code}：它在 post_tx ③⑤ / cancel_tx 回滚里有专用落账，\
             通用引擎再跑一次就是重复落账"
        );
    }
    for code in [spec::ACTION_SEND_NOTICE, spec::ACTION_CREATE_TASK] {
        assert!(
            !spec::STOCKTAKE_EXCLUDED_ACTIONS.contains(&code),
            "{code} 不能被盘点单排除：通知 / 建任务就靠通用引擎的 AfterCommit 动作"
        );
    }

    // 两条状态变更路（过账 post_tx + 作废 cancel_tx）都要在事务内调 hook，
    // 且两个入口都要在 commit 之后 flush。
    let handler = include_str!("../../erp/stocktake_handler.rs");
    assert_eq!(
        handler.matches("stocktake_linkage::run_in_tx(").count(),
        2,
        "盘点单过账（post_tx ⑥）与作废（cancel_tx ③）都必须真的调 \
         stocktake_linkage::run_in_tx（只改 spec.rs 的 wired 标记不算接线）"
    );
    assert_eq!(
        handler
            .matches("stocktake_linkage::run_deferred_after_commit(")
            .count(),
        2,
        "过账 / 作废两个入口都要在 commit 之后 flush（少一处，那条路径的通知永远不发）"
    );
}

/// 盘点单的排除清单在**语义上**真的会把那三类动作拦下来（不是只有字符串写对）。
///
/// `engine::is_routed_elsewhere` 是引擎判定「这条规则交给专用路径」的唯一判据：
/// 命中的规则**不产生 `RuleRun`**（`run_excluding` 里 `continue`）。
#[test]
fn stocktake_excluded_actions_are_routed_elsewhere() {
    use crate::business::flow::linkage::engine;
    for code in spec::STOCKTAKE_EXCLUDED_ACTIONS {
        assert!(
            engine::is_routed_elsewhere(spec::STOCKTAKE_EXCLUDED_ACTIONS, code),
            "{code} 在排除清单里却没被判成「专用路由」：清单传下去也拦不住它"
        );
    }
    for code in [spec::ACTION_SEND_NOTICE, spec::ACTION_CREATE_TASK] {
        assert!(
            !engine::is_routed_elsewhere(spec::STOCKTAKE_EXCLUDED_ACTIONS, code),
            "{code} 被误判成专用路由：盘点单的通知 / 建任务会静默不执行"
        );
    }
}

/// 费用报销（`EXPENSE_REPORT`）的接线细节（LNK-05）：**五个状态推进点都要调引擎**，
/// 且必须排除会与 `expense_bridge` 重复落账的两类凭证动作。
///
/// 这条防的五种漂移：
///
/// 1. 只在 `spec.rs` 把 `wired` 改成 true、handler / bridge 里没调用（P1-FIX-5C 的原罪：
///    规则能保存、能启用、永远不触发）；
/// 2. 只接 handler 里的提交 / 付清 / 作废，漏掉 bridge 里的通过 / 打回 →
///    `EXPENSE_REPORT/APPROVED`（财务最关心的「报销通过了」）与 `DRAFT`（驳回 / 撤回）
///    的规则永远不响；
/// 3. 排除清单丢了 → 审批通过时通用引擎再跑一次 `GENERATE_VOUCHER`：一张报销单两张计提
///    凭证；作废时再跑一次 `REVERSE_VOUCHER`：同一张蓝字被红冲两次；
/// 4. 排除清单把通知 / 建任务也排掉 → 联动配了等于没配（不报错、不落账）；
/// 5. 只接「付清」却漏了「部分付款不触发」的判断 → 配一条「报销付清」规则，付几笔发几条信。
#[test]
fn expense_wires_generic_engine_and_excludes_voucher() {
    let hook = include_str!("../../erp/expense_linkage.rs");
    assert!(
        hook.contains("linkage::engine::run_excluding("),
        "报销单状态变更必须调用通用引擎（run_excluding）：否则 EXPENSE_REPORT 的联动规则配了不触发"
    );
    assert!(
        hook.contains("linkage::spec::EXPENSE_EXCLUDED_ACTIONS"),
        "报销单调通用引擎时必须传 spec::EXPENSE_EXCLUDED_ACTIONS（不许改成写死的空清单）"
    );
    assert!(
        hook.contains("linkage::engine::run_deferred"),
        "报销单的提交后动作必须交给 run_deferred（commit 之后跑），否则通知 / 建任务永远不执行"
    );
    for code in [spec::ACTION_GENERATE_VOUCHER, "REVERSE_VOUCHER"] {
        assert!(
            spec::EXPENSE_EXCLUDED_ACTIONS.contains(&code),
            "报销单的排除清单漏了 {code}：凭证由 expense_bridge 的专用路径出（计提 / 付款 / 红冲），\
             通用引擎再跑一次就是重复落账"
        );
    }
    for code in [spec::ACTION_SEND_NOTICE, spec::ACTION_CREATE_TASK] {
        assert!(
            !spec::EXPENSE_EXCLUDED_ACTIONS.contains(&code),
            "{code} 不能被报销单排除：通知 / 建任务就靠通用引擎的 AfterCommit 动作"
        );
    }

    // handler 里三条路：提交（DRAFT → SUBMITTED）/ 付清（APPROVED → PAID）/ 作废（→ CANCELLED），
    // 每条都要在事务内调 hook，并在 commit 之后 flush。
    let handler = include_str!("../../erp/expense_handler.rs");
    assert_eq!(
        handler.matches("expense_linkage::run_in_tx(").count(),
        3,
        "报销单提交 / 付清 / 作废三条路都必须真的调 expense_linkage::run_in_tx\
         （只改 spec.rs 的 wired 标记不算接线）"
    );
    assert_eq!(
        handler
            .matches("expense_linkage::run_deferred_after_commit(")
            .count(),
        3,
        "提交 / 付清 / 作废三个入口都要在 commit 之后 flush（少一处，那条路径的通知永远不发）"
    );

    // bridge 里两条路：审批通过（SUBMITTED → APPROVED）/ 打回（驳回 / 撤回 → DRAFT）。
    // 不接这两条，配了已发布 expense 流程的生产环境里 APPROVED / DRAFT 的规则永远不触发。
    let bridge = include_str!("../../erp/expense_bridge.rs");
    assert_eq!(
        bridge.matches("expense_linkage::run_in_tx(").count(),
        2,
        "审批回调（expense_bridge::execute_approval）与打回（revert_to_draft）都必须调 \
         expense_linkage::run_in_tx：否则「报销审批通过 → 通知财务待付款」配了也不触发"
    );
    assert_eq!(
        bridge
            .matches("expense_linkage::run_deferred_after_commit(")
            .count(),
        2,
        "审批通过 / 打回两个事务都在 commit 之后 flush（少一处，那条路径的通知永远不发）"
    );
}

/// 报销单的排除清单在**语义上**真的会把那两类凭证动作拦下来（不是只有字符串写对）。
///
/// `engine::is_routed_elsewhere` 是引擎判定「这条规则交给专用路径」的唯一判据：
/// 命中的规则**不产生 `RuleRun`**（`run_excluding` 里 `continue`）。
#[test]
fn expense_excluded_actions_are_routed_elsewhere() {
    use crate::business::flow::linkage::engine;
    for code in spec::EXPENSE_EXCLUDED_ACTIONS {
        assert!(
            engine::is_routed_elsewhere(spec::EXPENSE_EXCLUDED_ACTIONS, code),
            "{code} 在排除清单里却没被判成「专用路由」：清单传下去也拦不住它"
        );
    }
    for code in [spec::ACTION_SEND_NOTICE, spec::ACTION_CREATE_TASK] {
        assert!(
            !engine::is_routed_elsewhere(spec::EXPENSE_EXCLUDED_ACTIONS, code),
            "{code} 被误判成专用路由：报销单的通知 / 建任务会静默不执行"
        );
    }
}

/// 凭证（`VOUCHER`）的接线细节（LNK-06）：**两条闸都要在**，且四个事务入口都真的调 hook。
///
/// 这条防的四种漂移（凭证是唯一一个「配错就死循环」的实体，所以比别的触发点多一道闸）：
///
/// 1. 只在 `spec.rs` 把 `wired` 改成 true、`voucher_write.rs` 里没调用（P1-FIX-5C 的原罪）；
/// 2. **运行时排除清单丢了** → 凭证过账时通用引擎跑一遍 `GENERATE_VOUCHER`：
///    生成新凭证（`POSTED`）→ 又触发 → 不收敛的「凭证生凭证」递归；
/// 3. **配置期禁令丢了** → 管理员能存一条永远不执行的规则（`run_excluding` 会把它静默跳过），
///    配置页看起来「生效中」，账上一分没动；
/// 4. 只接记账、漏了手工落草稿（`DRAFT`）与红冲（红字凭证也是 `POSTED`）→ 两个能走到的
///    状态上配规则永远不响。
#[test]
fn voucher_wires_generic_engine_excludes_and_forbids_generate() {
    let hook = include_str!("../../erp/voucher_linkage.rs");
    assert!(
        hook.contains("linkage::engine::run_excluding("),
        "凭证状态变更必须调用通用引擎（run_excluding）：否则 VOUCHER 的联动规则配了不触发"
    );
    assert!(
        hook.contains("linkage::spec::VOUCHER_EXCLUDED_ACTIONS"),
        "凭证调通用引擎时必须传 spec::VOUCHER_EXCLUDED_ACTIONS（不许改成写死的空清单）"
    );
    assert!(
        hook.contains("linkage::engine::run_deferred"),
        "凭证的提交后动作必须交给 run_deferred（commit 之后跑），否则通知 / 建任务永远不执行"
    );
    assert!(
        spec::VOUCHER_EXCLUDED_ACTIONS.contains(&spec::ACTION_GENERATE_VOUCHER),
        "凭证的运行时排除清单漏了 GENERATE_VOUCHER：过账 → 生成凭证 → 再过账，死循环"
    );
    for code in [spec::ACTION_SEND_NOTICE, spec::ACTION_CREATE_TASK] {
        assert!(
            !spec::VOUCHER_EXCLUDED_ACTIONS.contains(&code),
            "{code} 不能被凭证排除：通知 / 建任务就靠通用引擎的 AfterCommit 动作"
        );
    }

    // 第二道闸：配置期（保存接口 + 配置页预览）必须真的读实体级禁令。
    assert!(
        spec::forbidden_actions(crate::business::shared::state_machine::ENTITY_VOUCHER)
            .contains(&spec::ACTION_GENERATE_VOUCHER),
        "凭证实体没有声明 GENERATE_VOUCHER 禁令：保存时拦不住「凭证生凭证」"
    );
    let model = include_str!("model.rs");
    assert!(
        model.contains("spec::forbidden_action_reason("),
        "写规则的唯一入口 model::upsert_rule 必须读 spec::forbidden_action_reason：\
         只在 spec 里加常量不算「保存时 400」"
    );
    let admin = include_str!("admin.rs");
    assert!(
        admin.contains("spec::forbidden_action_reason("),
        "配置页预览 check_draft / 规则体检也必须读同一条禁令（否则配置页显示「可保存」）"
    );

    // 四个事务入口：审核链（审核 / 反审核 / 记账共用 transition_voucher_in_tx）、
    // 手工凭证落草稿、手工红冲、红冲审批通过。每个都要调 hook 并在 commit 之后 flush。
    let write = include_str!("../../erp/voucher_write.rs");
    assert_eq!(
        write.matches("voucher_linkage::run_in_tx(").count(),
        4,
        "凭证的四个事务入口（手工落草稿 / 审核链 / 手工红冲 / 红冲审批通过）都必须真的调 \
         voucher_linkage::run_in_tx（只改 spec.rs 的 wired 标记不算接线）"
    );
    assert_eq!(
        write
            .matches("voucher_linkage::run_deferred_after_commit(")
            .count(),
        4,
        "四个入口都要在 commit 之后 flush（少一处，那条路径的通知永远不发）"
    );

    // 排除清单在语义上真的会拦（不是只有字符串写对）
    use crate::business::flow::linkage::engine;
    for code in spec::VOUCHER_EXCLUDED_ACTIONS {
        assert!(
            engine::is_routed_elsewhere(spec::VOUCHER_EXCLUDED_ACTIONS, code),
            "{code} 在凭证的排除清单里却没被判成「专用路由」：清单传下去也拦不住它"
        );
    }
}

/// 合同（`CONTRACT`）的接线细节（LNK-07）：**三个状态推进点都要调 hook**，且必须排除
/// 合同上不可能成立的凭证动作。
///
/// 这条防的五种漂移：
///
/// 1. 只在 `spec.rs` 把 `wired` 改成 true、handler / bridge 里没调用（P1-FIX-5C 的原罪：
///    规则能保存、能启用、永远不触发）；
/// 2. 只接 handler 的「生效 / 完成 / 终止」，漏掉**审批通过**（`contract_bridge::settle`）→
///    `CONTRACT/SIGNED` 这条最关键的规则永远不响（合同签了没人知道）；
/// 3. 漏掉「提交审批」那一步 → `CONTRACT/PENDING_APPROVAL` 上配的规则能存能启用但不响；
/// 4. 排除清单丢了 → 通用引擎在合同上跑 `GENERATE_VOUCHER`，按来源单据兜出一张没有
///    借贷科目口径的「合同凭证」，进总账就是脏账；
/// 5. 排除清单把通知 / 建任务也排掉 → 联动配了等于没配（不报错、不落账）。
#[test]
fn contract_wires_generic_engine_and_excludes_voucher() {
    let hook = include_str!("../../erp/contract_linkage.rs");
    assert!(
        hook.contains("linkage::engine::run_excluding("),
        "合同状态变更必须调用通用引擎（run_excluding）：否则 CONTRACT 的联动规则配了不触发"
    );
    assert!(
        hook.contains("&ctx, linkage::spec::CONTRACT_EXCLUDED_ACTIONS"),
        "合同调通用引擎时必须**真的把** spec::CONTRACT_EXCLUDED_ACTIONS 传下去（不许改成写死的 \
         空清单）：只在文档里提这个名字救不了重复落账"
    );
    assert!(
        hook.contains("linkage::engine::run_deferred"),
        "合同的提交后动作必须交给 run_deferred（commit 之后跑），否则通知 / 建任务永远不执行"
    );
    assert!(
        spec::CONTRACT_EXCLUDED_ACTIONS.contains(&spec::ACTION_GENERATE_VOUCHER),
        "合同的排除清单漏了 GENERATE_VOUCHER：合同没有借贷科目口径，跑出来的凭证就是脏账"
    );
    // 第二道闸：配置期（保存接口 + 配置页预览）必须读实体级禁令 ——
    // 合同上跑 GENERATE_VOUCHER 永远只会被 Skipped，不能让管理员配得上一份「看起来生效」的规则。
    assert!(
        spec::forbidden_actions(crate::business::shared::state_machine::ENTITY_CONTRACT)
            .contains(&spec::ACTION_GENERATE_VOUCHER),
        "合同实体没有声明 GENERATE_VOUCHER 禁令：保存时拦不住「配了永远不执行」的规则"
    );
    for code in [spec::ACTION_SEND_NOTICE, spec::ACTION_CREATE_TASK] {
        assert!(
            !spec::CONTRACT_EXCLUDED_ACTIONS.contains(&code),
            "{code} 不能被合同排除：通知 / 建任务就靠通用引擎的 AfterCommit 动作"
        );
    }

    // handler 两条路：提交审批（DRAFT → PENDING_APPROVAL）+ 通用分支（生效 / 完成 / 终止 / 撤回）
    let handler = include_str!("../../erp/contract_handler.rs");
    assert_eq!(
        handler.matches("contract_linkage::run_in_tx(").count(),
        2,
        "合同提交审批与通用流转两条路都必须真的调 contract_linkage::run_in_tx\
         （只改 spec.rs 的 wired 标记不算接线）"
    );
    assert_eq!(
        handler
            .matches("contract_linkage::run_deferred_after_commit(")
            .count(),
        2,
        "提交审批 / 通用流转两个入口都要在 commit 之后 flush（少一处，那条路径的通知永远不发）"
    );

    // 审批回调（`contract_bridge::settle`）：审批通过 / 驳回都要接，且必须仍然先过 settle_guard
    // ——「重复回调不重复触发」的前置闸就在这里，拆了它联动就会跑第二遍。
    let bridge = include_str!("../../erp/contract_bridge.rs");
    assert_eq!(
        bridge.matches("contract_linkage::run_in_tx(").count(),
        1,
        "审批回调（contract_bridge::settle）必须调 contract_linkage::run_in_tx：\
         否则配了已发布 contract_approve 流程的环境里 CONTRACT/SIGNED 的规则永远不触发"
    );
    assert_eq!(
        bridge
            .matches("contract_linkage::run_deferred_after_commit(")
            .count(),
        1,
        "审批回调的提交后动作同样必须在 commit 之后跑（run_deferred_after_commit）"
    );
    assert!(
        bridge.contains("settle_guard::gate("),
        "审批回调必须先过 settle_guard（AlreadySettled 挡重复回调），否则联动会跑第二遍"
    );

    // 排除清单在语义上真的会拦（不是只有字符串写对）
    use crate::business::flow::linkage::engine;
    for code in spec::CONTRACT_EXCLUDED_ACTIONS {
        assert!(
            engine::is_routed_elsewhere(spec::CONTRACT_EXCLUDED_ACTIONS, code),
            "{code} 在合同的排除清单里却没被判成「专用路由」：清单传下去也拦不住它"
        );
    }
    for code in [spec::ACTION_SEND_NOTICE, spec::ACTION_CREATE_TASK] {
        assert!(
            !engine::is_routed_elsewhere(spec::CONTRACT_EXCLUDED_ACTIONS, code),
            "{code} 被误判成专用路由：合同的通知 / 建任务会静默不执行"
        );
    }
}

/// 会计期间（`ACCOUNTING_PERIOD`）的接线细节（LNK-08）：结账 / 反结账两处都调 hook，
/// 且**通知必须排在 `tx.commit()` 之后**（结账是长事务，事务内发通知一旦回滚就是
/// 「通知已发、结账失败」——这是卡面点名的理由）。
///
/// 这条防的五种漂移：
///
/// 1. 只在 `spec.rs` 把 `wired` 改成 true、`period.rs` 里没调用（P1-FIX-5C 的原罪：
///    规则能保存、能启用、永远不触发）；
/// 2. 只接结账、漏掉反结账 → `ACCOUNTING_PERIOD/OPEN` 上配的「反结账要上报」永远不响；
/// 3. 排除清单丢了 → 通用引擎在结账事务里再跑 `GENERATE_VOUCHER`（结账第 ⑤ 步已经
///    出过期末汇兑重估凭证）或写库存 / 应付余额（期间根本没有这些明细列）；
/// 4. 把 `run_deferred_after_commit` 挪进事务里（或在 `tx.commit()` 之前调）→
///    通知先发、结账失败即幽灵通知；
/// 5. 排除清单把通知 / 建任务也排掉 → 联动配了等于没配（不报错、不落账）。
#[test]
fn period_wires_generic_engine_and_excludes_ledger_actions() {
    let hook = include_str!("../../erp/period_linkage.rs");
    assert!(
        hook.contains("linkage::engine::run_excluding("),
        "会计期间状态变更必须调用通用引擎（run_excluding）：否则 ACCOUNTING_PERIOD 的联动规则配了不触发"
    );
    assert!(
        hook.contains("&ctx, linkage::spec::PERIOD_EXCLUDED_ACTIONS"),
        "期间调通用引擎时必须**真的把** spec::PERIOD_EXCLUDED_ACTIONS 传下去（不许改成写死的 \
         空清单）：只在文档里提这个名字救不了重复落账"
    );
    assert!(
        hook.contains("linkage::engine::run_deferred"),
        "期间的提交后动作必须交给 run_deferred（commit 之后跑），否则通知 / 建任务永远不执行"
    );
    for code in [
        "WRITE_INVENTORY",
        "WRITE_INVENTORY_TXN",
        "UPDATE_COST",
        "UPDATE_PAYABLE_BALANCE",
        spec::ACTION_GENERATE_VOUCHER,
    ] {
        assert!(
            spec::PERIOD_EXCLUDED_ACTIONS.contains(&code),
            "期间的排除清单漏了 {code}：结账第 ⑤ 步自己出汇兑重估凭证，期间又没有库存 / \
             应付明细列，通用引擎再跑一次就是重复落账或 ERROR 噪音"
        );
    }
    for code in [spec::ACTION_SEND_NOTICE, spec::ACTION_CREATE_TASK] {
        assert!(
            !spec::PERIOD_EXCLUDED_ACTIONS.contains(&code),
            "{code} 不能被期间排除：通知 / 建任务就靠通用引擎的 AfterCommit 动作"
        );
    }

    // 两个状态推进点：结账（OPEN → CLOSED）与反结账（CLOSED → OPEN）
    let source = include_str!("../../erp/period.rs");
    assert_eq!(
        source.matches("period_linkage::run_in_tx(").count(),
        2,
        "结账 close_period 与反结账 reopen_period 都必须真的调 period_linkage::run_in_tx\
         （只改 spec.rs 的 wired 标记不算接线）"
    );
    assert_eq!(
        source
            .matches("period_linkage::run_deferred_after_commit(")
            .count(),
        2,
        "结账 / 反结账两个入口都要在 commit 之后 flush（少一处，那条路径的通知永远不发）"
    );

    // 铁律：flush 必须在 `tx.commit()` 之后（事务内发通知 = 回滚后幽灵通知）
    let close_start = source
        .find("pub async fn close_period(")
        .expect("period.rs 必须有 close_period");
    let reopen_start = source
        .find("pub async fn reopen_period(")
        .expect("period.rs 必须有 reopen_period");
    assert!(
        close_start < reopen_start,
        "夹具：close_period 应在 reopen_period 之前"
    );
    for (name, body) in [
        ("close_period", &source[close_start..reopen_start]),
        ("reopen_period", &source[reopen_start..]),
    ] {
        let commit = body
            .find("tx.commit()")
            .unwrap_or_else(|| panic!("{name} 必须在事务里 commit"));
        let flush = body
            .find("period_linkage::run_deferred_after_commit(")
            .unwrap_or_else(|| panic!("{name} 必须在 commit 之后调 run_deferred_after_commit"));
        assert!(
            commit < flush,
            "{name}：run_deferred_after_commit 必须排在 tx.commit() **之后**（当前 commit@{commit} \
             / flush@{flush}）—— 事务内发通知一旦回滚就是「通知已发、结账失败」"
        );
        let run_in_tx = body
            .find("period_linkage::run_in_tx(")
            .unwrap_or_else(|| panic!("{name} 必须在事务内调 run_in_tx"));
        assert!(
            run_in_tx < commit,
            "{name}：run_in_tx 必须在 tx.commit() **之前**（事务内动作失败要整笔回滚）"
        );
    }

    // 排除清单在语义上真的会拦（不是只有字符串写对）
    use crate::business::flow::linkage::engine;
    for code in spec::PERIOD_EXCLUDED_ACTIONS {
        assert!(
            engine::is_routed_elsewhere(spec::PERIOD_EXCLUDED_ACTIONS, code),
            "{code} 在期间的排除清单里却没被判成「专用路由」：清单传下去也拦不住它"
        );
    }
    for code in [spec::ACTION_SEND_NOTICE, spec::ACTION_CREATE_TASK] {
        assert!(
            !engine::is_routed_elsewhere(spec::PERIOD_EXCLUDED_ACTIONS, code),
            "{code} 被误判成专用路由：期间的通知 / 建任务会静默不执行"
        );
    }
}

/// 固定资产（`FIXED_ASSET`）的接线细节（LNK-09）：处置 / 作废处置两处都调 hook，
/// 且必须排除会与 `asset_bridge` 的处置凭证 / 红冲重复落账的两类动作。
///
/// 这条防的五种漂移：
///
/// 1. 只在 `spec.rs` 把 `wired` 改成 true、`asset_bridge.rs` / `asset_handler.rs` 里没调用
///    （P1-FIX-5C 的原罪：规则能保存、能启用、永远不触发）；
/// 2. 只接「作废处置」漏掉「处置」→ 最关心的「资产被处置了」永远不响；
///    （处置的真实入口在审批回调里，不在 handler 的 dispose 提交上 —— 提交只是发起审批）
/// 3. 排除清单丢了 → 处置时通用引擎再跑一次 `GENERATE_VOUCHER`：一张卡两套处置凭证，
///    1606 清理科目翻倍（巡检 K7 当场红）；作废处置再跑一次 `REVERSE_VOUCHER`：
///    同一张蓝字凭证红冲两次 → 500；
/// 4. 把 `run_deferred_after_commit` 挪进事务里（或在 `tx.commit()` 之前调）→
///    通知先发、处置失败即幽灵通知；
/// 5. 排除清单把通知 / 建任务也排掉 → 联动配了等于没配（不报错、不落账）。
#[test]
fn asset_wires_generic_engine_and_excludes_voucher() {
    let hook = include_str!("../../erp/asset_linkage.rs");
    assert!(
        hook.contains("linkage::engine::run_excluding("),
        "资产状态变更必须调用通用引擎（run_excluding）：否则 FIXED_ASSET 的联动规则配了不触发"
    );
    assert!(
        hook.contains("&ctx, linkage::spec::FIXED_ASSET_EXCLUDED_ACTIONS"),
        "资产调通用引擎时必须**真的把** spec::FIXED_ASSET_EXCLUDED_ACTIONS 传下去（不许改成 \
         写死的空清单）：只在文档里提这个名字救不了重复落账"
    );
    assert!(
        hook.contains("linkage::engine::run_deferred"),
        "资产的提交后动作必须交给 run_deferred（commit 之后跑），否则通知 / 建任务永远不执行"
    );
    for code in [spec::ACTION_GENERATE_VOUCHER, "REVERSE_VOUCHER"] {
        assert!(
            spec::FIXED_ASSET_EXCLUDED_ACTIONS.contains(&code),
            "固定资产的排除清单漏了 {code}：处置凭证 / 红冲由 asset_bridge 专用路径完成，\
             通用引擎再跑一次就是重复落账"
        );
    }
    for code in [spec::ACTION_SEND_NOTICE, spec::ACTION_CREATE_TASK] {
        assert!(
            !spec::FIXED_ASSET_EXCLUDED_ACTIONS.contains(&code),
            "{code} 不能被资产排除：通知 / 建任务就靠通用引擎的 AfterCommit 动作"
        );
    }

    // 三处真实状态推进：① 处置（asset_bridge::execute，审批通过后）② 作废处置（asset_dispose_cancel）
    // ③ 停用 / 启用（asset_status_change，FIN-ASSET-ST02）—— 后两者都在 handler 里，
    //    所以下面对 handler 的 `run_in_tx(` / `run_deferred_after_commit(` 计数是 **2**。
    let bridge = include_str!("../../erp/asset_bridge.rs");
    assert_eq!(
        bridge.matches("asset_linkage::run_in_tx(").count(),
        1,
        "资产处置（asset_bridge::execute）必须真的调 asset_linkage::run_in_tx\
         （只改 spec.rs 的 wired 标记不算接线）"
    );
    assert_eq!(
        bridge
            .matches("asset_linkage::run_deferred_after_commit(")
            .count(),
        1,
        "处置入口必须在 commit 之后 flush（事务内发通知 = 回滚后幽灵通知）"
    );

    let handler = include_str!("../../erp/asset_handler.rs");
    assert_eq!(
        handler.matches("asset_linkage::run_in_tx(").count(),
        2,
        "作废处置（asset_dispose_cancel）与停用 / 启用（asset_status_change）都必须真的调 \
         asset_linkage::run_in_tx（只改 spec.rs 的 wired 标记不算接线）"
    );
    assert_eq!(
        handler
            .matches("asset_linkage::run_deferred_after_commit(")
            .count(),
        2,
        "作废处置与停用 / 启用两个入口都必须在 commit 之后 flush（事务内发通知 = 回滚后幽灵通知）"
    );

    // 铁律：flush 必须排在 `tx.commit()` 之后。切片段时要避开注释里的字面量
    // （本仓踩过：注释写 `tx.commit()` 会把 find 命中提前，断言假红）。
    let execute = bridge
        .find("async fn execute(instance_id: Uuid)")
        .expect("asset_bridge.rs 必须有 execute");
    let body = &bridge[execute..];
    let commit = body.find("tx.commit()").expect("处置必须在事务里 commit");
    let flush = body
        .find("asset_linkage::run_deferred_after_commit(")
        .expect("处置必须在提交之后 flush");
    assert!(
        commit < flush,
        "asset_bridge::execute：run_deferred_after_commit 必须排在事务提交**之后**\
         （当前 commit@{commit} / flush@{flush}）"
    );
    let run_in_tx = body
        .find("asset_linkage::run_in_tx(")
        .expect("处置必须在事务内调 run_in_tx");
    assert!(
        run_in_tx < commit,
        "asset_bridge::execute：run_in_tx 必须在事务提交**之前**（事务内动作失败要整笔回滚）"
    );

    let cancel = handler
        .find("pub async fn asset_dispose_cancel(")
        .expect("asset_handler.rs 必须有 asset_dispose_cancel");
    let body = &handler[cancel..];
    let commit = body.find("tx.commit()").expect("作废处置必须 commit");
    let flush = body
        .find("asset_linkage::run_deferred_after_commit(")
        .expect("作废处置必须在提交之后 flush");
    assert!(
        commit < flush,
        "asset_dispose_cancel：flush 必须排在事务提交**之后**（当前 commit@{commit} / flush@{flush}）"
    );
    let run_in_tx = body
        .find("asset_linkage::run_in_tx(")
        .expect("作废处置必须在事务内调 run_in_tx");
    assert!(
        run_in_tx < commit,
        "asset_dispose_cancel：run_in_tx 必须在事务提交**之前**"
    );

    // ③ 停用 / 启用（FIN-ASSET-ST02）：handler 必须存在、状态 / 变动类型只用常量，
    //    且 `run_in_tx < tx.commit() < run_deferred_after_commit`（与另两处同序）。
    assert!(
        handler.contains("pub async fn asset_status_change("),
        "asset_handler.rs 必须有 asset_status_change：没有 HTTP 入口时，\
         FIXED_ASSET/IDLE 上配的规则能保存、永远不触发（本卡要修的就是这个）"
    );
    for literal in [
        "asset_dto::STATUS_IDLE",
        "asset_dto::CHANGE_IDLE",
        "asset_dto::CHANGE_ENABLE",
    ] {
        assert!(
            handler.contains(literal),
            "停用 / 启用必须直接用状态 / 变动类型常量 {literal}\
             （写字符串字面量会与状态机 / 词表悄悄漂移）"
        );
    }
    let status_change = handler
        .find("pub async fn asset_status_change(")
        .expect("asset_handler.rs 必须有 asset_status_change");
    let body = &handler[status_change..];
    let run_in_tx = body
        .find("asset_linkage::run_in_tx(")
        .expect("停用 / 启用必须在事务内调 run_in_tx");
    let commit = body.find("tx.commit()").expect("停用 / 启用必须 commit");
    let flush = body
        .find("asset_linkage::run_deferred_after_commit(")
        .expect("停用 / 启用必须在提交之后 flush");
    assert!(
        run_in_tx < commit && commit < flush,
        "asset_status_change 的顺序必须是 run_in_tx < tx.commit() < run_deferred_after_commit\
         （当前 run_in_tx@{run_in_tx} / commit@{commit} / flush@{flush}）"
    );

    // 排除清单在语义上真的会拦（不是只有字符串写对）
    use crate::business::flow::linkage::engine;
    for code in spec::FIXED_ASSET_EXCLUDED_ACTIONS {
        assert!(
            engine::is_routed_elsewhere(spec::FIXED_ASSET_EXCLUDED_ACTIONS, code),
            "{code} 在资产的排除清单里却没被判成「专用路由」：清单传下去也拦不住它"
        );
    }
    for code in [spec::ACTION_SEND_NOTICE, spec::ACTION_CREATE_TASK] {
        assert!(
            !engine::is_routed_elsewhere(spec::FIXED_ASSET_EXCLUDED_ACTIONS, code),
            "{code} 被误判成专用路由：资产的通知 / 建任务会静默不执行"
        );
    }
}

/// 折旧批次（`DEPRECIATION`）的接线细节（LNK-10）：过账 / 红冲两处都调 hook，
/// 且必须排除会与批次自己的凭证落账重复的两类动作。
///
/// 这条防的五种漂移：
///
/// 1. 只在 `spec.rs` 把 `wired` 改成 true、`depreciation_model.rs` 里没调用
///    （P1-FIX-5C 的原罪：规则能保存、能启用、永远不触发）；
/// 2. 只接「过账」漏掉「红冲」→ `DEPRECIATION/REVERSED` 上的规则永远不响；
/// 3. 排除清单丢了 → 过账时通用引擎再跑一次 `GENERATE_VOUCHER`：同一期间第二张折旧凭证，
///    累计折旧科目直接翻倍；红冲时再跑 `REVERSE_VOUCHER`：同一张蓝字凭证被红冲两次 → 500；
/// 4. 把 `run_deferred_after_commit` 挪进事务里（或在 `tx.commit()` 之前调）→
///    批次是**大批量长事务**，通知先发、计提失败即「财务收到某月已计提、实际没提」；
/// 5. 排除清单把通知 / 建任务也排掉 → 联动配了等于没配（不报错、不落账）。
#[test]
fn depreciation_wires_generic_engine_and_excludes_voucher() {
    let hook = include_str!("../../erp/depreciation_linkage.rs");
    assert!(
        hook.contains("linkage::engine::run_excluding("),
        "折旧批次状态变更必须调用通用引擎（run_excluding）：否则 DEPRECIATION 的联动规则配了不触发"
    );
    assert!(
        hook.contains("&ctx, linkage::spec::DEPRECIATION_EXCLUDED_ACTIONS"),
        "折旧批次调通用引擎时必须**真的把** spec::DEPRECIATION_EXCLUDED_ACTIONS 传下去\
         （不许改成写死的空清单）：只在文档里提这个名字救不了重复落账"
    );
    assert!(
        hook.contains("linkage::engine::run_deferred"),
        "折旧批次的提交后动作必须交给 run_deferred（commit 之后跑），否则通知 / 建任务永远不执行"
    );
    for code in [spec::ACTION_GENERATE_VOUCHER, "REVERSE_VOUCHER"] {
        assert!(
            spec::DEPRECIATION_EXCLUDED_ACTIONS.contains(&code),
            "折旧批次的排除清单漏了 {code}：计提凭证由 voucher_multi::post_multi_debit 出、\
             红冲由 voucher::reverse_posted 做，通用引擎再跑一次就是重复落账"
        );
    }
    for code in [spec::ACTION_SEND_NOTICE, spec::ACTION_CREATE_TASK] {
        assert!(
            !spec::DEPRECIATION_EXCLUDED_ACTIONS.contains(&code),
            "{code} 不能被折旧批次排除：通知 / 建任务就靠通用引擎的 AfterCommit 动作"
        );
    }

    // 两处真实状态推进：① 过账（run_batch ⑤ 步）② 红冲（reverse_batch ⑥ 步）
    let model = include_str!("../../erp/depreciation_model.rs");
    assert_eq!(
        model.matches("depreciation_linkage::run_in_tx(").count(),
        2,
        "折旧批次必须在**两处**真的调 depreciation_linkage::run_in_tx（过账 + 红冲）：
         只改 spec.rs 的 wired 标记不算接线"
    );
    assert_eq!(
        model
            .matches("depreciation_linkage::run_deferred_after_commit(")
            .count(),
        2,
        "过账与红冲都必须在 commit 之后 flush（事务内发通知 = 回滚后幽灵通知）"
    );

    // 铁律：flush 必须排在 `tx.commit()` 之后、`run_in_tx` 必须排在之前。
    // 切片段时避开注释里的字面量（本仓踩过：注释写 `tx.commit()` 会把 find 命中提前，断言假红）。
    for (marker, label) in [
        ("pub async fn run_batch(", "过账（run_batch）"),
        ("pub async fn reverse_batch(", "红冲（reverse_batch）"),
    ] {
        let start = model
            .find(marker)
            .unwrap_or_else(|| panic!("depreciation_model.rs 必须有 {label}"));
        let body = &model[start..];
        let commit = body
            .find("tx.commit()")
            .unwrap_or_else(|| panic!("{label} 必须在事务里 commit"));
        let flush = body
            .find("depreciation_linkage::run_deferred_after_commit(")
            .unwrap_or_else(|| panic!("{label} 必须在提交之后 flush"));
        assert!(
            commit < flush,
            "{label}：run_deferred_after_commit 必须排在事务提交**之后**\
             （当前 commit@{commit} / flush@{flush}）"
        );
        let run_in_tx = body
            .find("depreciation_linkage::run_in_tx(")
            .unwrap_or_else(|| panic!("{label} 必须在事务内调 run_in_tx"));
        assert!(
            run_in_tx < commit,
            "{label}：run_in_tx 必须在事务提交**之前**（事务内动作失败要整笔回滚）"
        );
    }

    // 排除清单在语义上真的会拦（不是只有字符串写对）
    use crate::business::flow::linkage::engine;
    for code in spec::DEPRECIATION_EXCLUDED_ACTIONS {
        assert!(
            engine::is_routed_elsewhere(spec::DEPRECIATION_EXCLUDED_ACTIONS, code),
            "{code} 在折旧批次的排除清单里却没被判成「专用路由」：清单传下去也拦不住它"
        );
    }
    for code in [spec::ACTION_SEND_NOTICE, spec::ACTION_CREATE_TASK] {
        assert!(
            !engine::is_routed_elsewhere(spec::DEPRECIATION_EXCLUDED_ACTIONS, code),
            "{code} 被误判成专用路由：折旧批次的通知 / 建任务会静默不执行"
        );
    }
}

/// 会计年度（`ACCOUNTING_YEAR`）的接线细节（LNK-11）：年结 / 反年结两处都调 hook，
/// 且**通知必须排在 `tx.commit()` 之后**（年结是全仓最长的账务事务，事务内发通知一旦回滚
/// 就是「财务收到年度已结转、实际还开着」——卡面点名的最高风险）。
///
/// 这条防的六种漂移：
///
/// 1. 只在 `spec.rs` 把 `wired` 改成 true、`year_end.rs` 里没调用（P1-FIX-5C 的原罪）；
/// 2. 只接年结、漏掉反年结 → `ACCOUNTING_YEAR/NOT_CLOSED` 上配的「反年结要上报」永远不响；
/// 3. 排除清单丢了或没传下去 → 通用引擎在年结事务里再跑 `GENERATE_VOUCHER`（年结第 ① 步
///    已经出过唯一一张 `YEAR_END` 结转凭证，再出一次撞 `uk_biz_voucher_year_end_period`）
///    或写库存 / 应付余额（年度根本没有这些明细列）；
/// 4. 把 `run_deferred_after_commit` 挪进事务里（或在 `tx.commit()` 之前调）→
///    通知先发、年结失败即幽灵通知；
/// 5. 排除清单把通知 / 建任务也排掉 → 联动配了等于没配（不报错、不落账）；
/// 6. 排除清单只排凭证、忘了库存 / 余额（卡面明确要求三类都排）。
#[test]
fn year_end_wires_generic_engine_and_excludes_ledger_actions() {
    let hook = include_str!("../../erp/year_end_linkage.rs");
    assert!(
        hook.contains("linkage::engine::run_excluding("),
        "年度结转必须调用通用引擎（run_excluding）：否则 ACCOUNTING_YEAR 的联动规则配了不触发"
    );
    assert!(
        hook.contains("&ctx, linkage::spec::YEAR_END_EXCLUDED_ACTIONS"),
        "年结调通用引擎时必须**真的把** spec::YEAR_END_EXCLUDED_ACTIONS 传下去（不许改成写死的 \
         空清单）：只在文档里提这个名字救不了重复落账"
    );
    assert!(
        hook.contains("linkage::engine::run_deferred"),
        "年结的提交后动作必须交给 run_deferred（commit 之后跑），否则通知 / 建任务永远不执行"
    );

    // 卡面点名要排除的三类：库存 / 余额 / 凭证
    for code in [
        "WRITE_INVENTORY",
        "WRITE_INVENTORY_TXN",
        "UPDATE_COST",
        "UPDATE_PAYABLE_BALANCE",
        spec::ACTION_GENERATE_VOUCHER,
        "REVERSE_VOUCHER",
    ] {
        assert!(
            spec::YEAR_END_EXCLUDED_ACTIONS.contains(&code),
            "年结的排除清单漏了 {code}：年结第 ① 步自己出结转凭证、第 ② 步写期初台账，\
             年度又没有库存 / 应付明细列，通用引擎再跑一次就是重复落账或 ERROR 噪音"
        );
    }
    for code in [spec::ACTION_SEND_NOTICE, spec::ACTION_CREATE_TASK] {
        assert!(
            !spec::YEAR_END_EXCLUDED_ACTIONS.contains(&code),
            "{code} 不能被年结排除：通知 / 建任务就靠通用引擎的 AfterCommit 动作"
        );
    }

    // 两个状态推进点：年结（NOT_CLOSED → CLOSED）与反年结（CLOSED → NOT_CLOSED）
    let source = include_str!("../../erp/year_end.rs");
    assert_eq!(
        source.matches("year_end_linkage::run_in_tx(").count(),
        2,
        "年结 close_year 与反年结 reopen_year 都必须真的调 year_end_linkage::run_in_tx\
         （只改 spec.rs 的 wired 标记不算接线）"
    );
    assert_eq!(
        source
            .matches("year_end_linkage::run_deferred_after_commit(")
            .count(),
        2,
        "年结 / 反年结两个入口都要在 commit 之后 flush（少一处，那条路径的通知永远不发）"
    );

    // 铁律：flush 必须在 `tx.commit()` 之后；run_in_tx 必须在 commit 之前
    let close_start = source
        .find("pub async fn close_year(")
        .expect("year_end.rs 必须有 close_year");
    let reopen_start = source
        .find("pub async fn reopen_year(")
        .expect("year_end.rs 必须有 reopen_year");
    assert!(
        close_start < reopen_start,
        "夹具：close_year 应在 reopen_year 之前"
    );
    for (name, body) in [
        ("close_year", &source[close_start..reopen_start]),
        ("reopen_year", &source[reopen_start..]),
    ] {
        let commit = body
            .find("tx.commit()")
            .unwrap_or_else(|| panic!("{name} 必须在事务里 commit"));
        let flush = body
            .find("year_end_linkage::run_deferred_after_commit(")
            .unwrap_or_else(|| panic!("{name} 必须在 commit 之后调 run_deferred_after_commit"));
        assert!(
            commit < flush,
            "{name}：run_deferred_after_commit 必须排在 tx.commit() **之后**（当前 commit@{commit} \
             / flush@{flush}）—— 事务内发通知一旦回滚就是「通知已发、年结失败」"
        );
        let run_in_tx = body
            .find("year_end_linkage::run_in_tx(")
            .unwrap_or_else(|| panic!("{name} 必须在事务内调 run_in_tx"));
        assert!(
            run_in_tx < commit,
            "{name}：run_in_tx 必须在 tx.commit() **之前**（事务内动作失败要整笔回滚）"
        );
    }

    // 排除清单在语义上真的会拦（不是只有字符串写对）
    use crate::business::flow::linkage::engine;
    for code in spec::YEAR_END_EXCLUDED_ACTIONS {
        assert!(
            engine::is_routed_elsewhere(spec::YEAR_END_EXCLUDED_ACTIONS, code),
            "{code} 在年结的排除清单里却没被判成「专用路由」：清单传下去也拦不住它"
        );
    }
    for code in [spec::ACTION_SEND_NOTICE, spec::ACTION_CREATE_TASK] {
        assert!(
            !engine::is_routed_elsewhere(spec::YEAR_END_EXCLUDED_ACTIONS, code),
            "{code} 被误判成专用路由：年结的通知 / 建任务会静默不执行"
        );
    }
}

/// 审批实例（`FLOW_INSTANCE`）的接线细节（LNK-12）。
///
/// 这条防的是本卡最容易假绿的两件事：
///
/// 1. **只改 spec.rs 的 wired 标记**（P1-FIX-5C 的原罪）：所以逐个数调用点 ——
///    `flow/handler.rs` 三个（approve / reject / withdraw）+ `flow/timeout.rs` 一个
///    （超时自动代点，没有 HTTP handler 的子路径，最容易漏）；
/// 2. **顺序反了**：`fire_after_commit` 必须排在 `task_bridge` / `purchase_bridge` 之前
///    （实例是原因、单据是结果；桥的错误被吞掉，排在后面等于「桥挂了通知就没了」）。
///
/// 另外钉住「可配状态 = 会触发的状态」：`spec` 的 status_labels、写入口的
/// `model::known_statuses`、hook 的入口状态闸三者同集，否则就是「配得上、永远不响」。
#[test]
fn flow_instance_wires_generic_engine_and_excludes_ledger_actions() {
    // ---------- ① hook 本身真的调引擎（不是只有注释） ----------
    let hook = include_str!("../instance_linkage.rs");
    assert!(
        hook.contains("linkage::engine::run_excluding("),
        "审批实例联动 hook 必须调用通用引擎（run_excluding）：否则 FLOW_INSTANCE 的规则配了不触发"
    );
    assert!(
        hook.contains("spec::FLOW_INSTANCE_EXCLUDED_ACTIONS"),
        "调通用引擎时必须传 spec::FLOW_INSTANCE_EXCLUDED_ACTIONS（不许改成写死的空清单）"
    );
    assert!(
        hook.contains("linkage::engine::run_deferred"),
        "提交后动作必须交给 run_deferred（审批事务已提交，InTx 动作一律不许跑）"
    );

    // ---------- ② 四个调用点，一个不能少 ----------
    let handler = include_str!("../handler.rs");
    assert_eq!(
        handler
            .matches("instance_linkage::fire_after_commit(")
            .count(),
        4,
        "approve_task / reject_task / withdraw_instance / create_instance（发起即自动走完）\
         四个位置都必须真的调 hook（只改 spec.rs 的 wired 标记不算接线）"
    );
    let timeout = include_str!("../timeout.rs");
    assert!(
        timeout.contains("instance_linkage::fire_after_commit("),
        "超时自动代点（timeout::auto_decide）也必须调 hook：这条路径没有 HTTP handler，\
         漏了就是「人工审批会响、自动审批不响」的半接线"
    );

    // ---------- ③ 顺序：审批实例先、业务单据后 ----------
    let approve_start = handler
        .find("pub async fn approve_task(")
        .expect("handler.rs 必须有 approve_task");
    let reject_start = handler
        .find("pub async fn reject_task(")
        .expect("handler.rs 必须有 reject_task");
    assert!(
        approve_start < reject_start,
        "夹具：approve_task 应在 reject_task 之前"
    );
    let approve = &handler[approve_start..reject_start];
    let fire = approve
        .find("instance_linkage::fire_after_commit(")
        .expect("approve_task 必须在审批事务提交之后调 hook");
    let first_bridge = approve
        .find("task_bridge::on_instance_settled(")
        .expect("approve_task 必须调 task_bridge");
    let po_bridge = approve
        .find("purchase_bridge::on_po_settled(")
        .expect("approve_task 必须调 purchase_bridge");
    assert!(
        fire < first_bridge && fire < po_bridge,
        "FLOW_INSTANCE 触发点必须排在业务桥**之前**（当前 fire@{fire} / \
         task_bridge@{first_bridge} / purchase_bridge@{po_bridge}）：\
         业务桥的错误被吞掉，排在它们后面就是「桥挂了 → 审批通过的通知永远丢」"
    );
    let drain = approve
        .find("drain(&mut out)")
        .expect("approve_task 必须先 drain 引擎通知");
    assert!(
        drain < fire,
        "hook 必须在 drain（事务后副作用）之后调，否则引擎自己的通知还没发出就跑了联动"
    );

    // ---------- ④ 排除清单真的有牙 ----------
    for code in [
        "WRITE_INVENTORY",
        "WRITE_INVENTORY_TXN",
        "UPDATE_COST",
        spec::ACTION_GENERATE_VOUCHER,
        "REVERSE_VOUCHER",
        spec::ACTION_UPDATE_RECEIVED_QTY,
        "UPDATE_PAYABLE_BALANCE",
        "UPDATE_DOC_STATUS",
        "LOCK_DOCUMENT",
    ] {
        assert!(
            spec::FLOW_INSTANCE_EXCLUDED_ACTIONS.contains(&code),
            "审批实例的排除清单漏了 {code}：它由业务桥的专用实现落账，通用引擎再跑一次就是重复落账"
        );
        assert!(
            spec::FLOW_INSTANCE_FORBIDDEN_ACTIONS.contains(&code),
            "配置期禁令漏了 {code}：能配得上但永远不执行（或更糟：跑成无事务裸写）"
        );
    }
    for code in [spec::ACTION_SEND_NOTICE, spec::ACTION_CREATE_TASK] {
        assert!(
            !spec::FLOW_INSTANCE_EXCLUDED_ACTIONS.contains(&code),
            "{code} 不能被排除：审批结论的「通知 / 建任务」就靠通用引擎的 AfterCommit 动作"
        );
    }
    use crate::business::flow::linkage::engine;
    for code in spec::FLOW_INSTANCE_EXCLUDED_ACTIONS {
        assert!(
            engine::is_routed_elsewhere(spec::FLOW_INSTANCE_EXCLUDED_ACTIONS, code),
            "{code} 在排除清单里却没被判成「专用路由」：清单传下去也拦不住它"
        );
    }

    // ---------- ⑤ 可配状态 = 会触发的状态（P1-FIX-5C：不许留「配了不响」的状态） ----------
    let decl =
        spec::trigger_spec(spec::TRIGGER_FLOW_INSTANCE).expect("FLOW_INSTANCE 必须在接线表里");
    let codes: Vec<&str> = decl.status_labels.iter().map(|(code, _)| *code).collect();
    assert_eq!(
        codes,
        vec!["APPROVED", "REJECTED", "WITHDRAWN"],
        "审批实例只允许配三种**结论**：PENDING（发起，流程通知节点负责）/ SKIPPED（从来不是\
         实例状态）/ CANCELLED（级联终止，其中删单据那条走 SQL 不经过引擎）配了都不会响"
    );
    assert_eq!(
        super::model::known_statuses(spec::TRIGGER_FLOW_INSTANCE),
        codes,
        "写入口的 known_statuses 必须与 spec 的可配状态同集"
    );
    assert_eq!(
        spec::forbidden_actions(spec::TRIGGER_FLOW_INSTANCE),
        spec::FLOW_INSTANCE_EXCLUDED_ACTIONS,
        "配置期禁令与运行时排除必须同集，否则会出现「配得上、跑起来被静默跳过」"
    );
}

// ============================================================================
// 采购退货（PURCHASE_RETURN）接线守卫（P2-03 · 补 TST-01）
// ============================================================================

/// 采购退货的接线细节：**确认退货是唯一个动账点**，必须真的走 `run_excluding` +
/// 专用排除清单，且通知上下文（`with_notice`）必须在。
///
/// 这条防的四种漂移（前三种与出库 / 调拨同型，第四种是本触发点独有的）：
///
/// 1. `spec.rs` 把 `wired` 改成 true、handler 里却没调（P1-FIX-5C 的原罪）；
/// 2. 排除清单从 `GENERATE_VOUCHER` 里丢一项 → 一张退货单两张冲销凭证（凭证生凭证）；
/// 3. 排除清单丢了库存 / 应付余额动作 → 同一行明细扣两次库存、`unpaid_amount` 直接负数；
/// 4. 上下文没带通知草稿 → `SEND_NOTICE` **每次都 Skipped**（日志里看得见、界面上什么都没变），
///    V0351 预置的那条规则就成了摆设。这正是 V0323 记录过的「看起来配好了，实际不工作」。
///
/// 另：`spec.rs` 的 `PURCHASE_RETURN_EXCLUDED_ACTIONS` 文档注释里点名了本测试，
/// 删 / 改名会让那条注释变成死链接。
#[test]
fn purchase_return_wires_generic_engine_and_excludes_accounting() {
    let handler = include_str!("../../erp/purchase_return_handler.rs");
    assert!(
        handler.contains("linkage::engine::run_excluding("),
        "采购退货确认必须调用通用引擎（run_excluding）：否则 PURCHASE_RETURN/CONFIRMED 的联动规则配了也不触发"
    );
    assert!(
        handler.contains("linkage_spec::PURCHASE_RETURN_EXCLUDED_ACTIONS"),
        "采购退货确认调通用引擎时必须传 spec::PURCHASE_RETURN_EXCLUDED_ACTIONS（不许改成写死的空清单）"
    );
    assert!(
        handler.contains("linkage::engine::run_deferred"),
        "采购退货的提交后动作必须交给 run_deferred（commit 之后跑），否则通知 / 建任务永远不执行"
    );
    for code in [
        "WRITE_INVENTORY",
        "WRITE_INVENTORY_TXN",
        "UPDATE_COST",
        "UPDATE_PAYABLE_BALANCE",
        spec::ACTION_GENERATE_VOUCHER,
    ] {
        assert!(
            spec::PURCHASE_RETURN_EXCLUDED_ACTIONS.contains(&code),
            "采购退货的排除清单漏了 {code}：它在 purchase_return_posting::confirm_tx 有专用落账，\
             通用引擎再跑一次就是重复落账"
        );
    }
    for code in [spec::ACTION_SEND_NOTICE, spec::ACTION_CREATE_TASK] {
        assert!(
            !spec::PURCHASE_RETURN_EXCLUDED_ACTIONS.contains(&code),
            "{code} 不能被采购退货排除：通知 / 建任务就靠通用引擎的 AfterCommit 动作"
        );
    }

    // ④ 通知草稿：`SEND_NOTICE` 的基线（配置不覆盖时逐字沿用触发点算出来的文案 / 收件人）。
    assert!(
        handler.contains(".with_notice("),
        "采购退货的 LinkageContext 必须附通知草稿（with_notice）：否则 SEND_NOTICE 规则永远 Skipped"
    );

    // 触发点本身必须标 wired（接线表里查得到）。
    let decl = spec::trigger_spec(crate::business::shared::state_machine::ENTITY_PURCHASE_RETURN)
        .expect("PURCHASE_RETURN 必须在 spec::TRIGGER_SPECS 里（白名单与接线表逐字同序）");
    assert!(
        decl.wired,
        "PURCHASE_RETURN 必须标 wired = true —— confirm_return 真的在调引擎"
    );
}
