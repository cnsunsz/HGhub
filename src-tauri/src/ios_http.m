#import <Foundation/Foundation.h>
#import <CFNetwork/CFNetwork.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>

@interface HgNoRedirect : NSObject <NSURLSessionTaskDelegate>
@end

@implementation HgNoRedirect
- (void)URLSession:(NSURLSession *)session
              task:(NSURLSessionTask *)task
willPerformHTTPRedirection:(NSHTTPURLResponse *)response
        newRequest:(NSURLRequest *)request
 completionHandler:(void (^)(NSURLRequest * _Nullable))completionHandler {
    (void)session;
    (void)task;
    (void)response;
    (void)request;
    // 不自动跟随，把 3xx 交回 Rust 按安全策略检查后再决定。
    completionHandler(nil);
}
@end

static char *hghub_dup_nsstring(NSString *s) {
    if (!s) {
        return NULL;
    }
    const char *utf8 = s.UTF8String;
    if (!utf8) {
        return NULL;
    }
    size_t n = strlen(utf8);
    char *out = (char *)malloc(n + 1);
    if (!out) {
        return NULL;
    }
    memcpy(out, utf8, n + 1);
    return out;
}

static NSString *hghub_ios_error(NSError *error) {
    if (!error) {
        return @"请求失败";
    }
    switch (error.code) {
        case NSURLErrorTimedOut:
            return @"请求超时";
        case NSURLErrorNotConnectedToInternet:
            return @"网络不可用";
        case NSURLErrorCannotFindHost:
        case NSURLErrorDNSLookupFailed:
            return @"DNS 解析失败";
        case NSURLErrorCannotConnectToHost:
        case NSURLErrorNetworkConnectionLost:
            return @"连接失败";
        case NSURLErrorSecureConnectionFailed:
        case NSURLErrorServerCertificateUntrusted:
        case NSURLErrorServerCertificateHasUnknownRoot:
            return @"HTTPS 证书校验失败";
        default:
            break;
    }
    // 310 = kCFErrorHTTPSProxyConnectionFailure，306 = kCFErrorHTTPProxyConnectionFailure
    if ([error.domain isEqualToString:(NSString *)kCFErrorDomainCFNetwork]
        && (error.code == 310 || error.code == 306)) {
        return @"系统 HTTPS 代理连接失败，请将本地代理保持关闭";
    }
    if (error.localizedDescription.length) {
        return error.localizedDescription;
    }
    return @"请求失败";
}

int32_t hghub_ios_http(const char *method,
                       const char *url,
                       const char *header_block,
                       const uint8_t *body,
                       size_t body_len,
                       const char *proxy_scheme,
                       const char *proxy_host,
                       int32_t proxy_port,
                       double timeout_sec,
                       int32_t *out_status,
                       char **out_headers,
                       uint8_t **out_body,
                       size_t *out_body_len,
                       char **out_error) {
    if (out_status) *out_status = 0;
    if (out_headers) *out_headers = NULL;
    if (out_body) *out_body = NULL;
    if (out_body_len) *out_body_len = 0;
    if (out_error) *out_error = NULL;

    if (!method || !url || !out_status || !out_headers || !out_body || !out_body_len || !out_error) {
        return -1;
    }

    @autoreleasepool {
        NSString *urlStr = [NSString stringWithUTF8String:url];
        NSURL *nsurl = urlStr ? [NSURL URLWithString:urlStr] : nil;
        if (!nsurl) {
            *out_error = hghub_dup_nsstring(@"目标地址无效");
            return -1;
        }

        NSMutableURLRequest *req = [NSMutableURLRequest requestWithURL:nsurl];
        req.HTTPMethod = [NSString stringWithUTF8String:method] ?: @"GET";
        req.timeoutInterval = timeout_sec > 0 ? timeout_sec : 20;
        req.cachePolicy = NSURLRequestReloadIgnoringLocalCacheData;
        req.HTTPShouldHandleCookies = NO;
        // 与官方安卓 reqwest（TCP HTTP/1.1 或 HTTP/2）对齐：不走 QUIC/HTTP3，
        // 小火箭等隧道对 UDP 支持较差时，HTTP3 会表现为「开 VPN 反而失败」。
        if (@available(iOS 14.0, *)) {
            req.assumesHTTP3Capable = NO;
        }

        if (header_block && header_block[0]) {
            NSString *block = [NSString stringWithUTF8String:header_block];
            for (NSString *line in [block componentsSeparatedByString:@"\r\n"]) {
                NSRange r = [line rangeOfString:@": "];
                if (r.location == NSNotFound) {
                    r = [line rangeOfString:@":"];
                }
                if (r.location == NSNotFound || r.location == 0) {
                    continue;
                }
                NSString *k = [[line substringToIndex:r.location]
                    stringByTrimmingCharactersInSet:NSCharacterSet.whitespaceCharacterSet];
                NSString *v = [[line substringFromIndex:NSMaxRange(r)]
                    stringByTrimmingCharactersInSet:NSCharacterSet.whitespaceCharacterSet];
                if (k.length) {
                    [req setValue:v forHTTPHeaderField:k];
                }
            }
        }

        if (body && body_len > 0) {
            req.HTTPBody = [NSData dataWithBytes:body length:body_len];
        }

        NSURLSessionConfiguration *cfg = [NSURLSessionConfiguration ephemeralSessionConfiguration];
        cfg.requestCachePolicy = NSURLRequestReloadIgnoringLocalCacheData;
        cfg.timeoutIntervalForRequest = req.timeoutInterval;
        cfg.timeoutIntervalForResource = req.timeoutInterval + 5;
        cfg.HTTPCookieStorage = nil;
        cfg.URLCache = nil;

        if (proxy_host && proxy_host[0] && proxy_port > 0 && proxy_port <= 65535) {
            NSString *host = [NSString stringWithUTF8String:proxy_host];
            NSNumber *port = @(proxy_port);
            BOOL socks = NO;
            if (proxy_scheme && proxy_scheme[0]) {
                NSString *scheme = [[NSString stringWithUTF8String:proxy_scheme] lowercaseString];
                socks = [scheme hasPrefix:@"socks"];
            }
            if (socks) {
                cfg.connectionProxyDictionary = @{
                    (NSString *)kCFStreamPropertySOCKSProxyHost: host,
                    (NSString *)kCFStreamPropertySOCKSProxyPort: port,
                    (NSString *)kCFStreamPropertySOCKSVersion: (NSString *)kCFStreamSocketSOCKSVersion5
                };
            } else {
                cfg.connectionProxyDictionary = @{
                    @"HTTPEnable": @YES,
                    @"HTTPProxy": host,
                    @"HTTPPort": port,
                    @"HTTPSEnable": @YES,
                    @"HTTPSProxy": host,
                    @"HTTPSPort": port
                };
            }
        } else {
            // 默认「关闭」必须显式关掉 CFNetwork 的 HTTP/HTTPS/SOCKS/PAC。
            // 只把 connectionProxyDictionary 留空时，URLSession 仍会跟随小火箭
            // VPN 下发的本地 HTTPS 代理。该代理不可用时就会报
            // kCFErrorHTTPSProxyConnectionFailure (310)。
            // 关掉后请求以普通 TCP 进入 Packet Tunnel，与官方安卓 VpnService 一致。
            cfg.connectionProxyDictionary = @{
                @"HTTPEnable": @NO,
                @"HTTPSEnable": @NO,
                @"SOCKSEnable": @NO,
                @"ProxyAutoConfigEnable": @NO
            };
        }

        static HgNoRedirect *redirects;
        static dispatch_once_t once;
        dispatch_once(&once, ^{
            redirects = [[HgNoRedirect alloc] init];
        });

        NSURLSession *session = [NSURLSession sessionWithConfiguration:cfg
                                                             delegate:redirects
                                                        delegateQueue:nil];

        __block NSHTTPURLResponse *httpResp = nil;
        __block NSData *data = nil;
        __block NSError *error = nil;
        dispatch_semaphore_t sem = dispatch_semaphore_create(0);

        NSURLSessionDataTask *task = [session dataTaskWithRequest:req
                                                completionHandler:^(NSData *d, NSURLResponse *r, NSError *e) {
            data = d;
            if ([r isKindOfClass:[NSHTTPURLResponse class]]) {
                httpResp = (NSHTTPURLResponse *)r;
            }
            error = e;
            dispatch_semaphore_signal(sem);
        }];
        [task resume];

        int64_t wait_ns = (int64_t)((req.timeoutInterval + 3) * (double)NSEC_PER_SEC);
        if (dispatch_semaphore_wait(sem, dispatch_time(DISPATCH_TIME_NOW, wait_ns)) != 0) {
            [task cancel];
            [session invalidateAndCancel];
            *out_error = hghub_dup_nsstring(@"请求超时");
            return -1;
        }
        [session finishTasksAndInvalidate];

        if (error && !httpResp) {
            *out_error = hghub_dup_nsstring(hghub_ios_error(error));
            return -1;
        }
        if (!httpResp) {
            *out_error = hghub_dup_nsstring(@"请求失败");
            return -1;
        }

        *out_status = (int32_t)httpResp.statusCode;

        NSMutableString *headerOut = [NSMutableString string];
        NSDictionary *fields = httpResp.allHeaderFields;
        [fields enumerateKeysAndObjectsUsingBlock:^(id key, id obj, BOOL *stop) {
            (void)stop;
            [headerOut appendFormat:@"%@: %@\r\n", key, obj];
        }];
        *out_headers = hghub_dup_nsstring(headerOut);

        if (data && data.length > 0) {
            uint8_t *buf = (uint8_t *)malloc(data.length);
            if (!buf) {
                *out_error = hghub_dup_nsstring(@"内存不足");
                return -1;
            }
            memcpy(buf, data.bytes, data.length);
            *out_body = buf;
            *out_body_len = (size_t)data.length;
        }
        return 0;
    }
}

void hghub_ios_http_free(void *ptr) {
    free(ptr);
}
