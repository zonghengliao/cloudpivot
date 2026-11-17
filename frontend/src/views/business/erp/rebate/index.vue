<!--
  销售返利（M3-19 前端 · 政策配置 + 结算单）

  ```
  两个 tab：返利政策（政策头 + 规则档位 CRUD）
            返利结算（试算 / 计算落库 / 整期跑批 / 重算 / 作废 / 详情）
  ```

  # 一个侧边栏节点、两个 tab（不是两个菜单行）

  `sys_menu` 里本模块只有一个页面节点（`…4380`，V0438 建），5 个按钮码全部挂在它下面
  （政策 3 个 V0438、结算 3 个中的 list/calc/delete 由 V0439 挂、invoice 由 V0440 挂）。
  菜单总数实测已顶 `menu_invariants` ⑤ 的上限（540），**不能**再插一行页面节点，
  所以结算做成同一页的第二个 tab；V0443 只把这一行的 path / component / is_visible 补齐
  （`/business/erp/rebate-policy` + `business/erp/rebate/index`），title 收敛成侧栏短名「销售返利」。

  # 接口（全部走 `@/api/business/erp/rebate`）

  | 块 | 接口 | 权限码 |
  |---|---|---|
  | 政策列表 / 详情 | `GET /rebate-policy/page`、`GET /rebate-policy/{id}` | `business:erp:rebate-policy:list` |
  | 政策增改删 | `POST` / `PUT` / `DELETE /rebate-policy[...]` | `…:create` / `…:update` / `…:delete` |
  | 试算（不落库） | `POST /rebate-settlement/preview` | `business:erp:rebate-settlement:calc` |
  | 计算落库 / 跑批 / 重算 | `POST /rebate-settlement`、`/run`、`/{id}/recalc` | 同上 |
  | 结算列表 / 详情 | `GET /rebate-settlement/page`、`/{id}` | `business:erp:rebate-settlement:list` |
  | 作废 | `DELETE /rebate-settlement/{id}?remark=` | `…:delete` |

  # 六条与「想当然」不一样的交互（每条都有真接口依据）

  1. **政策编辑是整单覆盖**（`PUT` 全量）：弹窗必须提交全部档位 —— 只发改动的那一档，
     别的档位会被整单替换掉（后端 `rebate_handler::update_policy_of` 的注释原文）。
  2. **三种规则类型的「档位形状」是硬约束**（前端 `validateRules` 与后端 `checked_rules`
     同序）：按量/按额恰好 1 档且 `0 ~ ∞`，阶梯 2~20 档首尾相接、末档开口。切换类型时
     表单自动整形（`onRuleTypeChange`），不让人点了类型就撞 400。
  3. **试算不落库、且要显示「这期已经算过」**：`preview.existing` 非空时提示已有结算单，
     避免同一期间算两遍（库层 `uk_biz_srs_period` 会 409，但先提示体验更好）。
  4. **「没有政策」是业务结果不是系统错误**：`409 + 66091` 的报文写清缺哪段日期，
     页面原样展示（`t-alert` 里），不做「操作失败」这类泛化。
  5. **跑批逐客户独立**：`CREATED` / `EXISTS` / `FAILED` 三种结果都要列出来，
     失败的排最前面 —— 「跑完 0 张」和「跑完 5 张有 1 家挂」是两件事。
  6. **作废只对 `DRAFT`**：`SETTLED` 已开红字发票（M3-17），想改只能红冲；按钮不渲染，
     后端也会 409。

  # 本页不做的（已立卡/上报）

  - **「生成红字发票」按钮**：接口与权限码（`business:erp:rebate-settlement:invoice`，M3-17）
    已就绪，但票面预览 + 凭证口径确认是独立交互，本卡只读展示 `invoice_no` / `settled_at`。
-->
<template>
  <div class="rebate-page">
    <!-- ==================== ⓞ tab（两个 tab 各自一个 ListPage） ==================== -->
    <t-tabs v-model="activeTab" class="page-tabs" data-testid="rebate-tabs" @change="handleTabChange">
      <t-tab-panel value="policy" label="返利政策" data-testid="rebate-tab-policy" />
      <t-tab-panel value="settlement" label="返利结算" data-testid="rebate-tab-settlement" />
    </t-tabs>

    <!-- ==================== ① 返利政策 ==================== -->
    <ListPage
      v-if="activeTab === 'policy'"
      ref="policyListRef"
      title="返利政策"
      meta-unit="条"
      testid="rebate-policy"
      table-testid="rebate-policy-table"
      meta-testid="rebate-policy-meta"
      btn-prefix="btn-rebate-policy"
      :columns="POLICY_COLUMNS"
      :filters="policyFilters"
      :filter-state="policyFilter"
      :initial-page="policyPage"
      :fetcher="policyFetcher"
      :query-builder="policyQueryBuilder"
      :page-size="POLICY_PAGE_SIZE"
      @loaded="onPolicyLoaded"
    >
      <template #head-extra>
        <span class="head-tags">
          <t-tag size="small" variant="light" theme="primary">按量 / 按额 / 阶梯</t-tag>
          <span class="page-hint">同一客户 × 同一粒度，启用政策的有效期不能重叠</span>
        </span>
      </template>

      <template #filter-footer>
        <p class="page-tip">
          返利金额 = 期间<b>净基数</b>（已确认订单 − 已过账退货）× 政策费率；按量按销量、按额按销售额。
          阶梯是<b>落档</b>不是累进（命中哪档就按哪档费率乘）。编辑政策是<b>整单覆盖</b>：改类型时旧口径的档位会被一并替换。
        </p>
      </template>

      <template #toolbar>
        <t-tooltip
          :content="
            canPolicyCreate
              ? '新建一条返利政策（取号 BR-yyyyMMdd-000001；客户须为启用状态）'
              : '当前账号没有 business:erp:rebate-policy:create 权限'
          "
          placement="top-right"
        >
          <span class="gate-wrap">
            <t-button
              theme="primary"
              data-testid="btn-rebate-policy-new"
              :disabled="!canPolicyCreate"
              @click="openPolicyCreate"
            >
              <template #icon><t-icon name="add" /></template>
              新建政策
            </t-button>
          </span>
        </t-tooltip>
      </template>

      <template #rule_type="{ row }">
        <t-tag size="small" variant="outline" :theme="ruleTypeTheme(row.rule_type)">
          {{ ruleTypeLabel(row.rule_type) }}
        </t-tag>
      </template>
      <template #customer="{ row }">
        <span class="ellipsis-cell">{{ customerText(row as RebatePolicyRow) }}</span>
      </template>
      <template #rule_detail="{ row }">
        <span class="mono ellipsis-cell">{{ policyRuleSummary(row as RebatePolicyRow) }}</span>
      </template>
      <template #settle_period="{ row }">
        <span>{{ settlePeriodLabel(row.settle_period) }}</span>
      </template>
      <template #validity="{ row }">
        <span class="mono nowrap">{{ validityText(row.effective_from, row.effective_to) }}</span>
      </template>
      <template #status="{ row }">
        <t-tag size="small" variant="light" :theme="policyStatusTheme(row.status)">
          {{ policyStatusLabel(row.status) }}
        </t-tag>
      </template>

      <template #op="{ row }">
        <div class="op-row">
          <t-link
            v-privilege="[PERM.policyUpdate]"
            theme="primary"
            data-testid="btn-rebate-policy-edit"
            @click="openPolicyEdit(row as RebatePolicyRow)"
          >
            编辑
          </t-link>
          <t-link
            v-privilege="[PERM.policyUpdate]"
            :theme="row.status === 1 ? 'warning' : 'success'"
            data-testid="btn-rebate-policy-toggle"
            @click="confirmPolicyToggle(row as RebatePolicyRow)"
          >
            {{ row.status === 1 ? '停用' : '启用' }}
          </t-link>
          <t-link
            v-privilege="[PERM.policyDelete]"
            theme="danger"
            data-testid="btn-rebate-policy-delete"
            @click="confirmPolicyDelete(row as RebatePolicyRow)"
          >
            删除
          </t-link>
        </div>
      </template>

      <template #empty>
        <div class="panel-empty" data-testid="rebate-policy-empty">
          <t-icon name="file" class="panel-empty__icon" />
          <p class="panel-empty__title">{{ policyEmpty }}</p>
        </div>
      </template>
    </ListPage>

    <!-- ==================== ② 返利结算 ==================== -->
    <ListPage
      v-else
      ref="settleListRef"
      title="返利结算"
      meta-unit="张"
      testid="rebate-settle"
      table-testid="rebate-settle-table"
      meta-testid="rebate-settle-meta"
      btn-prefix="btn-rebate-settle"
      :columns="SETTLEMENT_COLUMNS"
      :filters="settleFilters"
      :filter-state="settleFilter"
      :initial-page="settlePage"
      :fetcher="settleFetcher"
      :query-builder="settleQueryBuilder"
      :page-size="SETTLE_PAGE_SIZE"
      @loaded="onSettleLoaded"
    >
      <template #head-extra>
        <span class="head-tags">
          <t-tag size="small" variant="light" theme="primary">净销售口径 · 期间 × 客户</t-tag>
          <span class="page-hint">一客户 × 一粒度 × 一期间只留一张未作废单</span>
        </span>
      </template>

      <template #filter-footer>
        <p class="page-tip">
          基数 = 期间内<b>已确认订单 − 已过账退货</b>（订单白名单 APPROVED / PARTIAL_OUT / OUTBOUND / CLOSED；
          期间按订单日期）。净额为负时按 <b>0</b> 落档 —— 负返利属于红冲，不在这里算。
          草稿可重算 / 可作废；已结算（已开红字发票）只能红冲。
        </p>
      </template>

      <template #toolbar>
        <div class="toolbar-row">
          <t-tooltip
            :content="
              canSettleCalc
                ? '选定客户 + 粒度 + 期间：先试算看数，再决定是否落库'
                : '当前账号没有 business:erp:rebate-settlement:calc 权限'
            "
            placement="top-right"
          >
            <span class="gate-wrap">
              <t-button
                theme="primary"
                data-testid="btn-rebate-settle-calc"
                :disabled="!canSettleCalc"
                @click="openCalc"
              >
                <template #icon><t-icon name="calculator" /></template>
                试算 / 计算
              </t-button>
            </span>
          </t-tooltip>
          <t-tooltip
            :content="
              canSettleCalc
                ? '按期间给「该期间有政策的全部客户」各算一张草稿（已有则跳过）'
                : '当前账号没有 business:erp:rebate-settlement:calc 权限'
            "
            placement="top-right"
          >
            <span class="gate-wrap">
              <t-button
                variant="outline"
                data-testid="btn-rebate-settle-run"
                :disabled="!canSettleCalc"
                @click="openRun"
              >
                <template #icon><t-icon name="play-circle" /></template>
                整期跑批
              </t-button>
            </span>
          </t-tooltip>
        </div>
      </template>

      <template #customer="{ row }">
        <span class="ellipsis-cell">{{ customerText(row as RebateSettlementRow) }}</span>
      </template>
      <template #period="{ row }">
        <span class="mono nowrap">
          {{ periodCodeLabel(row.period_code) }}
          <span class="period-range">{{ periodRangeText(row.period_from, row.period_last_day) }}</span>
        </span>
      </template>
      <template #policy_no="{ row }">
        <span class="mono ellipsis-cell">{{ row.policy_no }}</span>
      </template>
      <template #base="{ row }">
        <span class="mono nowrap" :class="{ 'text-danger': isNegativeBase(row.base) }">
          {{ baseCellText(row.base) }}
        </span>
      </template>
      <template #tier="{ row }">
        <span class="mono ellipsis-cell">{{ tierHitText(row as RebateSettlementRow) }}</span>
      </template>
      <template #rebate_amount="{ row }">
        <span class="mono amount-cell" data-testid="rebate-settle-amount">
          {{ moneyText(row.rebate_amount) }}
        </span>
      </template>
      <template #status="{ row }">
        <t-tag size="small" variant="light" :theme="settleStatusTheme(row.status)">
          {{ settleStatusLabel(row.status) }}
        </t-tag>
      </template>
      <template #invoice="{ row }">
        <span class="mono ellipsis-cell">{{ invoiceText(row as RebateSettlementRow) }}</span>
      </template>

      <template #op="{ row }">
        <div class="op-row">
          <t-link theme="primary" data-testid="btn-rebate-settle-detail" @click="openDetail(row as RebateSettlementRow)">
            详情
          </t-link>
          <t-link
            v-if="isEditable(row as RebateSettlementRow)"
            v-privilege="[PERM.settleCalc]"
            theme="warning"
            data-testid="btn-rebate-settle-recalc"
            @click="openRecalc(row as RebateSettlementRow)"
          >
            重算
          </t-link>
          <t-link
            v-if="isEditable(row as RebateSettlementRow)"
            v-privilege="[PERM.settleDelete]"
            theme="danger"
            data-testid="btn-rebate-settle-cancel"
            @click="openCancel(row as RebateSettlementRow)"
          >
            作废
          </t-link>
        </div>
      </template>

      <!-- 页脚合计：只汇总当前页（拿不到全量，绝不假装有） -->
      <template #footer>
        <div v-if="settleTotals.count" class="totals-strip" data-testid="rebate-settle-totals">
          <span>本页 {{ settleTotals.count }} 张</span>
          <span class="totals-strip__sep">·</span>
          <span>净销售额 {{ moneyText(settleTotals.netAmount) }}</span>
          <span class="totals-strip__sep">·</span>
          <span>应返合计 {{ moneyText(settleTotals.rebateAmount) }}</span>
          <span class="totals-strip__hint">（只合计当前页，不是全量）</span>
        </div>
      </template>

      <template #empty>
        <div class="panel-empty" data-testid="rebate-settle-empty">
          <t-icon name="file" class="panel-empty__icon" />
          <p class="panel-empty__title">{{ settleEmpty }}</p>
        </div>
      </template>
    </ListPage>

    <!-- ==================== ③ 政策新建 / 编辑弹窗 ==================== -->
    <t-dialog
      v-model:visible="policyFormVisible"
      :header="policyEditingId ? '编辑返利政策' : '新建返利政策'"
      :width="860"
      :confirm-btn="{ content: policyEditingId ? '保存' : '创建', loading: policySubmitting, disabled: policySubmitting }"
      :cancel-btn="{ disabled: policySubmitting }"
      :close-on-overlay-click="false"
      data-testid="rebate-policy-form-dialog"
      @confirm="submitPolicyForm"
    >
      <div class="form-body">
        <t-form label-width="120px" class="form">
          <t-form-item label="政策名称" required-mark>
            <t-input
              v-model="policyForm.policy_name"
              :maxlength="POLICY_NAME_MAX"
              placeholder="人读名，例如「2026 年度华东经销商返利」"
              data-testid="rebate-policy-form-name"
            />
          </t-form-item>

          <t-form-item label="客户" required-mark>
            <t-select
              v-if="!customerOptionsFailed"
              v-model="policyForm.customer_id"
              :options="customerOptions"
              filterable
              clearable
              placeholder="选择客户（只列启用客户）"
              data-testid="rebate-policy-form-customer"
            />
            <t-input
              v-else
              v-model="policyForm.customer_id"
              placeholder="候选列表无权限读取：直接粘贴客户 UUID（biz_customer.id）"
              data-testid="rebate-policy-form-customer-id"
            />
          </t-form-item>

          <t-form-item label="规则类型" required-mark>
            <div class="rule-type-block">
              <t-radio-group
                :value="policyForm.rule_type"
                data-testid="rebate-policy-form-type"
                @change="handleRuleTypeChange"
              >
                <t-radio v-for="option in RULE_TYPE_OPTIONS" :key="option.value" :value="option.value">
                  {{ option.label }}
                </t-radio>
              </t-radio-group>
              <p class="form-hint">{{ currentRuleTypeHint }}</p>
            </div>
          </t-form-item>

          <t-form-item v-if="policyForm.rule_type === 'TIERED'" label="计价单位" required-mark>
            <t-select
              v-model="policyForm.rate_unit"
              :options="RATE_UNIT_OPTIONS"
              data-testid="rebate-policy-form-unit"
            />
          </t-form-item>
          <t-form-item v-else label="计价单位">
            <span class="form-static" data-testid="rebate-policy-form-unit-fixed">
              {{ rateUnitLabel(policyForm.rate_unit) }}（由规则类型固定）
            </span>
          </t-form-item>

          <t-form-item label="结算粒度" required-mark>
            <t-radio-group v-model="policyForm.settle_period" data-testid="rebate-policy-form-period">
              <t-radio value="MONTH">月度（yyyy-MM）</t-radio>
              <t-radio value="QUARTER">季度（yyyy-QN）</t-radio>
            </t-radio-group>
          </t-form-item>

          <t-form-item label="生效日" required-mark>
            <t-date-picker
              v-model="policyForm.effective_from"
              value-type="YYYY-MM-DD"
              format="YYYY-MM-DD"
              data-testid="rebate-policy-form-from"
            />
          </t-form-item>

          <t-form-item label="失效日">
            <t-date-picker
              v-model="policyForm.effective_to"
              value-type="YYYY-MM-DD"
              format="YYYY-MM-DD"
              placeholder="留空 = 长期有效"
              clearable
              data-testid="rebate-policy-form-to"
            />
          </t-form-item>

          <t-form-item label="状态">
            <t-radio-group v-model="policyForm.status" data-testid="rebate-policy-form-status">
              <t-radio :value="1">启用（参与结算）</t-radio>
              <t-radio :value="0">停用（保留历史，不参与结算）</t-radio>
            </t-radio-group>
          </t-form-item>

          <!-- 档位编辑器：按量/按额是单档（只填费率）；阶梯是 2~20 行 -->
          <t-form-item :label="isFlatRule ? '返利费率' : '规则档位'" required-mark>
            <div class="tier-block">
              <!-- 平铺：一档，区间固定 0 ~ ∞ -->
              <div v-if="isFlatRule" class="flat-tier">
                <span class="flat-tier__range mono">0 ~ ∞（平铺单档）</span>
                <t-input-number
                  v-model="policyForm.rules[0].rate"
                  theme="normal"
                  :decimal-places="6"
                  :min="0"
                  align="right"
                  :suffix="rateSuffix(policyForm.rate_unit)"
                  :placeholder="ratePlaceholder(policyForm.rate_unit)"
                  data-testid="rebate-policy-form-rate"
                />
              </div>

              <!-- 阶梯：区间左闭右开，末档开口 -->
              <div v-else class="tier-table" data-testid="rebate-policy-form-tiers">
                <div class="tier-row tier-row--head">
                  <span>档位</span>
                  <span>下限（含）</span>
                  <span>上限（不含）</span>
                  <span>费率</span>
                  <span>备注</span>
                  <span />
                </div>
                <div v-for="(tier, index) in policyForm.rules" :key="index" class="tier-row">
                  <span class="tier-seq">第 {{ index + 1 }} 档</span>
                  <t-input-number
                    v-model="tier.tier_from"
                    theme="normal"
                    :decimal-places="4"
                    :min="0"
                    align="right"
                    :disabled="index === 0"
                    :data-testid="`rebate-policy-form-tier-from-${index}`"
                  />
                  <t-input-number
                    v-model="tier.tier_to"
                    theme="normal"
                    :decimal-places="4"
                    :min="0"
                    align="right"
                    :placeholder="index === policyForm.rules.length - 1 ? '留空 = 无上限' : '必填'"
                    :data-testid="`rebate-policy-form-tier-to-${index}`"
                    @change="syncTierBound(policyForm, index)"
                  />
                  <t-input-number
                    v-model="tier.rate"
                    theme="normal"
                    :decimal-places="6"
                    :min="0"
                    align="right"
                    :suffix="rateSuffix(policyForm.rate_unit)"
                    :placeholder="ratePlaceholder(policyForm.rate_unit)"
                    :data-testid="`rebate-policy-form-tier-rate-${index}`"
                  />
                  <t-input
                    v-model="tier.remark"
                    placeholder="选填"
                    :data-testid="`rebate-policy-form-tier-remark-${index}`"
                  />
                  <t-link
                    v-if="index === policyForm.rules.length - 1 && policyForm.rules.length > MIN_TIER_TIERS"
                    theme="danger"
                    data-testid="btn-rebate-policy-tier-remove"
                    @click="handleRemoveTier(index)"
                  >
                    删除
                  </t-link>
                  <span v-else />
                </div>
                <t-button
                  variant="text"
                  theme="primary"
                  data-testid="btn-rebate-policy-tier-add"
                  @click="handleAddTier"
                >
                  <template #icon><t-icon name="add" /></template>
                  添加一档
                </t-button>
              </div>

              <p class="form-hint">
                <template v-if="isFlatRule">
                  按量填「每件返多少元」，按额填「销售额百分比」（填 2 表示 2%，不是 0.02）。
                </template>
                <template v-else>
                  区间左闭右开：正好等于上限的基数落<b>下一档</b>。首档必须从 0 起、逐档首尾相接、末档必须留空上限。
                </template>
              </p>
              <ul v-if="tierPreview.length" class="tier-preview" data-testid="rebate-policy-form-preview">
                <li v-for="(line, index) in tierPreview" :key="index" class="mono">{{ line }}</li>
              </ul>
            </div>
          </t-form-item>

          <t-form-item label="备注">
            <t-textarea
              v-model="policyForm.remark"
              :maxlength="POLICY_REMARK_MAX"
              placeholder="选填，例如合同号 / 审批单号"
              data-testid="rebate-policy-form-remark"
            />
          </t-form-item>
        </t-form>

        <t-alert
          v-if="customerOptionsHint"
          theme="warning"
          :message="customerOptionsHint"
          class="form-alert"
          data-testid="rebate-policy-form-customer-hint"
        />
        <t-alert
          v-if="policyFormError"
          theme="error"
          :message="policyFormError"
          class="form-alert"
          data-testid="rebate-policy-form-error"
        />
      </div>
    </t-dialog>

    <!-- ==================== ④ 试算 / 计算弹窗 ==================== -->
    <t-dialog
      v-model:visible="calcVisible"
      header="返利试算 / 计算"
      :width="720"
      :footer="false"
      :close-on-overlay-click="false"
      data-testid="rebate-calc-dialog"
    >
      <div class="form-body">
        <t-form label-width="110px" class="form">
          <t-form-item label="客户" required-mark>
            <t-select
              v-if="!customerOptionsFailed"
              v-model="calcForm.customer_id"
              :options="customerOptions"
              filterable
              clearable
              placeholder="选择客户（只列启用客户）"
              data-testid="rebate-calc-customer"
            />
            <t-input
              v-else
              v-model="calcForm.customer_id"
              placeholder="候选列表无权限读取：直接粘贴客户 UUID"
              data-testid="rebate-calc-customer-id"
            />
          </t-form-item>

          <t-form-item label="结算粒度" required-mark>
            <t-radio-group
              v-model="calcForm.settle_period"
              data-testid="rebate-calc-period-type"
              @change="handleCalcPeriodTypeChange"
            >
              <t-radio value="MONTH">月度</t-radio>
              <t-radio value="QUARTER">季度</t-radio>
            </t-radio-group>
          </t-form-item>

          <t-form-item label="结算期间" required-mark>
            <t-select
              v-model="calcForm.period_code"
              :options="calcPeriodOptions"
              filterable
              creatable
              placeholder="选最近期间，或直接输入 yyyy-MM / yyyy-QN"
              data-testid="rebate-calc-period-code"
            />
          </t-form-item>

          <t-form-item label="备注">
            <t-input
              v-model="calcForm.remark"
              :maxlength="SETTLE_REMARK_MAX"
              placeholder="选填，写进结算单（重算原因等）"
              data-testid="rebate-calc-remark"
            />
          </t-form-item>
        </t-form>

        <t-alert
          v-if="calcError"
          theme="error"
          :message="calcError"
          class="form-alert"
          data-testid="rebate-calc-error"
        />

        <!-- 试算结果：先报钱，再报前提；已有结算单要说清 -->
        <div v-if="calcPreview" class="preview-panel" data-testid="rebate-calc-preview">
          <div class="preview-panel__head">
            <span class="preview-panel__amount mono">{{ moneyText(calcPreview.rebate_amount) }}</span>
            <span class="preview-panel__label">应返金额</span>
          </div>
          <p class="preview-panel__line">{{ previewHeadline(calcPreview) }}</p>
          <p class="preview-panel__line">
            <b>基数</b>：{{ baseDetailText(calcPreview.base) }}
          </p>
          <p class="preview-panel__line">
            <b>落档</b>：
            {{
              calcPreview.tier_seq === null
                ? '未命中档位'
                : `第 ${calcPreview.tier_seq} 档 ${qtyText(calcPreview.tier_from)} ~ ${calcPreview.tier_to === null ? '+∞' : qtyText(calcPreview.tier_to)} ⇒ ${rateText(calcPreview.rate, calcPreview.policy?.rate_unit ?? '')}`
            }}
          </p>
          <p v-if="isNegativeBase(calcPreview.base)" class="preview-panel__line preview-panel__line--warn">
            净基数为负（退货多于销售）：按 0 落档，本期不产生返利。
          </p>
          <ul v-if="previewWarnings(calcPreview).length" class="preview-panel__warn">
            <li v-for="(line, index) in previewWarnings(calcPreview)" :key="index">{{ line }}</li>
          </ul>
          <div v-if="calcPreview.policy" class="preview-panel__policy">
            <p class="preview-panel__line">
              <b>适用政策</b>：{{ calcPreview.policy.policy_no }} {{ calcPreview.policy.policy_name }}
              （{{ ruleTypeLabel(calcPreview.policy.rule_type) }} · {{ rateUnitLabel(calcPreview.policy.rate_unit) }}）
            </p>
            <ul class="tier-preview">
              <li v-for="(line, index) in previewTierLines(calcPreview)" :key="index" class="mono">{{ line }}</li>
            </ul>
          </div>
          <t-alert
            v-if="calcPreview.existing"
            theme="warning"
            :message="`该客户该期间已有结算单 ${calcPreview.existing.settlement_no}（${calcPreview.existing.status_label}，应返 ${moneyText(calcPreview.existing.rebate_amount)}）：落库会被拒（同期间唯一），请先作废或改用重算。`"
            class="form-alert"
            data-testid="rebate-calc-existing"
          />
        </div>

        <div class="dialog-actions">
          <t-button
            variant="outline"
            data-testid="btn-rebate-calc-preview"
            :loading="calcPreviewing"
            :disabled="calcPreviewing || calcSaving"
            @click="submitPreview"
          >
            试算（不落库）
          </t-button>
          <t-button
            theme="primary"
            data-testid="btn-rebate-calc-save"
            :loading="calcSaving"
            :disabled="calcPreviewing || calcSaving"
            @click="submitCalcSave"
          >
            计算并落库（草稿）
          </t-button>
          <t-button variant="text" :disabled="calcPreviewing || calcSaving" @click="calcVisible = false">
            关闭
          </t-button>
        </div>
      </div>
    </t-dialog>

    <!-- ==================== ⑤ 整期跑批弹窗 ==================== -->
    <t-dialog
      v-model:visible="runVisible"
      header="整期跑批"
      :width="720"
      :footer="false"
      :close-on-overlay-click="false"
      data-testid="rebate-run-dialog"
    >
      <div class="form-body">
        <t-form label-width="110px" class="form">
          <t-form-item label="结算粒度" required-mark>
            <t-radio-group
              v-model="runForm.settle_period"
              data-testid="rebate-run-period-type"
              @change="handleRunPeriodTypeChange"
            >
              <t-radio value="MONTH">月度</t-radio>
              <t-radio value="QUARTER">季度</t-radio>
            </t-radio-group>
          </t-form-item>
          <t-form-item label="结算期间" required-mark>
            <t-select
              v-model="runForm.period_code"
              :options="runPeriodOptions"
              filterable
              creatable
              placeholder="选最近期间，或直接输入 yyyy-MM / yyyy-QN"
              data-testid="rebate-run-period-code"
            />
          </t-form-item>
          <t-form-item label="备注">
            <t-input
              v-model="runForm.remark"
              :maxlength="SETTLE_REMARK_MAX"
              placeholder="选填，写进本次新建的结算单"
              data-testid="rebate-run-remark"
            />
          </t-form-item>
        </t-form>
        <p class="form-hint">
          跑批范围 = 该期间<b>有覆盖整期启用政策</b>的全部客户；已经算过的客户返回「已有跳过」，不会重复算。
          每个客户一个独立事务：某家挂了不影响其它家，修好再跑一遍即可（幂等）。
        </p>

        <t-alert
          v-if="runError"
          theme="error"
          :message="runError"
          class="form-alert"
          data-testid="rebate-run-error"
        />

        <!-- 跑批结果：失败排最前 -->
        <div v-if="runResult" class="run-result" data-testid="rebate-run-result">
          <p class="preview-panel__line" data-testid="rebate-run-summary">{{ runResultText(runResult) }}</p>
          <template v-for="group in runGroups" :key="group.key">
            <p v-if="group.items.length" class="run-result__title">
              {{ group.label }}（{{ group.items.length }}）
            </p>
            <ul v-if="group.items.length" class="run-result__list">
              <li
                v-for="item in group.items"
                :key="item.customer_id"
                :class="{ 'text-danger': group.key === 'failed' }"
              >
                {{ runItemText(item) }}
              </li>
            </ul>
          </template>
        </div>

        <div class="dialog-actions">
          <t-button
            theme="primary"
            data-testid="btn-rebate-run-submit"
            :loading="runSubmitting"
            :disabled="runSubmitting"
            @click="submitRun"
          >
            开始跑批
          </t-button>
          <t-button variant="text" :disabled="runSubmitting" @click="runVisible = false">关闭</t-button>
        </div>
      </div>
    </t-dialog>

    <!-- ==================== ⑥ 结算单详情抽屉 ==================== -->
    <t-drawer
      v-model:visible="detailVisible"
      :header="detailTitle"
      :size="'760px'"
      :footer="false"
      data-testid="rebate-settle-detail-drawer"
    >
      <div v-if="detailLoading" class="detail-loading">
        <t-loading size="small" text="加载中…" />
      </div>
      <t-alert v-else-if="detailError" theme="error" :message="detailError" />
      <div v-else-if="detailRow" class="detail-body">
        <t-descriptions :column="2" size="small" bordered>
          <t-descriptions-item label="结算单号">
            <span class="mono">{{ detailRow.settlement_no }}</span>
          </t-descriptions-item>
          <t-descriptions-item label="状态">
            <t-tag size="small" variant="light" :theme="settleStatusTheme(detailRow.status)">
              {{ settleStatusLabel(detailRow.status) }}
            </t-tag>
          </t-descriptions-item>
          <t-descriptions-item label="客户">{{ customerText(detailRow) }}</t-descriptions-item>
          <t-descriptions-item label="期间">
            {{ periodCodeLabel(detailRow.period_code) }}（{{ detailRow.settle_period_label }}）
          </t-descriptions-item>
          <t-descriptions-item label="期间区间">
            <span class="mono">{{ periodRangeText(detailRow.period_from, detailRow.period_last_day) }}</span>
          </t-descriptions-item>
          <t-descriptions-item label="适用政策">
            <span class="mono">{{ detailRow.policy_no }}</span> {{ detailRow.policy_name }}
          </t-descriptions-item>
          <t-descriptions-item label="规则口径">
            {{ detailRow.rule_type_label }} · {{ detailRow.rate_unit_label }}
          </t-descriptions-item>
          <t-descriptions-item label="政策有效期">
            <span class="mono">{{ validityText(detailRow.policy_effective_from, detailRow.policy_effective_to) }}</span>
          </t-descriptions-item>
          <t-descriptions-item label="命中档">
            <span class="mono">{{ tierHitText(detailRow) }}</span>
          </t-descriptions-item>
          <t-descriptions-item label="应返金额">
            <span class="mono amount-cell">{{ moneyText(detailRow.rebate_amount) }}</span>
          </t-descriptions-item>
          <t-descriptions-item label="红字发票">
            <span class="mono">{{ invoiceText(detailRow) }}</span>
          </t-descriptions-item>
          <t-descriptions-item label="转结时间">
            <span class="mono">{{ settledAtText(detailRow.settled_at) }}</span>
          </t-descriptions-item>
          <t-descriptions-item label="基数（订单 − 退货 = 净额）" :span="2">
            <span class="mono">{{ baseDetailText(detailRow.base) }}</span>
          </t-descriptions-item>
          <t-descriptions-item label="备注" :span="2">{{ detailRow.remark || '—' }}</t-descriptions-item>
        </t-descriptions>

        <h4 class="detail-section">政策档位快照（本单按这份条款算出来的）</h4>
        <t-table
          row-key="seq"
          size="small"
          :data="detailRow.policy_tiers"
          :columns="detailTierColumns"
          data-testid="rebate-settle-detail-tiers"
        >
          <template #tier_to="{ row }">
            <span class="mono">{{ row.tier_to === null ? '+∞' : qtyText(row.tier_to) }}</span>
          </template>
          <template #tier_from="{ row }">
            <span class="mono">{{ qtyText(row.tier_from) }}</span>
          </template>
          <template #rate="{ row }">
            <span class="mono">{{ rateText(row.rate, detailRow.rate_unit) }}</span>
          </template>
        </t-table>
      </div>
    </t-drawer>

    <!-- ==================== ⑦ 重算 / 作废（同一个轻量对话框） ==================== -->
    <t-dialog
      v-model:visible="actionVisible"
      :header="actionKind === 'cancel' ? '作废返利结算单' : '重算返利结算单'"
      :width="560"
      :confirm-btn="{ content: actionKind === 'cancel' ? '确认作废' : '重算', loading: actionSubmitting, disabled: actionSubmitting }"
      :cancel-btn="{ disabled: actionSubmitting }"
      :close-on-overlay-click="false"
      :theme="actionKind === 'cancel' ? 'warning' : 'info'"
      data-testid="rebate-settle-action-dialog"
      @confirm="submitAction"
    >
      <div class="form-body">
        <p class="form-hint">
          <template v-if="actionKind === 'cancel'">
            作废后这张单不再占用「同客户同粒度同期间唯一」的额度，可以重新算一张；行本身保留供审计（不物理删）。
          </template>
          <template v-else>
            重算会用<b>当前最新</b>的订单 / 退货与政策覆盖情况重新算一遍，覆盖本单的基数、命中档与金额（仅草稿可重算）。
          </template>
        </p>
        <t-textarea
          v-model="actionRemark"
          :maxlength="SETTLE_REMARK_MAX"
          :placeholder="actionKind === 'cancel' ? '作废原因（建议填写，便于事后追溯）' : '重算原因（选填）'"
          data-testid="rebate-settle-action-remark"
        />
        <t-alert v-if="actionError" theme="error" :message="actionError" />
      </div>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 销售返利页（M3-19 · 政策配置 + 结算单）
 *
 * 纯逻辑全在 `policy-logic.ts` / `settle-logic.ts`（被 `tests/unit/erp-rebate.test.ts` 钉住），
 * 本文件只做「数据从哪来 + 点下去做什么」。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin } from 'tdesign-vue-next';
import { useUserStore } from '@/stores/user';
import { ApiError } from '@/api/request';
import { fetchCustomerPage, type Customer } from '@/api/business/erp';
import {
  REBATE_PERMISSIONS,
  cancelRebateSettlement,
  createRebatePolicy,
  createRebateSettlement,
  deleteRebatePolicy,
  fetchRebatePolicyPage,
  fetchRebateSettlement,
  fetchRebateSettlementPage,
  previewRebateSettlement,
  recalcRebateSettlement,
  runRebateSettlement,
  updateRebatePolicy,
  type RebatePolicyFilter,
  type RebatePolicyRow,
  type RebatePreview,
  type RebateRunRes,
  type RebateRuleType,
  type RebateSettlementFilter,
  type RebateSettlementRow,
  type RebateSettlePeriod,
} from '@/api/business/erp/rebate';
import ListPage from '@/components/ListPage/index.vue';
import type { ColumnDef, FilterDef, ListFetcher, ListQueryBuilder } from '@/components/ListPage/types';
import { todayString } from '@/views/business/erp/shared/money';
import {
  MIN_TIER_TIERS,
  POLICY_COLUMNS,
  POLICY_NAME_MAX,
  POLICY_PAGE_SIZE,
  POLICY_REMARK_MAX,
  RATE_UNIT_OPTIONS,
  RULE_TYPE_FILTER_OPTIONS,
  RULE_TYPE_OPTIONS,
  SETTLE_PERIOD_FILTER_OPTIONS,
  POLICY_STATUS_FILTER_OPTIONS,
  addTier,
  buildPolicyPayload,
  emptyPolicyFilter,
  emptyPolicyForm,
  isPolicyFiltered,
  onRuleTypeChange,
  policyEmptyText,
  policyFormFrom,
  policyRuleSummary,
  policyStatusLabel,
  policyStatusTheme,
  ratePlaceholder,
  rateSuffix,
  rateUnitLabel,
  removeTier,
  ruleTypeLabel,
  settlePeriodLabel,
  syncTierBound,
  tierPreviewText,
  validityText,
  validatePolicyForm,
  type PolicyFormDraft,
} from './policy-logic';
import {
  SETTLEMENT_COLUMNS,
  SETTLE_PAGE_SIZE,
  SETTLE_PERIOD_FILTER_OPTIONS as SETTLE_GRANULARITY_FILTERS,
  SETTLE_REMARK_MAX,
  SETTLE_STATUS_OPTIONS,
  baseCellText,
  baseDetailText,
  buildRunReq,
  buildSettleReq,
  customerText as settleCustomerText,
  emptySettlementFilter,
  invoiceText,
  isEditable,
  isNegativeBase,
  moneyText,
  normalizePeriodCode,
  pageTotals,
  periodCodeLabel,
  periodOptionsOf,
  periodRangeText,
  previewHeadline,
  previewTierLines,
  previewWarnings,
  qtyText,
  rateText,
  runItemText,
  runResultGroups,
  runResultText,
  settleStatusLabel,
  settleStatusTheme,
  settlementEmptyText,
  settledAtText,
  tierHitText,
  validatePeriodCode,
} from './settle-logic';

/** 本页权限码（与 V0438 / V0439 / V0440 的 auth_code 逐字一致） */
const PERM = REBATE_PERMISSIONS;

const userStore = useUserStore();
const canPolicyCreate = computed(() => userStore.hasPermission(PERM.policyCreate));
const canSettleCalc = computed(() => userStore.hasPermission(PERM.settleCalc));

/** 错误 → 人话（`ApiError` 带后端 msg，别吃掉） */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

const activeTab = ref<'policy' | 'settlement'>('policy');

/** tab 切换：切过去时让对应的 ListPage 自己首屏加载（v-if 重建，无需手动拉） */
function handleTabChange(): void {
  void ensureCustomerOptions();
}

// ==================== 客户候选（两个 tab 共用） ====================

const customers = ref<Customer[]>([]);
const customerOptionsFailed = ref(false);
const customerLoaded = ref(false);

const customerOptions = computed(() =>
  customers.value.map((item) => ({
    label: `${item.code} ${item.name}`,
    value: item.id,
  })),
);

const customerOptionsHint = computed(() =>
  customerOptionsFailed.value
    ? '当前账号没有 business:erp:customer:list 权限，客户候选拉不到：请粘贴客户 UUID（可让管理员在「客户档案」里查）。'
    : '',
);

/** 拉客户候选：失败只置降级标记（手填 UUID），不影响列表 */
async function ensureCustomerOptions(): Promise<void> {
  if (customerLoaded.value) {
    return;
  }
  customerLoaded.value = true;
  try {
    const data = await fetchCustomerPage({ status: 'ACTIVE', page: 1, page_size: 200 });
    customers.value = data.list ?? [];
    customerOptionsFailed.value = customers.value.length === 0;
  } catch {
    customers.value = [];
    customerOptionsFailed.value = true;
  }
}

onMounted(() => {
  void ensureCustomerOptions();
});

// ==================== 政策列表 ====================

/** 政策列表筛选 */
const policyFilter = reactive<RebatePolicyFilter>(emptyPolicyFilter());
const policyPage = ref(1);
const policyListRef = ref<InstanceType<typeof ListPage> | null>(null);
const policyEmpty = computed(() => {
  void policyFilter.keyword;
  void policyFilter.rule_type;
  void policyFilter.settle_period;
  void policyFilter.status;
  void policyFilter.active_on;
  return isPolicyFiltered(policyFilter)
    ? '没有匹配的返利政策：换个客户 / 类型 / 粒度再查'
    : policyEmptyText(policyFilter);
});

const policyFilters: FilterDef[] = [
  {
    key: 'keyword',
    type: 'keyword',
    placeholder: '政策编号 / 名称 / 客户',
    width: 240,
    testid: 'rebate-policy-filter-keyword',
  },
  {
    key: 'rule_type',
    type: 'select',
    placeholder: '全部类型',
    width: 140,
    options: RULE_TYPE_FILTER_OPTIONS.filter((item) => item.value !== '').map((item) => ({ ...item })),
    testid: 'rebate-policy-filter-type',
  },
  {
    key: 'settle_period',
    type: 'select',
    placeholder: '全部粒度',
    width: 140,
    options: SETTLE_PERIOD_FILTER_OPTIONS.filter((item) => item.value !== '').map((item) => ({ ...item })),
    testid: 'rebate-policy-filter-period',
  },
  {
    key: 'status',
    type: 'select',
    placeholder: '全部状态',
    width: 130,
    options: POLICY_STATUS_FILTER_OPTIONS.filter((item) => item.value !== '').map((item) => ({
      label: item.label,
      value: String(item.value),
    })),
    testid: 'rebate-policy-filter-status',
  },
];

const policyFetcher: ListFetcher<RebatePolicyRow> = (query) =>
  fetchRebatePolicyPage(query as RebatePolicyFilter);

const policyQueryBuilder: ListQueryBuilder = (state, page, size) => {
  const filter = state as RebatePolicyFilter;
  // 状态下拉的值是字符串（TDesign 把数字 0 当空），发出去前转回数字；空串不发
  const status =
    filter.status === '' || filter.status === undefined || filter.status === null
      ? undefined
      : Number(filter.status);
  return {
    ...filter,
    status,
    page,
    page_size: size,
  };
};

function onPolicyLoaded(payload: { page: number }): void {
  policyPage.value = payload.page;
}

async function reloadPolicyList(page: number = policyPage.value): Promise<void> {
  await policyListRef.value?.load(page);
}

// ==================== 政策表单 ====================

const policyFormVisible = ref(false);
const policySubmitting = ref(false);
const policyEditingId = ref('');
const policyFormError = ref('');
const policyForm = reactive<PolicyFormDraft>(emptyPolicyForm(todayString()));

const isFlatRule = computed(() => policyForm.rule_type !== 'TIERED');
const currentRuleTypeHint = computed(
  () => RULE_TYPE_OPTIONS.find((item) => item.value === policyForm.rule_type)?.hint ?? '',
);
const tierPreview = computed(() => tierPreviewText(policyForm));

function resetPolicyForm(): void {
  Object.assign(policyForm, emptyPolicyForm(todayString()));
  policyFormError.value = '';
  policyEditingId.value = '';
}

function openPolicyCreate(): void {
  resetPolicyForm();
  policyFormVisible.value = true;
}

function openPolicyEdit(row: RebatePolicyRow): void {
  resetPolicyForm();
  Object.assign(policyForm, policyFormFrom(row));
  policyEditingId.value = row.id;
  policyFormVisible.value = true;
}

/** 类型变化：表单整形（按量/按额压成单档、阶梯补模板） */
function handleRuleTypeChange(value: unknown): void {
  onRuleTypeChange(policyForm, value as RebateRuleType);
}

function handleAddTier(): void {
  const result = addTier(policyForm);
  if (!result.ok) {
    MessagePlugin.warning(result.reason ?? '不能加档');
  }
}

function handleRemoveTier(index: number): void {
  const result = removeTier(policyForm, index);
  if (!result.ok) {
    MessagePlugin.warning(result.reason ?? '不能删档');
  }
}

async function submitPolicyForm(): Promise<void> {
  if (policySubmitting.value) {
    return;
  }
  const error = validatePolicyForm(policyForm);
  if (error) {
    policyFormError.value = error;
    return;
  }
  policyFormError.value = '';
  policySubmitting.value = true;
  try {
    const payload = buildPolicyPayload(policyForm);
    if (policyEditingId.value) {
      await updateRebatePolicy(policyEditingId.value, payload);
      MessagePlugin.success('返利政策已保存');
    } else {
      await createRebatePolicy(payload);
      MessagePlugin.success('返利政策已创建');
    }
    policyFormVisible.value = false;
    await reloadPolicyList(policyEditingId.value ? policyPage.value : 1);
  } catch (err) {
    // 后端错误原样展示（66089 档位 / 66090 有效期重叠 / 409 客户冻结）
    policyFormError.value = messageOf(err);
  } finally {
    policySubmitting.value = false;
  }
}

/** 停用 / 启用：走整单覆盖的 PUT，只改 status（其余字段原样回写） */
function confirmPolicyToggle(row: RebatePolicyRow): void {
  const nextStatus = row.status === 1 ? 0 : 1;
  const verb = nextStatus === 1 ? '启用' : '停用';
  const dialog = DialogPlugin.confirm({
    header: `${verb}返利政策`,
    body: `确认${verb}「${row.policy_name}」？${
      nextStatus === 0
        ? '停用后不再参与新结算（历史结算单里有政策快照，不受影响）。'
        : '启用后立即参与结算匹配；同客户同粒度的启用政策有效期不能重叠（重叠会被后端拒）。'
    }`,
    confirmBtn: verb,
    theme: nextStatus === 0 ? 'warning' : 'info',
    onConfirm: async () => {
      try {
        const draft = policyFormFrom(row);
        draft.status = nextStatus as 0 | 1;
        await updateRebatePolicy(row.id, buildPolicyPayload(draft));
        MessagePlugin.success(`已${verb}`);
        dialog.destroy();
        await reloadPolicyList();
      } catch (error) {
        MessagePlugin.error(`${verb}失败：${messageOf(error)}`);
      }
    },
  });
}

/** 删除（软删）：已产生结算单的政策应停用而不是删除 */
function confirmPolicyDelete(row: RebatePolicyRow): void {
  const dialog = DialogPlugin.confirm({
    header: '删除返利政策',
    body: `确认删除「${row.policy_name}」？软删后历史结算单仍能读到（政策快照在单上），但这条政策不再出现在列表里，也不能再参与结算。若只是不想再用，请点「停用」。`,
    confirmBtn: '确认删除',
    theme: 'danger',
    onConfirm: async () => {
      try {
        await deleteRebatePolicy(row.id);
        MessagePlugin.success('已删除');
        dialog.destroy();
        await reloadPolicyList();
      } catch (error) {
        MessagePlugin.error(`删除失败：${messageOf(error)}`);
      }
    },
  });
}

// ==================== 结算列表 ====================

const settleFilter = reactive<RebateSettlementFilter>(emptySettlementFilter());
const settlePage = ref(1);
const settleListRef = ref<InstanceType<typeof ListPage> | null>(null);
const settlePageRows = ref<RebateSettlementRow[]>([]);
const settleTotals = computed(() => pageTotals(settlePageRows.value));
const settleEmpty = computed(() => {
  void settleFilter.keyword;
  void settleFilter.settle_period;
  void settleFilter.period_code;
  void settleFilter.status;
  return settlementEmptyText(settleFilter);
});

/** 期间筛选项：最近 13 个月 + 最近 8 个季度（合并成一份下拉） */
const settlePeriodOptions = computed(() => [
  ...periodOptionsOf('MONTH'),
  ...periodOptionsOf('QUARTER'),
]);

const settleFilters: FilterDef[] = [
  {
    key: 'keyword',
    type: 'keyword',
    placeholder: '结算单号 / 客户 / 政策号',
    width: 230,
    testid: 'rebate-settle-filter-keyword',
  },
  {
    key: 'settle_period',
    type: 'select',
    placeholder: '全部粒度',
    width: 130,
    options: SETTLE_GRANULARITY_FILTERS.filter((item) => item.value !== '').map((item) => ({ ...item })),
    testid: 'rebate-settle-filter-granularity',
  },
  {
    key: 'period_code',
    type: 'select',
    placeholder: '全部期间',
    width: 165,
    options: settlePeriodOptions.value,
    filterable: true,
    testid: 'rebate-settle-filter-period',
  },
  {
    key: 'status',
    type: 'select',
    placeholder: '全部状态',
    width: 165,
    options: SETTLE_STATUS_OPTIONS.filter((item) => item.value !== '').map((item) => ({ ...item })),
    testid: 'rebate-settle-filter-status',
  },
];

const settleFetcher: ListFetcher<RebateSettlementRow> = (query) =>
  fetchRebateSettlementPage(query as RebateSettlementFilter);

const settleQueryBuilder: ListQueryBuilder = (state, page, size) => ({
  ...(state as RebateSettlementFilter),
  page,
  page_size: size,
});

function onSettleLoaded(payload: { rows: RebateSettlementRow[]; page: number }): void {
  settlePage.value = payload.page;
  settlePageRows.value = payload.rows ?? [];
}

async function reloadSettleList(page: number = settlePage.value): Promise<void> {
  await settleListRef.value?.load(page);
}

// ==================== 试算 / 计算 ====================

const calcVisible = ref(false);
const calcError = ref('');
const calcPreview = ref<RebatePreview | null>(null);
const calcPreviewing = ref(false);
const calcSaving = ref(false);
const calcForm = reactive({
  customer_id: '',
  settle_period: 'MONTH' as RebateSettlePeriod,
  period_code: '',
  remark: '',
});

const calcPeriodOptions = computed(() => periodOptionsOf(calcForm.settle_period));

function openCalc(): void {
  calcForm.customer_id = '';
  calcForm.settle_period = 'MONTH';
  calcForm.period_code = calcPeriodOptions.value[0]?.value ?? '';
  calcForm.remark = '';
  calcError.value = '';
  calcPreview.value = null;
  calcVisible.value = true;
}

/** 粒度变了：期间码跟着换成同粒度的最近期间（月度 ↔ 季度期间码互不通用） */
function handleCalcPeriodTypeChange(value: unknown): void {
  calcForm.settle_period = value as RebateSettlePeriod;
  calcForm.period_code = calcPeriodOptions.value[0]?.value ?? '';
  calcPreview.value = null;
  calcError.value = '';
}

/** 试算前置校验（客户必填 + 期间形状与粒度互配） */
function validateCalcForm(): string | null {
  if (!calcForm.customer_id.trim()) {
    return '请选择客户';
  }
  return validatePeriodCode(calcForm.settle_period, calcForm.period_code);
}

async function submitPreview(): Promise<void> {
  if (calcPreviewing.value) {
    return;
  }
  const error = validateCalcForm();
  if (error) {
    calcError.value = error;
    return;
  }
  calcError.value = '';
  calcPreviewing.value = true;
  try {
    const payload = buildSettleReq(
      calcForm.customer_id,
      calcForm.settle_period,
      normalizePeriodCode(calcForm.period_code),
      calcForm.remark,
    );
    calcPreview.value = await previewRebateSettlement(payload);
    MessagePlugin.success('试算完成（未落库）');
  } catch (err) {
    calcPreview.value = null;
    calcError.value = messageOf(err);
  } finally {
    calcPreviewing.value = false;
  }
}

async function submitCalcSave(): Promise<void> {
  if (calcSaving.value) {
    return;
  }
  const error = validateCalcForm();
  if (error) {
    calcError.value = error;
    return;
  }
  calcError.value = '';
  calcSaving.value = true;
  try {
    const payload = buildSettleReq(
      calcForm.customer_id,
      calcForm.settle_period,
      normalizePeriodCode(calcForm.period_code),
      calcForm.remark,
    );
    const created = await createRebateSettlement(payload);
    MessagePlugin.success(`已生成结算单 ${created.settlement_no}（草稿，应返 ${moneyText(created.rebate_amount)}）`);
    calcVisible.value = false;
    activeTab.value = 'settlement';
    await reloadSettleList(1);
  } catch (err) {
    calcError.value = messageOf(err);
  } finally {
    calcSaving.value = false;
  }
}

// ==================== 整期跑批 ====================

const runVisible = ref(false);
const runError = ref('');
const runSubmitting = ref(false);
const runResult = ref<RebateRunRes | null>(null);
const runForm = reactive({
  settle_period: 'MONTH' as RebateSettlePeriod,
  period_code: '',
  remark: '',
});

const runPeriodOptions = computed(() => periodOptionsOf(runForm.settle_period));

const runGroups = computed(() => {
  if (!runResult.value) {
    return [];
  }
  const groups = runResultGroups(runResult.value);
  return [
    { key: 'failed', label: '失败', items: groups.failed },
    { key: 'created', label: '新建', items: groups.created },
    { key: 'skipped', label: '已有跳过', items: groups.skipped },
  ];
});

function openRun(): void {
  runForm.settle_period = 'MONTH';
  runForm.period_code = runPeriodOptions.value[0]?.value ?? '';
  runForm.remark = '';
  runError.value = '';
  runResult.value = null;
  runVisible.value = true;
}

function handleRunPeriodTypeChange(value: unknown): void {
  runForm.settle_period = value as RebateSettlePeriod;
  runForm.period_code = runPeriodOptions.value[0]?.value ?? '';
  runResult.value = null;
  runError.value = '';
}

async function submitRun(): Promise<void> {
  if (runSubmitting.value) {
    return;
  }
  const error = validatePeriodCode(runForm.settle_period, runForm.period_code);
  if (error) {
    runError.value = error;
    return;
  }
  runError.value = '';
  runSubmitting.value = true;
  try {
    const res = await runRebateSettlement(
      buildRunReq(runForm.settle_period, normalizePeriodCode(runForm.period_code), [], runForm.remark),
    );
    runResult.value = res;
    // 弹窗不自动关：用户要看逐客户结果（失败原因在列表里）
    if (res.failed > 0) {
      MessagePlugin.warning(runResultText(res));
    } else {
      MessagePlugin.success(runResultText(res));
    }
    await reloadSettleList(1);
  } catch (err) {
    runError.value = messageOf(err);
  } finally {
    runSubmitting.value = false;
  }
}

// ==================== 详情抽屉 ====================

const detailVisible = ref(false);
const detailLoading = ref(false);
const detailError = ref('');
const detailRow = ref<RebateSettlementRow | null>(null);
const detailTitle = computed(() =>
  detailRow.value ? `返利结算 · ${detailRow.value.settlement_no}` : '返利结算详情',
);

/** 详情里的档位表列（`seq` 是行主键） */
const detailTierColumns: ColumnDef[] = [
  { colKey: 'seq', title: '档号', width: 70 },
  { colKey: 'tier_from', title: '下限（含）', width: 120, align: 'right' },
  { colKey: 'tier_to', title: '上限（不含）', width: 120, align: 'right' },
  { colKey: 'rate', title: '费率', width: 130, align: 'right' },
  { colKey: 'remark', title: '备注', minWidth: 140, ellipsis: true },
];

async function openDetail(row: RebateSettlementRow): Promise<void> {
  detailRow.value = row;
  detailVisible.value = true;
  detailLoading.value = true;
  detailError.value = '';
  try {
    detailRow.value = await fetchRebateSettlement(row.id);
  } catch (error) {
    detailError.value = `详情加载失败：${messageOf(error)}`;
  } finally {
    detailLoading.value = false;
  }
}

// ==================== 重算 / 作废 ====================

const actionVisible = ref(false);
const actionKind = ref<'recalc' | 'cancel'>('recalc');
const actionSubmitting = ref(false);
const actionError = ref('');
const actionRemark = ref('');
const actionRow = ref<RebateSettlementRow | null>(null);

function openRecalc(row: RebateSettlementRow): void {
  actionRow.value = row;
  actionKind.value = 'recalc';
  actionRemark.value = '';
  actionError.value = '';
  actionVisible.value = true;
}

function openCancel(row: RebateSettlementRow): void {
  actionRow.value = row;
  actionKind.value = 'cancel';
  actionRemark.value = '';
  actionError.value = '';
  actionVisible.value = true;
}

async function submitAction(): Promise<void> {
  const row = actionRow.value;
  if (!row || actionSubmitting.value) {
    return;
  }
  actionSubmitting.value = true;
  actionError.value = '';
  try {
    if (actionKind.value === 'cancel') {
      await cancelRebateSettlement(row.id, actionRemark.value);
      MessagePlugin.success(`已作废 ${row.settlement_no}`);
    } else {
      const updated = await recalcRebateSettlement(
        row.id,
        buildSettleReq(row.customer_id, row.settle_period, row.period_code, actionRemark.value),
      );
      MessagePlugin.success(`已重算：应返 ${moneyText(updated.rebate_amount)}`);
    }
    actionVisible.value = false;
    await reloadSettleList();
  } catch (err) {
    actionError.value = messageOf(err);
  } finally {
    actionSubmitting.value = false;
  }
}

// ==================== 模板里直接用的格式化（避免在 .vue 里写判断） ====================

/** 客户展示（政策行 / 结算行同口径：两者的客户字段同名同形） */
const customerText = (row: RebatePolicyRow | RebateSettlementRow): string =>
  settleCustomerText(row as RebateSettlementRow);

/** 规则类型配色（按量主色 / 按额成功色 / 阶梯警告色：阶梯最容易配错） */
function ruleTypeTheme(value: string | null | undefined): 'primary' | 'success' | 'warning' | 'default' {
  if (value === 'BY_QTY') {
    return 'primary';
  }
  if (value === 'BY_AMOUNT') {
    return 'success';
  }
  if (value === 'TIERED') {
    return 'warning';
  }
  return 'default';
}
</script>

<style scoped lang="less">
.rebate-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.page-tabs {
  margin-bottom: 4px;
}

/* 表头右侧：口径标签 + 说明 */
.head-tags {
  display: inline-flex;
  align-items: center;
  gap: 8px;
}

.page-hint {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.page-tip {
  margin: 0;
  font-size: 12px;
  line-height: 20px;
  color: var(--td-text-color-secondary);
}

.gate-wrap {
  display: inline-flex;
}

.toolbar-row {
  display: inline-flex;
  align-items: center;
  gap: 8px;
}

.mono {
  font-family: var(--td-font-family-mono, monospace);
}

.nowrap {
  white-space: nowrap;
}

.ellipsis-cell {
  display: inline-block;
  max-width: 100%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.period-range {
  display: block;
  font-size: 11px;
  color: var(--td-text-color-placeholder);
}

.amount-cell {
  font-weight: 600;
}

.text-danger {
  color: var(--td-error-color, #d54941);
}

.op-row {
  display: flex;
  align-items: center;
  gap: 12px;
  white-space: nowrap;
}

.totals-strip {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 13px;
  color: var(--td-text-color-secondary);
}

.totals-strip__sep {
  color: var(--td-text-color-placeholder);
}

.totals-strip__hint {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.form-body {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.form :deep(.t-form__item) {
  margin-bottom: 12px;
}

/* 日期 / 下拉铺满一行：TDesign 默认宽度按内容，860px 弹窗里日期会被截断 */
.form :deep(.t-form__controls-content .t-date-picker),
.form :deep(.t-form__controls-content .t-select) {
  width: 100%;
}

.form-hint {
  margin: 0;
  font-size: 12px;
  line-height: 20px;
  color: var(--td-text-color-secondary);
}

.form-static {
  font-size: 13px;
  color: var(--td-text-color-primary);
}

.form-alert {
  margin-top: 8px;
}

.rule-type-block {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.tier-block {
  display: flex;
  flex-direction: column;
  gap: 8px;
  width: 100%;
}

.flat-tier {
  display: flex;
  align-items: center;
  gap: 12px;
}

.flat-tier__range {
  font-size: 12px;
  color: var(--td-text-color-secondary);
  white-space: nowrap;
}

.tier-table {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.tier-row {
  display: grid;
  grid-template-columns: 70px 1fr 1fr 1.3fr 1.2fr 60px;
  align-items: center;
  gap: 8px;
}

.tier-row--head {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.tier-seq {
  font-size: 12px;
  color: var(--td-text-color-secondary);
  white-space: nowrap;
}

.tier-preview {
  margin: 0;
  padding-left: 18px;
  font-size: 12px;
  line-height: 20px;
  color: var(--td-text-color-secondary);
}

.preview-panel {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 12px;
  border-radius: 6px;
  background: var(--td-bg-color-container-hover, #f5f5f5);
}

.preview-panel__head {
  display: flex;
  align-items: baseline;
  gap: 8px;
}

.preview-panel__amount {
  font-size: 24px;
  font-weight: 600;
}

.preview-panel__label {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.preview-panel__line {
  margin: 0;
  font-size: 13px;
  line-height: 20px;
}

.preview-panel__line--warn {
  color: var(--td-warning-color, #e37318);
}

.preview-panel__warn {
  margin: 0;
  padding-left: 18px;
  font-size: 12px;
  color: var(--td-warning-color, #e37318);
}

.preview-panel__policy {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding-top: 6px;
  border-top: 1px dashed var(--td-component-stroke, #e7e7e7);
}

.run-result {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 12px;
  border-radius: 6px;
  background: var(--td-bg-color-container-hover, #f5f5f5);
  max-height: 320px;
  overflow: auto;
}

.run-result__title {
  margin: 6px 0 0;
  font-size: 13px;
  font-weight: 600;
}

.run-result__list {
  margin: 0;
  padding-left: 18px;
  font-size: 12px;
  line-height: 20px;
}

.dialog-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  justify-content: flex-end;
  margin-top: 8px;
}

.detail-body {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.detail-loading {
  padding: 24px 0;
  text-align: center;
}

.detail-section {
  margin: 8px 0 0;
  font-size: 14px;
  font-weight: 600;
}

.panel-empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  padding: 32px 0;
}

.panel-empty__icon {
  font-size: 32px;
  color: var(--td-text-color-placeholder);
}

.panel-empty__title {
  margin: 0;
  font-size: 13px;
  color: var(--td-text-color-secondary);
}
</style>
