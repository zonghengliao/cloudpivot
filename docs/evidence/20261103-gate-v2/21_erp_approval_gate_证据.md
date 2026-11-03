# 闸② 裁定证据 · business/erp/approval_gate.rs — biz_flow_def

- **裁定**：exempt（函数级，2 处 SQL 同属一个函数）
- **依据（口径 D3）**：函数级 `SCOPE-EXEMPT`，理由同时回答「为什么不需要数据范围」与「权限在哪校验（file:line）」；未把任何「产出用户可见行集」的查询豁免。

## 1. 查询点（实测）

- `approval_gate.rs` `inspect()`：`FROM biz_flow_def d LEFT JOIN biz_flow_def_version v …`（自检逐 biz_type 查当前版本）
- `approval_gate.rs` `health()`：循环 8 个 `FLOW_TYPES` 调 `inspect()`，不直接发 SQL

## 2. 调用链（查询函数 → 调用方）

- `approval_flow_health()`（approval_gate.rs:519，`#[nik::permission]` 在 :516）→ `health()`（:467）→ `inspect()`（:371）

## 3. 权限校验点（file:line）

`business:erp:approval-flow:health`（approval_gate.rs:516 注解 = V0202 迁移同值）；注册宏 `#[nik::get("/business/erp/approval-flow/health")]`。这是配置体检端点，只给管理员 / 实施，不面向业务行集。

## 4. 为什么不注入（注入了会怎样错）

`biz_flow_def` / `biz_flow_def_version` 是**全局流程配置**（无 `dept_id` / 归属人列，V0xxx 建表），不是部门数据。自检的语义是「8 个 biz_type 各自运行时实际会选中那一条定义是否已发布、图形是否校验过」——若注入部门范围，别部门已发布的流程会被判成「没有配置」，上线体检假红，错的是告警而不是数据。

## 5. 补充

与批次 D · D1 口径同源（定义侧全局配置），写法参照 `business/flow/model.rs` 文件头的既有豁免。
