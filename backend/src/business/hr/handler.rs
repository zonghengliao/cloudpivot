//! HR 模块接口层（骨架 · HR-09）。
//!
//! 本卡只注册**一条**接口（模块体检探针），业务路由由后续卡逐条落地 —— 契约表
//! [`ROUTE_CONTRACT`] 把设计 §5.2 的 46 条接口（方法 / 路径 / 权限码 / 落地卡）写死在这里，
//! 后续卡实现时**照表对齐**，单测把表与菜单种子 `V0387__hr_menu_perms` 对拍。
//!
//! # 为什么不先把 46 条路由注册成「未实现」的空壳
//!
//! 注册空壳的函数体只能是 `todo!()` / `Err(未实现)`：前者一调就 500 并 panic 掉整个
//! 请求任务，后者给前端的信号是「接口存在但坏了」。真正没实现时更该是 **404**
//! （前端 `/api` 路径写错、跳转落在未落地页，表现一致且不骗人）。所以：
//!
//! > **路由 = 已实现**。本卡只注册能真正工作的那条（体检探针），
//! > 其余 45 条留给对应的卡（见 `card` 列）。
//!
//! # 接口一览（本卡）
//!
//! | 方法 | 路径 | 权限码 | 说明 |
//! |------|------|--------|------|
//! | GET | `/business/hr/health` | 无（**登录即可**） | 10 张表存在性 + 就绪判断 + 人话提示 |
//!
//! 探针为什么不挂 `business:hr:*` 权限码：它的用途是**排障**（「HR 页面白屏 / 接口 404，
//! 到底是迁移没跑还是权限没配」），而 HR 角色往往正是出问题的那一环（没配角色的账号来问，
//! 挂了权限码就永远问不到答案）。响应体只含表名与布尔，不含任何员工数据。
//! 同一取舍的既有先例：`GET /business/erp/cost-switch`（`business/erp/cost_switch_handler.rs`）。

use nik::prelude::*;

use crate::business::hr::dto::{HrHealthRes, HrTablePresence};
use crate::business::hr::model;

/// 模块名（响应体与日志里的固定标识）。
pub const MODULE_NAME: &str = "hr";

/// 本模块的路由前缀（**不含** `/api`；前端调用要写全 `/api/business/hr/...`）。
pub const PATH_PREFIX: &str = "/business/hr";

/// 体检探针路径（不属设计 §5.2 的 46 条业务接口，故不进 [`ROUTE_CONTRACT`]）。
pub const HEALTH_PATH: &str = "/business/hr/health";

/// 一条接口契约：设计 §5.2 的一行。
///
/// 这**不是**运行时路由注册（真正的注册由 `#[nik::get]` 等宏在 `App::scan()` 时完成），
/// 而是「这一行的路径与权限码长什么样」的**编译期常量**。存在的理由有三个：
///
/// 1. 后续每张卡开工时不用回翻 1711 行的设计文档，照着 `path` + `permission` 抄；
/// 2. 权限码一旦写错（`business:hr:employee:details`）**不会有任何编译错误**，
///    只会在线上变成 403 —— [`tests::contract_permissions_are_seeded_by_v0387`] 拿
///    `V0387__hr_menu_perms` 的种子对拍，把这类错挡在 CI；
/// 3. 路径写错（漏 `/employee`、`/leave` 写成 `/leaves`）同理，
///    [`tests::every_macro_path_in_module_is_in_contract`] 反向扫描本目录所有 `#[nik::*]`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteSpec {
    /// HTTP 方法（`GET` / `POST` / `PUT` / `DELETE`）。
    pub method: &'static str,
    /// 路径（不含 `/api` 前缀）。
    pub path: &'static str,
    /// 权限码；`None` = 登录即可（设计 §5.2 里的自助接口）。
    pub permission: Option<&'static str>,
    /// 落地卡号；`"TBD"` = 设计 §5.2 有这个接口，但 §8.1 的卡清单里**没有**对应的卡。
    pub card: &'static str,
}

/// 设计 §5.2 的 46 条接口契约（顺序与设计文档编号 1~46 一致）。
///
/// # 与文档的三处统计口径差异（以本表为准）
///
/// - §5.2 表头写「API 清单（**42 条**）」，但正文编号到 **46**、脚注也写「合计 46 条」；
///   卡面转述的「42 条」沿用了表头 → **实际 46 条**；
/// - §5.2 脚注写「其中 **4 处**是 `/my` 自助接口」，表里只列出 **2** 个
///   （`/attendance/my`、`/salary/my`）；第 3 个 `/leave/my` 在 HR-41 落地，第 4 个
///   §5.2 未列 —— 届时补行并同步本条断言；
/// - `card = "TBD"` 的两行（员工导入 / 导出）在 §8.1 的批 2 里**没有排卡**，
///   但权限码 `business:hr:employee:import/export` 已在 `V0387` 种下 → 属卡清单漏项，
///   本卡只登记不实现。
pub const ROUTE_CONTRACT: [RouteSpec; 46] = [
    // ---- 员工档案（#1~#12） ----
    RouteSpec {
        method: "GET",
        path: "/business/hr/employee/page",
        permission: Some("business:hr:employee:list"),
        card: "HR-12",
    },
    RouteSpec {
        method: "GET",
        path: "/business/hr/employee/{id}",
        permission: Some("business:hr:employee:detail"),
        card: "HR-12",
    },
    RouteSpec {
        method: "POST",
        path: "/business/hr/employee",
        permission: Some("business:hr:employee:create"),
        card: "HR-12",
    },
    RouteSpec {
        method: "PUT",
        path: "/business/hr/employee/{id}",
        permission: Some("business:hr:employee:update"),
        card: "HR-12",
    },
    RouteSpec {
        method: "DELETE",
        path: "/business/hr/employee/{id}",
        permission: Some("business:hr:employee:delete"),
        card: "HR-12",
    },
    RouteSpec {
        method: "POST",
        path: "/business/hr/employee/{id}/status",
        permission: Some("business:hr:employee:update"),
        card: "HR-12",
    },
    RouteSpec {
        method: "GET",
        path: "/business/hr/employee/{id}/sensitive",
        permission: Some("business:hr:employee:sensitive"),
        card: "HR-13",
    },
    RouteSpec {
        method: "POST",
        path: "/business/hr/employee/{id}/bind-user",
        permission: Some("business:hr:employee:update"),
        card: "HR-12",
    },
    RouteSpec {
        method: "GET",
        path: "/business/hr/employee/next-no",
        permission: Some("business:hr:employee:create"),
        card: "HR-14",
    },
    RouteSpec {
        method: "POST",
        path: "/business/hr/employee/seed-from-users",
        permission: Some("business:hr:employee:create"),
        card: "HR-17",
    },
    RouteSpec {
        method: "POST",
        path: "/business/hr/employee/import",
        permission: Some("business:hr:employee:import"),
        card: "TBD",
    },
    RouteSpec {
        method: "GET",
        path: "/business/hr/employee/export",
        permission: Some("business:hr:employee:export"),
        card: "TBD",
    },
    // ---- 劳动合同（#13~#15，挂在员工下） ----
    RouteSpec {
        method: "POST",
        path: "/business/hr/employee/{id}/contract",
        permission: Some("business:hr:contract:manage"),
        card: "HR-15",
    },
    RouteSpec {
        method: "PUT",
        path: "/business/hr/contract/{contract_id}",
        permission: Some("business:hr:contract:manage"),
        card: "HR-15",
    },
    RouteSpec {
        method: "POST",
        path: "/business/hr/contract/{contract_id}/renew",
        permission: Some("business:hr:contract:manage"),
        card: "HR-15",
    },
    // ---- 员工薪资标准（#16~#17） ----
    RouteSpec {
        method: "GET",
        path: "/business/hr/employee/{id}/salary",
        permission: Some("business:hr:employee:detail"),
        card: "HR-16",
    },
    RouteSpec {
        method: "POST",
        path: "/business/hr/employee/{id}/salary",
        permission: Some("business:hr:salary:calc"),
        card: "HR-16",
    },
    // ---- 考勤（#18~#25） ----
    RouteSpec {
        method: "GET",
        path: "/business/hr/attendance/period/page",
        permission: Some("business:hr:attendance:list"),
        card: "HR-19",
    },
    RouteSpec {
        method: "POST",
        path: "/business/hr/attendance/period",
        permission: Some("business:hr:attendance:period"),
        card: "HR-19",
    },
    RouteSpec {
        method: "POST",
        path: "/business/hr/attendance/period/{id}/status",
        permission: Some("business:hr:attendance:lock"),
        card: "HR-23",
    },
    RouteSpec {
        method: "GET",
        path: "/business/hr/attendance/record/page",
        permission: Some("business:hr:attendance:record"),
        card: "HR-20",
    },
    RouteSpec {
        method: "PUT",
        path: "/business/hr/attendance/record/{id}",
        permission: Some("business:hr:attendance:record"),
        card: "HR-20",
    },
    RouteSpec {
        method: "POST",
        path: "/business/hr/attendance/import",
        permission: Some("business:hr:attendance:import"),
        card: "HR-21",
    },
    RouteSpec {
        method: "GET",
        path: "/business/hr/attendance/export",
        permission: Some("business:hr:attendance:export"),
        card: "HR-30",
    },
    RouteSpec {
        method: "GET",
        path: "/business/hr/attendance/my",
        permission: None,
        card: "HR-41",
    },
    // ---- 请假（#26~#31） ----
    RouteSpec {
        method: "GET",
        path: "/business/hr/leave/page",
        permission: Some("business:hr:leave:list"),
        card: "HR-25",
    },
    RouteSpec {
        method: "GET",
        path: "/business/hr/leave/{id}",
        permission: Some("business:hr:leave:detail"),
        card: "HR-25",
    },
    RouteSpec {
        method: "POST",
        path: "/business/hr/leave",
        permission: Some("business:hr:leave:create"),
        card: "HR-25",
    },
    RouteSpec {
        method: "PUT",
        path: "/business/hr/leave/{id}",
        permission: Some("business:hr:leave:update"),
        card: "HR-25",
    },
    RouteSpec {
        method: "POST",
        path: "/business/hr/leave/{id}/submit",
        permission: Some("business:hr:leave:submit"),
        card: "HR-25",
    },
    RouteSpec {
        method: "POST",
        path: "/business/hr/leave/{id}/cancel",
        permission: Some("business:hr:leave:cancel"),
        card: "HR-25",
    },
    // ---- 薪资项（#32~#35） ----
    RouteSpec {
        method: "GET",
        path: "/business/hr/salary-item/page",
        permission: Some("business:hr:salary-item:list"),
        card: "HR-31",
    },
    RouteSpec {
        method: "POST",
        path: "/business/hr/salary-item",
        permission: Some("business:hr:salary-item:create"),
        card: "HR-31",
    },
    RouteSpec {
        method: "PUT",
        path: "/business/hr/salary-item/{id}",
        permission: Some("business:hr:salary-item:update"),
        card: "HR-31",
    },
    RouteSpec {
        method: "POST",
        path: "/business/hr/salary-item/validate",
        permission: Some("business:hr:salary-item:update"),
        card: "HR-33",
    },
    // ---- 薪资单（#36~#46） ----
    RouteSpec {
        method: "GET",
        path: "/business/hr/salary/page",
        permission: Some("business:hr:salary:list"),
        card: "HR-36",
    },
    RouteSpec {
        method: "GET",
        path: "/business/hr/salary/{id}",
        permission: Some("business:hr:salary:detail"),
        card: "HR-36",
    },
    RouteSpec {
        method: "GET",
        path: "/business/hr/salary/{id}/detail/page",
        permission: Some("business:hr:salary:detail"),
        card: "HR-36",
    },
    RouteSpec {
        method: "POST",
        path: "/business/hr/salary/calc",
        permission: Some("business:hr:salary:calc"),
        card: "HR-34",
    },
    RouteSpec {
        method: "POST",
        path: "/business/hr/salary",
        permission: Some("business:hr:salary:calc"),
        card: "HR-36",
    },
    RouteSpec {
        method: "PUT",
        path: "/business/hr/salary/{id}/detail/{detail_id}",
        permission: Some("business:hr:salary:calc"),
        card: "HR-37",
    },
    RouteSpec {
        method: "POST",
        path: "/business/hr/salary/{id}/submit",
        permission: Some("business:hr:salary:submit"),
        card: "HR-39",
    },
    RouteSpec {
        method: "POST",
        path: "/business/hr/salary/{id}/post",
        permission: Some("business:hr:salary:post"),
        card: "HR-39",
    },
    RouteSpec {
        method: "POST",
        path: "/business/hr/salary/{id}/reverse",
        permission: Some("business:hr:salary:reverse"),
        card: "HR-40",
    },
    RouteSpec {
        method: "GET",
        path: "/business/hr/salary/{id}/export",
        permission: Some("business:hr:salary:export"),
        card: "HR-42",
    },
    RouteSpec {
        method: "GET",
        path: "/business/hr/salary/my",
        permission: Some("business:hr:salary:my"),
        card: "HR-41",
    },
];

/// HR 模块体检（`GET /business/hr/health`）。
///
/// # 返回值
///
/// [`HrHealthRes`]：`tables` 恒为 10 项（缺表也只是 `present = false`，不报错）；
/// `ready = true` 当且仅当 10 张表全部存在。
///
/// # 异常情况
///
/// | 码 | 场景 |
/// |----|------|
/// | 401 | 未登录（框架认证层拦截） |
/// | 500 | 数据库连接 / 查询失败（不是「缺表」） |
#[nik::get("/business/hr/health")]
pub async fn health() -> ResResult<HrHealthRes> {
    let presence = model::table_presence().await?;
    Ok(Res::ok(summarize(presence)))
}

/// 把「逐表存在性」汇总成响应体（纯函数，方便单测覆盖就绪 / 缺表两种形态）。
fn summarize(presence: Vec<(&'static str, bool)>) -> HrHealthRes {
    let missing: Vec<&str> = presence
        .iter()
        .filter(|(_, present)| !present)
        .map(|(table, _)| *table)
        .collect();
    // 就绪 = 张数对得上**且**每张都在：只判 `all()` 的话，`presence` 万一被上层截短
    // （比如以后改成按分支跳过某些表）会「空集合 ⇒ 全真 ⇒ 就绪」，那是最坏的误报。
    let ready = presence.len() == model::HR_TABLES.len() && missing.is_empty();

    let hint = if ready {
        format!(
            "HR 建表迁移已生效（{} / {} 张表在位），接口按设计 §5.2 逐卡落地",
            presence.len() - missing.len(),
            model::HR_TABLES.len()
        )
    } else {
        format!(
            "HR 建表迁移未全部生效（缺 {} 张：{}）。在 backend/ 目录执行 `cargo run`（启动自动迁移 \
             V0383__hr_tables），或人工核对 migrations/business/V0383__hr_tables.up.sql",
            missing.len(),
            missing.join("、")
        )
    };

    HrHealthRes {
        module: MODULE_NAME.to_string(),
        prefix: PATH_PREFIX.to_string(),
        api_planned: ROUTE_CONTRACT.len(),
        tables: presence
            .into_iter()
            .map(|(table, present)| HrTablePresence {
                table: table.to_string(),
                present,
            })
            .collect(),
        ready,
        hint,
    }
}

// ==================== 单测（不连库） ====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 菜单 / 权限种子原文（编译期嵌入）：契约表的权限码必须全在种子里。
    const V0387_SEED: &str = include_str!("../../../migrations/business/V0387__hr_menu_perms.up.sql");

    /// 设计 §5.2 定稿 46 条；路径前缀统一、方法合法、`(方法, 路径)` 唯一。
    ///
    /// 唯一性是真会踩的坑：`GET /salary/{id}` 与 `GET /salary/my` 这种「静态段 vs 参数段」
    /// 兄弟路径在 axum（matchit）里是合法的（静态优先），但**两条一模一样的**声明会让
    /// `App::scan()` 在启动时 panic —— 那种崩在启动期、日志只有一行 panic 的故障最难查。
    #[test]
    fn contract_has_46_unique_routes() {
        assert_eq!(ROUTE_CONTRACT.len(), 46, "设计 §5.2 编号 1~46");

        let mut seen = std::collections::HashSet::new();
        for spec in ROUTE_CONTRACT {
            assert!(
                ["GET", "POST", "PUT", "DELETE"].contains(&spec.method),
                "方法不合法：{} {}",
                spec.method,
                spec.path
            );
            assert!(
                spec.path.starts_with(PATH_PREFIX),
                "路径必须以 {PATH_PREFIX} 开头（不含 /api）：{}",
                spec.path
            );
            assert!(
                seen.insert((spec.method, spec.path)),
                "重复声明（启动期会 panic）：{} {}",
                spec.method,
                spec.path
            );
            // 权限码三段的形状：business:hr:{资源}:{动作}
            if let Some(permission) = spec.permission {
                let parts: Vec<&str> = permission.split(':').collect();
                assert_eq!(parts.len(), 4, "权限码必须是 4 段：{permission}");
                assert_eq!(parts[0], "business", "权限码前缀：{permission}");
                assert_eq!(parts[1], "hr", "权限码模块段：{permission}");
            }
            // 卡号要么是 HR-xx，要么显式标 TBD（不允许留空字符串蒙混）
            assert!(
                spec.card == "TBD"
                    || (spec.card.starts_with("HR-")
                        && spec.card.len() == 5
                        && spec.card[3..].chars().all(|c| c.is_ascii_digit())),
                "卡号形状不对：{}（{}）",
                spec.card,
                spec.path
            );
        }
    }

    /// 契约里的每个权限码都必须是 `V0387` 种下的那 35 个之一（5 页面 + 30 按钮）。
    ///
    /// 设计 §5.2 与菜单种子是**两处**词表：码写错不会有编译错误，只会在线上变成
    /// 「超管之外的账号全 403」，而且看起来像「权限没配」而不是「码写错」。
    #[test]
    fn contract_permissions_are_seeded_by_v0387() {
        let pattern = regex::Regex::new(r"business:hr:[a-z0-9-]+:[a-z0-9-]+").expect("正则字面量合法");
        let seeded: std::collections::HashSet<String> = pattern
            .find_iter(V0387_SEED)
            .map(|found| found.as_str().to_string())
            .collect();
        // 种子里 5 个页面码 + 30 个按钮码 = 35（V0387 自检 §5.4 断言的就是 30 个按钮）
        assert_eq!(seeded.len(), 35, "V0387 应种 35 个 business:hr 权限码");

        for spec in ROUTE_CONTRACT {
            let Some(permission) = spec.permission else {
                continue;
            };
            assert!(
                seeded.contains(permission),
                "权限码 {permission}（{} {}）不在 V0387 的菜单种子里 —— 线上会 403",
                spec.method,
                spec.path
            );
        }
    }

    /// 反向扫描：本目录所有 `#[nik::get/post/put/delete(...)]` 声明的路径都必须在契约表里。
    ///
    /// 后续卡把接口写进 `employee_handler.rs` 时，如果路径拼错（`/employee/{id}` 写成
    /// `/employees/{id}`）或干脆忘了登记，这条测试会红 —— 否则错误只会在前端表现为
    /// 「响应体格式不符合」（请求落到了 SPA fallback，返回 HTML）。
    #[test]
    fn every_macro_path_in_module_is_in_contract() {
        let dir = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src/business/hr"));
        let macro_pattern =
            regex::Regex::new(r#"#\[nik::(get|post|put|delete|patch)\(\s*"([^"]+)""#).expect("正则字面量合法");

        let mut declared: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(dir).expect("读取 src/business/hr") {
            let path = entry.expect("目录项").path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("读取源文件");
            for capture in macro_pattern.captures_iter(&source) {
                declared.push(capture[2].to_string());
            }
        }

        assert!(
            declared.contains(&HEALTH_PATH.to_string()),
            "体检探针 {HEALTH_PATH} 必须真的挂上宏（否则「路由注册到主 router」不成立）"
        );

        // 探针不属设计 §5.2 的 46 条业务接口，是唯一允许不在契约表里的路径
        let contract_paths: std::collections::HashSet<&str> = ROUTE_CONTRACT.iter().map(|spec| spec.path).collect();
        for path in &declared {
            if path == HEALTH_PATH {
                continue;
            }
            assert!(
                contract_paths.contains(path.as_str()),
                "宏声明的路径 {path} 不在 ROUTE_CONTRACT 里 —— 先登记契约再实现"
            );
        }
    }

    /// 探针不能被登记成「第 47 条业务接口」（它免权限码，混进契约表会让人误以为
    /// 设计 §5.2 有 47 条，也会让「权限码必须在种子里」的断言需要开例外）。
    #[test]
    fn health_probe_is_not_part_of_the_api_contract() {
        assert!(!ROUTE_CONTRACT.iter().any(|spec| spec.path == HEALTH_PATH));
        assert!(HEALTH_PATH.starts_with(PATH_PREFIX));
    }

    /// 10 张表全在 → 就绪；缺一张 → 未就绪且提示语点名缺的是哪张（排障靠这句）。
    #[test]
    fn summarize_reports_ready_and_missing() {
        let all_present: Vec<(&'static str, bool)> = model::HR_TABLES.iter().map(|table| (*table, true)).collect();
        let ready = summarize(all_present);
        assert!(ready.ready);
        assert_eq!(ready.tables.len(), 10);
        assert_eq!(ready.module, "hr");
        assert_eq!(ready.prefix, "/business/hr");
        assert_eq!(ready.api_planned, 46);
        assert!(ready.hint.contains("10 / 10"), "提示语：{}", ready.hint);

        let mut with_hole = model::HR_TABLES.iter().map(|table| (*table, true)).collect::<Vec<_>>();
        // 第 ⑦ 张（薪资项）缺失：模拟「V0383 只跑了一半 / 被手工 drop」
        with_hole[6].1 = false;
        let not_ready = summarize(with_hole);
        assert!(!not_ready.ready);
        assert_eq!(not_ready.tables.len(), 10, "缺表也要逐张列出来");
        assert!(not_ready.hint.contains("biz_salary_item"), "提示语：{}", not_ready.hint);
        assert!(
            not_ready.hint.contains("V0383__hr_tables"),
            "提示语：{}",
            not_ready.hint
        );
        // 缺表的项在 tables 里仍然是 false（前端按行渲染红/绿）
        assert!(!not_ready.tables[6].present);
    }

    /// 空集合不能算「就绪」（`all()` 对空集恒真 —— 那会把「什么都没查到」报成健康）。
    #[test]
    fn empty_presence_is_never_ready() {
        let empty = summarize(Vec::new());
        assert!(!empty.ready);
        assert!(empty.tables.is_empty());
    }
}
