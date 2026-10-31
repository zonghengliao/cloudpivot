# -*- coding: utf-8 -*-
"""E2E 用例总表 · 生成器包。

| 模块 | 内容 |
|---|---|
| `state_machines.py` | 六张单据的状态机全组合（N×N），边表来自 `backend/src/business/shared/state_machine.rs` |
| `flows.py` | 审批流 A1–A15 × 角色视角 |
| `rest.py` | AUTH / PERM / CONS / BOUND / FE / BASE 人工设计用例 |

数据与渲染分家：改用例只改这里，跑 `python3 scripts/verify/gen_test_cases.py` 重出文档，
覆盖率报告里的数字**由本包数据算出**，不手写，避免"表里写 30、实际 28"这类漂移。
"""
