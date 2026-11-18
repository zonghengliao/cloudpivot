// ============================================================================
// stockin_handler_tests_zz_shared_roles.rs —— 模块收尾：把**共享种子角色**上的借出/自建绑定收干净
//
// 卡 `t_18b222b9`（测试卫生·ERP）：同一库连跑两轮全量时，第二轮 15 条红。
//
// # 为什么单独一个模块、名字还带 `zz`
//
// libtest 按**完整测试路径**排序跑（同模块内 `zz_` 必然最后），而模块路径本身也参与排序：
// `...tests::stockin_handler_tests_fixtures::zz_module_namespace_has_no_residue` 这个名字虽然
// 以 `zz_` 开头，却排在 `...tests::stockin_handler_tests_p16::*` **之前**（`f` < `p`）——
// 于是它清理完，后面的收货用例又把 `p3a_wh_mgr` / `p16_reviewer` 绑回 `WAREHOUSE_MANAGER`，
// 全量跑完库里就永远留着这两个夹具成员（仓库里 `t_9476d9f1` 也记过「别依赖 `zz_` 排序」这件事）。
//
// 所以这里用**模块名**排到最后（`stockin_handler_tests_zz_shared_roles` > 所有
// `stockin_handler_tests_*`），在模块最后一个跑，把共享角色上的东西收口：
// ① 摘掉本模块夹具自建的绑定（自己的临时账号不该在活儿干完之后还占着岗位）；
// ② 把**借出**的绑定（演示种子 `demo_wh` / 兄弟模块的）原样挂回；
// ③ 按 `system::setup::model::DEMO_USERS` 逐条对账「演示账号在它该在的岗位上」——
//    漏还一条当场点名，不靠人肉 psql 复查。
// ============================================================================

use super::stockin_handler_tests_fixtures::{ROLE_WH_MGR, U_CANCEL_REVIEWER, U_WH_MGR, ensure_db, uid_of};
use crate::business::erp::fixture_hygiene;
use nik::prelude::*;

/// 模块收尾：`WAREHOUSE_MANAGER` 上只该剩「种子 + 别人自己的绑定」。
#[tokio::test]
async fn zz_teardown_returns_shared_role_bindings() {
    if !ensure_db().await {
        return;
    }
    let _guard = crate::business::erp::fixture_hygiene::SEED_ROLE_LOCK.lock().await;

    // ① 摘掉本模块夹具（`p3a_wh_mgr` / `p16_reviewer`）自建的绑定，并从借出台账里划掉
    fixture_hygiene::release_fixture_role_bindings(
        &[uid_of(ROLE_WH_MGR)],
        &[uid_of(U_WH_MGR), uid_of(U_CANCEL_REVIEWER)],
    )
    .await;

    // ②③ 归还借出的绑定 + 逐条对账演示种子岗位绑定
    fixture_hygiene::restore_and_assert_role_bindings("stockin::zz_teardown_returns_shared_role_bindings").await;

    // ④ 再回库确认：`WAREHOUSE_MANAGER` 上不许有本模块夹具账号（自己造的自己收）
    let strangers: i64 = db()
        .scalar_with_params::<Option<i64>>(
            "SELECT count(*) FROM sys_user_role ur \
               JOIN sys_user u ON u.id = ur.user_id \
              WHERE ur.role_id = ?::uuid AND u.username IN ('p3a_wh_mgr', 'p16_reviewer')",
            vec![Value::from(&uid_of(ROLE_WH_MGR))],
        )
        .await
        .expect("查本模块夹具残留绑定")
        .unwrap_or(0);
    assert_eq!(
        strangers, 0,
        "模块收尾后 WAREHOUSE_MANAGER 上仍留着 stockin 夹具账号的绑定（{strangers} 行）—— \
         同一库连跑第二遍的基线就会漂"
    );
    println!("[stockin] 收尾：WAREHOUSE_MANAGER 上的夹具绑定 0 行，演示种子岗位绑定完好 ✓");
}
