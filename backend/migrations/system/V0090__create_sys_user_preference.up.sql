-- 用户偏好体系（通用 KV 主表 + 字典元数据表）
--
-- 设计目标（老大原话「把偏好表设计出来，用好」）：
--   **新增一个偏好 = 往 sys_preference_def 插一行**，不改表结构、不改后端代码。
--   值一律进 sys_user_preference 的 KV 行（jsonb，string/number/boolean/json 都能装）。
--
-- 两级结构的职责边界：
--   - sys_preference_def：**字典 / 元数据**（键名、中文名、类型、默认值、分组、校验规则）；
--     「这个键存不存在」「值该是什么类型」「合法取值有哪些」全部由它回答，
--     请求侧因此可以做到「字典里没有的键一律拒绝」。
--   - sys_user_preference：**用户值**（一个用户一个键一行），只存「用户显式设置过」的键；
--     没设置过的键在读接口里用字典的 default_value 合并出来，所以不需要预插默认行。
--
-- 主键选择：PK(user_id, pref_key) —— user_id 在最左，天然覆盖
--   「查某用户全部偏好」（WHERE user_id = ?）这条唯一的热路径，
--   不需要额外的 user_id 单列索引。

CREATE TABLE sys_preference_def (
    pref_key      VARCHAR(64)  PRIMARY KEY, -- 偏好键，形如 ui.palette / notify.email
    pref_name     VARCHAR(100) NOT NULL, -- 中文名（前端设置页直接拿来显示）
    value_type    VARCHAR(16)  NOT NULL
        CHECK (value_type IN ('string', 'number', 'boolean', 'json')), -- 值类型
    default_value JSONB        NOT NULL, -- 默认值（GET 在用户未设置时返回它）
    scope         VARCHAR(16)  NOT NULL DEFAULT 'ui', -- 分组：ui / table / notify（前端按此分区渲染）
    validator     VARCHAR(300), -- 值域校验规则：oneof:a,b,c / range:5-200；为空表示不校验
    sort_order    INT          NOT NULL DEFAULT 0, -- 同分组内的展示顺序
    status        SMALLINT     NOT NULL DEFAULT 1, -- 状态：1 启用 / 0 停用（停用=对读写接口不可见）
    remark        VARCHAR(255), -- 备注
    created_at    TIMESTAMPTZ  NOT NULL DEFAULT now(), -- 创建时间
    updated_at    TIMESTAMPTZ  NOT NULL DEFAULT now() -- 更新时间
);

COMMENT ON TABLE sys_preference_def IS '偏好字典表：定义有哪些偏好键、类型、默认值与校验规则（新增偏好不改表结构）';
COMMENT ON COLUMN sys_preference_def.pref_key IS '偏好键，格式 ^[a-z][a-z0-9_]*\.[a-z][a-z0-9_]*$（分组.键名）';
COMMENT ON COLUMN sys_preference_def.pref_name IS '偏好中文名，前端设置页直接显示';
COMMENT ON COLUMN sys_preference_def.value_type IS '值类型：string / number / boolean / json';
COMMENT ON COLUMN sys_preference_def.default_value IS '默认值（JSON）：用户未显式设置时由读接口合并返回';
COMMENT ON COLUMN sys_preference_def.scope IS '分组：ui / table / notify，前端据此分区渲染设置项';
COMMENT ON COLUMN sys_preference_def.validator IS '值域校验：oneof:a,b,c / range:min-max；为空表示只按 value_type 校验';
COMMENT ON COLUMN sys_preference_def.sort_order IS '同分组内的展示顺序（升序）';
COMMENT ON COLUMN sys_preference_def.status IS '状态：1 启用 / 0 停用（停用后读写接口都当作未知键拒绝）';
COMMENT ON COLUMN sys_preference_def.remark IS '备注';

CREATE TABLE sys_user_preference (
    user_id     UUID        NOT NULL, -- 用户 ID（sys_user.id）
    pref_key    VARCHAR(64) NOT NULL, -- 偏好键（与 sys_preference_def.pref_key 对应）
    pref_value  JSONB       NOT NULL, -- 偏好值（jsonb：string/number/boolean/对象数组都能装）
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(), -- 最近更新时间
    CONSTRAINT pk_sys_user_preference PRIMARY KEY (user_id, pref_key),
    -- 用户删除即连带清理其偏好；偏好是「用户附属数据」，没有独立生命周期
    CONSTRAINT fk_sys_user_preference_user FOREIGN KEY (user_id)
        REFERENCES sys_user (id) ON DELETE CASCADE,
    -- 键名格式与字典表同口径（拒绝 a..b / A.b / ui. 这类脏键）
    CONSTRAINT ck_sys_user_preference_key
        CHECK (pref_key ~ '^[a-z][a-z0-9_]*\.[a-z][a-z0-9_]*$')
);

COMMENT ON TABLE sys_user_preference IS '用户偏好值表（通用 KV）：一个用户一个键一行，只存用户显式设置过的键';
COMMENT ON COLUMN sys_user_preference.user_id IS '用户 ID（关联 sys_user.id，随用户删除级联清理）';
COMMENT ON COLUMN sys_user_preference.pref_key IS '偏好键，必须是 sys_preference_def 里启用的键';
COMMENT ON COLUMN sys_user_preference.pref_value IS '偏好值（jsonb，类型由字典的 value_type 约束）';
COMMENT ON COLUMN sys_user_preference.updated_at IS '最近更新时间';
