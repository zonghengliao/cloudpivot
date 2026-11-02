//! 合同数据层（A1 合同表基座）：**乙方引用解析**与状态机接线点。
//!
//! 依据：`docs/architecture/ARCH-2-合同表数据模型与校验规则.md`
//!       §3.2 的 V-11 / V-15 与 §3.2.3 的批次 2 替换点 R2；表结构见 `V0195__erp_contract.up.sql`。
//!
//! # 本文件为什么这么小（A1 的边界）
//!
//! A1 只交付**基座**：建表、状态机注册、错误码、权限码、乙方引用解析。
//! 合同的 CRUD / 列表 / 详情 / 状态流转接口是 A2（`contract_handler` / `contract_dto`），
//! 采购单挂合同是 A3，入库侧是 A4，前端是 A5。所以这里**故意没有**：
//! `ContractRow` 结构体、分页查询、取号（`next_contract_no`）、建改删。它们进 A2 时
//! 直接在本文件往下加（模块已声明、`erp/mod.rs` 已挂载，不需要再动装配）。
//!
//! # 乙方没有外键（DEC-02）——这是设计意图，不是漏了
//!
//! `biz_contract.party_b_id` 是**弱引用**（`biz_supplier`，批次 2 起还有 `biz_customer`）。
//! POC 实测：往合同里写一个指向不存在供应商的 id，**DB 写入成功**；随后孤儿巡检 SQL
//! 精准捞出这一行。这就是 DEC-02 的代价与配套：
//!
//! ```text
//! DB 不拦  →  应用层写入校验（本文件的 party_b_ref，A2 在 ensure_party_b 里调）
//!          →  每日巡检兜底（ARCH-2 §3.3 Q1，落地在 B1 的巡检卡）
//! ```
//!
//! **不要"顺手"给 `party_b_id` 补外键**：`.down.sql` 会因此不可回滚、软删主档会连带
//! 失败，而且违反 ARCH-2 §2.3 末段的实测结论。
//!
//! # 批次 2 的替换点（R2，写在这里免得后人翻文档）
//!
//! 客户主数据上线后，本文件只改一处：给 [`party_b_ref`] 增加 `CUSTOMER` 分支
//! （查 `biz_customer(id, deleted_at IS NULL, status = 1)` 返回 `customer_name`）。
//! 若客户表的列名不同（如 `customer_status`），映射**只能写在本函数内**，不外溢。

use nik::prelude::*;
use nik::uuid::Uuid;

/// 合同表名（A2 复用，避免各处硬编码）。
pub const CONTRACT_TABLE: &str = "biz_contract";

/// 乙方类型：供应商（`party_b_id → biz_supplier.id`）。
pub const PARTY_B_SUPPLIER: &str = "SUPPLIER";

/// 乙方类型：客户（表属批次 2，本期写入硬拒 66020，见 ARCH-2 §3.2.3）。
pub const PARTY_B_CUSTOMER: &str = "CUSTOMER";

/// 合同的 5 个状态（与 DDL 的 `ck_biz_contract_status` 白名单逐字一致）。
///
/// 状态推进**只能**走 [`crate::business::shared::state_machine::transition`]
/// （`Entity::Contract`，ARCH-2 DEC-10）；本模块不提供 `UPDATE ... SET status`，
/// 也不该有人提供 —— `rg "SET status" business/erp/contract_*` 必须 0 命中。
pub const STATUSES: [&str; 5] = ["DRAFT", "SIGNED", "ACTIVE", "COMPLETED", "TERMINATED"];

/// 草稿。
pub const STATUS_DRAFT: &str = "DRAFT";
/// 已签订（日期三件套必填；此后关键字段冻结）。
pub const STATUS_SIGNED: &str = "SIGNED";
/// 已生效。
pub const STATUS_ACTIVE: &str = "ACTIVE";
/// 履约完成（终态，不再接受新单）。
pub const STATUS_COMPLETED: &str = "COMPLETED";
/// 已终止（终态；原因写 `remark` 与 `biz_status_log.reason`）。
pub const STATUS_TERMINATED: &str = "TERMINATED";

/// 采购单可以引用的合同状态（ARCH-2 §5.1 V-22 步 2）。
///
/// **只在建单与换合同时用**：提交 / 审批 / 收货不再复检合同状态，
/// 否则「PO 建好三天后合同到期」会让一张已成立的承诺再也提交不了（§5.1 末段）。
pub const USABLE_FOR_ORDER: [&str; 2] = [STATUS_SIGNED, STATUS_ACTIVE];

/// 解析乙方引用：`SUPPLIER` 分支返回 `biz_supplier.supplier_name`（未软删才算存在）。
///
/// 照抄 [`crate::business::erp::purchase_model::supplier_ref`] 的写法（ARCH-2 附录 B #5
/// 指名了这个范本），签名多一个 `party_b_type`：批次 2 要在这里分叉到 `biz_customer`，
/// 现在就把闸口留好，届时只加分支、不改调用方（§3.2.3 R2）。
///
/// # 参数
///
/// * `party_b_type` —— `SUPPLIER` / `CUSTOMER`（其它值返回 `Ok(None)`，由调用方报 66020/66021）
/// * `id` —— 乙方主键
///
/// # 返回值
///
/// * `Ok(Some(name))` —— 主档存在且未软删；`name` 直接作为 `party_b_name` **快照**落库
///   （V-15：不接受前端自填，否则审计上的"当时乙方是谁"就不可信）
/// * `Ok(None)` —— 不存在 / 已软删 / 本期不支持的乙方类型
/// * `Err(DbError)` —— 数据库异常（连不上、类型不符），由调用方 `?` 上抛
///
/// # 幂等与副作用
///
/// 纯读，无副作用；不加锁（读主档名称不需要与主档写入串行）。
///
/// # 本期为什么 `CUSTOMER` 一定返回 `None`
///
/// 客户主数据（`biz_customer`）属批次 2。**不要**用「先存 id 后补主档」绕过 ——
/// ARCH-2 §3.2.3 明确禁止：那会制造一批永远无法校验的孤儿数据，等客户表上线时
/// 无人知道该把哪条 id 认领成客户。故此处直接返回 `None`，由 A2 的
/// `ensure_party_b` 第一行转成 `66020 客户类合同暂未开放`。
///
/// # 示例
///
/// ```ignore
/// // A2 的 ensure_party_b（ARCH-2 §3.2 V-11 / V-15）
/// match contract_model::party_b_ref(&req.party_b_type, req.party_b_id).await? {
///     Some(name) => party_b_name = name,                       // 快照用服务端读到的名字
///     None => return Err(ApiError::new(400, error_code::ERP_CONTRACT_PARTY_INVALID,
///                                      "乙方供应商不存在或已停用")),
/// }
/// ```
pub async fn party_b_ref(party_b_type: &str, id: Uuid) -> DbResult<Option<String>> {
    // SCOPE-EXEMPT: biz_supplier 是全局主数据（无 dept_id / 归属人列），
    // 这里是一次按主键的存在性解析，不是列表查询，不涉及数据范围。
    match party_b_type {
        PARTY_B_SUPPLIER => {
            let rows = db()
                .find_with_params(
                    "SELECT supplier_name FROM biz_supplier WHERE id = ?::uuid AND deleted_at IS NULL",
                    vec![Value::from(&id)],
                )
                .await?;
            match rows.first() {
                None => Ok(None),
                Some(row) => Ok(Some(row.get_typed("supplier_name")?)),
            }
        }
        // 批次 2 在这里加 `biz_customer` 分支（§3.2.3 R2）；本期一律 None → 66020。
        _ => Ok(None),
    }
}

/// 供应商是否可用于**新签**合同（ARCH-2 V-11：新建时额外要求 `status = 1`）。
///
/// 为什么与 [`party_b_ref`] 分开：V-11 明写「停用只在**新建**校验；修改其它字段不重验」——
/// 否则供应商一停用，连合同的备注都改不了。因此两个判定不能合并成一个函数，
/// 调用点必须显式表达「这是新建还是改其它字段」。
///
/// # 返回值
///
/// * `Ok(Some(name))` —— 存在、未软删、且 `status = 1`
/// * `Ok(None)` —— 不存在 / 已软删 / 已停用（三者对外话术一致：66021）
pub async fn party_b_ref_for_new(party_b_type: &str, id: Uuid) -> DbResult<Option<String>> {
    if party_b_type != PARTY_B_SUPPLIER {
        // 客户分支本期不开（66020），不查任何表
        return Ok(None);
    }
    let rows = db()
        .find_with_params(
            "SELECT supplier_name FROM biz_supplier \
              WHERE id = ?::uuid AND deleted_at IS NULL AND status = 1",
            vec![Value::from(&id)],
        )
        .await?;
    match rows.first() {
        None => Ok(None),
        Some(row) => Ok(Some(row.get_typed("supplier_name")?)),
    }
}

// ==================== 单测（真库用例需 ERP_DB_URL，未设置则跳过） ====================

#[cfg(test)]
mod tests {
    use super::*;

    static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// 本模块造的主数据编码前缀（清理用）。
    const PREFIX: &str = "A1CM-";

    /// 初始化全局 `db()`（进程内只做一次）；`false` = 没配 `ERP_DB_URL`。
    async fn ensure_db() -> bool {
        use tokio::sync::OnceCell;
        static READY: OnceCell<bool> = OnceCell::const_new();
        *READY
            .get_or_init(|| async {
                let Ok(url) = std::env::var("ERP_DB_URL") else {
                    println!("SKIP: 未设置 ERP_DB_URL（不连真实数据库）");
                    return false;
                };
                if try_db().is_some() {
                    return true;
                }
                let mut config = DatabaseConfig::from_url(&url);
                config.pool.max_lifetime = Some(std::time::Duration::ZERO);
                config.pool.idle_timeout = Some(std::time::Duration::ZERO);
                match Db::connect_with(&url, config).await {
                    Ok(connection) => set_db(connection).is_ok(),
                    Err(err) => {
                        println!("SKIP: ERP_DB_URL 连接失败（{err}）");
                        false
                    }
                }
            })
            .await
    }

    async fn setup() -> Option<tokio::sync::MutexGuard<'static, ()>> {
        let guard = DB_LOCK.lock().await;
        if !ensure_db().await {
            return None;
        }
        cleanup().await;
        Some(guard)
    }

    /// 物理删掉本模块造的主数据（真库用例外不留痕）。
    async fn cleanup() {
        db().execute_with_params(
            "DELETE FROM biz_supplier WHERE supplier_code LIKE 'A1CM-%'",
            vec![],
        )
        .await
        .unwrap_or_else(|err| panic!("清理 biz_supplier 夹具失败：{err}"));
    }

    /// 造一个供应商，返回 (id, name)。`status` 用来验「停用」分支。
    async fn make_supplier(tag: &str, status: i16) -> (Uuid, String) {
        let id = Uuid::now_v7();
        let name = format!("A1 合同乙方 {tag}");
        let code = format!("{PREFIX}{tag}-{}", &id.simple().to_string()[..8]);
        db().execute_with_params(
            "INSERT INTO biz_supplier (id, supplier_code, supplier_name, status) \
             VALUES (?::uuid, ?, ?, ?)",
            vec![
                Value::from(&id),
                Value::Text(code),
                Value::Text(name.clone()),
                Value::I16(status),
            ],
        )
        .await
        .unwrap_or_else(|err| panic!("造供应商失败：{err}"));
        (id, name)
    }

    /// ① 存在且未软删 → 返回主档名称（V-15 的快照来源）。
    #[tokio::test]
    async fn party_b_ref_returns_supplier_name() {
        let Some(_guard) = setup().await else { return };
        let (id, name) = make_supplier("OK", 1).await;

        let got = party_b_ref(PARTY_B_SUPPLIER, id).await.unwrap();
        assert_eq!(got.as_deref(), Some(name.as_str()), "应返回主档当前名称");
    }

    /// ② 不存在的 id → None（应用层据此给 66021）。
    #[tokio::test]
    async fn party_b_ref_unknown_id_is_none() {
        let Some(_guard) = setup().await else { return };
        // 随机 id 保证不会撞上任何真实主档
        let ghost = Uuid::now_v7();
        assert_eq!(
            party_b_ref(PARTY_B_SUPPLIER, ghost).await.unwrap(),
            None,
            "不存在的乙方必须返回 None，不能静默放过"
        );
    }

    /// ③ 软删的主档 → None（巡检 Q1 的病根在这条之后：软删是允许的，合同会变孤儿）。
    #[tokio::test]
    async fn party_b_ref_soft_deleted_is_none() {
        let Some(_guard) = setup().await else { return };
        let (id, _name) = make_supplier("DEL", 1).await;
        db().execute_with_params(
            "UPDATE biz_supplier SET deleted_at = now() WHERE id = ?::uuid",
            vec![Value::from(&id)],
        )
        .await
        .unwrap();

        assert_eq!(
            party_b_ref(PARTY_B_SUPPLIER, id).await.unwrap(),
            None,
            "已软删的主档不得作为新合同乙方"
        );
    }

    /// ④ CUSTOMER 分支本期不开：即使 id 存在也返回 None（§3.2.3 的硬拒口径）。
    ///
    /// 这条断言同时是「批次 2 的提醒」：R2 落地后，本用例要改成
    /// 「造一个 biz_customer 行 → 返回 customer_name」。
    #[tokio::test]
    async fn party_b_ref_customer_branch_is_closed() {
        let Some(_guard) = setup().await else { return };
        let (id, _name) = make_supplier("CUS", 1).await;
        assert_eq!(
            party_b_ref(PARTY_B_CUSTOMER, id).await.unwrap(),
            None,
            "客户主数据未上线，CUSTOMER 分支必须返回 None（调用方转 66020）"
        );
        assert_eq!(
            party_b_ref("UNKNOWN", id).await.unwrap(),
            None,
            "未知乙方类型也不能静默放过"
        );
    }

    /// ⑤ 新建口径更严：停用的供应商在 `party_b_ref_for_new` 里拿不到，但通用解析器仍能
    /// 取到名字 —— 这个**差异本身就是 V-11 的语义**（停用只拦新签，不拦改备注）。
    #[tokio::test]
    async fn party_b_ref_for_new_rejects_disabled_supplier() {
        let Some(_guard) = setup().await else { return };
        let (disabled_id, disabled_name) = make_supplier("OFF", 0).await;
        let (enabled_id, enabled_name) = make_supplier("ON", 1).await;

        // 通用解析器不看 status：停用供应商的行仍在库里，改其它字段时要能取到名字
        assert_eq!(
            party_b_ref(PARTY_B_SUPPLIER, disabled_id)
                .await
                .unwrap()
                .as_deref(),
            Some(disabled_name.as_str()),
            "通用解析器不按 status 过滤（否则供应商一停用，连合同备注都改不了，V-11 末段）"
        );
        // 新签闸口才看 status
        assert_eq!(
            party_b_ref_for_new(PARTY_B_SUPPLIER, disabled_id)
                .await
                .unwrap(),
            None,
            "停用供应商不得新签（V-11）"
        );
        assert_eq!(
            party_b_ref_for_new(PARTY_B_SUPPLIER, enabled_id)
                .await
                .unwrap()
                .as_deref(),
            Some(enabled_name.as_str()),
            "启用中的供应商可以新签"
        );
        // 客户分支在新签闸口同样关闭（不查表，直接 None）
        assert_eq!(
            party_b_ref_for_new(PARTY_B_CUSTOMER, enabled_id)
                .await
                .unwrap(),
            None,
            "CUSTOMER 本期不开（66020）"
        );
    }
}
