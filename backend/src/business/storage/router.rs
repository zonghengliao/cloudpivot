//! 存储路由：按配置持有多个后端实例，按「策略指定 → 默认后端」两条规则选路。
//!
//! # 为什么路由在业务侧
//!
//! 框架 `nik-fs` 的 `BackendRegistry` 以 `backend_type` 为键，同类型只能一个实例；
//! 本模块以 `config_code` 为键，支持两个 MinIO 桶、生产桶 + 备份桶并存。
//!
//! # 选路规则（与架构文档 §2.3 一致）
//!
//! ```text
//! 策略.storage_code 非空 ──▶ 该后端（不存在 → 409 STORAGE_BACKEND_UNAVAILABLE）
//! 策略.storage_code 为空 ──▶ 默认后端（没有默认 → 409 STORAGE_BACKEND_UNAVAILABLE）
//! ```
//!
//! # 热更新
//!
//! 配置写操作（新增 / 修改 / 删除 / 设默认 / 测试连接）成功后调用
//! [`crate::business::storage::reload_router`]，整表重建并**原子替换**内部 `HashMap`：
//! 读请求要么看到旧表、要么看到新表，不会读到「删了一半」的中间态。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use nik::prelude::*;

use crate::business::storage::cipher::{self, MasterKey};
use crate::business::storage::model::{SysStorageConfigModel, SysStoragePolicyModel};
use crate::business::storage::store::{LocalStore, ObjectStore, S3Store, S3StoreParams};
use crate::shared::error_code;

/// 存储路由器（进程级单例，见 [`crate::business::storage::global_router`]）。
#[derive(Default)]
pub struct StorageRouter {
    /// 后端编码 → 后端实例。
    stores: RwLock<HashMap<String, Arc<dyn ObjectStore>>>,
    /// 默认后端编码。
    default_code: RwLock<Option<String>>,
}

impl std::fmt::Debug for StorageRouter {
    /// 只打印编码列表与默认编码（`dyn ObjectStore` 没有 Debug，也不需要打印实例）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StorageRouter")
            .field("codes", &self.codes())
            .field("default_code", &self.default_code())
            .finish()
    }
}

impl StorageRouter {
    /// 构造空路由器（不含任何后端）。
    ///
    /// # 返回值
    ///
    /// 空实例；由 [`Self::replace`] 装载配置。
    pub fn new() -> Self {
        Self::default()
    }

    /// 用一组配置重建路由表（原子替换）。
    ///
    /// # 参数
    ///
    /// - `configs`：启用中的后端配置（[`super::model::load_enabled_configs`] 的结果）。
    /// - `key`：主密钥（解密 AK/SK 用）。
    ///
    /// # 返回值
    ///
    /// 成功装载的后端数量。单个后端构造失败**不阻断**其它后端：
    /// 记 `tracing::error!` 后跳过（该后端被引用时会 409，运维在配置页可见）。
    pub fn replace(&self, configs: &[SysStorageConfigModel], key: &MasterKey) -> usize {
        let mut built: HashMap<String, Arc<dyn ObjectStore>> = HashMap::new();
        let mut default_code = None;

        for config in configs.iter().filter(|config| config.is_usable()) {
            match build_store(config, key) {
                Ok(store) => {
                    if config.is_default && default_code.is_none() {
                        default_code = Some(config.config_code.clone());
                    }
                    built.insert(config.config_code.clone(), store);
                }
                Err(error) => {
                    tracing::error!(
                        config_code = %config.config_code,
                        backend_type = %config.backend_type,
                        code = error.code,
                        message = %error.msg,
                        "存储后端构造失败，已跳过（该后端被引用时将返回 STORAGE_BACKEND_UNAVAILABLE）"
                    );
                }
            }
        }

        let count = built.len();
        if let Ok(mut guard) = self.stores.write() {
            *guard = built;
        }
        if let Ok(mut guard) = self.default_code.write() {
            *guard = default_code;
        }
        count
    }

    /// 插入 / 覆盖单个后端。
    ///
    /// # 参数
    ///
    /// - `store`：后端实例。
    pub fn insert(&self, store: Arc<dyn ObjectStore>) {
        let code = store.config_code().to_string();
        if let Ok(mut guard) = self.stores.write() {
            guard.insert(code, store);
        }
    }

    /// 移除单个后端。
    ///
    /// # 参数
    ///
    /// - `code`：后端编码。
    pub fn remove(&self, code: &str) {
        if let Ok(mut guard) = self.stores.write() {
            guard.remove(code);
        }
    }

    /// 设置默认后端。
    ///
    /// # 参数
    ///
    /// - `code`：后端编码；必须已存在于路由表。
    ///
    /// # 返回值
    ///
    /// 设置成功返回 `true`；后端不存在返回 `false`（不改动现有默认）。
    pub fn set_default(&self, code: &str) -> bool {
        if self.by_code(code).is_none() {
            return false;
        }
        if let Ok(mut guard) = self.default_code.write() {
            *guard = Some(code.to_string());
        }
        true
    }

    /// 当前默认后端编码。
    ///
    /// # 返回值
    ///
    /// 没有默认后端时返回 `None`。
    pub fn default_code(&self) -> Option<String> {
        self.default_code
            .read()
            .ok()
            .and_then(|guard| guard.clone())
    }

    /// 按编码取后端。
    ///
    /// # 参数
    ///
    /// - `code`：后端编码。
    ///
    /// # 返回值
    ///
    /// 命中返回实例，未命中返回 `None`。
    pub fn by_code(&self, code: &str) -> Option<Arc<dyn ObjectStore>> {
        self.stores
            .read()
            .ok()
            .and_then(|guard| guard.get(code).cloned())
    }

    /// 已装载的后端编码（升序，便于日志与前端下拉）。
    ///
    /// # 返回值
    ///
    /// 编码列表。
    pub fn codes(&self) -> Vec<String> {
        let mut codes: Vec<String> = self
            .stores
            .read()
            .map(|guard| guard.keys().cloned().collect())
            .unwrap_or_default();
        codes.sort();
        codes
    }

    /// 已装载的后端数量。
    ///
    /// # 返回值
    ///
    /// 后端实例数。
    pub fn len(&self) -> usize {
        self.stores
            .read()
            .map(|guard| guard.len())
            .unwrap_or_default()
    }

    /// 路由表是否为空。
    ///
    /// # 返回值
    ///
    /// 没有任何后端时返回 `true`。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 按策略选后端。
    ///
    /// # 参数
    ///
    /// - `policy`：业务分类策略；`None` 表示无策略（按默认后端）。
    ///
    /// # 返回值
    ///
    /// 选中的后端实例。
    ///
    /// # 异常情况
    ///
    /// - 策略指定的后端不存在 / 已停用 → 409 [`error_code::STORAGE_BACKEND_UNAVAILABLE`]；
    /// - 没有默认后端 → 同样 409（提示运维在存储配置页设置默认后端）。
    pub fn pick(
        &self,
        policy: Option<&SysStoragePolicyModel>,
    ) -> std::result::Result<Arc<dyn ObjectStore>, ApiError> {
        let requested = policy
            .and_then(|policy| policy.storage_code.as_deref())
            .map(str::trim)
            .filter(|code| !code.is_empty());

        if let Some(code) = requested {
            return self.by_code(code).ok_or_else(|| {
                ApiError::new(
                    409,
                    error_code::STORAGE_BACKEND_UNAVAILABLE,
                    format!("文件所在存储后端 {code} 已停用或删除，请重新上传"),
                )
            });
        }

        let default_code = self.default_code().ok_or_else(|| {
            ApiError::new(
                409,
                error_code::STORAGE_BACKEND_UNAVAILABLE,
                "尚未配置默认存储后端，请先在「存储配置」页设置默认后端",
            )
        })?;
        self.by_code(&default_code).ok_or_else(|| {
            ApiError::new(
                409,
                error_code::STORAGE_BACKEND_UNAVAILABLE,
                format!("默认存储后端 {default_code} 不可用，请检查「存储配置」页"),
            )
        })
    }
}

/// 由一条配置构造后端实例（解密凭据只在此处发生）。
///
/// # 参数
///
/// - `config`：后端配置。
/// - `key`：主密钥。
///
/// # 返回值
///
/// 后端实例（`LOCAL` → [`LocalStore`]，`S3` → [`S3Store`]）。
///
/// # 异常情况
///
/// - 未知 `backend_type`：返回 500（数据库 CHECK 已限制为 `LOCAL`/`S3`，走到这里说明数据被绕过约束写入）；
/// - S3 凭据缺失或解密失败：返回 422 [`error_code::STORAGE_CREDENTIAL_INVALID`]
///   （**不区分**「缺失」与「解密失败」，避免把主密钥状态泄露给调用方）。
pub fn build_store(
    config: &SysStorageConfigModel,
    key: &MasterKey,
) -> std::result::Result<Arc<dyn ObjectStore>, ApiError> {
    use crate::business::storage::store::StoreError;

    match config.backend_type.as_str() {
        "LOCAL" => Ok(Arc::new(LocalStore::new(
            config.config_code.clone(),
            &config.base_path,
        ))),
        "S3" => {
            let (access_key, secret_key) = decrypt_credentials(config, key)?;
            let store = S3Store::new(S3StoreParams {
                code: &config.config_code,
                endpoint: config.endpoint.as_deref().unwrap_or_default(),
                bucket: config.bucket.as_deref().unwrap_or_default(),
                region: &config.region,
                path_style: config.path_style,
                base_path: &config.base_path,
                access_key: &access_key,
                secret_key: &secret_key,
                use_tls: config.use_tls,
            })
            .map_err(|error: StoreError| {
                tracing::error!(config_code = %config.config_code, error = %error, "S3 后端配置非法");
                ApiError::new(422, error_code::STORAGE_CREDENTIAL_INVALID, error.message())
            })?;
            Ok(Arc::new(store))
        }
        other => {
            tracing::error!(config_code = %config.config_code, backend_type = other, "未知存储后端类型");
            Err(ApiError::new(
                500,
                error_code::INTERNAL_ERROR,
                "存储后端类型非法（数据库 CHECK 约束应拦住该数据）",
            ))
        }
    }
}

/// 解密一条配置的 AK/SK。
///
/// # 参数
///
/// - `config`：后端配置（四列密文必须齐全）。
/// - `key`：主密钥。
///
/// # 返回值
///
/// `(AccessKey, SecretKey)` 明文（只在内存中使用）。
///
/// # 异常情况
///
/// 密文列缺失或解密失败返回 422 [`error_code::STORAGE_CREDENTIAL_INVALID`]。
pub fn decrypt_credentials(
    config: &SysStorageConfigModel,
    key: &MasterKey,
) -> std::result::Result<(String, String), ApiError> {
    let cipher_text = (
        config.ak_cipher.as_deref(),
        config.ak_nonce.as_deref(),
        config.sk_cipher.as_deref(),
        config.sk_nonce.as_deref(),
    );
    let (Some(ak_cipher), Some(ak_nonce), Some(sk_cipher), Some(sk_nonce)) = cipher_text else {
        return Err(ApiError::new(
            422,
            error_code::STORAGE_CREDENTIAL_INVALID,
            "存储凭据缺失：请重新填写 AccessKey / SecretKey",
        ));
    };

    let access_key = cipher::decrypt(key, ak_cipher, ak_nonce);
    let secret_key = cipher::decrypt(key, sk_cipher, sk_nonce);
    match (access_key, secret_key) {
        (Some(access_key), Some(secret_key)) => Ok((access_key, secret_key)),
        _ => Err(ApiError::new(
            422,
            error_code::STORAGE_CREDENTIAL_INVALID,
            "存储凭据解密失败：主密钥可能已更换，请重新填写 AccessKey / SecretKey",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::storage::store::MemoryStore;
    use crate::shared::constants::EnableStatus;

    /// 构造一条测试策略。
    fn policy_with(storage_code: Option<&str>) -> SysStoragePolicyModel {
        let now = chrono::Utc::now();
        SysStoragePolicyModel {
            id: uuid::Uuid::now_v7(),
            category: "attachment".to_string(),
            category_name: "附件".to_string(),
            max_size_mb: 50,
            allowed_extensions: "pdf".to_string(),
            allowed_mimes: String::new(),
            allow_overwrite: false,
            direct_upload: true,
            presign_ttl_secs: 900,
            storage_code: storage_code.map(str::to_string),
            status: EnableStatus::enabled,
            sort_order: 0,
            remark: None,
            created_at: now,
            updated_at: now,
            create_by: None,
            update_by: None,
            deleted_at: None,
        }
    }

    /// 构造一条测试后端配置。
    fn config_with(code: &str, backend_type: &str, is_default: bool) -> SysStorageConfigModel {
        let now = chrono::Utc::now();
        SysStorageConfigModel {
            id: uuid::Uuid::now_v7(),
            config_code: code.to_string(),
            config_name: code.to_string(),
            backend_type: backend_type.to_string(),
            endpoint: None,
            bucket: None,
            region: "us-east-1".to_string(),
            base_path: String::new(),
            use_tls: true,
            path_style: true,
            ak_cipher: None,
            ak_nonce: None,
            sk_cipher: None,
            sk_nonce: None,
            key_version: 1,
            is_default,
            status: EnableStatus::enabled,
            health_status: "UNKNOWN".to_string(),
            last_check_at: None,
            last_error: None,
            sort_order: 0,
            remark: None,
            created_at: now,
            updated_at: now,
            create_by: None,
            update_by: None,
            deleted_at: None,
        }
    }

    /// 正常路径：策略未指定后端时用默认后端。
    #[test]
    fn router_fallback_to_default() {
        let router = StorageRouter::new();
        router.insert(Arc::new(MemoryStore::new("local-default")));
        router.insert(Arc::new(MemoryStore::new("backup")));
        assert!(router.set_default("local-default"));
        assert_eq!(router.len(), 2);
        assert_eq!(
            router.codes(),
            vec!["backup".to_string(), "local-default".to_string()]
        );

        let store = router
            .pick(Some(&policy_with(None)))
            .expect("应回退到默认后端");
        assert_eq!(store.config_code(), "local-default");
        // 无策略时同样走默认
        assert_eq!(
            router.pick(None).expect("应回退到默认后端").config_code(),
            "local-default"
        );
        assert_eq!(router.default_code().as_deref(), Some("local-default"));
    }

    /// 正常路径：策略指定后端时优先用指定后端。
    #[test]
    fn router_picks_policy_backend() {
        let router = StorageRouter::new();
        router.insert(Arc::new(MemoryStore::new("local-default")));
        router.insert(Arc::new(MemoryStore::new("minio-a")));
        router.set_default("local-default");

        let store = router
            .pick(Some(&policy_with(Some("minio-a"))))
            .expect("应选中策略指定后端");
        assert_eq!(store.config_code(), "minio-a");

        // 空白编码视为未指定（避免前端传空串导致 409）
        assert_eq!(
            router
                .pick(Some(&policy_with(Some("   "))))
                .expect("空白编码应按默认后端")
                .config_code(),
            "local-default"
        );
    }

    /// 错误分支：后端不存在 → 409 / 60001（且消息含编码，便于运维定位）。
    #[test]
    fn router_missing_backend_conflict() {
        let router = StorageRouter::new();
        router.insert(Arc::new(MemoryStore::new("local-default")));
        router.set_default("local-default");

        let error = router
            .pick(Some(&policy_with(Some("deleted-backend"))))
            .err()
            .expect("后端不存在必须报 409");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::STORAGE_BACKEND_UNAVAILABLE);
        assert!(error.msg.contains("deleted-backend"), "{}", error.msg);
    }

    /// 错误分支：没有默认后端 → 409 / 60001（提示去配置页设置）。
    #[test]
    fn router_no_default_conflict() {
        let router = StorageRouter::new();
        router.insert(Arc::new(MemoryStore::new("orphan")));
        // 未设置默认
        let error = router.pick(None).err().expect("没有默认后端必须报 409");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, error_code::STORAGE_BACKEND_UNAVAILABLE);
        assert!(error.msg.contains("默认存储后端"), "{}", error.msg);

        // 设置了不存在的默认后端
        assert!(!router.set_default("not-exists"));
        assert_eq!(router.default_code(), None);
    }

    /// 正常路径：配置装载只收启用中的后端，默认后端取 `is_default` 行。
    #[test]
    fn router_replace_loads_enabled_configs() {
        let key =
            MasterKey::parse("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
                .expect("测试主密钥应合法");
        let mut disabled = config_with("disabled-one", "LOCAL", false);
        disabled.status = EnableStatus::disabled;

        let configs = vec![
            config_with("local-default", "LOCAL", true),
            config_with("backup-local", "LOCAL", false),
            disabled,
        ];

        let router = StorageRouter::new();
        let loaded = router.replace(&configs, &key);
        assert_eq!(loaded, 2, "停用后端不应装载");
        assert_eq!(
            router.codes(),
            vec!["backup-local".to_string(), "local-default".to_string()]
        );
        assert_eq!(router.default_code().as_deref(), Some("local-default"));
    }

    /// 错误分支：S3 配置缺密钥时构造失败（该后端不装载，其它后端不受影响）。
    #[test]
    fn router_skips_s3_without_credentials() {
        let key =
            MasterKey::parse("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
                .expect("测试主密钥应合法");
        let mut broken = config_with("s3-broken", "S3", false);
        broken.endpoint = Some("http://minio.local:9000".to_string());
        broken.bucket = Some("mybucket".to_string());

        let configs = vec![config_with("local-default", "LOCAL", true), broken];
        let router = StorageRouter::new();
        assert_eq!(router.replace(&configs, &key), 1);
        assert!(router.by_code("s3-broken").is_none());
        assert!(router.by_code("local-default").is_some());
    }
}
