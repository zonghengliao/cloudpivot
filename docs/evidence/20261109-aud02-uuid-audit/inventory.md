# AUD-02 盘点表（脚本生成，勿手改）

- 含人员字段的结构体：245
- 缺 name：161（出参 DTO 19）
- 生成命令：`python3 scripts/verify/aud02_person_name_inventory.py`

| # | 文件 | 结构体 | 分类 | 人字段 | 缺 name |
|---|---|---|---|---|---|
| 1 | `business/aiagent/dto.rs` | `AiUserRes` | 出参 | user_id | user_id |
| 2 | `business/aiagent/dto.rs` | `AiUserStatusRes` | 出参 | user_id | user_id |
| 3 | `business/aiagent/dto.rs` | `AiUserCredentialRes` | 出参 | user_id | user_id |
| 4 | `business/aiagent/dto.rs` | `AiUserDeleteRes` | 出参 | user_id | user_id |
| 5 | `business/aiagent/model.rs` | `AiAgentRow` | 库行 | user_id | user_id |
| 6 | `business/announcement/dto.rs` | `ReaderRes` | 出参 | user_id | user_id |
| 7 | `business/announcement/model.rs` | `Audience` | 其他 | user_id | user_id |
| 8 | `business/announcement/model.rs` | `ReaderRow` | 库行 | user_id | user_id |
| 9 | `business/changelog/dto.rs` | `ChangeLogPageQuery` | 入参 | operator_id | — |
| 10 | `business/changelog/dto.rs` | `ChangeLogStatsQuery` | 入参 | operator_id | — |
| 11 | `business/changelog/dto.rs` | `ChangeLogListItem` | 出参 | operator_id | — |
| 12 | `business/changelog/dto.rs` | `ChangeLogDetailRes` | 出参 | operator_id | — |
| 13 | `business/changelog/mod.rs` | `RequestCtx` | 其他 | operator_id | — |
| 14 | `business/changelog/query.rs` | `Filter` | 入参 | operator_id | — |
| 15 | `business/changelog/query.rs` | `ChangeLogRow` | 库行 | operator_id | — |
| 16 | `business/erp/budget_model.rs` | `BudgetRow` | 库行 | create_by | create_by |
| 17 | `business/erp/commission_dto.rs` | `SettleReq` | 入参 | salesperson_id | salesperson_id |
| 18 | `business/erp/commission_dto.rs` | `SkippedRes` | 出参 | salesperson_id | — |
| 19 | `business/erp/commission_dto.rs` | `SettlementRes` | 出参 | salesperson_id | — |
| 20 | `business/erp/commission_dto.rs` | `CommissionPageQuery` | 入参 | salesperson_id | salesperson_id |
| 21 | `business/erp/commission_dto.rs` | `CommissionFilter` | 入参 | salesperson_id | salesperson_id |
| 22 | `business/erp/commission_model.rs` | `SettlementRow` | 库行 | salesperson_id | salesperson_id |
| 23 | `business/erp/contract_dto.rs` | `CreateContractReq` | 入参 | owner_id | owner_id |
| 24 | `business/erp/contract_dto.rs` | `ContractDetailRes` | 出参 | owner_id, create_by, update_by | — |
| 25 | `business/erp/contract_model.rs` | `ContractRow` | 库行 | owner_id, create_by, update_by | owner_id, create_by, update_by |
| 26 | `business/erp/contract_model.rs` | `NewContract` | 其他 | owner_id | owner_id |
| 27 | `business/erp/customer_model.rs` | `CustomerRow` | 库行 | create_by, update_by | create_by, update_by |
| 28 | `business/erp/doc_pdf_model.rs` | `ExportLogOwned` | 其他 | user_id | user_id |
| 29 | `business/erp/dto.rs` | `CreateInventoryTxnReq` | 入参 | operator_id | operator_id |
| 30 | `business/erp/dto.rs` | `InventoryTxnRes` | 出参 | operator_id | — |
| 31 | `business/erp/expense_model.rs` | `ReportRow` | 库行 | create_by | create_by |
| 32 | `business/erp/model_inventory.rs` | `InventoryTxnRow` | 库行 | operator_id | operator_id |
| 33 | `business/erp/model_master.rs` | `UnitRow` | 库行 | create_by, update_by | create_by, update_by |
| 34 | `business/erp/model_master.rs` | `WarehouseRow` | 库行 | create_by, update_by | create_by, update_by |
| 35 | `business/erp/model_master.rs` | `LocationRow` | 库行 | create_by, update_by | create_by, update_by |
| 36 | `business/erp/model_master.rs` | `MaterialRow` | 库行 | create_by, update_by | create_by, update_by |
| 37 | `business/erp/model_master.rs` | `SupplierRow` | 库行 | create_by, update_by | create_by, update_by |
| 38 | `business/erp/model_master.rs` | `AccountRow` | 库行 | create_by, update_by | create_by, update_by |
| 39 | `business/erp/model_master.rs` | `SeqRow` | 库行 | create_by, update_by | create_by, update_by |
| 40 | `business/erp/model_order.rs` | `PurchaseOrderRow` | 库行 | create_by, update_by | create_by, update_by |
| 41 | `business/erp/period_owner.rs` | `Owner` | 其他 | user_id | — |
| 42 | `business/erp/purchase_requisition_dto.rs` | `CreateRequisitionReq` | 入参 | applicant_id | applicant_id |
| 43 | `business/erp/purchase_requisition_dto.rs` | `RequisitionRowRes` | 出参 | applicant_id | — |
| 44 | `business/erp/purchase_requisition_model.rs` | `RequisitionRow` | 库行 | applicant_id, create_by | create_by |
| 45 | `business/erp/purchase_requisition_model.rs` | `NewRequisition` | 其他 | applicant_id | — |
| 46 | `business/erp/purchase_return_model.rs` | `ReturnRow` | 库行 | create_by | create_by |
| 47 | `business/erp/receipt_refund.rs` | `RefundRow` | 库行 | create_by | create_by |
| 48 | `business/erp/reminder_contract.rs` | `ContractExpireCandidate` | 其他 | created_by, owner_id | created_by, owner_id |
| 49 | `business/erp/reminder_payable.rs` | `PayableCandidate` | 其他 | created_by | created_by |
| 50 | `business/erp/reminder_purchase.rs` | `PurchaseOverdueCandidate` | 其他 | created_by | created_by |
| 51 | `business/erp/sales_order_handler.rs` | `CreateSalesOrderReq` | 入参 | salesperson_id | salesperson_id |
| 52 | `business/erp/sales_order_handler.rs` | `UpdateSalesOrderReq` | 入参 | salesperson_id | salesperson_id |
| 53 | `business/erp/sales_order_handler.rs` | `SalesOrderRes` | 出参 | salesperson_id | — |
| 54 | `business/erp/sales_order_model.rs` | `SalesOrderRow` | 库行 | salesperson_id, create_by, update_by | salesperson_id, create_by, update_by |
| 55 | `business/erp/sales_return_model.rs` | `ReturnRow` | 库行 | create_by, update_by | create_by, update_by |
| 56 | `business/erp/serial_model.rs` | `SerialRow` | 库行 | create_by, update_by | create_by, update_by |
| 57 | `business/erp/shipment_model.rs` | `ShipmentRow` | 库行 | create_by, update_by | create_by, update_by |
| 58 | `business/erp/stockin_base.rs` | `StockInRow` | 库行 | create_by, update_by | create_by, update_by |
| 59 | `business/erp/stockin_dto.rs` | `StockInRes` | 出参 | create_by | — |
| 60 | `business/erp/stockout_base.rs` | `StockOutRow` | 库行 | create_by, update_by | create_by, update_by |
| 61 | `business/erp/stockout_dto.rs` | `StockOutRes` | 出参 | create_by | — |
| 62 | `business/erp/stocktake_dto.rs` | `StockTakeRes` | 出参 | create_by | — |
| 63 | `business/erp/stocktake_model.rs` | `StockTakeRow` | 库行 | create_by, update_by | create_by, update_by |
| 64 | `business/erp/transfer_base.rs` | `TransferRow` | 库行 | applicant_id, approver_id, create_by, update_by | approver_id, create_by, update_by |
| 65 | `business/erp/transfer_dto.rs` | `TransferRes` | 出参 | applicant_id | — |
| 66 | `business/erp/transfer_scan.rs` | `OverdueCandidate` | 其他 | applicant_id, approver_id | applicant_id, approver_id |
| 67 | `business/flow/assignee.rs` | `Candidate` | 其他 | user_id | user_id |
| 68 | `business/flow/assignee.rs` | `InstanceCtx` | 其他 | initiator_id | initiator_id |
| 69 | `business/flow/dto.rs` | `InstanceRes` | 出参 | initiator_id | — |
| 70 | `business/flow/dto.rs` | `TaskRes` | 出参 | assignee_id | — |
| 71 | `business/flow/dto.rs` | `PreviewAssigneeRes` | 出参 | user_id | user_id |
| 72 | `business/flow/dto.rs` | `InstanceTaskRes` | 出参 | owner_id | — |
| 73 | `business/flow/engine.rs` | `StartReq` | 入参 | initiator_id | — |
| 74 | `business/flow/flow_model_base.rs` | `InstanceRow` | 库行 | initiator_id | — |
| 75 | `business/flow/flow_model_base.rs` | `TaskRow` | 库行 | assignee_id | — |
| 76 | `business/flow/flow_model_base.rs` | `NewInstance` | 其他 | initiator_id | — |
| 77 | `business/flow/flow_model_base.rs` | `NewTask` | 其他 | assignee_id | — |
| 78 | `business/flow/flow_model_base.rs` | `FlowTaskRow` | 库行 | owner_id | — |
| 79 | `business/flow/handler.rs` | `SelfApprovalPersonRes` | 出参 | user_id | user_id |
| 80 | `business/flow/handler.rs` | `SelfApprovalTicketRes` | 出参 | user_id | user_id |
| 81 | `business/flow/internal_control.rs` | `Exclusion` | 其他 | user_id | user_id |
| 82 | `business/flow/self_approval.rs` | `Block` | 其他 | user_id | user_id |
| 83 | `business/flow/self_approval.rs` | `PersonRow` | 库行 | user_id | user_id |
| 84 | `business/flow/self_approval.rs` | `TicketRow` | 库行 | user_id | user_id |
| 85 | `business/help/dto.rs` | `HelpVersionRes` | 出参 | create_by | — |
| 86 | `business/help/model.rs` | `VersionRow` | 库行 | create_by | create_by |
| 87 | `business/impexp/model.rs` | `ImportTask` | 其他 | create_by | create_by |
| 88 | `business/impexp/model.rs` | `ImportTaskFilter` | 入参 | create_by | create_by |
| 89 | `business/impexp/model.rs` | `ExportTask` | 其他 | create_by | create_by |
| 90 | `business/impexp/model.rs` | `ExportTaskFilter` | 入参 | create_by | create_by |
| 91 | `business/masking/dto.rs` | `EffectiveMaskQuery` | 入参 | user_id | user_id |
| 92 | `business/masking/dto.rs` | `EffectiveMaskRes` | 出参 | user_id | user_id |
| 93 | `business/mutex/dto.rs` | `MutexRes` | 出参 | create_by | — |
| 94 | `business/mutex/dto.rs` | `MutexConflictQuery` | 入参 | user_id | user_id |
| 95 | `business/mutex/dto.rs` | `MutexHolderRes` | 出参 | user_id | user_id |
| 96 | `business/mutex/dto.rs` | `MutexConflictRes` | 出参 | user_id | user_id |
| 97 | `business/mutex/model.rs` | `MutexRow` | 库行 | create_by | create_by |
| 98 | `business/mutex/model.rs` | `HolderRow` | 库行 | user_id | user_id |
| 99 | `business/mutex/model.rs` | `UserBrief` | 其他 | user_id | user_id |
| 100 | `business/mutex/model.rs` | `RoleEnableHit` | 其他 | user_id | user_id |
| 101 | `business/notice/dto.rs` | `StreamQuery` | 入参 | user_id | user_id |
| 102 | `business/notice/dto.rs` | `StreamUserStat` | 其他 | user_id | user_id |
| 103 | `business/permission/dto.rs` | `SelfCheckRes` | 出参 | user_id | user_id |
| 104 | `business/permission/scope_model.rs` | `UserBrief` | 其他 | user_id | user_id |
| 105 | `business/project/dto.rs` | `ProjectPageQuery` | 入参 | owner_id | owner_id |
| 106 | `business/project/dto.rs` | `ProjectCreateReq` | 入参 | owner_id | owner_id |
| 107 | `business/project/dto.rs` | `ProjectUpdateReq` | 入参 | owner_id | owner_id |
| 108 | `business/project/dto.rs` | `ProjectMemberAddReq` | 入参 | user_id | user_id |
| 109 | `business/project/dto.rs` | `ProjectRes` | 出参 | owner_id | — |
| 110 | `business/project/dto.rs` | `ProjectMemberRes` | 出参 | user_id | user_id |
| 111 | `business/project/model.rs` | `ProjectRow` | 库行 | owner_id, create_by | create_by |
| 112 | `business/project/model.rs` | `ProjectFilter` | 入参 | owner_id | owner_id |
| 113 | `business/project/model.rs` | `NewProject` | 其他 | owner_id | owner_id |
| 114 | `business/project/model.rs` | `MemberRow` | 库行 | user_id | user_id |
| 115 | `business/shared/state_machine.rs` | `StatusLogRow` | 库行 | operator_id | — |
| 116 | `business/storage/dto.rs` | `StorageFileRes` | 出参 | uploaded_by | — |
| 117 | `business/storage/model.rs` | `SysStorageConfigModel` | 其他 | create_by, update_by | create_by, update_by |
| 118 | `business/storage/model.rs` | `SysStoragePolicyModel` | 其他 | create_by, update_by | create_by, update_by |
| 119 | `business/storage/model.rs` | `StorageFileModel` | 其他 | uploaded_by, create_by, update_by | uploaded_by, create_by, update_by |
| 120 | `business/task/batch.rs` | `BatchTaskAssignReq` | 入参 | owner_id | owner_id |
| 121 | `business/task/collab.rs` | `CommentRow` | 库行 | user_id | — |
| 122 | `business/task/collab.rs` | `WorklogRow` | 库行 | user_id | — |
| 123 | `business/task/dto.rs` | `TaskPageQuery` | 入参 | owner_id | owner_id |
| 124 | `business/task/dto.rs` | `TaskCreateReq` | 入参 | owner_id | owner_id |
| 125 | `business/task/dto.rs` | `TaskUpdateReq` | 入参 | owner_id | owner_id |
| 126 | `business/task/dto.rs` | `TaskCardRes` | 出参 | owner_id | — |
| 127 | `business/task/dto.rs` | `TaskMemberRes` | 出参 | user_id | — |
| 128 | `business/task/dto.rs` | `TaskCommentRes` | 出参 | user_id | — |
| 129 | `business/task/dto.rs` | `TaskDetailRes` | 出参 | creator_id, assigner_id | — |
| 130 | `business/task/dto.rs` | `TaskSaveRes` | 出参 | owner_id | owner_id |
| 131 | `business/task/dto.rs` | `TaskSubtaskCreateReq` | 入参 | owner_id | owner_id |
| 132 | `business/task/dto.rs` | `TaskAssignReq` | 入参 | owner_id | owner_id |
| 133 | `business/task/dto.rs` | `TaskAssignRes` | 出参 | owner_id, previous_owner_id, assigner_id | owner_id, previous_owner_id, assigner_id |
| 134 | `business/task/dto.rs` | `TaskMemberRemoveRes` | 出参 | user_id | user_id |
| 135 | `business/task/dto.rs` | `TaskDependencyItemRes` | 出参 | owner_id | — |
| 136 | `business/task/dto.rs` | `TaskGanttQuery` | 入参 | assignee_id | assignee_id |
| 137 | `business/task/dto.rs` | `GanttTaskRes` | 出参 | owner_id | — |
| 138 | `business/task/dto.rs` | `TaskCommentItemRes` | 出参 | user_id | — |
| 139 | `business/task/dto.rs` | `TaskWorklogItemRes` | 出参 | user_id | — |
| 140 | `business/task/state.rs` | `Actor` | 其他 | user_id | user_id |
| 141 | `business/task/state.rs` | `TaskPeople` | 其他 | owner_id, creator_id, assigner_id | owner_id, creator_id, assigner_id |
| 142 | `business/task/task_model_rows.rs` | `TaskCardRow` | 库行 | owner_id | — |
| 143 | `business/task/task_model_rows.rs` | `TaskDetailRow` | 库行 | creator_id, assigner_id | — |
| 144 | `business/task/task_model_rows.rs` | `TaskMemberRow` | 库行 | user_id | — |
| 145 | `business/task/task_model_rows.rs` | `TaskFilter` | 入参 | owner_id | owner_id |
| 146 | `business/task/task_model_rows.rs` | `TaskFlowDetailRow` | 库行 | initiator_id | — |
| 147 | `business/task/task_model_rows.rs` | `NewTask` | 其他 | owner_id, creator_id, assigner_id | owner_id, creator_id, assigner_id |
| 148 | `business/task/task_model_rows.rs` | `OwnerChange` | 其他 | owner_id, assigner_id | owner_id, assigner_id |
| 149 | `business/task/task_model_rows.rs` | `DependencyRow` | 库行 | owner_id | — |
| 150 | `business/task/task_model_rows.rs` | `GanttTaskRow` | 库行 | owner_id | — |
| 151 | `business/task/task_model_rows.rs` | `GanttFilter` | 入参 | owner_id | owner_id |
| 152 | `business/task/task_model_rows.rs` | `SyncTargetRow` | 库行 | owner_id, creator_id, assigner_id | owner_id, creator_id, assigner_id |
| 153 | `business/task/template.rs` | `TaskTemplateSaveReq` | 入参 | owner_id | owner_id |
| 154 | `business/task/template.rs` | `TaskTemplateRes` | 出参 | owner_id | owner_id |
| 155 | `business/task/template.rs` | `TaskFromTemplateReq` | 入参 | owner_id | owner_id |
| 156 | `business/task/template.rs` | `TemplateRow` | 库行 | owner_id, create_by | owner_id, create_by |
| 157 | `business/attachment/dto.rs` | `DocAttachmentItemRes` | 出参 | uploaded_by | — |
| 158 | `support/config/model.rs` | `SysConfigModel` | 其他 | create_by, update_by | create_by, update_by |
| 159 | `support/dict/model.rs` | `SysDictTypeModel` | 其他 | create_by, update_by | create_by, update_by |
| 160 | `support/dict/model.rs` | `SysDictItemModel` | 其他 | create_by, update_by | create_by, update_by |
| 161 | `support/file/dto.rs` | `FileRes` | 出参 | uploaded_by | — |
| 162 | `support/file/model.rs` | `SysFileModel` | 其他 | uploaded_by, create_by, update_by | uploaded_by, create_by, update_by |
| 163 | `support/job/model.rs` | `SysJobModel` | 其他 | create_by, update_by | create_by, update_by |
| 164 | `support/log/dto.rs` | `LogRes` | 出参 | user_id | user_id |
| 165 | `support/log/model.rs` | `OperationLogModel` | 其他 | user_id | user_id |
| 166 | `support/log/model.rs` | `AuditLogModel` | 其他 | user_id | user_id |
| 167 | `support/log/model.rs` | `AuthLogModel` | 其他 | user_id | user_id |
| 168 | `support/log/model.rs` | `SecurityLogModel` | 其他 | user_id | user_id |
| 169 | `support/log/model.rs` | `PasswordChangeLogModel` | 其他 | user_id, operator_id | user_id, operator_id |
| 170 | `support/online/dto.rs` | `OnlineRes` | 出参 | user_id | user_id |
| 171 | `support/online/model.rs` | `OnlineSession` | 其他 | user_id | user_id |
| 172 | `support/online/model.rs` | `RefreshSession` | 其他 | user_id | user_id |
| 173 | `system/dept/dto.rs` | `DeptNode` | 其他 | leader_id | — |
| 174 | `system/dept/dto.rs` | `DeptDetailRes` | 出参 | leader_id | — |
| 175 | `system/dept/dto.rs` | `DeptSaveReq` | 入参 | leader_id | leader_id |
| 176 | `system/menu/model.rs` | `SysMenuModel` | 其他 | create_by, update_by | create_by, update_by |
| 177 | `system/role/model.rs` | `SysRoleModel` | 其他 | create_by, update_by | create_by, update_by |
| 178 | `system/user/model.rs` | `SysUserModel` | 其他 | create_by, update_by | create_by, update_by |
| 179 | `system/user/model.rs` | `SysUserRoleModel` | 其他 | user_id | user_id |
| 180 | `api/business/announcement-api.ts` | `ReaderRow` | 前端类型 | user_id | user_id |
| 181 | `api/business/batch.ts` | `BatchTaskAssignPayload` | 前端类型 | owner_id | owner_id |
| 182 | `api/business/changelog-api.ts` | `ChangeLogPageQuery` | 前端类型 | operator_id | — |
| 183 | `api/business/changelog-api.ts` | `ChangeLogListItem` | 前端类型 | operator_id | — |
| 184 | `api/business/changelog-api.ts` | `ChangeLogDetail` | 前端类型 | operator_id | — |
| 185 | `api/business/flow-api.ts` | `FlowTaskItem` | 前端类型 | assignee_id | — |
| 186 | `api/business/flow-api.ts` | `FlowInstance` | 前端类型 | initiator_id | — |
| 187 | `api/business/flow-api.ts` | `PreviewAssigneeRes` | 前端类型 | user_id | user_id |
| 188 | `api/business/flow-api.ts` | `FlowSelfApprovalPerson` | 前端类型 | user_id | user_id |
| 189 | `api/business/flow-api.ts` | `FlowSelfApprovalTicket` | 前端类型 | user_id | user_id |
| 190 | `api/business/help-api.ts` | `HelpVersion` | 前端类型 | create_by | — |
| 191 | `api/business/mutex-api.ts` | `MutexRes` | 前端类型 | create_by | — |
| 192 | `api/business/mutex-api.ts` | `MutexHolderRes` | 前端类型 | user_id | user_id |
| 193 | `api/business/mutex-api.ts` | `MutexConflictRes` | 前端类型 | user_id | user_id |
| 194 | `api/business/permission-scope-api.ts` | `ScopeSelfCheckRes` | 前端类型 | user_id | user_id |
| 195 | `api/business/project-api.ts` | `ProjectItem` | 前端类型 | owner_id | — |
| 196 | `api/business/project-api.ts` | `ProjectMember` | 前端类型 | user_id | user_id |
| 197 | `api/business/project-api.ts` | `ProjectPageQuery` | 前端类型 | owner_id | owner_id |
| 198 | `api/business/project-api.ts` | `ProjectCreateReq` | 前端类型 | owner_id | owner_id |
| 199 | `api/business/project-api.ts` | `ProjectUpdateReq` | 前端类型 | owner_id | owner_id |
| 200 | `api/business/task-api.ts` | `TaskFlowDetail` | 前端类型 | initiator_id | — |
| 201 | `api/business/task-api.ts` | `TaskCard` | 前端类型 | owner_id | — |
| 202 | `api/business/task-api.ts` | `TaskMember` | 前端类型 | user_id | — |
| 203 | `api/business/task-api.ts` | `TaskPageQuery` | 前端类型 | owner_id | owner_id |
| 204 | `api/business/task-api.ts` | `TaskCreateReq` | 前端类型 | owner_id | owner_id |
| 205 | `api/business/task-api.ts` | `TaskUpdateReq` | 前端类型 | owner_id | owner_id |
| 206 | `api/business/task-api.ts` | `TaskSaveResult` | 前端类型 | owner_id | owner_id |
| 207 | `api/business/task-api.ts` | `TaskSubtaskCreateReq` | 前端类型 | owner_id | owner_id |
| 208 | `api/business/task-api.ts` | `TaskDependencyItem` | 前端类型 | owner_id | — |
| 209 | `api/business/task-api.ts` | `TaskGanttQuery` | 前端类型 | assignee_id | assignee_id |
| 210 | `api/business/task-api.ts` | `GanttTask` | 前端类型 | owner_id | — |
| 211 | `api/business/task-api.ts` | `TaskCommentItem` | 前端类型 | user_id | — |
| 212 | `api/business/task-api.ts` | `TaskCommentCreateResult` | 前端类型 | user_id | — |
| 213 | `api/business/task-api.ts` | `TaskWorklogItem` | 前端类型 | user_id | — |
| 214 | `api/business/task-api.ts` | `TaskTemplate` | 前端类型 | owner_id | owner_id |
| 215 | `api/business/task-api.ts` | `TaskTemplateSaveReq` | 前端类型 | owner_id | owner_id |
| 216 | `api/business/task-api.ts` | `TaskFromTemplateReq` | 前端类型 | owner_id | owner_id |
| 217 | `api/business/erp/attachment.ts` | `DocAttachmentItem` | 前端类型 | uploaded_by | — |
| 218 | `api/business/erp/commission.ts` | `CommissionSettlementRow` | 前端类型 | salesperson_id | — |
| 219 | `api/business/erp/commission.ts` | `CommissionSettlementFilter` | 前端类型 | salesperson_id | salesperson_id |
| 220 | `api/business/erp/commission.ts` | `SettleCommissionReq` | 前端类型 | salesperson_id | salesperson_id |
| 221 | `api/business/erp/commission.ts` | `CommissionSkippedRow` | 前端类型 | salesperson_id | — |
| 222 | `api/business/erp/index.ts` | `TransferRow` | 前端类型 | applicant_id, approver_id | approver_id |
| 223 | `api/business/erp/index.ts` | `StockInRow` | 前端类型 | create_by | — |
| 224 | `api/business/erp/index.ts` | `InventoryTxnRow` | 前端类型 | operator_id | — |
| 225 | `api/business/erp/index.ts` | `SalesOrder` | 前端类型 | salesperson_id | — |
| 226 | `api/business/erp/index.ts` | `CreateSalesOrderReq` | 前端类型 | salesperson_id | salesperson_id |
| 227 | `api/business/erp/index.ts` | `UpdateSalesOrderReq` | 前端类型 | salesperson_id | salesperson_id |
| 228 | `api/business/erp/purchase-requisition.ts` | `RequisitionRow` | 前端类型 | applicant_id | — |
| 229 | `api/business/erp/purchase-requisition.ts` | `CreateRequisitionPayload` | 前端类型 | applicant_id | applicant_id |
| 230 | `api/business/erp/stock-out.ts` | `StockOutRow` | 前端类型 | create_by | — |
| 231 | `api/business/erp/stock-take.ts` | `StockTakeRow` | 前端类型 | create_by | — |
| 232 | `api/business/storage/index.ts` | `StorageFileRes` | 前端类型 | uploaded_by | — |
| 233 | `api/support/file-api.ts` | `FileRes` | 前端类型 | uploaded_by | — |
| 234 | `api/support/log-api.ts` | `LogRes` | 前端类型 | user_id | user_id |
| 235 | `api/support/online-api.ts` | `OnlineRes` | 前端类型 | user_id | user_id |
| 236 | `api/system/ai-user-api.ts` | `AiUserRes` | 前端类型 | user_id | user_id |
| 237 | `api/system/ai-user-api.ts` | `AiUserStatusRes` | 前端类型 | user_id | user_id |
| 238 | `api/system/ai-user-api.ts` | `AiUserDeleteRes` | 前端类型 | user_id | user_id |
| 239 | `api/system/dept-api.ts` | `DeptNode` | 前端类型 | leader_id | — |
| 240 | `api/system/dept-api.ts` | `DeptSaveParams` | 前端类型 | leader_id | leader_id |
| 241 | `api/system/field-mask-api.ts` | `EffectiveMaskRes` | 前端类型 | user_id | user_id |
| 242 | `views/business/erp/sales-order/sales-order-query.ts` | `SalesOrderFormDraft` | 前端类型 | salesperson_id | salesperson_id |
| 243 | `views/business/flow/components/flow-progress-tasks.ts` | `FlowProgressTask` | 前端类型 | assignee_id | — |
| 244 | `views/business/storage/file/uploader-names.ts` | `UploaderRowLike` | 前端类型 | uploaded_by | — |
| 245 | `views/support/file/uploader-label.ts` | `UploaderFields` | 前端类型 | uploaded_by | — |
