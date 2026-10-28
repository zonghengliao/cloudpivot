//! 通知模板的变量渲染器（自研 `{{var}}` 替换器，**不引入模板引擎**）。
//!
//! # 为什么自研（架构文档 §4.6 决策 D11）
//!
//! 模板正文存在**数据库行**里，而框架的 `render_template(name, ctx)`
//! （`alun-web/src/resources.rs`）只按**名字**从文件系统取模板，DB 行用不了；
//! 引入 Tera / Jinja 类引擎等于给运维开一个「任意表达式求值」入口（`{{ x.__class__ }}`
//! 这类逃逸是真实攻击面），收益（循环 / 条件）在本批次用不上。因此只做值替换。
//!
//! # 五条规则（与架构文档 §4.6 逐条对应，全部有单测）
//!
//! | # | 规则 | 行为 |
//! |---|------|------|
//! | 1 | 语法 | `{{var}}` / `{{ var }}`；变量名 `[A-Za-z_][A-Za-z0-9_]{0,63}`；不支持表达式 / 函数 / 过滤器 |
//! | 2 | 缺变量 | 变量未提供或值为 `null` → `422 + 60023`，**不静默替换成空串**（否则会发出「尊敬的 ，」） |
//! | 3 | 转义 | `content_format = HTML` 时先对**变量值**做 HTML 转义再替换（防 XSS） |
//! | 4 | 长度 | 渲染后 `title ≤ 255` 字符、`content ≤ 64 KiB`，超限 → `422 + 60025` |
//! | 5 | 未知占位 | 模板里出现「未在 `variables` 声明中列出」的占位符 → `422 + 60024`（保存 / 预览时拦截） |
//!
//! 额外两条工程约束：
//!
//! - **单趟替换、不递归**：变量值里即使含 `{{other}}` 也原样输出（防止「值被当模板再渲染」
//!   造成的二次注入与递归爆炸）；
//! - **纯函数**：不碰数据库、不碰全局状态，故可完整单测（本文件 `mod tests`）。

use std::collections::BTreeSet;

use alun::prelude::*;

use crate::business::notice::dto::{ContentFormat, TemplateVarDecl};
use crate::shared::error_code;

/// 渲染后标题的最大字符数（与 `sys_notice.title VARCHAR(255)` 对齐，按字符而非字节）。
pub const MAX_TITLE_CHARS: usize = 255;

/// 渲染后正文的最大字节数（64 KiB，架构文档 §4.6 规则 4）。
pub const MAX_CONTENT_BYTES: usize = 64 * 1024;

/// 变量名最大长度（架构文档 §4.6 规则 1）。
pub const MAX_VAR_NAME_LEN: usize = 64;

/// 渲染模板（单趟替换，缺失变量报错）。
///
/// # 参数
///
/// - `template`：模板骨架，如 `你好 {{realName}}`。
/// - `vars`：变量表（JSON 对象）；非对象视为空表（所有占位符都会报缺变量）。
/// - `format`：正文格式；`HTML` 时对变量值做 HTML 转义。
///
/// # 返回值
///
/// 渲染后的字符串（未做长度校验，长度校验见 [`render_notice`]）。
///
/// # 异常情况
///
/// - 占位符未闭合 / 变量名为空或非法 → `422 + 60024`（模板语法错误）；
/// - 变量缺失或为 `null` → `422 + 60023`（缺变量）。
pub fn render(
    template: &str,
    vars: &serde_json::Value,
    format: ContentFormat,
) -> std::result::Result<String, ApiError> {
    let empty = serde_json::Map::new();
    let map = vars.as_object().unwrap_or(&empty);

    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            return Err(syntax_error("占位符缺少收尾的 `}}`"));
        };
        let raw_name = &after[..end];
        let name = raw_name.trim();
        validate_var_name(name)?;

        let Some(value) = map.get(name) else {
            return Err(ApiError::new(
                422,
                error_code::NOTICE_TEMPLATE_VAR_MISSING,
                format!("模板变量缺失：{name}"),
            ));
        };
        let Some(text) = scalar_to_text(value) else {
            // null 与缺失同义：静默替换成空串会发出残缺文案，故一律报错
            return Err(ApiError::new(
                422,
                error_code::NOTICE_TEMPLATE_VAR_MISSING,
                format!("模板变量缺失：{name}（值为 null）"),
            ));
        };
        if format.escape_values() {
            out.push_str(&escape_html(&text));
        } else {
            out.push_str(&text);
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// 渲染标题并做长度校验。
///
/// # 参数
///
/// - `template`：标题模板。
/// - `vars`：变量表。
/// - `format`：正文格式（标题同样按该格式决定是否转义）。
///
/// # 返回值
///
/// 渲染后的标题。
///
/// # 异常情况
///
/// 同 [`render`]；另：字符数超过 [`MAX_TITLE_CHARS`] → `422 + 60025`。
pub fn render_title(
    template: &str,
    vars: &serde_json::Value,
    format: ContentFormat,
) -> std::result::Result<String, ApiError> {
    let title = render(template, vars, format)?;
    let chars = title.chars().count();
    if chars > MAX_TITLE_CHARS {
        return Err(ApiError::new(
            422,
            error_code::NOTICE_TEMPLATE_TOO_LONG,
            format!("渲染后标题超长：{chars} 字符（上限 {MAX_TITLE_CHARS}）"),
        ));
    }
    Ok(title)
}

/// 渲染正文并做长度校验。
///
/// # 参数
///
/// - `template`：正文模板。
/// - `vars`：变量表。
/// - `format`：正文格式。
///
/// # 返回值
///
/// 渲染后的正文（`HTML` 格式时已净化骨架中的危险标签）。
///
/// # 异常情况
///
/// 同 [`render`]；另：字节数超过 [`MAX_CONTENT_BYTES`] → `422 + 60025`。
pub fn render_content(
    template: &str,
    vars: &serde_json::Value,
    format: ContentFormat,
) -> std::result::Result<String, ApiError> {
    // HTML 骨架先净化（`<script>` / 事件属性 / javascript: URL），再渲染变量值。
    // 顺序不能反：先渲染会把「已转义的值」再交给净化器，存在被再次解码的风险。
    let skeleton = match format {
        ContentFormat::Html => xss::sanitize_html(template),
        _ => template.to_string(),
    };
    let content = render(&skeleton, vars, format)?;
    let bytes = content.len();
    if bytes > MAX_CONTENT_BYTES {
        return Err(ApiError::new(
            422,
            error_code::NOTICE_TEMPLATE_TOO_LONG,
            format!("渲染后正文超长：{bytes} 字节（上限 {MAX_CONTENT_BYTES}）"),
        ));
    }
    Ok(content)
}

/// 一次渲染「标题 + 正文」（`dispatch` / 发送接口的统一入口）。
///
/// # 参数
///
/// - `title_template`：标题模板。
/// - `content_template`：正文模板。
/// - `vars`：变量表。
/// - `format`：正文格式。
///
/// # 返回值
///
/// `(标题, 正文)`，两者都已做长度校验。
///
/// # 异常情况
///
/// 同 [`render_title`] / [`render_content`]。
pub fn render_notice(
    title_template: &str,
    content_template: &str,
    vars: &serde_json::Value,
    format: ContentFormat,
) -> std::result::Result<(String, String), ApiError> {
    let title = render_title(title_template, vars, format)?;
    let content = render_content(content_template, vars, format)?;
    Ok((title, content))
}

/// 收集模板里出现的占位符名（去重、按字典序）。
///
/// # 参数
///
/// - `template`：任意模板骨架。
///
/// # 返回值
///
/// 占位符名集合。
///
/// # 异常情况
///
/// 语法非法（未闭合 / 变量名非法）→ `422 + 60024`。
pub fn collect_placeholders(template: &str) -> std::result::Result<BTreeSet<String>, ApiError> {
    let mut names = BTreeSet::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            return Err(syntax_error("占位符缺少收尾的 `}}`"));
        };
        let name = after[..end].trim();
        validate_var_name(name)?;
        names.insert(name.to_string());
        rest = &after[end + 2..];
    }
    Ok(names)
}

/// 校验模板语法（保存模板时用，不产生任何变量值）。
///
/// # 参数
///
/// - `template`：标题或正文模板。
///
/// # 返回值
///
/// 语法合法返回 `Ok(())`。
///
/// # 异常情况
///
/// 同 [`collect_placeholders`]。
pub fn validate_syntax(template: &str) -> std::result::Result<(), ApiError> {
    collect_placeholders(template).map(|_| ())
}

/// 校验「模板占位符」与「变量声明」一致（规则 5：未知占位报错）。
///
/// # 参数
///
/// - `template`：标题或正文模板。
/// - `decls`：变量声明（模板保存时提交的 `variables`）。
///
/// # 返回值
///
/// 全部占位符都已声明时返回 `Ok(())`。
///
/// # 异常情况
///
/// 出现未声明的占位符 → `422 + 60024`，消息里列出全部未声明变量（一次性告知，不用逐个试）。
pub fn ensure_declared(
    template: &str,
    decls: &[TemplateVarDecl],
) -> std::result::Result<(), ApiError> {
    let placeholders = collect_placeholders(template)?;
    let declared: BTreeSet<&str> = decls.iter().map(|decl| decl.name.as_str()).collect();
    let undeclared: Vec<&str> = placeholders
        .iter()
        .map(String::as_str)
        .filter(|name| !declared.contains(name))
        .collect();
    if undeclared.is_empty() {
        return Ok(());
    }
    Err(ApiError::new(
        422,
        error_code::NOTICE_TEMPLATE_SYNTAX,
        format!("模板占位符未在 variables 中声明：{}", undeclared.join(", ")),
    ))
}

/// 用变量声明的 `sample` 值构造示例变量表（保存模板时试渲染用）。
///
/// # 参数
///
/// - `decls`：变量声明。
///
/// # 返回值
///
/// JSON 对象；未给 `sample` 的变量回退为 `label`，再回退为 `变量名`（保证试渲染能通过）。
pub fn sample_vars(decls: &[TemplateVarDecl]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for decl in decls {
        let value = decl
            .sample
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| {
                decl.label
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| decl.name.clone());
        map.insert(decl.name.clone(), serde_json::Value::String(value));
    }
    serde_json::Value::Object(map)
}

// ==================== 内部工具 ====================

/// 校验变量名：`[A-Za-z_][A-Za-z0-9_]{0,63}`。
fn validate_var_name(name: &str) -> std::result::Result<(), ApiError> {
    if name.is_empty() {
        return Err(syntax_error("占位符变量名为空"));
    }
    if name.chars().count() > MAX_VAR_NAME_LEN {
        return Err(syntax_error("占位符变量名超过 64 个字符"));
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap_or('_');
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(syntax_error(&format!(
            "占位符变量名非法：{name}（首字符必须是字母或下划线）"
        )));
    }
    if !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        return Err(syntax_error(&format!(
            "占位符变量名非法：{name}（只允许字母、数字、下划线；不支持表达式 / 过滤器）"
        )));
    }
    Ok(())
}

/// JSON 标量 / 复合值 → 文本（`null` 返回 `None`，由调用方按「缺变量」处理）。
fn scalar_to_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Null => None,
        serde_json::Value::Bool(flag) => Some(flag.to_string()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        serde_json::Value::String(text) => Some(text.clone()),
        // 数组 / 对象按紧凑 JSON 输出（如模板要展示明细，调用方先自行序列化也可以）
        other => Some(other.to_string()),
    }
}

/// HTML 转义（`& < > " '`），防止变量值把骨架标签打破。
fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// 构造统一的「模板语法错误」。
fn syntax_error(detail: &str) -> ApiError {
    ApiError::new(
        422,
        error_code::NOTICE_TEMPLATE_SYNTAX,
        format!("模板语法错误：{detail}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decl(name: &str) -> TemplateVarDecl {
        TemplateVarDecl {
            name: name.to_string(),
            label: Some(format!("{name} 标签")),
            required: true,
            sample: Some(format!("{name}-sample")),
        }
    }

    /// 规则 1：两种写法（紧凑 / 带空格）都能渲染；多个占位符按出现顺序替换。
    #[test]
    fn render_supports_both_spacings() {
        let vars = json!({"a": "1", "b": "2"});
        let out = render("x={{a}}, y={{ b }}", &vars, ContentFormat::Text).expect("应渲染成功");
        assert_eq!(out, "x=1, y=2");
    }

    /// 规则 1：未闭合的占位符是语法错误，而不是被当普通文本。
    #[test]
    fn render_rejects_unclosed_placeholder() {
        let error = render("你好 {{name", &json!({}), ContentFormat::Text).unwrap_err();
        assert_eq!(error.code, error_code::NOTICE_TEMPLATE_SYNTAX);
        assert_eq!(error.status, 422);
    }

    /// 规则 1：不支持表达式 / 过滤器，`{{user.name}}` 这类写法直接拒绝。
    #[test]
    fn render_rejects_expression_style_names() {
        let error = render("{{user.name}}", &json!({}), ContentFormat::Text).unwrap_err();
        assert_eq!(error.code, error_code::NOTICE_TEMPLATE_SYNTAX);

        let error = render("{{ name | upper }}", &json!({}), ContentFormat::Text).unwrap_err();
        assert_eq!(error.code, error_code::NOTICE_TEMPLATE_SYNTAX);
    }

    /// 规则 2（缺变量）：未提供 → 422 + 60023，且**不得**静默留空。
    #[test]
    fn render_missing_variable_errors() {
        // 完全没提供
        let error = render("尊敬的 {{realName}}", &json!({}), ContentFormat::Text).unwrap_err();
        assert_eq!(error.code, error_code::NOTICE_TEMPLATE_VAR_MISSING);
        assert_eq!(error.status, 422);
        assert!(error.msg.contains("realName"), "消息要指明缺哪个变量");

        // 提供了但值为 null，与缺失同义
        let error = render(
            "尊敬的 {{realName}}",
            &json!({"realName": null}),
            ContentFormat::Text,
        )
        .unwrap_err();
        assert_eq!(error.code, error_code::NOTICE_TEMPLATE_VAR_MISSING);
    }

    /// 规则 3（转义）：HTML 格式对变量值转义，TEXT 格式不转义。
    #[test]
    fn render_html_escapes_values() {
        let vars = json!({"name": "<script>alert(1)</script>", "q": "a\"&'b"});
        let html = render("<p>{{name}}</p>", &vars, ContentFormat::Html).expect("应渲染成功");
        assert_eq!(
            html, "<p>&lt;script&gt;alert(1)&lt;/script&gt;</p>",
            "变量值里的标签必须转义成实体"
        );
        let quoted = render("{{q}}", &vars, ContentFormat::Html).expect("应渲染成功");
        assert_eq!(quoted, "a&quot;&amp;&#39;b");

        // TEXT 格式保持原样（站内信纯文本，不涉及 HTML 上下文）
        let text = render("<p>{{name}}</p>", &vars, ContentFormat::Text).expect("应渲染成功");
        assert_eq!(text, "<p><script>alert(1)</script></p>");
    }

    /// 规则 3 加强：HTML 骨架里的 `<script>` 在渲染前被净化掉，变量值保持转义。
    #[test]
    fn render_content_sanitizes_html_skeleton() {
        let vars = json!({"name": "<b>张三</b>"});
        let content = render_content(
            "<p>你好 {{name}}</p><script>alert(1)</script>",
            &vars,
            ContentFormat::Html,
        )
        .expect("应渲染成功");
        assert!(
            content.contains("&lt;b&gt;张三&lt;/b&gt;"),
            "变量值必须被转义"
        );
        assert!(
            !content.contains("<script>"),
            "骨架脚本标签必须被净化：{content}"
        );
    }

    /// 规则 3 补充（嵌套 / 单趟替换）：变量值里的 `{{...}}` 不再被二次渲染。
    #[test]
    fn render_does_not_rescan_values() {
        let vars = json!({"a": "{{b}}", "b": "内层"});
        let out = render("{{a}}", &vars, ContentFormat::Text).expect("应渲染成功");
        assert_eq!(out, "{{b}}", "变量值必须原样输出，不允许递归渲染");
    }

    /// 规则 4（类型转换）：数字 / 布尔 / 数组 / 对象都能落到文本。
    #[test]
    fn render_converts_json_scalars() {
        let vars = json!({
            "int": 42,
            "float": 1.5,
            "flag": true,
            "list": ["a", "b"],
            "obj": {"k": "v"}
        });
        let out = render(
            "{{int}}|{{float}}|{{flag}}|{{list}}|{{obj}}",
            &vars,
            ContentFormat::Text,
        )
        .expect("应渲染成功");
        assert_eq!(out, r#"42|1.5|true|["a","b"]|{"k":"v"}"#);
    }

    /// 规则 5（未知占位）：占位符未在 variables 声明中列出 → 422 + 60024。
    #[test]
    fn ensure_declared_rejects_unknown_placeholder() {
        let ok = ensure_declared("你好 {{name}}", &[decl("name")]);
        assert!(ok.is_ok(), "已声明占位符应通过");

        let error = ensure_declared("你好 {{name}} {{extra}}", &[decl("name")]).unwrap_err();
        assert_eq!(error.code, error_code::NOTICE_TEMPLATE_SYNTAX);
        assert!(error.msg.contains("extra"), "消息要列出未声明变量");
        assert!(!error.msg.contains("name}"), "已声明的变量不该出现在错误里");
    }

    /// 规则 4（长度）：标题按字符数、正文按字节数卡上限。
    #[test]
    fn render_enforces_length_limits() {
        let long_title = "标".repeat(MAX_TITLE_CHARS + 1);
        let error = render_title(&long_title, &json!({}), ContentFormat::Text).unwrap_err();
        assert_eq!(error.code, error_code::NOTICE_TEMPLATE_TOO_LONG);

        let long_content = "正".repeat(MAX_CONTENT_BYTES / 3 + 1);
        let error = render_content(&long_content, &json!({}), ContentFormat::Text).unwrap_err();
        assert_eq!(error.code, error_code::NOTICE_TEMPLATE_TOO_LONG);

        // 边界内允许
        assert!(
            render_title(
                &"标".repeat(MAX_TITLE_CHARS),
                &json!({}),
                ContentFormat::Text
            )
            .is_ok()
        );
    }

    /// 采集与试渲染：`collect_placeholders` 去重排序；`sample_vars` 覆盖缺 sample 的声明。
    #[test]
    fn collect_and_sample_vars() {
        let names = collect_placeholders("{{b}} {{a}} {{b}}").expect("语法应合法");
        assert_eq!(names.into_iter().collect::<Vec<_>>(), vec!["a", "b"]);

        let decls = vec![
            TemplateVarDecl {
                name: "withSample".into(),
                label: Some("有样例".into()),
                required: true,
                sample: Some("样例值".into()),
            },
            TemplateVarDecl {
                name: "noSample".into(),
                label: Some("无样例".into()),
                required: false,
                sample: None,
            },
        ];
        let vars = sample_vars(&decls);
        assert_eq!(vars["withSample"], "样例值");
        assert_eq!(vars["noSample"], "无样例", "缺 sample 时回退到 label");
        // 用样例值试渲染必须成功（模板保存时的即时反馈）
        assert!(
            render_notice("{{withSample}}", "{{noSample}}", &vars, ContentFormat::Text).is_ok()
        );
    }
}
