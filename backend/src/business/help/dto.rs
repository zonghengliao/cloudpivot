//! 帮助文档（批次 C · C4）的请求 / 响应 DTO。
//!
//! 契约来源：票面 §接口（`/api/business/help/*`）+ 架构 §3.6（字段语义）。
//!
//! # 四条约定
//!
//! 1. **可空列降级为空串**（与底座其它 DTO 同口径）：`icon` / `description` /
//!    `keywords` 在库里可空，前端不判 `null`；
//! 2. **时间一律 `Option<DateTime<Utc>>`**（RFC3339），可空时间由前端渲染 `-`；
//! 3. **ID 一律 `String`**（`uuid::Uuid` 没开 serde feature，与 `business/storage/dto.rs`
//!    一致）；
//! 4. **检索不给 HTML**：`snippet` 是纯文本，高亮用 `matched_terms` 交给前端渲染
//!    （H-10：片段里不允许出现 `<` / `>`，否则就是新的 XSS 面）。

use nik::prelude::*;
use serde::{Deserialize, Serialize};

use crate::business::help::model::{
    self, CategoryRow, DocCardRow, DocDetailRow, SearchHitRow, VersionRow,
};
use crate::business::shared::content::TocNode;

// ==================== 请求：分类 ====================

/// 分类树查询。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CategoryTreeQuery {
    /// 是否包含空分类（没有任何已发布文档的分支）。管理侧传 `true`。
    pub include_empty: Option<bool>,
    /// 是否包含**停用**分类。管理侧传 `true`（用户侧默认只看启用）。
    pub include_disabled: Option<bool>,
}

/// 分类新建 / 编辑请求。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CategorySaveReq {
    /// 父分类 ID（空 / 不传 = 根）。
    pub parent_id: Option<String>,
    /// 显示名（必填，≤ 100 字）。
    pub name: String,
    /// 编码（必填，`^[a-z][a-z0-9_-]{1,63}$`）。
    pub code: String,
    /// 图标名。
    pub icon: Option<String>,
    /// 描述。
    pub description: Option<String>,
    /// 排序号。
    pub sort_order: Option<i32>,
    /// 状态（1 启用 / 0 停用），缺省 1。
    pub status: Option<i16>,
}

// ==================== 请求：文档 ====================

/// 文档列表查询。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DocPageQuery {
    /// 页码。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
    /// 分类 ID。
    pub category_id: Option<String>,
    /// 是否含子分类（缺省 `true`：用户点一级分类时想看整棵子树）。
    pub include_children: Option<bool>,
    /// 标题 / 摘要关键词。
    pub keyword: Option<String>,
    /// 状态（`DRAFT` / `PUBLISHED` / `OFFLINE`）。**仅管理侧有效**，需要
    /// `system:help:doc:list`（见 handler 的说明）。
    pub status: Option<String>,
    /// 是否包含未发布（同 `status` 的管理侧开关）。
    pub include_unpublished: Option<bool>,
}

impl DocPageQuery {
    /// 转义后的 `ILIKE` 模式（空 / 纯空白返回 `None`）。
    ///
    /// # 为什么不复用 `utils::like_pattern`
    ///
    /// 它只做 `%{k}%` 包装、**不转义**通配符：用户搜 `100%` 会退化成
    /// `LIKE '%100%%'`（语义漂移），搜 `a_b` 会把 `_` 当单字符通配（H-08 的反面）。
    pub fn keyword_pattern(&self) -> Option<String> {
        let keyword = self.keyword.as_deref().map(str::trim).unwrap_or_default();
        if keyword.is_empty() {
            return None;
        }
        Some(model::like_pattern(keyword))
    }
}

/// 文档新建 / 编辑请求。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DocSaveReq {
    /// 分类 ID（必填）。
    pub category_id: String,
    /// 标题（必填，≤ 200 字）。
    pub title: String,
    /// 对外 slug（必填，`^[a-z0-9][a-z0-9-]{1,119}$`）。
    pub slug: String,
    /// 摘要（留空则从正文前 120 字自动派生）。
    pub summary: Option<String>,
    /// Markdown 正文（必填，≤ 256 KiB）。
    pub content_md: String,
    /// 关键词（逗号分隔，≤ 255 字）。
    pub keywords: Option<String>,
    /// 排序号。
    pub sort_order: Option<i32>,
}

/// 发布 / 回滚的变更说明（请求体可为空）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ChangeNoteReq {
    /// 变更说明（写进 `sys_help_doc_version.change_note`）。
    pub change_note: Option<String>,
}

/// 检索查询。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct HelpSearchQuery {
    /// 检索串（`?q=`，多词空格分隔，AND 连接，最多 5 个词）。
    pub q: Option<String>,
    /// 页码。
    pub page: Option<u64>,
    /// 每页条数。
    pub page_size: Option<u64>,
    /// 状态（管理侧可传 `DRAFT`）。
    pub status: Option<String>,
    /// 分类 ID（含子树）。
    pub category_id: Option<String>,
}

// ==================== 响应：分类 ====================

/// 分类树节点。
#[derive(Debug, Clone, Serialize, Default)]
pub struct HelpCategoryNodeRes {
    /// 主键。
    pub id: String,
    /// 父 ID（根为空）。
    pub parent_id: Option<String>,
    /// 显示名。
    pub name: String,
    /// 编码。
    pub code: String,
    /// 图标名（空串表示未设置）。
    pub icon: String,
    /// 描述（空串表示未设置）。
    pub description: String,
    /// 排序号。
    pub sort_order: i32,
    /// 状态（1 启用 / 0 停用）。
    pub status: i16,
    /// 该分类下已发布文档数。
    pub doc_count: i64,
    /// 子节点。
    pub children: Vec<HelpCategoryNodeRes>,
}

impl HelpCategoryNodeRes {
    /// 由分类行构造（`children` 由 `utils::build_tree` 回填）。
    pub fn from_row(row: &CategoryRow) -> Self {
        Self {
            id: row.id.to_string(),
            parent_id: row.parent_id.map(|v| v.to_string()),
            name: row.name.clone(),
            code: row.code.clone(),
            icon: row.icon.clone().unwrap_or_default(),
            description: row.description.clone().unwrap_or_default(),
            sort_order: row.sort_order,
            status: row.status,
            doc_count: row.doc_count,
            children: Vec::new(),
        }
    }

    /// 节点自身的已发布文档数 + 所有后代的（父节点角标要含子树，否则一级分类永远是 0）。
    ///
    /// # 返回值
    ///
    /// 子树文档合计。
    pub fn subtree_doc_count(&self) -> i64 {
        self.doc_count
            + self
                .children
                .iter()
                .map(Self::subtree_doc_count)
                .sum::<i64>()
    }

    /// 递归累加子树文档数（**不删任何分支**），父节点角标 = 子树合计。
    ///
    /// # 返回值
    ///
    /// 子树文档合计。
    pub fn accumulate(&mut self) -> i64 {
        let mut total = self.doc_count;
        for child in &mut self.children {
            total += child.accumulate();
        }
        self.doc_count = total;
        total
    }

    /// 递归累加子树文档数，并**剪掉**整棵子树都没有已发布文档的分支。
    ///
    /// # 返回值
    ///
    /// 剪枝后的子树文档合计；返回 `0` 表示整棵子树都没有已发布文档（调用方可丢弃）。
    pub fn prune_and_sum(&mut self) -> i64 {
        let mut total = self.doc_count;
        let mut kept = Vec::new();
        for mut child in std::mem::take(&mut self.children) {
            let child_total = child.prune_and_sum();
            if child_total > 0 {
                total += child_total;
                kept.push(child);
            }
        }
        self.children = kept;
        self.doc_count = total;
        total
    }
}

// ==================== 响应：文档 ====================

/// 文档卡片（列表项，不含正文）。
#[derive(Debug, Clone, Serialize)]
pub struct HelpDocCardRes {
    /// 主键。
    pub id: String,
    /// 标题。
    pub title: String,
    /// slug。
    pub slug: String,
    /// 摘要。
    pub summary: String,
    /// 分类 ID。
    pub category_id: String,
    /// 分类名。
    pub category_name: String,
    /// 关键词（逗号分隔，空串表示未填）。
    pub keywords: String,
    /// 当前版本号。
    pub version: i32,
    /// 状态。
    pub status: String,
    /// 排序号。
    pub sort_order: i32,
    /// 阅读量。
    pub view_count: i32,
    /// 发布时间。
    pub published_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl HelpDocCardRes {
    /// 由卡片行构造。
    pub fn from_row(row: &DocCardRow) -> Self {
        Self {
            id: row.id.to_string(),
            title: row.title.clone(),
            slug: row.slug.clone(),
            summary: row.summary.clone(),
            category_id: row.category_id.to_string(),
            category_name: row.category_name.clone(),
            keywords: row.keywords.clone(),
            version: row.version,
            status: row.status.clone(),
            sort_order: row.sort_order,
            view_count: row.view_count,
            published_at: row.published_at,
            updated_at: row.updated_at,
        }
    }
}

/// 目录节点（TOC）。
#[derive(Debug, Clone, Serialize)]
pub struct HelpTocRes {
    /// 偏移后的标题层级（2..=6）。
    pub level: u8,
    /// 标题纯文本。
    pub text: String,
    /// 锚点 id（`h-{level}-{index}`，HTML 里 `h2#h-2-1` 可直接跳）。
    pub id: String,
    /// 子节点。
    pub children: Vec<HelpTocRes>,
}

impl HelpTocRes {
    /// 由内容管线的目录树转换。
    pub fn from_node(node: &TocNode) -> Self {
        Self {
            level: node.level,
            text: node.text.clone(),
            id: node.id.clone(),
            children: node.children.iter().map(Self::from_node).collect(),
        }
    }

    /// 整棵树的节点数（写日志 / 断言用）。
    pub fn count(&self) -> usize {
        1 + self.children.iter().map(Self::count).sum::<usize>()
    }
}

/// 文档详情。
#[derive(Debug, Clone, Serialize)]
pub struct HelpDocDetailRes {
    /// 主键。
    pub id: String,
    /// 标题。
    pub title: String,
    /// slug。
    pub slug: String,
    /// 摘要。
    pub summary: String,
    /// 分类 ID。
    pub category_id: String,
    /// 分类名。
    pub category_name: String,
    /// 关键词。
    pub keywords: String,
    /// Markdown 原文（管理侧回显用；用户侧也会返回，方便"复制为 Markdown"）。
    pub content_md: String,
    /// **净化后**的 HTML（闸 3 已过；由 `shared/content` 生成）。
    pub content_html: String,
    /// 目录树。
    pub toc: Vec<HelpTocRes>,
    /// 当前版本号。
    pub version: i32,
    /// 状态。
    pub status: String,
    /// 阅读量。
    pub view_count: i32,
    /// 发布时间。
    pub published_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 创建时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 更新时间。
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl HelpDocDetailRes {
    /// 由详情行 + 已净化的 HTML + 目录树构造。
    ///
    /// # 参数
    ///
    /// - `row`：详情行；
    /// - `content_html`：**已经过闸 3 净化**的 HTML；
    /// - `toc`：目录树。
    pub fn from_row(row: &DocDetailRow, content_html: String, toc: Vec<HelpTocRes>) -> Self {
        Self {
            id: row.id.to_string(),
            title: row.title.clone(),
            slug: row.slug.clone(),
            summary: row.summary.clone(),
            category_id: row.category_id.to_string(),
            category_name: row.category_name.clone(),
            keywords: row.keywords.clone(),
            content_md: row.content_md.clone(),
            content_html,
            toc,
            version: row.version,
            status: row.status.clone(),
            view_count: row.view_count,
            published_at: row.published_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// 版本快照列表项。
#[derive(Debug, Clone, Serialize)]
pub struct HelpVersionRes {
    /// 版本号。
    pub version: i32,
    /// 该版本的标题。
    pub title: String,
    /// 变更说明（未填为空串）。
    pub change_note: String,
    /// 快照时间。
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 操作人 ID（未知为空）。
    pub create_by: String,
    /// 正文字节数。
    pub content_bytes: i64,
}

impl HelpVersionRes {
    /// 由版本行构造。
    pub fn from_row(row: &VersionRow) -> Self {
        Self {
            version: row.version,
            title: row.title.clone(),
            change_note: row.change_note.clone().unwrap_or_default(),
            created_at: row.created_at,
            create_by: row.create_by.map(|v| v.to_string()).unwrap_or_default(),
            content_bytes: row.content_bytes,
        }
    }
}

/// 检索命中项。
#[derive(Debug, Clone, Serialize)]
pub struct HelpSearchHitRes {
    /// 主键。
    pub id: String,
    /// 标题。
    pub title: String,
    /// slug。
    pub slug: String,
    /// 摘要。
    pub summary: String,
    /// 分类名。
    pub category_name: String,
    /// 命中片段（**纯文本**，无 `<` / `>`）。
    pub snippet: String,
    /// 本次实际命中的检索词（前端据此做高亮，服务端不返回 HTML 标记）。
    pub matched_terms: Vec<String>,
    /// 状态。
    pub status: String,
    /// 发布时间。
    pub published_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl HelpSearchHitRes {
    /// 由命中行构造（片段与命中词在应用层算，避免把 `content_md` 的 20 KB 直接下发）。
    pub fn from_row(row: &SearchHitRow, terms: &[String]) -> Self {
        let plain = crate::business::shared::content::to_plain_text(&row.content_md);
        let (snippet, matched_terms) = model::make_snippet(&plain, terms);
        Self {
            id: row.id.to_string(),
            title: row.title.clone(),
            slug: row.slug.clone(),
            summary: row.summary.clone(),
            category_name: row.category_name.clone(),
            snippet,
            matched_terms,
            status: row.status.clone(),
            published_at: row.published_at,
        }
    }
}

/// 阅读量 +1 的结果。
#[derive(Debug, Clone, Serialize)]
pub struct HelpReadRes {
    /// 当前阅读量。
    pub view_count: i32,
    /// 本次是否真的计了数（`false` = 同日重复访问，被去重）。
    pub counted: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, doc_count: i64, children: Vec<HelpCategoryNodeRes>) -> HelpCategoryNodeRes {
        HelpCategoryNodeRes {
            id: id.to_string(),
            doc_count,
            children,
            ..Default::default()
        }
    }

    /// 分类树角标：父节点要含子树（否则一级分类永远显示 0）。
    #[test]
    fn subtree_doc_count_sums_children() {
        let tree = node(
            "root",
            1,
            vec![
                node("a", 2, vec![node("a1", 3, vec![])]),
                node("b", 4, vec![]),
            ],
        );
        assert_eq!(tree.subtree_doc_count(), 10);
    }

    /// 剪枝：整棵子树没有文档的分支要被删掉，有文档的分支保留但角标改成合计。
    #[test]
    fn prune_removes_empty_branches() {
        let mut tree = node(
            "root",
            0,
            vec![
                node("empty", 0, vec![node("empty-child", 0, vec![])]),
                node("has", 0, vec![node("has-child", 5, vec![])]),
            ],
        );
        let total = tree.prune_and_sum();
        assert_eq!(total, 5);
        assert_eq!(tree.children.len(), 1, "空分支必须被剪掉");
        assert_eq!(tree.children[0].id, "has");
        assert_eq!(tree.children[0].doc_count, 5, "父节点角标应为子树合计");
    }

    /// 关键词模式必须转义通配符（H-08 的反面）。
    #[test]
    fn keyword_pattern_escapes_wildcards() {
        let query = DocPageQuery {
            keyword: Some(" 100%_x ".to_string()),
            ..Default::default()
        };
        assert_eq!(query.keyword_pattern().as_deref(), Some("%100\\%\\_x%"));

        let blank = DocPageQuery {
            keyword: Some("   ".to_string()),
            ..Default::default()
        };
        assert_eq!(blank.keyword_pattern(), None);
    }

    /// TOC 节点计数（含子节点）。
    #[test]
    fn toc_count_is_recursive() {
        let toc = HelpTocRes {
            level: 2,
            text: "一".into(),
            id: "h-2-1".into(),
            children: vec![HelpTocRes {
                level: 3,
                text: "1.1".into(),
                id: "h-3-2".into(),
                children: vec![],
            }],
        };
        assert_eq!(toc.count(), 2);
    }
}
