//! iOS 用系统 URLSession 发请求，这样 Shadowrocket 的 VPN/DNS 能接管流量，
//! 行为与 Safari、系统应用以及安卓 VpnService 一致。
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::time::Duration;

pub struct RawResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

unsafe extern "C" {
    fn hghub_ios_http(
        method: *const c_char,
        url: *const c_char,
        header_block: *const c_char,
        body: *const u8,
        body_len: usize,
        proxy_scheme: *const c_char,
        proxy_host: *const c_char,
        proxy_port: c_int,
        timeout_sec: f64,
        out_status: *mut c_int,
        out_headers: *mut *mut c_char,
        out_body: *mut *mut u8,
        out_body_len: *mut usize,
        out_error: *mut *mut c_char,
    ) -> c_int;
    fn hghub_ios_http_free(ptr: *mut c_void);
}

fn parse_proxy(raw: Option<&str>) -> (CString, CString, c_int) {
    let empty = || CString::new("").unwrap();
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return (empty(), empty(), 0);
    };
    let Ok(url) = url::Url::parse(raw) else {
        return (empty(), empty(), 0);
    };
    let scheme = url.scheme().to_string();
    let Some(host) = url.host_str().map(str::to_string) else {
        return (empty(), empty(), 0);
    };
    let default_port = match scheme.as_str() {
        "https" => 443,
        "socks" | "socks5" | "socks5h" => 1080,
        _ => 80,
    };
    let port = url.port().unwrap_or(default_port) as c_int;
    (
        CString::new(scheme).unwrap_or_else(|_| empty()),
        CString::new(host).unwrap_or_else(|_| empty()),
        port,
    )
}

fn take_cstr(ptr: *mut c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let s = unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned();
    unsafe { hghub_ios_http_free(ptr as *mut c_void) };
    s
}

fn take_bytes(ptr: *mut u8, len: usize) -> Vec<u8> {
    if ptr.is_null() || len == 0 {
        if !ptr.is_null() {
            unsafe { hghub_ios_http_free(ptr as *mut c_void) };
        }
        return Vec::new();
    }
    let buf = unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec();
    unsafe { hghub_ios_http_free(ptr as *mut c_void) };
    buf
}

fn parse_header_block(block: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in block.split("\r\n") {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim();
            if !k.is_empty() {
                out.push((k.to_string(), v.trim().to_string()));
            }
        }
    }
    out
}

pub async fn send(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: Option<&[u8]>,
    timeout: Duration,
    proxy: Option<&str>,
) -> Result<RawResponse, String> {
    let method = method.to_string();
    let url = url.to_string();
    let header_block = headers
        .iter()
        .map(|(k, v)| format!("{k}: {v}"))
        .collect::<Vec<_>>()
        .join("\r\n");
    let body = body.map(|b| b.to_vec());
    let proxy = proxy.map(str::to_string);
    tokio::task::spawn_blocking(move || {
        let method = CString::new(method).map_err(|_| "请求方法无效")?;
        let url = CString::new(url).map_err(|_| "目标地址含非法字符")?;
        let header_block = CString::new(header_block).map_err(|_| "请求头含非法字符")?;
        let (proxy_scheme, proxy_host, proxy_port) = parse_proxy(proxy.as_deref());
        let body_ptr = body.as_ref().map(|b| b.as_ptr()).unwrap_or(std::ptr::null());
        let body_len = body.as_ref().map(|b| b.len()).unwrap_or(0);
        let mut status: c_int = 0;
        let mut headers_ptr: *mut c_char = std::ptr::null_mut();
        let mut body_out: *mut u8 = std::ptr::null_mut();
        let mut body_out_len: usize = 0;
        let mut error_ptr: *mut c_char = std::ptr::null_mut();
        let rc = unsafe {
            hghub_ios_http(
                method.as_ptr(),
                url.as_ptr(),
                header_block.as_ptr(),
                body_ptr,
                body_len,
                proxy_scheme.as_ptr(),
                proxy_host.as_ptr(),
                proxy_port,
                timeout.as_secs_f64().max(1.0),
                &mut status,
                &mut headers_ptr,
                &mut body_out,
                &mut body_out_len,
                &mut error_ptr,
            )
        };
        if rc != 0 {
            let err = take_cstr(error_ptr);
            let _ = take_cstr(headers_ptr);
            let _ = take_bytes(body_out, body_out_len);
            return Err(if err.is_empty() {
                "请求失败".into()
            } else {
                err
            });
        }
        let _ = take_cstr(error_ptr);
        let header_text = take_cstr(headers_ptr);
        let body = take_bytes(body_out, body_out_len);
        let status = u16::try_from(status).unwrap_or(0);
        if status == 0 {
            return Err("请求失败".into());
        }
        Ok(RawResponse {
            status,
            headers: parse_header_block(&header_text),
            body,
        })
    })
    .await
    .map_err(|_| "请求线程异常".to_string())?
}
