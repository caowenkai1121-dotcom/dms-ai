//! 【D1】settings.json 敏感字段的对称加密：AES-256-GCM，密文格式
//! `enc:v1:<base64(nonce‖ciphertext‖tag)>`（nonce 96bit 随机，tag 128bit，无 AAD）。
//!
//! 三条纪律：
//! 1. **纯函数**：`encrypt_with` / `decrypt_with` 只认入参里的钥匙，不读环境、不写日志 ——
//!    单测用固定钥匙，进程级钥匙只经 `default_key()` 这一处派生（OnceLock 缓存）。
//! 2. **前缀判定**：`enc:v1:` 开头 = 密文，否则一律按明文原样放行 —— 向后兼容旧配置，
//!    且重复跑迁移是幂等的（已是密文的字段不会再被包一层）。
//! 3. **钥匙永不落盘、永不进日志**：只在内存里由 `DMS_SECRET_KEY` 或机器指纹派生；
//!    任何错误文案只报字段名，不带密文/明文片段。
//!
//! 字段清单（哪些键算敏感）不在本文件 —— 那是 `db.rs` 的 `encrypt_sensitive_fields` /
//! `Settings::decrypt_secrets`；两边改动必须同步（Python 侧镜像在 `tools/settings.py`）。

use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use ring::rand::{SecureRandom, SystemRandom};

use base64::Engine as _;

/// 密文前缀：版本号留在格式里，将来换算法/换参数只认新前缀，老密文照常解。
pub const ENC_PREFIX: &str = "enc:v1:";
/// AES-GCM 标准 nonce 长度（96bit）
const NONCE_LEN: usize = 12;
/// GCM tag 长度（128bit）—— ring 的 `seal_in_place_append_tag` 固定追加这么长
const TAG_LEN: usize = 16;

/// 是否是密文（读取侧的唯一判定：看前缀，不猜内容）。
pub fn is_encrypted(s: &str) -> bool {
    s.starts_with(ENC_PREFIX)
}

/// 加解密失败。**刻意不带任何值片段**：这类错误的文案会进日志/启动报错。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptoError {
    /// base64 解不出来 —— 密文损坏（或被手改过）
    Decode,
    /// 连 nonce‖tag 都不够长 —— 密文被截断
    Truncated,
    /// GCM 认证失败 —— 钥匙不对，或密文被改过一个字节
    Auth,
    /// 系统随机源失败（理论上到不了；不 unwrap，留给启动路径响亮失败）
    Rng,
    /// 解出的字节不是 UTF-8 —— 加密的都是 `str`，出现即损坏
    Utf8,
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Decode => "密文不是合法 base64（已损坏）",
            Self::Truncated => "密文长度不足（被截断）",
            Self::Auth => "GCM 认证失败（钥匙不对或密文被篡改）",
            Self::Rng => "系统随机源不可用",
            Self::Utf8 => "解密结果不是 UTF-8（密文损坏）",
        })
    }
}

impl std::error::Error for CryptoError {}

/// 明文 → `enc:v1:...`。每次加密都取新随机 nonce —— 同一明文两次密文不同（语义安全）。
pub fn encrypt_with(key: &[u8; 32], plain: &str) -> Result<String, CryptoError> {
    let mut nonce = [0u8; NONCE_LEN];
    SystemRandom::new().fill(&mut nonce).map_err(|_| CryptoError::Rng)?;
    let k = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, key).map_err(|_| CryptoError::Auth)?);
    let mut buf = plain.as_bytes().to_vec();
    k.seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut buf)
        .map_err(|_| CryptoError::Auth)?;
    let mut blob = Vec::with_capacity(NONCE_LEN + buf.len());
    blob.extend_from_slice(&nonce);
    blob.append(&mut buf);
    Ok(format!(
        "{ENC_PREFIX}{}",
        base64::engine::general_purpose::STANDARD.encode(&blob)
    ))
}

/// `enc:v1:...` → 明文；**无前缀原样返回**（旧配置明文兼容 + 二次运行幂等）。
pub fn decrypt_with(key: &[u8; 32], s: &str) -> Result<String, CryptoError> {
    let Some(body) = s.strip_prefix(ENC_PREFIX) else {
        return Ok(s.to_string());
    };
    let blob = base64::engine::general_purpose::STANDARD
        .decode(body)
        .map_err(|_| CryptoError::Decode)?;
    if blob.len() < NONCE_LEN + TAG_LEN {
        return Err(CryptoError::Truncated);
    }
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&blob[..NONCE_LEN]);
    let k = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, key).map_err(|_| CryptoError::Auth)?);
    let mut buf = blob[NONCE_LEN..].to_vec();
    let plain = k
        .open_in_place(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut buf)
        .map_err(|_| CryptoError::Auth)?;
    String::from_utf8(plain.to_vec()).map_err(|_| CryptoError::Utf8)
}

/// 幂等加密：空串与已是密文的原样返回，其余加密。空串不包一层 ——
/// 「没配」与「配了」一眼可辨，迁移也不会把几十个空字段都变成密文噪音。
pub fn encrypt_if_plain_with(key: &[u8; 32], s: &str) -> Result<String, CryptoError> {
    if s.is_empty() || is_encrypted(s) {
        return Ok(s.to_string());
    }
    encrypt_with(key, s)
}

/// 读取侧的透明解密入口（`resolve_provider` / `dsn_map` 等）：无前缀原样返回
/// —— 连派钥都不触发（内存 cfg 正常已是明文，这步是零成本保险）；真撞上密文才用进程钥匙解。
pub fn decrypt_auto(s: &str) -> Result<String, CryptoError> {
    if is_encrypted(s) {
        decrypt_with(&default_key().0, s)
    } else {
        Ok(s.to_string())
    }
}

/// 进程级钥匙的来源（`load_settings` 按它决定要不要 warn 一次）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    /// `DMS_SECRET_KEY`（≥32 字节）sha2-256 派生 —— 可跨机迁移的推荐形态
    Env,
    /// `DMS_SECRET_KEY` 配了但少于 32 字节：能用，但熵不足，warn 一次
    EnvShort,
    /// 未配置环境变量：由机器指纹派生（见 `machine_fingerprint` 的运维语义）
    Machine,
}

/// 进程级钥匙（OnceLock 派生一次）。**返回值只许用于加解密，永不格式化进日志。**
pub fn default_key() -> ([u8; 32], KeySource) {
    static KEY: std::sync::OnceLock<([u8; 32], KeySource)> = std::sync::OnceLock::new();
    *KEY.get_or_init(|| match std::env::var("DMS_SECRET_KEY") {
        Ok(s) if !s.is_empty() => (
            sha256(s.as_bytes()),
            if s.len() >= 32 { KeySource::Env } else { KeySource::EnvShort },
        ),
        _ => (sha256(machine_fingerprint().as_bytes()), KeySource::Machine),
    })
}

/// sha2-256（`ring::digest`）—— `DMS_SECRET_KEY` 是「≥32 字节任意串」，
/// 哈希一次正好收敛到 32B 钥匙；机器指纹同理。
fn sha256(data: &[u8]) -> [u8; 32] {
    let d = ring::digest::digest(&ring::digest::SHA256, data);
    let mut out = [0u8; 32];
    out.copy_from_slice(d.as_ref());
    out
}

/// 未配 `DMS_SECRET_KEY` 时的兜底钥匙材料：hostname + username。
///
/// ⚠️ **运维语义：跨机不可迁移**。换机器、换运行账号、容器重建（容器 hostname
/// 是容器 ID，每次重建都变）之后，这份 settings.json 里的密文就解不开 ——
/// 那是设计如此的响亮失败（启动报错指回 DMS_SECRET_KEY），不是数据损坏：
/// 重填明文凭据（或配上 `DMS_SECRET_KEY`）即可。要把 settings.json 搬出本机，
/// 唯一受支持的方式就是部署时显式配置 `DMS_SECRET_KEY`。
fn machine_fingerprint() -> String {
    let env = |names: &[&str]| {
        names
            .iter()
            .find_map(|n| std::env::var(n).ok())
            .filter(|s| !s.trim().is_empty())
    };
    let host = env(&["HOSTNAME", "COMPUTERNAME"]).unwrap_or_else(|| "unknown-host".into());
    let user = env(&["USER", "USERNAME"]).unwrap_or_else(|| "unknown-user".into());
    // 域分隔串：同样的 host/user 不会与别的用途撞出同一把钥匙。
    // ⚠️ `tools/settings.py` 有一模一样的一份 —— 改这里必须同步改那边。
    format!("dms-ai/settings-enc v1\nhost={host}\nuser={user}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const K1: [u8; 32] = [7u8; 32];
    const K2: [u8; 32] = [9u8; 32];

    /// 往返：加密 → 前缀对 → 解密还原；且两次加密同一明文结果不同（随机 nonce）
    #[test]
    fn roundtrip_and_random_nonce() {
        let c1 = encrypt_with(&K1, "mysql://root:s3cret@10.0.0.7:3306/dms").unwrap();
        let c2 = encrypt_with(&K1, "mysql://root:s3cret@10.0.0.7:3306/dms").unwrap();
        assert!(is_encrypted(&c1) && is_encrypted(&c2));
        assert_ne!(c1, c2, "同一明文两次密文必须不同（nonce 随机）");
        // 布局：base64(nonce(12)‖ct‖tag(16))
        let blob = base64::engine::general_purpose::STANDARD
            .decode(c1.strip_prefix(ENC_PREFIX).unwrap())
            .unwrap();
        assert_eq!(blob.len(), NONCE_LEN + "mysql://root:s3cret@10.0.0.7:3306/dms".len() + TAG_LEN);
        assert_eq!(
            decrypt_with(&K1, &c1).unwrap(),
            "mysql://root:s3cret@10.0.0.7:3306/dms"
        );
        assert_eq!(decrypt_with(&K1, &c2).unwrap(), decrypt_with(&K1, &c1).unwrap());
        // 空串与中文也成立（配置值没有字符集假设）
        assert_eq!(decrypt_with(&K1, &encrypt_with(&K1, "").unwrap()).unwrap(), "");
        assert_eq!(decrypt_with(&K1, &encrypt_with(&K1, "口令@中文").unwrap()).unwrap(), "口令@中文");
    }

    /// 错钥匙 = GCM 认证失败（不是乱码明文）；密文改一个字节同样失败
    #[test]
    fn wrong_key_and_tampering_are_loud() {
        let c = encrypt_with(&K1, "sk-live-key").unwrap();
        assert_eq!(decrypt_with(&K2, &c).unwrap_err(), CryptoError::Auth);
        // 篡改密文体（base64 尾部一个字符）
        let mut bad = c.clone();
        let n = bad.len();
        // 🔴 按**被替换的那一位**选替换值，不是按末位：原写法判 `ends_with('A')` 却改
        // 倒数第二位，当那一位本来就是 'A' 而末位不是时，这次「篡改」是空操作 →
        // 密文没变 → 解密成功 → 断言随机红（随机 nonce 下约 1/64 一次，2026-08-13 实测撞到）。
        let target = bad.as_bytes()[n - 2] as char;
        bad.replace_range(n - 2..n - 1, if target == 'A' { "B" } else { "A" });
        assert!(decrypt_with(&K1, &bad).is_err(), "改过的密文必须响亮失败");
        // 截断（连 tag 都不剩）
        let short = format!("{ENC_PREFIX}{}", base64::engine::general_purpose::STANDARD.encode([1u8; 8]));
        assert_eq!(decrypt_with(&K1, &short).unwrap_err(), CryptoError::Truncated);
        // 非法 base64
        assert_eq!(decrypt_with(&K1, "enc:v1:不是base64!!").unwrap_err(), CryptoError::Decode);
        // 错误文案不带任何值片段（会进日志）
        for e in [CryptoError::Auth, CryptoError::Decode, CryptoError::Truncated] {
            assert!(!e.to_string().contains("sk-live"), "{e}");
        }
    }

    /// 明文兼容：无前缀值解密 = 原样；幂等加密：密文不再包第二层，空串不加密
    #[test]
    fn plaintext_passthrough_and_idempotent_encrypt() {
        assert_eq!(decrypt_with(&K1, "sk-plain"), Ok("sk-plain".to_string()));
        assert_eq!(decrypt_with(&K1, "mysql://u:p@h/db").unwrap(), "mysql://u:p@h/db");
        let once = encrypt_if_plain_with(&K1, "sk-1").unwrap();
        assert!(is_encrypted(&once));
        assert_eq!(encrypt_if_plain_with(&K1, &once).unwrap(), once, "二次加密必须逐字节不变");
        assert_eq!(encrypt_if_plain_with(&K1, "").unwrap(), "", "空串保持空串");
        assert_eq!(decrypt_with(&K1, &once).unwrap(), "sk-1");
    }

    /// decrypt_auto：明文零成本放行；密文走进程默认钥匙（自洽：同进程派钥稳定）
    #[test]
    fn decrypt_auto_uses_process_key_only_for_ciphertext() {
        assert_eq!(decrypt_auto("plain").unwrap(), "plain");
        let (key, _) = default_key();
        let c = encrypt_with(&key, "sk-process").unwrap();
        assert_eq!(decrypt_auto(&c).unwrap(), "sk-process");
        // 默认钥匙只派生一次（同进程两次调用同源）
        assert_eq!(default_key().0, key);
    }

    /// 🔴 跨实现互认：这份密文是 **Python `cryptography`** 用 key=[7;32]、nonce=0..11 封的
    /// （`tools/settings.py` 判官链要解的就是 Rust 写的，反之亦然 —— 线格式必须两边同解）。
    /// 换格式（nonce/tag 长度、base64  alphabet、AAD）这条当场红。
    #[test]
    fn python_sealed_ciphertext_decrypts() {
        const SEALED_BY_PYTHON: &str =
            "enc:v1:AAECAwQFBgcICQoLdfiaAXEz9mYF3Yro2CtfmZNZws4pUlkBOP5NjbTkXjXoQCKP5M8FC3i6A7imXT70jrtCUOw=";
        assert_eq!(
            decrypt_with(&K1, SEALED_BY_PYTHON).unwrap(),
            "mysql://root:s3cret@10.0.0.7:3306/dms"
        );
    }
}
