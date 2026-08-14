//! 知识库 URL 入库的受控外呼边界。
//!
//! 每次跳转都重新校验 URL 与 DNS，并把校验通过的地址钉进该跳专用 client，防止
//! redirect / DNS rebinding 绕过 SSRF 护栏。只返回 HTML/PDF，响应体严格限制为 5MB。

use reqwest::header;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::time::Duration;

const URL_FETCH_TIMEOUT: Duration = Duration::from_secs(15);
const URL_FETCH_MAX_BYTES: usize = 5 * 1_048_576;
const URL_FETCH_MAX_REDIRECTS: usize = 3;
const URL_MAX_LEN: usize = 2048;

/// URL 抓取失败分类。server 只负责把分类映射成 HTTP 状态码，安全判定与文案留在本边界。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlFetchError {
    BadInput(String),
    Upstream(String),
    Internal(String),
}

impl UrlFetchError {
    fn bad_input(msg: impl Into<String>) -> Self {
        Self::BadInput(msg.into())
    }

    fn upstream(msg: impl Into<String>) -> Self {
        Self::Upstream(msg.into())
    }

    fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }
}

impl fmt::Display for UrlFetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadInput(msg) | Self::Upstream(msg) | Self::Internal(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for UrlFetchError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FetchedKind {
    Html,
    Pdf,
}

impl FetchedKind {
    fn ext(self) -> &'static str {
        match self {
            Self::Html => "html",
            Self::Pdf => "pdf",
        }
    }

    fn mime(self) -> &'static str {
        match self {
            Self::Html => "text/html",
            Self::Pdf => "application/pdf",
        }
    }
}

/// 已通过安全护栏与内容白名单的抓取结果。
pub struct FetchedUrl {
    pub bytes: Vec<u8>,
    pub final_url: String,
    pub file_name: String,
    kind: FetchedKind,
}

impl FetchedUrl {
    pub fn mime(&self) -> &'static str {
        self.kind.mime()
    }
}

/// 把本地绝对路径编码成 `file:` URL，供只接受 URL 的本地进程参数使用。
pub fn local_file_url(path: &Path) -> Option<String> {
    reqwest::Url::from_file_path(path).ok().map(|url| url.to_string())
}

/// 抓取 URL，并在每次跳转重新执行 URL 形状、DNS/IP 与 DNS pin 安全检查。
pub async fn fetch_guarded(raw: &str) -> Result<FetchedUrl, UrlFetchError> {
    let mut current = checked_url_shape(raw)?;
    for hop in 0..=URL_FETCH_MAX_REDIRECTS {
        let addrs = resolve_checked(&current).await?;
        let client = reqwest::Client::builder()
            .timeout(URL_FETCH_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .resolve_to_addrs(current.host_str().unwrap_or_default(), &addrs)
            .user_agent(concat!("dms-kb-url-ingest/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| UrlFetchError::internal("抓取客户端初始化失败"))?;
        let mut resp = client
            .get(current.clone())
            .send()
            .await
            .map_err(|_| UrlFetchError::upstream("目标地址抓取失败或超时（15s）"))?;
        let status = resp.status();
        if status.is_redirection() {
            if hop == URL_FETCH_MAX_REDIRECTS {
                return Err(UrlFetchError::bad_input("重定向次数过多（最多 3 次）"));
            }
            let location = resp
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| UrlFetchError::upstream("上游返回了无效的重定向"))?;
            current = checked_redirect(&current, location)?;
            continue;
        }
        if !status.is_success() {
            return Err(UrlFetchError::upstream(format!("目标地址返回 HTTP {status}")));
        }
        if resp.content_length().is_some_and(|n| n as usize > URL_FETCH_MAX_BYTES) {
            return Err(UrlFetchError::bad_input("页面超过 5MB 上限，未入库"));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|_| UrlFetchError::upstream("读取目标内容失败或超时"))?
        {
            if !capped_append(&mut bytes, &chunk) {
                return Err(UrlFetchError::bad_input("页面超过 5MB 上限，未入库"));
            }
        }
        if bytes.is_empty() {
            return Err(UrlFetchError::bad_input("目标页面为空"));
        }
        let content_type = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok());
        let kind = classify_content(content_type, &bytes).ok_or_else(|| {
            UrlFetchError::bad_input("只支持 HTML 页面或 PDF 文档（按 Content-Type 与内容判定）")
        })?;
        let file_name = url_file_name(&current, kind);
        return Ok(FetchedUrl { bytes, kind, final_url: current.to_string(), file_name });
    }
    unreachable!("重定向跳数闸在循环内已返回")
}

fn checked_redirect(current: &reqwest::Url, location: &str) -> Result<reqwest::Url, UrlFetchError> {
    let next = current
        .join(location)
        .map_err(|_| UrlFetchError::upstream("上游返回了无效的重定向"))?;
    checked_url_shape(next.as_str())
}

/// 仅 http/https、必须有 host、禁 userinfo、只放 80/443、长度封顶。
fn checked_url_shape(raw: &str) -> Result<reqwest::Url, UrlFetchError> {
    if raw.is_empty() || raw.len() > URL_MAX_LEN {
        return Err(UrlFetchError::bad_input(format!("URL 不能为空且不超过 {URL_MAX_LEN} 字符")));
    }
    let url = reqwest::Url::parse(raw).map_err(|_| UrlFetchError::bad_input("URL 格式无效"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(UrlFetchError::bad_input("只支持 http:// 或 https:// 地址"));
    }
    if url.host_str().is_none() {
        return Err(UrlFetchError::bad_input("URL 缺少主机名"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UrlFetchError::bad_input("URL 不允许携带账号信息"));
    }
    if !matches!(url.port_or_known_default(), Some(80 | 443)) {
        return Err(UrlFetchError::bad_input("只支持 80/443 端口的地址"));
    }
    Ok(url)
}

/// 本机/私网/链路本地/保留段一律拒；v4 映射 v6 解包后按 v4 判定。
fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_broadcast()
                || octets[0] == 0
                || (octets[0] == 100 && (64..=127).contains(&octets[1]))
                || octets[0] >= 240
        }
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_forbidden_ip(IpAddr::V4(mapped));
            }
            let segments = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (segments[0] & 0xfe00) == 0xfc00
                || (segments[0] & 0xffc0) == 0xfe80
        }
    }
}

/// DNS 全量校验后返回同一批地址，调用方将其钉入本跳 client 防 DNS rebinding。
async fn resolve_checked(url: &reqwest::Url) -> Result<Vec<SocketAddr>, UrlFetchError> {
    let host = url.host_str().unwrap_or_default();
    let port = url.port_or_known_default().unwrap_or(443);
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| UrlFetchError::upstream("目标地址无法解析"))?
        .collect();
    if addrs.is_empty() {
        return Err(UrlFetchError::upstream("目标地址无法解析"));
    }
    if addrs.iter().any(|addr| is_forbidden_ip(addr.ip())) {
        return Err(UrlFetchError::bad_input("目标地址指向内网或本机，不允许抓取"));
    }
    Ok(addrs)
}

fn capped_append(buf: &mut Vec<u8>, chunk: &[u8]) -> bool {
    if buf.len() + chunk.len() > URL_FETCH_MAX_BYTES {
        return false;
    }
    buf.extend_from_slice(chunk);
    true
}

fn classify_content(content_type: Option<&str>, bytes: &[u8]) -> Option<FetchedKind> {
    let content_type = content_type.unwrap_or_default().to_ascii_lowercase();
    let content_type = content_type.split(';').next().unwrap_or_default().trim();
    if content_type.contains("text/html") || content_type.contains("application/xhtml") {
        return Some(FetchedKind::Html);
    }
    if content_type.contains("application/pdf") || bytes.starts_with(b"%PDF-") {
        return Some(FetchedKind::Pdf);
    }
    if (content_type.is_empty() || content_type.contains("text/plain")) && looks_like_html(bytes) {
        return Some(FetchedKind::Html);
    }
    None
}

fn looks_like_html(bytes: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_ascii_lowercase();
    let text = head.trim_start_matches('\u{feff}').trim_start();
    text.starts_with("<!doctype html") || text.starts_with("<html")
}

fn url_file_name(url: &reqwest::Url, kind: FetchedKind) -> String {
    let raw = url
        .path_segments()
        .and_then(|segments| segments.filter(|segment| !segment.is_empty()).next_back())
        .map(percent_decode)
        .unwrap_or_else(|| url.host_str().unwrap_or("page").to_string());
    let stem = match raw.rsplit_once('.') {
        Some((stem, ext)) if matches!(ext.to_ascii_lowercase().as_str(), "html" | "htm" | "pdf") => stem,
        _ => raw.as_str(),
    };
    let mut slug = String::new();
    let mut len = 0usize;
    for ch in stem.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') || ('\u{4e00}'..='\u{9fff}').contains(&ch) {
            slug.push(ch);
            len += 1;
        } else if !slug.ends_with('_') {
            slug.push('_');
            len += 1;
        }
        if len >= 60 {
            break;
        }
    }
    let slug = slug.trim_matches('_');
    let slug = if slug.is_empty() { "page" } else { slug };
    format!("{slug}.{}", kind.ext())
}

fn percent_decode(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex_val(bytes[index + 1]), hex_val(bytes[index + 2])) {
                out.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_shape_gate_rejects_non_web_and_confusing_targets() {
        for bad in [
            "file:///etc/passwd",
            "ftp://example.com/a.pdf",
            "javascript:alert(1)",
            "http://",
            "http://user:pass@example.com/",
            "http://user@example.com/",
            "http://example.com:8080/",
            "https://example.com:6379/",
            "gopher://example.com/",
        ] {
            assert!(checked_url_shape(bad).is_err(), "{bad}");
        }
        let too_long = format!("https://example.com/{}", "a".repeat(URL_MAX_LEN));
        assert!(checked_url_shape(&too_long).is_err());
        assert!(checked_url_shape("").is_err());
        for ok in [
            "http://example.com/a/b.html",
            "https://example.com:443/x.pdf",
            "http://example.com:80/",
            "https://example.com",
            "https://example.com/path?q=1#frag",
        ] {
            assert!(checked_url_shape(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn redirect_target_rechecks_full_url_shape() {
        let current = checked_url_shape("https://example.com/docs/page").unwrap();
        for bad in [
            "ftp://example.net/file.pdf",
            "https://user:pass@example.net/file.pdf",
            "https://example.net:8080/file.pdf",
        ] {
            assert!(checked_redirect(&current, bad).is_err(), "{bad}");
        }
        let too_long = format!("https://example.net/{}", "a".repeat(URL_MAX_LEN));
        assert!(checked_redirect(&current, &too_long).is_err());

        let next = checked_redirect(&current, "../safe.pdf").unwrap();
        assert_eq!(next.as_str(), "https://example.com/safe.pdf");
    }

    #[test]
    fn ssrf_ip_blocklist_covers_private_loopback_and_reserved() {
        let blocked = [
            "127.0.0.1", "127.0.1.1", "10.0.0.1", "10.255.255.255", "172.16.0.1",
            "172.31.255.255", "192.168.1.1", "169.254.1.1", "0.0.0.0", "0.1.2.3",
            "100.64.0.1", "100.127.255.254", "224.0.0.1", "240.0.0.1",
            "255.255.255.255", "::1", "::", "fe80::1", "fc00::1", "fd00::1",
            "ff02::1", "::ffff:127.0.0.1", "::ffff:10.0.0.1", "::ffff:192.168.0.1",
        ];
        for ip in blocked {
            assert!(is_forbidden_ip(ip.parse::<IpAddr>().unwrap()), "{ip} 应被拒");
        }
        let allowed = [
            "8.8.8.8",
            "1.1.1.1",
            "100.63.255.255",
            "100.128.0.1",
            "172.15.0.1",
            "172.32.0.1",
            "2606:4700:4700::1111",
        ];
        for ip in allowed {
            assert!(!is_forbidden_ip(ip.parse::<IpAddr>().unwrap()), "{ip} 应放行");
        }
    }

    #[test]
    fn fetch_cap_aborts_over_5mb_stream() {
        let mut buf = Vec::new();
        assert!(capped_append(&mut buf, &vec![0u8; URL_FETCH_MAX_BYTES]));
        assert_eq!(buf.len(), URL_FETCH_MAX_BYTES);
        assert!(!capped_append(&mut buf, &[1u8; 1]), "超帽必须拒");
        let mut small = Vec::new();
        assert!(capped_append(&mut small, &[0u8; 1024]));
        assert!(!capped_append(&mut small, &vec![0u8; URL_FETCH_MAX_BYTES]));
        assert_eq!(small.len(), 1024, "拒绝时不得污染已读内容");
    }

    #[test]
    fn fetched_content_classification_is_html_or_pdf_only() {
        assert_eq!(classify_content(Some("text/html; charset=utf-8"), b"x"), Some(FetchedKind::Html));
        assert_eq!(classify_content(Some("application/xhtml+xml"), b"x"), Some(FetchedKind::Html));
        assert_eq!(classify_content(Some("application/pdf"), b"x"), Some(FetchedKind::Pdf));
        assert_eq!(classify_content(None, b"%PDF-1.7 rest"), Some(FetchedKind::Pdf));
        assert_eq!(classify_content(Some("application/octet-stream"), b"%PDF-1.4"), Some(FetchedKind::Pdf));
        assert_eq!(classify_content(Some("text/plain"), b"  <!DOCTYPE html><html>"), Some(FetchedKind::Html));
        assert_eq!(classify_content(None, b"<html lang=\"zh\">"), Some(FetchedKind::Html));
        assert_eq!(classify_content(Some("text/plain"), b"\xef\xbb\xbf<html>"), Some(FetchedKind::Html));
        for (content_type, body) in [
            (Some("image/png"), &b"\x89PNG"[..]),
            (Some("application/zip"), &b"PK\x03\x04"[..]),
            (Some("text/plain"), &b"just some text"[..]),
            (None, &b"{}"[..]),
        ] {
            assert_eq!(classify_content(content_type, body), None, "{content_type:?}");
        }
    }

    #[test]
    fn url_file_name_sanitizes_and_caps() {
        let url = |raw: &str| reqwest::Url::parse(raw).unwrap();
        assert_eq!(url_file_name(&url("https://example.com/a/b.html"), FetchedKind::Html), "b.html");
        assert_eq!(url_file_name(&url("https://example.com/report.pdf"), FetchedKind::Pdf), "report.pdf");
        assert_eq!(url_file_name(&url("https://example.com/report.pdf?v=2"), FetchedKind::Pdf), "report.pdf");
        assert_eq!(url_file_name(&url("https://example.com/"), FetchedKind::Html), "example_com.html");
        assert_eq!(url_file_name(&url("https://example.com/.../!!!/"), FetchedKind::Html), "page.html");
        assert_eq!(url_file_name(&url("https://example.com/报销制度-2026"), FetchedKind::Html), "报销制度-2026.html");
        assert_eq!(url_file_name(&url("https://example.com/%E6%8A%A5%E9%94%80"), FetchedKind::Html), "报销.html");
        assert_eq!(url_file_name(&url("https://example.com/%zz%2"), FetchedKind::Html), "zz_2.html");
        let name = url_file_name(&url("https://example.com/dir/../../etc/passwd"), FetchedKind::Html);
        assert!(!name.contains('/') && !name.contains('\\'), "{name}");
        let long = url_file_name(&url(&format!("https://example.com/{}", "长".repeat(200))), FetchedKind::Html);
        assert!(long.trim_end_matches(".html").chars().count() <= 60, "{long}");
    }

    #[test]
    fn local_file_url_percent_encodes_paths() {
        let path = if cfg!(windows) {
            Path::new(r"C:\tmp\profile dir")
        } else {
            Path::new("/tmp/profile dir")
        };
        let url = local_file_url(path).expect("absolute path");
        assert!(url.starts_with("file://") && url.contains("profile%20dir"), "{url}");
    }
}
