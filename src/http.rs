//! 极简 HTTP 响应/URL 解析 — OTA 下载用, host 可单测
//!
//! 只覆盖设备侧真正需要的子集: 状态行、`Content-Length`、响应拆分、
//! `http://host[:port]/path`。不处理 chunked/重定向/TLS。

/// 从响应中拆出 (状态码, body); 无完整头部返回 None
pub fn split_response(resp: &str) -> Option<(u16, &str)> {
    let header_end = resp.find("\r\n\r\n")?;
    let status = parse_status(&resp[..header_end + 4])?;
    Some((status, &resp[header_end + 4..]))
}

/// 从状态行取状态码 (`HTTP/1.x 200 ...`)
pub fn parse_status(head: &str) -> Option<u16> {
    let line_end = head.find("\r\n").unwrap_or(head.len());
    let line = &head[..line_end];
    let mut parts = line.split_whitespace();
    let proto = parts.next()?;
    if !proto.starts_with("HTTP/") {
        return None;
    }
    parts.next()?.parse().ok()
}

/// 从响应头取 `Content-Length` (行首、大小写不敏感; 缺省 None)
///
/// 只在真正的头字段行匹配, 不会被 `X-Foo: content-length: 123` 误命中。
pub fn parse_content_length(head: &str) -> Option<u32> {
    head.lines().find_map(|line| {
        let (k, v) = line.split_once(':')?;
        if k.trim().eq_ignore_ascii_case("content-length") {
            v.trim().parse().ok()
        } else {
            None
        }
    })
}

/// 从 HTTP 响应头取字段值 (大小写不敏感; 跳过状态行)
pub fn header_value<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().skip(1).find_map(|line| {
        let (k, v) = line.split_once(':')?;
        if k.trim().eq_ignore_ascii_case(name) {
            Some(v.trim())
        } else {
            None
        }
    })
}

/// 标准 base64 (带填充); 输出容量不足时截断
pub fn base64_encode(input: &[u8], out: &mut heapless::String<32>) {
    const TBL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let _ = out.push(TBL[(n >> 18) as usize & 63] as char);
        let _ = out.push(TBL[(n >> 12) as usize & 63] as char);
        let _ = out.push(if chunk.len() > 1 {
            TBL[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        let _ = out.push(if chunk.len() > 2 {
            TBL[n as usize & 63] as char
        } else {
            '='
        });
    }
}

/// WebSocket 握手期望的 `Sec-WebSocket-Accept`
///
/// RFC 6455: base64(SHA1(key || "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"))
pub fn websocket_accept(key: &str) -> heapless::String<32> {
    let mut src = heapless::String::<64>::new();
    let _ = src.push_str(key);
    let _ = src.push_str("258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    let digest = crate::sha1::sha1(src.as_bytes());
    let mut out = heapless::String::<32>::new();
    base64_encode(&digest, &mut out);
    out
}

/// URL/form 百分号解码 (`+` → 空格, `%XX` → 字节), 最后统一做 UTF-8 校验
///
/// 逐字节解码后再校验, 避免把中文 UTF-8 多字节序列拆成 Latin-1。
pub fn url_decode<const N: usize>(s: &str) -> heapless::String<N> {
    let mut bytes: heapless::Vec<u8, N> = heapless::Vec::new();
    let src = s.as_bytes();
    let mut i = 0usize;
    while i < src.len() && !bytes.is_full() {
        match src[i] {
            b'+' => {
                let _ = bytes.push(b' ');
                i += 1;
            },
            b'%' if i + 2 < src.len() => match (hex_val(src[i + 1]), hex_val(src[i + 2])) {
                (Some(hi), Some(lo)) => {
                    let _ = bytes.push((hi << 4) | lo);
                    i += 3;
                },
                _ => {
                    let _ = bytes.push(src[i]);
                    i += 1;
                },
            },
            c => {
                let _ = bytes.push(c);
                i += 1;
            },
        }
    }
    core::str::from_utf8(&bytes)
        .ok()
        .and_then(|s| heapless::String::try_from(s).ok())
        .unwrap_or_default()
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// 解析 `host:port` (MQTT endpoint); 端口必填且 host 非空
pub fn parse_host_port(s: &str) -> Option<(&str, u16)> {
    let (host, port) = s.rsplit_once(':')?;
    let port = port.parse().ok()?;
    if host.is_empty() {
        return None;
    }
    Some((host, port))
}

/// 从 url 取 (host, 端口, 路径): 端口缺省 8003, 路径为 host 之后到末尾
pub fn parse_url(url: &str) -> (&str, u16, &str) {
    let after = url.find("://").map(|i| &url[i + 3..]).unwrap_or(url);
    let (hostport, path) = match after.find('/') {
        Some(i) => (&after[..i], &after[i..]),
        None => (after, "/"),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => match p.parse::<u16>() {
            Ok(port) => (h, port),
            Err(_) => (hostport, 8003),
        },
        None => (hostport, 8003),
    };
    (host, port, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_lines() {
        assert_eq!(parse_status("HTTP/1.1 200 OK\r\n"), Some(200));
        assert_eq!(parse_status("HTTP/1.0 404 Not Found\r\n"), Some(404));
        assert_eq!(parse_status("ICY 200 OK\r\n"), None);
        assert_eq!(parse_status("HTTP/1.1 abc\r\n"), None);
    }

    #[test]
    fn splits_headers_and_body() {
        let resp = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}";
        let (status, body) = split_response(resp).unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, "{}");
        assert!(split_response("no header").is_none());
    }

    #[test]
    fn content_length_is_line_scoped_and_case_insensitive() {
        assert_eq!(parse_content_length("Content-Length: 123\r\n"), Some(123));
        assert_eq!(parse_content_length("content-length:456\r\n"), Some(456));
        assert_eq!(
            parse_content_length("X-Foo: content-length: 999\r\nContent-Length: 12\r\n"),
            Some(12)
        );
        assert_eq!(parse_content_length("Content-Length: abc\r\n"), None);
        assert_eq!(parse_content_length(""), None);
    }

    #[test]
    fn websocket_accept_matches_rfc6455_vector() {
        // RFC 6455 §1.3 示例
        assert_eq!(
            websocket_accept("dGhlIHNhbXBsZSBub25jZQ==").as_str(),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn header_value_is_case_insensitive_and_skips_status() {
        let head = "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nsec-websocket-accept: abc=\r\n\r\n";
        assert_eq!(header_value(head, "Sec-WebSocket-Accept"), Some("abc="));
        assert_eq!(header_value(head, "upgrade"), Some("websocket"));
        assert_eq!(header_value(head, "missing"), None);
        // 状态行里的 "101" 不能被当成头
        assert_eq!(header_value(head, "101"), None);
    }

    #[test]
    fn url_decode_handles_plus_percent_and_utf8() {
        assert_eq!(url_decode::<64>("hello+world").as_str(), "hello world");
        assert_eq!(url_decode::<64>("%41%42%43").as_str(), "ABC");
        // 中文 UTF-8 必须整体解码, 不能拆成 Latin-1
        assert_eq!(url_decode::<64>("%E4%B8%AD").as_str(), "中");
        // 非法百分号按字面保留
        assert_eq!(url_decode::<64>("a%zzb").as_str(), "a%zzb");
        // 末尾不完整的 % 也保留
        assert_eq!(url_decode::<64>("a%4").as_str(), "a%4");
    }

    #[test]
    fn url_decode_is_bounded() {
        // 容量不足时截断; 若截断点落在 UTF-8 中间则返回空串
        assert_eq!(url_decode::<2>("abcdef").as_str(), "ab");
        assert_eq!(url_decode::<2>("%E4%B8%AD").as_str(), "");
        assert_eq!(url_decode::<3>("abc").as_str(), "abc");
    }

    #[test]
    fn parses_host_port() {
        assert_eq!(
            parse_host_port("192.168.1.2:1883"),
            Some(("192.168.1.2", 1883))
        );
        assert_eq!(parse_host_port("gw.local:8884"), Some(("gw.local", 8884)));
        assert_eq!(parse_host_port("host"), None);
        assert_eq!(parse_host_port("host:abc"), None);
        assert_eq!(parse_host_port(":1883"), None);
        assert_eq!(parse_host_port("host:70000"), None); // 端口越界
    }

    #[test]
    fn parses_urls() {
        assert_eq!(
            parse_url("http://192.168.1.7:8003/fw/x.bin"),
            ("192.168.1.7", 8003, "/fw/x.bin")
        );
        assert_eq!(
            parse_url("http://example.com/fw.bin"),
            ("example.com", 8003, "/fw.bin")
        );
        assert_eq!(
            parse_url("https://example.com:8443/a"),
            ("example.com", 8443, "/a")
        );
        assert_eq!(parse_url("192.168.1.7:9000"), ("192.168.1.7", 9000, "/"));
        assert_eq!(parse_url("http://example.com"), ("example.com", 8003, "/"));
    }
}
