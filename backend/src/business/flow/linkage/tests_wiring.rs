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
    assert_eq!(entities.len(), 15, "当前白名单是 15 个触发点");
    assert_eq!(
        spec::wired_triggers(),
        vec![
            "STOCK_IN",
            "STOCK_OUT",
            "STOCK_TRANSFER",
            "PURCHASE_ORDER",
            "PAYABLE",
            "VOUCHER",
            "STOCK_TAKE",
            "EXPENSE_REPORT",
            "TASK"
        ],
        "已接线的触发点：STOCK_IN（收货过账）、STOCK_OUT（出库过账，P1-FIX-5C 新接）、\
         STOCK_TRANSFER（库存调拨状态变更，LNK-01 新接）、\
         PURCHASE_ORDER（采购单状态变更 + 部分/全部收货，LNK-02 新接）、\
         PAYABLE（应付单付款 / 部分付款 / 结清，LNK-03 新接）、\
         VOUCHER（凭证手工落草稿 / 审核 / 反审核 / 记账 / 红冲，LNK-06 新接）、\
         STOCK_TAKE（盘点单过账 / 作废，LNK-04 新接）、\
         EXPENSE_REPORT（报销单提交 / 通过 / 付清 / 作废 / 打回，LNK-05 新接）、\
         TASK（任务状态变更）；\
         接线状态变化时这份断言要跟着改，改之前先想清楚「真的有人调引擎了吗」"
    );
    assert_eq!(spec::unwired_triggers().len(), 6, "其余 6 个仍未接线");
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
        10,
        "当前真实调用点 10 个文件（收货过账 / 出库过账 handler + 出库联动 hook + 调拨联动 hook + \
         采购联动 hook + 应付联动 hook + 盘点联动 hook + 报销联动 hook + 凭证联动 hook + 任务状态通知）：{actual:?}"
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
