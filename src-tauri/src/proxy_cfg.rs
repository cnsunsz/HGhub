//! iOS / 本机代理配置。
//!
//! reqwest 的 `system-proxy` 只读 Windows 注册表和 `HTTP_PROXY` 一类环境变量。
//! iOS 上既没有这些环境变量，Shadowrocket 的 VPN/规则模式也看不到「先解析再钉死 IP」
//! 的直连，所以默认改为自动检测本机 HTTP 代理（小火箭默认 `127.0.0.1:1082`）。
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use url::Url;

use crate::net::redact_proxy;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyMode {
    Auto,
    Off,
    Manual,
}

impl ProxyMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Off => "off",
            Self::Manual => "manual",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "off" | "none" | "disabled" => Some(Self::Off),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
struct ProxySettings {
    mode: ProxyMode,
    url: String,
}

fn default_mode() -> ProxyMode {
    if cfg!(target_os = "ios") {
        ProxyMode::Auto
    } else {
        ProxyMode::Off
    }
}

static SETTINGS: Lazy<Mutex<ProxySettings>> = Lazy::new(|| {
    Mutex::new(ProxySettings {
        mode: default_mode(),
        url: String::new(),
    })
});
static SETTINGS_FILE: Lazy<Mutex<Option<PathBuf>>> = Lazy::new(|| Mutex::new(None));
static PROBE_CACHE: Lazy<Mutex<(Option<Instant>, Option<String>)>> =
    Lazy::new(|| Mutex::new((None, None)));

const PROBE_TTL: Duration = Duration::from_secs(15);

/// 启动时读取落盘的代理设置；文件不存在则使用平台默认（iOS 为自动检测）。
pub fn bind(dir: &Path) {
    let path = dir.join("proxy.json");
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            let mode = v
                .get("mode")
                .and_then(|x| x.as_str())
                .and_then(ProxyMode::parse)
                .unwrap_or_else(default_mode);
            let url = v
                .get("url")
                .and_then(|x| x.as_str())
                .map(str::trim)
                .unwrap_or("")
                .to_string();
            if let Ok(mut s) = SETTINGS.lock() {
                s.mode = mode;
                s.url = url;
            }
        }
    }
    if let Ok(mut f) = SETTINGS_FILE.lock() {
        *f = Some(path);
    }
}

/// 校验并规整用户填写的代理地址。允许带用户名密码的 HTTP(S)/SOCKS5。
pub fn parse_proxy_url(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("请填写代理地址".into());
    }
    if raw.chars().any(char::is_control) {
        return Err("代理地址含控制字符".into());
    }
    let url = Url::parse(raw).map_err(|_| "代理地址无效")?;
    if !matches!(url.scheme(), "http" | "https" | "socks5" | "socks5h") {
        return Err("代理请使用 http 或 socks5 地址，例如 http://127.0.0.1:1082".into());
    }
    if url.host().is_none() || url.host_str().is_some_and(|h| h.is_empty()) {
        return Err("代理缺少主机名".into());
    }
    Ok(url.to_string())
}

fn env_override() -> Option<String> {
    std::env::var("HGHUB_PROXY")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 当前应当交给 reqwest 的代理 URL。`HGHUB_PROXY` 始终优先。
pub fn effective_proxy() -> Option<(String, &'static str)> {
    if let Some(url) = env_override() {
        return Some((url, "env"));
    }
    let (mode, url) = {
        let settings = SETTINGS.lock().ok()?;
        (settings.mode, settings.url.clone())
    };
    match mode {
        ProxyMode::Off => None,
        ProxyMode::Manual => {
            let url = parse_proxy_url(&url).ok()?;
            Some((url, "manual"))
        }
        ProxyMode::Auto => detect_auto(),
    }
}

fn detect_auto() -> Option<(String, &'static str)> {
    #[cfg(any(target_os = "ios", target_os = "macos"))]
    if let Some(url) = cf_system_proxy() {
        return Some((url, "system"));
    }
    if let Some(url) = probe_local_cached() {
        return Some((url, "local"));
    }
    None
}

pub fn invalidate_probe_cache() {
    if let Ok(mut c) = PROBE_CACHE.lock() {
        *c = (None, None);
    }
}

fn probe_local_cached() -> Option<String> {
    let now = Instant::now();
    if let Ok(c) = PROBE_CACHE.lock() {
        if let Some(at) = c.0 {
            if now.saturating_duration_since(at) < PROBE_TTL {
                return c.1.clone();
            }
        }
    }
    let found = probe_local_now();
    if let Ok(mut c) = PROBE_CACHE.lock() {
        *c = (Some(Instant::now()), found.clone());
    }
    found
}

/// 探测本机常见代理端口。小火箭 HTTP 默认 1082，SOCKS 常见 1080，Surge 6152。
fn probe_local_now() -> Option<String> {
    const CANDIDATES: &[(&str, u16)] = &[
        ("http", 1082),
        ("http", 6152),
        ("http", 7890),
        ("socks5", 1080),
        ("socks5", 1081),
        ("socks5", 6153),
        ("socks5", 7891),
    ];
    let timeout = Duration::from_millis(400);
    let mut handles = Vec::new();
    for (scheme, port) in CANDIDATES {
        let scheme = *scheme;
        let port = *port;
        handles.push(std::thread::spawn(move || {
            let addr = SocketAddr::from(([127, 0, 0, 1], port));
            TcpStream::connect_timeout(&addr, timeout)
                .ok()
                .map(|_| format!("{scheme}://127.0.0.1:{port}"))
        }));
    }
    let mut found: Vec<(usize, String)> = Vec::new();
    for (i, h) in handles.into_iter().enumerate() {
        if let Ok(Some(url)) = h.join() {
            found.push((i, url));
        }
    }
    found.sort_by_key(|(i, _)| *i);
    found.into_iter().next().map(|(_, url)| url)
}

pub fn set_proxy(mode: &str, url: Option<&str>) -> Result<Value, String> {
    let mode = ProxyMode::parse(mode).ok_or("代理模式无效")?;
    let url = match mode {
        ProxyMode::Manual => parse_proxy_url(url.unwrap_or(""))?,
        ProxyMode::Auto | ProxyMode::Off => url
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(parse_proxy_url)
            .transpose()?
            .unwrap_or_default(),
    };
    let file = SETTINGS_FILE.lock().map_err(|_| "代理设置锁失败")?;
    let path = file.as_ref().ok_or("代理设置尚未初始化")?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(
        &temp,
        json!({ "mode": mode.as_str(), "url": url }).to_string(),
    )
    .and_then(|_| std::fs::rename(&temp, path))
    .map_err(|_| "无法保存代理设置，请检查应用数据目录")?;
    drop(file);
    if let Ok(mut s) = SETTINGS.lock() {
        s.mode = mode;
        s.url = url;
    }
    invalidate_probe_cache();
    Ok(status())
}

pub fn status() -> Value {
    let (mode, url) = {
        let settings = SETTINGS.lock().ok();
        (
            settings
                .as_ref()
                .map(|s| s.mode)
                .unwrap_or_else(default_mode),
            settings.as_ref().map(|s| s.url.clone()).unwrap_or_default(),
        )
    };
    let effective = effective_proxy();
    json!({
        "mode": mode.as_str(),
        "url": url,
        "effective": effective.as_ref().map(|(u, _)| redact_proxy(u)),
        "source": effective.as_ref().map(|(_, s)| *s),
        "default_mode": default_mode().as_str(),
        "env_override": env_override().is_some(),
    })
}

#[cfg(any(target_os = "ios", target_os = "macos"))]
fn cf_system_proxy() -> Option<String> {
    cf_proxy::system_proxy()
}

#[cfg(any(target_os = "ios", target_os = "macos"))]
mod cf_proxy {
    use std::ffi::{c_void, CStr};

    type CFTypeRef = *const c_void;
    type CFDictionaryRef = *const c_void;
    type CFStringRef = *const c_void;
    type CFNumberRef = *const c_void;

    #[link(name = "CoreFoundation", kind = "framework")]
    #[link(name = "CFNetwork", kind = "framework")]
    unsafe extern "C" {
        fn CFNetworkCopySystemProxySettings() -> CFDictionaryRef;
        fn CFDictionaryGetValue(dict: CFDictionaryRef, key: CFStringRef) -> CFTypeRef;
        fn CFRelease(cf: CFTypeRef);
        fn CFGetTypeID(cf: CFTypeRef) -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFNumberGetTypeID() -> usize;
        fn CFBooleanGetTypeID() -> usize;
        fn CFBooleanGetValue(boolean: CFTypeRef) -> u8;
        fn CFStringCreateWithCString(
            alloc: *const c_void,
            c_str: *const i8,
            encoding: u32,
        ) -> CFStringRef;
        fn CFStringGetCString(
            the_string: CFStringRef,
            buffer: *mut i8,
            buffer_size: isize,
            encoding: u32,
        ) -> u8;
        fn CFNumberGetValue(number: CFNumberRef, the_type: i32, value_ptr: *mut c_void) -> u8;
    }

    const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const CF_NUMBER_SINT32_TYPE: i32 = 3;

    // iOS 不导出 kCFNetworkProxiesHTTPS* / SOCKS* 符号，改用字符串键。
    unsafe fn cf_key(name: &CStr) -> CFStringRef {
        CFStringCreateWithCString(std::ptr::null(), name.as_ptr(), CF_STRING_ENCODING_UTF8)
    }

    unsafe fn cf_bool(dict: CFDictionaryRef, key: CFStringRef) -> bool {
        if dict.is_null() || key.is_null() {
            return false;
        }
        let val = unsafe { CFDictionaryGetValue(dict, key) };
        if val.is_null() {
            return false;
        }
        if unsafe { CFGetTypeID(val) } == unsafe { CFBooleanGetTypeID() } {
            return unsafe { CFBooleanGetValue(val) } != 0;
        }
        if unsafe { CFGetTypeID(val) } == unsafe { CFNumberGetTypeID() } {
            return cf_i32(val) != 0;
        }
        false
    }

    unsafe fn cf_i32(number: CFNumberRef) -> i32 {
        let mut out: i32 = 0;
        if unsafe {
            CFNumberGetValue(number, CF_NUMBER_SINT32_TYPE, &mut out as *mut i32 as *mut c_void)
        } != 0
        {
            out
        } else {
            0
        }
    }

    unsafe fn cf_string(val: CFTypeRef) -> Option<String> {
        if val.is_null() || unsafe { CFGetTypeID(val) } != unsafe { CFStringGetTypeID() } {
            return None;
        }
        let mut buf = [0i8; 256];
        if unsafe {
            CFStringGetCString(val, buf.as_mut_ptr(), buf.len() as isize, CF_STRING_ENCODING_UTF8)
        } == 0
        {
            return None;
        }
        let s = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
            .to_str()
            .ok()?
            .trim();
        if s.is_empty() { None } else { Some(s.to_string()) }
    }

    unsafe fn proxy_from(
        dict: CFDictionaryRef,
        enable: CFStringRef,
        host: CFStringRef,
        port: CFStringRef,
        scheme: &str,
    ) -> Option<String> {
        if enable.is_null() || host.is_null() || port.is_null() || !cf_bool(dict, enable) {
            return None;
        }
        let host_val = CFDictionaryGetValue(dict, host);
        let port_val = CFDictionaryGetValue(dict, port);
        let host = cf_string(host_val)?;
        let port = if port_val.is_null() {
            0
        } else if CFGetTypeID(port_val) == CFNumberGetTypeID() {
            cf_i32(port_val)
        } else {
            cf_string(port_val)?.parse().unwrap_or(0)
        };
        if port <= 0 || port > 65535 {
            return None;
        }
        let raw = format!("{scheme}://{host}:{port}");
        super::parse_proxy_url(&raw).ok()
    }

    unsafe fn proxy_from_names(
        dict: CFDictionaryRef,
        enable: &CStr,
        host: &CStr,
        port: &CStr,
        scheme: &str,
    ) -> Option<String> {
        let e = cf_key(enable);
        let h = cf_key(host);
        let p = cf_key(port);
        let found = proxy_from(dict, e, h, p, scheme);
        if !e.is_null() {
            CFRelease(e);
        }
        if !h.is_null() {
            CFRelease(h);
        }
        if !p.is_null() {
            CFRelease(p);
        }
        found
    }

    pub fn system_proxy() -> Option<String> {
        unsafe {
            let dict = CFNetworkCopySystemProxySettings();
            if dict.is_null() {
                return None;
            }
            let found = proxy_from_names(
                dict,
                CStr::from_bytes_with_nul_unchecked(b"HTTPEnable\0"),
                CStr::from_bytes_with_nul_unchecked(b"HTTPProxy\0"),
                CStr::from_bytes_with_nul_unchecked(b"HTTPPort\0"),
                "http",
            )
            .or_else(|| {
                proxy_from_names(
                    dict,
                    CStr::from_bytes_with_nul_unchecked(b"HTTPSEnable\0"),
                    CStr::from_bytes_with_nul_unchecked(b"HTTPSProxy\0"),
                    CStr::from_bytes_with_nul_unchecked(b"HTTPSPort\0"),
                    "http",
                )
            });
            CFRelease(dict);
            found
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_urls_are_validated() {
        assert!(parse_proxy_url("http://127.0.0.1:1082").is_ok());
        assert!(parse_proxy_url("socks5://127.0.0.1:1080").is_ok());
        assert!(parse_proxy_url("ftp://127.0.0.1:21").is_err());
        assert!(parse_proxy_url("file:///tmp/x").is_err());
        assert!(parse_proxy_url("http://127.0.0.1:1082\u{0007}").is_err());
        assert!(parse_proxy_url("").is_err());
        assert!(parse_proxy_url("not a url").is_err());
    }

    #[test]
    fn mode_aliases_parse() {
        assert_eq!(ProxyMode::parse("auto"), Some(ProxyMode::Auto));
        assert_eq!(ProxyMode::parse("OFF"), Some(ProxyMode::Off));
        assert_eq!(ProxyMode::parse("none"), Some(ProxyMode::Off));
        assert_eq!(ProxyMode::parse("manual"), Some(ProxyMode::Manual));
        assert_eq!(ProxyMode::parse("weird"), None);
    }
}
