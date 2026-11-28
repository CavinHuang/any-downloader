//! HTTP 能力域：客户端构建（identity 编码 + 代理）与 URL 探测决策表。

pub mod client;
pub mod diag;
pub mod interstitial;
pub mod page_sniff;
pub mod probe;

pub use client::{
    apply_proxy, build_client, build_pooled_worker_client, build_worker_client, classify_status,
    detect_system_proxy, identity_headers, is_direct_proxy, map_reqwest_err, prefers_own_ua,
    remember_own_ua, retry_after_secs, DIRECT_PROXY, OWN_USER_AGENT, USER_AGENT,
};
pub use probe::{probe as probe_url, ProbeResult};
