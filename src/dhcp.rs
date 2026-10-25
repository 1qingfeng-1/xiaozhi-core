//! 极简 DHCP 服务器响应构造 — 纯逻辑, host 可单测
//!
//! 固定租约 192.168.4.2 / 网关 192.168.4.1; 只处理 DISCOVER→OFFER 与
//! REQUEST→ACK。BOOTP 最小包长 300B, 不足用 padding option 补齐。

/// 固定客户端地址 / AP 地址
const CLIENT_IP: [u8; 4] = [192, 168, 4, 2];
const AP_IP: [u8; 4] = [192, 168, 4, 1];

/// 解析 DHCP 请求并构造 OFFER/ACK; 非 BOOTREQUEST/未知类型返回 None
pub fn build_reply(req: &[u8]) -> Option<heapless::Vec<u8, 320>> {
    if req.len() < 240 || req[0] != 1 {
        return None; // 非 BOOTREQUEST
    }
    let msg_type = option(req, 53)?;
    let reply_type: u8 = match msg_type {
        1 => 2, // DISCOVER → OFFER
        3 => 5, // REQUEST → ACK
        _ => return None,
    };

    let mut out: heapless::Vec<u8, 320> = heapless::Vec::new();
    out.extend_from_slice(&[2, 1, 6, 0]).ok()?; // op=BOOTREPLY, htype, hlen, hops
    out.extend_from_slice(&req[4..8]).ok()?; // xid
    out.extend_from_slice(&[0, 0, 0, 0]).ok()?; // secs(2) + flags(2)
    out.extend_from_slice(&req[12..16]).ok()?; // ciaddr
    out.extend_from_slice(&CLIENT_IP).ok()?; // yiaddr
    out.extend_from_slice(&AP_IP).ok()?; // siaddr
    out.extend_from_slice(&[0; 4]).ok()?; // giaddr
    out.extend_from_slice(&req[28..44]).ok()?; // chaddr
    out.extend_from_slice(&[0; 64]).ok()?; // sname
    out.extend_from_slice(&[0; 128]).ok()?; // file
    out.extend_from_slice(&[0x63, 0x82, 0x53, 0x63]).ok()?; // magic cookie

    let mut opt = |code: u8, data: &[u8]| -> Option<()> {
        out.extend_from_slice(&[code, data.len() as u8]).ok()?;
        out.extend_from_slice(data).ok()
    };
    opt(53, &[reply_type])?;
    opt(54, &AP_IP)?; // server identifier
    opt(51, &[0xFF, 0xFF, 0xFF, 0xFF])?; // lease: 无限
    opt(1, &[255, 255, 255, 0])?; // subnet mask
    opt(3, &AP_IP)?; // router
    // BOOTP 最小包长 300B: 不足则用 padding option (0) 填充
    while out.len() < 299 {
        out.extend_from_slice(&[0]).ok()?;
    }
    out.extend_from_slice(&[255]).ok()?; // end
    Some(out)
}

/// 在 DHCP options 区查找指定 option 的值
fn option(req: &[u8], code: u8) -> Option<u8> {
    let mut i = 240;
    while i + 2 <= req.len() {
        let c = req[i];
        if c == 255 {
            return None;
        }
        let len = req[i + 1] as usize;
        if c == code {
            return req.get(i + 2).copied();
        }
        i += 2 + len;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(msg_type: u8) -> [u8; 300] {
        let mut req = [0u8; 300];
        req[0] = 1; // BOOTREQUEST
        req[1] = 1;
        req[2] = 6;
        req[4..8].copy_from_slice(&0x1234_5678u32.to_be_bytes()); // xid
        req[28..34].copy_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]); // chaddr
        req[240] = 53;
        req[241] = 1;
        req[242] = msg_type;
        req[243] = 255; // end
        req
    }

    #[test]
    fn discover_offer_and_request_ack() {
        let offer = build_reply(&request(1)).unwrap();
        assert_eq!(offer[0], 2); // BOOTREPLY
        assert_eq!(&offer[4..8], &0x1234_5678u32.to_be_bytes());
        assert_eq!(&offer[16..20], &CLIENT_IP);
        assert_eq!(&offer[20..24], &AP_IP);
        assert_eq!(&offer[28..34], &[0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
        assert_eq!(offer[242], 2); // OFFER
        assert!(offer.len() >= 300);
        assert_eq!(*offer.last().unwrap(), 255);

        let ack = build_reply(&request(3)).unwrap();
        assert_eq!(ack[242], 5); // ACK
    }

    #[test]
    fn rejects_bad_requests() {
        assert!(build_reply(&[0u8; 239]).is_none()); // 太短
        let mut bad = request(1);
        bad[0] = 2; // BOOTREPLY, 非请求
        assert!(build_reply(&bad).is_none());
        assert!(build_reply(&request(2)).is_none()); // 未支持的 DHCP 类型
        let mut no_opt = request(1);
        no_opt[240] = 255; // 没有 option 53
        assert!(build_reply(&no_opt).is_none());
    }

    #[test]
    fn option_parser_walks_variable_lengths() {
        let mut req = [0u8; 260];
        // 跳过两个无关 option, 再放 option 53
        req[240] = 12;
        req[241] = 2;
        req[242] = 1;
        req[243] = 2;
        req[244] = 53;
        req[245] = 1;
        req[246] = 7;
        req[247] = 255;
        assert_eq!(option(&req, 53), Some(7));
        assert_eq!(option(&req, 99), None);
    }
}
