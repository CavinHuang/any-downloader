//! TLS ClientHello 的 SNI 解析（无外部依赖的最小实现）。
//!
//! 代理在 CONNECT 隧道建立后、决定「解密 or 透传」前，需要在不完成握手的
//! 情况下读取客户端首个 ClientHello 里的 server_name。只解析到 SNI 扩展为止，
//! 不校验密码套件等其余内容。

/// 从缓冲区解析 SNI 主机名。返回 None 表示数据不足或不是 ClientHello
/// （调用方可继续读取后重试）。
pub fn parse_sni(buf: &[u8]) -> Option<String> {
    // TLSPlaintext: type(1)=22 handshake, version(2), length(2)
    if buf.len() < 5 || buf[0] != 0x16 {
        return None;
    }
    let rec_len = u16::from_be_bytes([buf[3], buf[4]]) as usize;
    if buf.len() < 5 + rec_len {
        return None; // 记录未读全
    }
    let hs = &buf[5..5 + rec_len];
    // Handshake: msg_type(1)=0x01 ClientHello, length(3)
    if hs.len() < 4 || hs[0] != 0x01 {
        return None;
    }
    let body = &hs[4..];

    struct Cur<'a>(&'a [u8]);
    impl<'a> Cur<'a> {
        fn take(&mut self, n: usize) -> Option<&'a [u8]> {
            if self.0.len() < n {
                return None;
            }
            let (h, t) = self.0.split_at(n);
            self.0 = t;
            Some(h)
        }
        fn u8(&mut self) -> Option<u8> {
            Some(self.take(1)?[0])
        }
        fn u16(&mut self) -> Option<u16> {
            let b = self.take(2)?;
            Some(u16::from_be_bytes([b[0], b[1]]))
        }
    }

    let mut c = Cur(body);
    c.take(2)?; // client_version
    c.take(32)?; // random
    let sid_len = c.u8()? as usize;
    c.take(sid_len)?; // session_id
    let cs_len = c.u16()? as usize;
    c.take(cs_len)?; // cipher_suites
    let cm_len = c.u8()? as usize;
    c.take(cm_len)?; // compression_methods
    let ext_len = c.u16()? as usize;
    let mut ext = Cur(c.take(ext_len)?);

    while ext.0.len() >= 4 {
        let etype = ext.u16()?;
        let elen = ext.u16()? as usize;
        let data = ext.take(elen)?;
        if etype != 0x0000 {
            continue; // server_name
        }
        // ServerNameList: list_length(2), entry: type(1)=0 host_name, length(2), bytes
        if data.len() < 2 {
            continue;
        }
        let list_len = u16::from_be_bytes([data[0], data[1]]) as usize;
        let list = &data[2..data.len().min(2 + list_len)];
        let mut lc = Cur(list);
        let ntype = lc.u8()?;
        let nlen = lc.u16()? as usize;
        let name = lc.take(nlen)?;
        if ntype == 0 {
            return Some(String::from_utf8_lossy(name).into_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个最小合法 ClientHello（无密码学意义，只为走通解析）。
    fn client_hello(sni: Option<&str>) -> Vec<u8> {
        let mut ext = Vec::new();
        if let Some(name) = sni {
            let mut entry = vec![0u8];
            entry.extend_from_slice(&(name.len() as u16).to_be_bytes());
            entry.extend_from_slice(name.as_bytes());
            let mut list = (entry.len() as u16).to_be_bytes().to_vec();
            list.extend_from_slice(&entry);
            ext.extend_from_slice(&0x0000u16.to_be_bytes());
            ext.extend_from_slice(&(list.len() as u16).to_be_bytes());
            ext.extend_from_slice(&list);
        }
        // 支持组扩展凑第二个扩展，验证遍历
        ext.extend_from_slice(&0x000au16.to_be_bytes());
        ext.extend_from_slice(&4u16.to_be_bytes());
        ext.extend_from_slice(&[0x00, 0x02, 0x00, 0x17]);

        let mut body = Vec::new();
        body.extend_from_slice(&[0x03, 0x03]); // client_version TLS1.2
        body.extend_from_slice(&[0x42u8; 32]); // random
        body.push(0); // session_id
        body.extend_from_slice(&[0x00, 0x02, 0x13, 0x01]); // 1 cipher
        body.push(1); // compression
        body.push(0); // null
        body.extend_from_slice(&(ext.len() as u16).to_be_bytes());
        body.extend_from_slice(&ext);

        let mut hs = vec![0x01];
        let l = body.len() as u32;
        hs.extend_from_slice(&l.to_be_bytes()[1..4]);
        hs.extend_from_slice(&body);

        let mut rec = vec![0x16, 0x03, 0x01];
        rec.extend_from_slice(&(hs.len() as u16).to_be_bytes());
        rec.extend_from_slice(&hs);
        rec
    }

    #[test]
    fn parses_sni() {
        let b = client_hello(Some("finder.video.qq.com"));
        assert_eq!(parse_sni(&b).as_deref(), Some("finder.video.qq.com"));
    }

    #[test]
    fn no_sni_returns_none() {
        let b = client_hello(None);
        assert_eq!(parse_sni(&b), None);
    }

    #[test]
    fn incomplete_returns_none() {
        let b = client_hello(Some("a.com"));
        assert_eq!(parse_sni(&b[..b.len() - 5]), None);
        assert_eq!(parse_sni(&[]), None);
        assert_eq!(parse_sni(&[0x17, 0x03, 0x03, 0x00, 0x10]), None); // 非 handshake 记录
    }
}
