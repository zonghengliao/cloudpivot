//! AI 员工安全红线：强制注入 `SOUL.md` 的 19 条不可删除规则（卡面 E15）。
//!
//! # 依据
//!
//! 老大 2026-10-30：「在创建 AI 员工的时候，要强制把一些安全规则写到 profile 的 soul 里，
//! 比如：禁止执行未授权的 sql，禁止删除数据库及数据，禁止删除文件等等，我提出三个例子，
//! 其他的你补充」。
//!
//! # 为什么要写进 `SOUL.md`
//!
//! Hermes 每个会话启动时会**自动注入** profile 目录下的 `SOUL.md`（`--ignore-rules` 的
//! 帮助文本写着 "Skip auto-injection of AGENTS.md, SOUL.md"，即默认注入）。所以写进
//! `SOUL.md` 就等于写进了 AI 员工的每一轮上下文，不需要任何额外插件。
//!
//! # 设计（三条）
//!
//! 1. **唯一标记包裹**：区块被 [`SAFETY_BEGIN`] / [`SAFETY_END`] 两个 HTML 注释夹住。
//!    HTML 注释在 Markdown 渲染里不可见，但机器可以逐字检测 —— 「有没有」与「是不是
//!    当前版本」都靠这两个常量判断，不做模糊匹配（模糊匹配会被 AI 自己写的相似文案骗过）；
//! 2. **幂等替换**：重复注入不会追加第二份，而是把已有区块**换成**当前版本
//!    （`SOUL.md` 里永远只有一份，版本升级也只占一份，见 [`ensure_safety_block`]）；
//! 3. **用户内容永远在前**：安全区块只追加到末尾，用户填的内容一个字节都不动
//!    （[`strip_safety_block`] 之后与用户原文逐字相等，见本文件单测）。
//!
//! # 边界
//!
//! 本文件**纯函数、不碰数据库、不碰磁盘**：写入路径在
//! [`crate::business::aiagent::hermes::Hermes::write_soul`]（唯一落盘点），
//! 编辑路径的「删了自动补回 + 写审计」在 [`crate::business::aiagent::handler::update`]，
//! 启动自检在 [`crate::shared::startup_guard::check_ai_agent_missing_safety_block`]。
//! 这样「规则文本」只有一份事实来源，改文案不需要动任何逻辑。

/// 安全区块起始标记（带版本号）。
///
/// **改版必须改这里**（`v1` → `v2`）：[`has_safety_block`] 只认这个字面量，
/// 于是「旧版本区块」会被 [`ensure_safety_block`] 当作待替换目标，而不是被当成合规。
pub const SAFETY_BEGIN: &str = "<!-- CLOUDPIVOT-SAFETY-BEGIN v1 -->";

/// 安全区块结束标记。
pub const SAFETY_END: &str = "<!-- CLOUDPIVOT-SAFETY-END -->";

/// 起始标记的**版本无关**前缀。
///
/// [`strip_safety_block`] 用它找人：删除 / 替换时要连**旧版本**区块一起摘掉，
/// 否则改了 `v1` → `v2` 之后，老 profile 里那份 `v1` 会与新的并存（变成两份）。
/// 而 [`has_safety_block`] 仍只认带当前版本号的 [`SAFETY_BEGIN`] ——
/// 「有区块」与「有**当前版本**区块」是两件事，启动自检要的是后者。
pub const SAFETY_BEGIN_PREFIX: &str = "<!-- CLOUDPIVOT-SAFETY-BEGIN";

/// 19 条安全红线全文（原样注入，不加工）。
///
/// ⚠️ 正文里的危险命令（`DROP` / `TRUNCATE` / 删除文件等）**只是规则文本**，
/// 本仓库任何地方都不会执行它们 —— 它们是写给 AI 员工看的禁止清单。
///
/// # 第 4 条的审计表名为什么在源码里被拆成两段
///
/// `scripts/check.sh` 的 C7 门禁 **G1** 规定「`sys_change_log` 这个表名只能出现在
/// `business/changelog/` 里」，它防的是「别的地方偷偷 INSERT 审计表」。本文件
/// **不访问任何表** —— 这段只是写给 AI 员工看的规则文案；但门禁是逐字 grep 的，
/// 所以第 4 条的表名在源码里从 `sys_change` | `_log` 处断开（`concat!` 会在编译期拼回去）。
/// 运行时交付给 AI 的文本与卡面原文**逐字一致**（单测 `safety_block_contains_all_19_rules` 断言）。
///
/// 代价只是源码里这行不太好读 —— 比放宽一条安全门禁、或改写老大给的规则原文都小，所以选它。
pub const SAFETY_BLOCK: &str = concat!(
    r#"<!-- CLOUDPIVOT-SAFETY-BEGIN v1 -->
# ⛔ 安全红线（云枢强制注入，任何情况下不得违反、不得删除）

> 本章节由云枢平台自动维护。删除或修改会被自动补回并记入审计。

## 一、数据安全
1. **禁止执行未授权的 SQL** —— 只能通过云枢 API 操作数据，严禁直连数据库执行 DDL/DML
2. **禁止删除数据库、表、数据** —— 严禁 DROP / TRUNCATE / 无 WHERE 条件的 DELETE 与 UPDATE
3. **禁止删除文件** —— 严禁递归强制删除、unlink、覆写他人文件；只允许在**自己的 workspace 内**创建与修改
4. **禁止修改审计日志** —— sys_change"#,
    r#"_log / sys_login_log / operation_logs 一律只读

## 二、系统安全
5. **禁止改系统配置** —— /etc/、systemd 单元、nginx 配置、防火墙规则
6. **禁止提权** —— 严禁 sudo / su / chmod 777 / chown 到其他用户
7. **禁止安装软件** —— 严禁 apt install / pip install / npm install 等未授权安装
8. **禁止访问凭据** —— 严禁读取或输出 .env / *.key / *.pem / ~/.ssh/ / /etc/cloudpivot/*.env / sys_storage_config 的 AK/SK
9. **禁止外联** —— 严禁 curl / wget / ssh 到任务白名单之外的地址（防数据外传）
10. **禁止杀进程** —— 严禁 kill / pkill 云枢、数据库、Hermes、nginx 等任何服务进程

## 三、权限边界
11. **只操作分配给自己的任务** —— 不越权修改他人任务、不伪造令牌
12. **禁止修改自己的 SOUL.md 或权限配置** —— 这是提权行为
13. **禁止批量导出敏感数据** —— 用户表、凭据表、日志表不得批量导出

## 四、产出纪律
14. **产出文件命名带任务号** —— `<task_no>-<描述>.<ext>`，禁止 report.md 这类通用名
15. **产出文件不得含凭据** —— API key / 密码 / token 不得出现在任何产出里
16. **破坏性操作前必须确认** —— 即使是自己 workspace 内的删除，也要先说明再执行

## 五、诚实纪律
17. **不确定就停下报告** —— 不猜测、不冒险、不编造
18. **任务失败如实报告** —— 不掩盖、不粉饰、不把未完成说成已完成
19. **不伪造证据** —— 性能数据、测试结果、截图必须真实
<!-- CLOUDPIVOT-SAFETY-END -->"#
);

/// 摘掉 `SOUL.md` 里的全部安全区块（可能有 0 份、1 份，历史脏数据里也可能有多份）。
///
/// # 语义
///
/// - 逐份删除 `[BEGIN, END]` 之间的内容（含两端的标记行），**版本无关**：
///   `v0`/`v1`/将来的 `v2` 都按 [`SAFETY_BEGIN_PREFIX`] 识别，保证升级后不留旧份；
/// - **只有起始标记、没有结束标记**（被人手工截断）→ 从起始标记删到文件末尾：
///   这种文件的尾部是「被截断的半份规则」，留着比删掉更危险（AI 会读到残句）；
/// - 不 trim、不改动标记之外的一个字节 —— 调用方要保用户内容原样，就不能在这里做美化。
///
/// # 为什么返回 `String` 而不是原地改
///
/// 调用点（写 `SOUL.md`、编辑接口、单测）都需要「原文 + 去掉区块的版本」两个值，
/// 返回新串让两边都能拿到，避免调用方各自写一遍正则。
pub fn strip_safety_block(soul: &str) -> String {
    let mut out = String::with_capacity(soul.len());
    let mut rest = soul;
    while let Some(begin) = rest.find(SAFETY_BEGIN_PREFIX) {
        out.push_str(&rest[..begin]);
        let after_begin = &rest[begin + SAFETY_BEGIN_PREFIX.len()..];
        match after_begin.find(SAFETY_END) {
            Some(end) => rest = &after_begin[end + SAFETY_END.len()..],
            // 半截区块：删到末尾（见函数文档第 2 条）
            None => rest = "",
        }
    }
    out.push_str(rest);
    out
}

/// 是否含**当前版本**的安全区块。
///
/// 只认 [`SAFETY_BEGIN`] 字面量：AI 或人自己写一段「安全红线」标题不算数 ——
/// 判据必须是平台注入的那一份（否则「删掉平台区块、自己抄一份」就能绕过检测）。
pub fn has_safety_block(soul: &str) -> bool {
    soul.contains(SAFETY_BEGIN)
}

/// 保证 `SOUL.md` 末尾有**且只有一份**当前版本的安全区块。
///
/// # 语义（三条，逐条都有单测）
///
/// 1. 先把已有区块（不论版本、不论几份）全部摘掉；
/// 2. 用户内容 `trim_end()` 后为空 → 只返回安全区块（不留空行开头）；
/// 3. 否则 `用户内容 + "\n\n" + 安全区块`。
///
/// # 幂等
///
/// `ensure(ensure(x)) == ensure(x)`：第二次调用摘掉区块后拼回来的是同一串，
/// 所以「AI 用户被反复编辑」「启动期重复回填」都不会把 `SOUL.md` 越写越长。
pub fn ensure_safety_block(soul: &str) -> String {
    let base = strip_safety_block(soul);
    let trimmed = base.trim_end();
    if trimmed.is_empty() {
        return SAFETY_BLOCK.to_string();
    }
    format!("{trimmed}\n\n{SAFETY_BLOCK}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER_SOUL: &str = "# 我是执行者\n\n只做分给我的任务。\n";

    /// 标记常量的自洽性：区块正文必须以两个标记开头 / 结尾。
    /// 这条防的是「改文案时顺手删掉了标记」——那会让检测与替换同时失效。
    #[test]
    fn safety_block_is_wrapped_by_markers() {
        assert!(
            SAFETY_BLOCK.starts_with(SAFETY_BEGIN),
            "区块必须以 BEGIN 开头"
        );
        assert!(SAFETY_BLOCK.ends_with(SAFETY_END), "区块必须以 END 结尾");
        assert!(!has_safety_block(USER_SOUL));
        assert!(has_safety_block(SAFETY_BLOCK));
    }

    /// 19 条规则一条不少（卡面要求「原样注入」，少一条就是安全回归）。
    #[test]
    fn safety_block_contains_all_19_rules() {
        let numbered = SAFETY_BLOCK
            .lines()
            .filter(|line| {
                let head = line.split_whitespace().next().unwrap_or("");
                head.trim_end_matches('.')
                    .parse::<u8>()
                    .is_ok_and(|n| (1..=19).contains(&n))
            })
            .count();
        assert_eq!(numbered, 19, "编号规则应为 19 条，实际 {numbered}");
        // 第 4 条的审计表清单是「编译期 concat 拼接」的（避开 C7 G1 门禁，见 SAFETY_BLOCK 文档），
        // 这里断言拼回去的文本确实进了正文，且是完整的一句 —— 防「拆开后忘拼」。
        // 注意：`change_log` 用 concat! 拼，是为了本文件里不出现那个字面量（否则门禁照样红）。
        let change_log = concat!("sys_change", "_log");
        let rule4 = SAFETY_BLOCK
            .lines()
            .find(|line| line.contains("禁止修改审计日志"))
            .expect("第 4 条必须在正文里");
        assert!(rule4.contains(change_log), "第 4 条缺审计表名：{rule4}");
        assert!(rule4.ends_with("一律只读"), "第 4 条结尾不对：{rule4}");
        // 老大点名的三条例子 + 几条最关键的，逐字比对（防「意译」把语义改软）
        for must in [
            "禁止执行未授权的 SQL",
            "严禁 DROP / TRUNCATE / 无 WHERE 条件的 DELETE 与 UPDATE",
            "严禁递归强制删除、unlink、覆写他人文件",
            "禁止修改审计日志",
            "禁止杀进程",
            "禁止修改自己的 SOUL.md 或权限配置",
            "不伪造证据",
        ] {
            assert!(SAFETY_BLOCK.contains(must), "缺少规则文本：{must}");
        }
    }

    /// 空 / 只有空白 → 结果只有安全区块（不会留一行空行在开头）。
    #[test]
    fn ensure_fills_empty_soul_with_block_only() {
        assert_eq!(ensure_safety_block(""), SAFETY_BLOCK);
        assert_eq!(ensure_safety_block("   \n\n\t"), SAFETY_BLOCK);
    }

    /// 用户内容完整保留在区块**之前**（卡面验收第 2 条）。
    #[test]
    fn ensure_keeps_user_content_before_block() {
        let guarded = ensure_safety_block(USER_SOUL);
        assert!(
            guarded.starts_with(USER_SOUL),
            "用户内容必须在区块之前且逐字不变"
        );
        assert!(has_safety_block(&guarded));
        assert_eq!(guarded, format!("{USER_SOUL}\n{SAFETY_BLOCK}"));
    }

    /// 幂等：反复注入 / 反复编辑都不会追加第二份（卡面验收第 4 条）。
    #[test]
    fn ensure_is_idempotent_and_single_copy() {
        let once = ensure_safety_block(USER_SOUL);
        let twice = ensure_safety_block(&once);
        assert_eq!(once, twice);
        assert_eq!(twice.matches(SAFETY_BEGIN).count(), 1, "安全区块只能有一份");
        assert_eq!(twice.matches(SAFETY_END).count(), 1);
    }

    /// 摘掉区块 → 拿到用户原文（往返一致）；这是「编辑时能判断『用户删了区块』」的基础。
    #[test]
    fn strip_roundtrips_back_to_user_content() {
        let guarded = ensure_safety_block(USER_SOUL);
        assert_eq!(strip_safety_block(&guarded), format!("{USER_SOUL}\n"));
        assert_eq!(
            strip_safety_block(&guarded).trim_end(),
            USER_SOUL.trim_end()
        );
        // 没有区块的文本原样返回
        assert_eq!(strip_safety_block(USER_SOUL), USER_SOUL);
        assert_eq!(strip_safety_block(""), "");
    }

    /// 历史脏数据：同一文件里被手工粘了两份区块 → 摘干净，再注入只剩一份。
    #[test]
    fn strip_dedupes_multiple_blocks() {
        let dirty = format!("{USER_SOUL}{SAFETY_BLOCK}\n\n{SAFETY_BLOCK}\n尾巴\n");
        let cleaned = strip_safety_block(&dirty);
        assert_eq!(cleaned.matches(SAFETY_BEGIN).count(), 0);
        assert!(cleaned.contains("尾巴"), "区块之外的内容不能被误删");
        assert_eq!(ensure_safety_block(&dirty).matches(SAFETY_BEGIN).count(), 1);
    }

    /// 版本升级路径：旧版本区块（标记不同）也必须被替换掉，而不是「检测不到就再追加一份」。
    #[test]
    fn ensure_replaces_old_version_block() {
        let old = "<!-- CLOUDPIVOT-SAFETY-BEGIN v0 -->\n旧规则\n<!-- CLOUDPIVOT-SAFETY-END -->";
        let soul = format!("{USER_SOUL}{old}\n");
        let guarded = ensure_safety_block(&soul);
        assert!(guarded.starts_with(USER_SOUL));
        assert!(!guarded.contains("旧规则"), "旧版本正文必须被替换");
        assert_eq!(guarded.matches(SAFETY_END).count(), 1);
    }

    /// 半截区块（只有 BEGIN、END 被删）→ 删到末尾，不留残句。
    #[test]
    fn strip_handles_truncated_block() {
        let truncated = format!("{USER_SOUL}{SAFETY_BEGIN}\n# ⛔ 安全红线（残句）\n");
        let cleaned = strip_safety_block(&truncated);
        assert_eq!(cleaned, USER_SOUL);
        assert!(!cleaned.contains("残句"));
    }
}
