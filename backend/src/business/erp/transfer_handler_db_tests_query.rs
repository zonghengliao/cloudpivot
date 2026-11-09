// ============================================================================
// transfer_handler_db_tests_query.rs —— 真库：详情（明细 + 序列号分组）与分页过滤
//
// 由 `transfer_handler.rs` 按职责拆出（TD-2 拆分）。**本文件不含任何行为变更**：
// 所有函数体、SQL、签名与拆分前一致，只是换了落点；公开 API 由 facade
// `transfer_handler.rs` 用 `pub use` 重导出，调用方一行不用改。
// ============================================================================

use super::db_tests_fixtures::*;
use super::*;
use crate::business::erp::transfer_model::{self, STATUS_PENDING, TransferQuery};

// ---------- ⑱ 详情 / 序列号分组 ----------
#[tokio::test]
async fn detail_and_serials_are_grouped_by_line() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let mut sn_line = item(M_SN, "2");
    sn_line.serial_ids = vec![SERIAL_1.to_string(), SERIAL_2.to_string()];
    let id = create_transfer(&body(vec![item(M_PLAIN, "1"), sn_line]), Some(uuid(U_APPLICANT)), None)
        .await
        .expect("建单");
    let detail = detail_of(id).await.expect("详情");
    assert_eq!(detail.items.len(), 2);
    assert_eq!(detail.items[0].serial_ids.len(), 0);
    assert_eq!(detail.items[1].serial_ids.len(), 2);
    assert_eq!(detail.items[1].material_code.as_deref(), Some("W3B-M-SN"));

    let serials = serials_of_transfer(id).await.expect("序列号分组");
    assert_eq!(serials.total, 2);
    assert_eq!(serials.groups.len(), 2, "两行都要出现（空行也是空分组）");
    let sn_group = serials
        .groups
        .iter()
        .find(|group| group.line_no == 2)
        .expect("第 2 行的分组");
    assert_eq!(sn_group.serials.len(), 2);
    assert!(
        sn_group
            .serials
            .iter()
            .all(|serial| serial.serial_no.as_deref().unwrap_or("").starts_with("W3B-SN-")),
        "必须回读出序列号文本"
    );
}

// ---------- ⑲ 列表：状态 / 仓库筛选 + 分页 ----------
#[tokio::test]
async fn page_filters_by_status_and_warehouse() {
    let Some(_guard) = prepare().await else {
        return;
    };
    let applicant = uuid(U_APPLICANT);
    let pending = create_transfer(&body(vec![item(M_PLAIN, "1")]), Some(applicant), None)
        .await
        .expect("建单");
    change_transfer_status(pending, &status_req(STATUS_PENDING, None), applicant)
        .await
        .expect("提交审批");
    create_transfer(&body(vec![item(M_PLAIN, "2")]), Some(applicant), None)
        .await
        .expect("建单");

    let query = TransferQuery {
        status: Some(STATUS_PENDING.to_string()),
        from_warehouse_id: Some(uuid(WH_FROM)),
        to_warehouse_id: Some(uuid(WH_TO)),
        transfer_no: None,
        page: 1,
        page_size: 50,
        ..Default::default()
    };
    let (rows, total) = transfer_model::page(&query).await.expect("分页");
    assert!(total >= 1);
    assert!(rows.iter().all(|row| row.status == STATUS_PENDING), "状态筛选必须生效");
    assert!(
        rows.iter().all(|row| row.from_warehouse_id == uuid(WH_FROM)),
        "仓库筛选必须生效"
    );
    // 单号前缀筛选能命中自己
    let no = refresh(pending).await.transfer_no;
    let query = TransferQuery {
        from_warehouse_id: Some(uuid(WH_FROM)),
        transfer_no: Some(no.clone()),
        page: 1,
        page_size: 10,
        ..Default::default()
    };
    let (rows, _) = transfer_model::page(&query).await.expect("分页");
    assert_eq!(rows.len(), 1, "单号前缀应当唯一命中：{no}");
    assert_eq!(rows[0].transfer_no, no);
}
