//! MITM 根证书与按 SNI 的叶子证书。
//!
//! 根证书生成一次存 `data_dir/sniff/`（PEM），由用户安装进 CurrentUser Root
//! 信任后，本代理可为解密白名单域名签发即时叶子证书。密钥文件仅本机落盘
//! （目录权限沿用数据目录），卸载证书 + 删除目录即完全撤销信任。

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use rcgen::{BasicConstraints, Certificate, CertificateParams, IsCa, KeyPair, KeyUsagePurpose};
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::ServerConfig;

pub const CA_CN: &str = "Anything Downloader Sniff CA";

pub struct SniffCa {
    cert: Certificate,
    key: KeyPair,
    leaf_cache: Mutex<HashMap<String, Arc<ServerConfig>>>,
}

impl SniffCa {
    /// 加载或生成 CA。cert PEM 供安装；key PEM 仅本地保存复用。
    pub fn load_or_create(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).context("create sniff ca dir")?;
        let cert_path = dir.join("ca-cert.pem");
        let key_path = dir.join("ca-key.pem");
        if cert_path.exists() && key_path.exists() {
            let cert_pem = std::fs::read_to_string(&cert_path)?;
            let key_pem = std::fs::read(&key_path)?;
            let key_der = rustls_pemfile::private_key(&mut key_pem.as_slice())?
                .ok_or_else(|| anyhow!("ca-key.pem 无私钥块"))?;
            let key = KeyPair::try_from(&key_der).map_err(|e| anyhow!("CA 密钥不可用: {e}"))?;
            // x509-parser 逆向出 params（含原始序列号/SKI，PreSpecified 保证
            // self_signed 逐位重建原证书），再由 key 确定性自签恢复对象
            let params = CertificateParams::from_ca_cert_pem(&cert_pem)
                .map_err(|e| anyhow!("CA 证书不可用: {e}"))?;
            let cert = params
                .self_signed(&key)
                .map_err(|e| anyhow!("CA 重建失败: {e}"))?;
            return Ok(Self { cert, key, leaf_cache: Mutex::new(HashMap::new()) });
        }

        let key = KeyPair::generate().context("generate CA key")?;
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::DigitalSignature];
        let mut dn = rcgen::DistinguishedName::new();
        dn.push(rcgen::DnType::CommonName, CA_CN);
        dn.push(rcgen::DnType::OrganizationName, "Anything Downloader");
        params.distinguished_name = dn;
        let cert = params
            .self_signed(&key)
            .map_err(|e| anyhow!("自签 CA: {e}"))?;
        std::fs::write(&cert_path, cert.pem())?;
        std::fs::write(&key_path, key.serialize_pem())?;
        tracing::info!(?cert_path, "sniff CA 生成");
        Ok(Self { cert, key, leaf_cache: Mutex::new(HashMap::new()) })
    }

    /// CA 证书 PEM（安装到系统信任用）
    pub fn cert_pem(&self) -> String {
        self.cert.pem()
    }

    /// 为 SNI 签发（缓存的）rustls ServerConfig。
    pub fn leaf_config(&self, sni: &str) -> Result<Arc<ServerConfig>> {
        if let Some(cfg) = self.leaf_cache.lock().unwrap().get(sni) {
            return Ok(cfg.clone());
        }
        let leaf_key = KeyPair::generate().context("generate leaf key")?;
        let mut params = CertificateParams::new(vec![sni.to_string()])?;
        let mut dn = rcgen::DistinguishedName::new();
        dn.push(rcgen::DnType::CommonName, sni.to_string());
        params.distinguished_name = dn;
        let cert = params
            .signed_by(&leaf_key, &self.cert, &self.key)
            .map_err(|e| anyhow!("签发叶子证书: {e}"))?;
        let key = PrivateKeyDer::Pkcs8(leaf_key.serialize_der().into());
        let cfg = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![CertificateDer::from(cert.der().to_vec())], key)
            .map_err(|e| anyhow!("leaf ServerConfig: {e}"))?;
        let cfg = Arc::new(cfg);
        self.leaf_cache.lock().unwrap().insert(sni.to_string(), cfg.clone());
        Ok(cfg)
    }
}
