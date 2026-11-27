//! 每主机连接数上限自学习（参照 MisterDownloadManager hosts.rs）：
//! 当某主机在仍有其他连接正在接收数据时拒绝额外连接，记录并收敛其允许的最大并发数；
//! 避免额外连接空耗重试或触发封禁。超过 `DECAY` 无异常则自动遗忘恢复探测。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 学习到的上限若超过此时长未再触发，则遗忘以重新探测网络潜力。
pub const DECAY: Duration = Duration::from_secs(10 * 60);
/// 连续观察到至少这么多次拒绝后再应用上限（避免单次网络抖动误判）。
const NEEDED: u32 = 2;

#[derive(Debug, Default)]
pub struct HostLimits {
    hosts: Mutex<HashMap<String, Learned>>,
}

#[derive(Debug, Clone, Copy)]
struct Learned {
    cap: usize,
    seen: u32,
    at: Instant,
}

impl HostLimits {
    /// 查询主机当前已生效的并发连接上限（若未达到学习阈值或已过期则返回 None）。
    pub fn cap(&self, host: &str) -> Option<usize> {
        if host.is_empty() {
            return None;
        }
        let mut hosts = self.hosts.lock().unwrap();
        match hosts.get(host) {
            Some(l) if l.at.elapsed() >= DECAY => {
                hosts.remove(host);
                None
            }
            Some(l) if l.seen >= NEEDED => Some(l.cap),
            _ => None,
        }
    }

    /// 当向 `host` 发起的额外连接被拒绝（同时已有 `working` 个其他连接在接收数据）时记录。
    /// 若已达到观察阈值，返回应施加的并发上限。
    pub fn refused(&self, host: &str, working: usize) -> Option<usize> {
        if host.is_empty() {
            return None;
        }
        let working = working.max(1);
        let mut hosts = self.hosts.lock().unwrap();
        let l = hosts.entry(host.to_ascii_lowercase()).or_insert(Learned {
            cap: working,
            seen: 0,
            at: Instant::now(),
        });
        if l.at.elapsed() >= DECAY {
            *l = Learned {
                cap: working,
                seen: 0,
                at: Instant::now(),
            };
        }
        l.cap = l.cap.max(working);
        l.seen += 1;
        l.at = Instant::now();
        (l.seen >= NEEDED).then_some(l.cap)
    }

    /// 实际观察到有 `working` 个连接同时顺利接收数据：若此前学到的上限偏低，向上修正。
    pub fn working(&self, host: &str, working: usize) {
        if host.is_empty() {
            return;
        }
        let mut hosts = self.hosts.lock().unwrap();
        if let Some(l) = hosts.get_mut(&host.to_ascii_lowercase()) {
            if working > l.cap {
                l.cap = working;
            }
        }
    }

    pub fn learned(&self) -> Vec<(String, usize)> {
        let hosts = self.hosts.lock().unwrap();
        let mut v: Vec<(String, usize)> = hosts
            .iter()
            .filter(|(_, l)| l.seen >= NEEDED && l.at.elapsed() < DECAY)
            .map(|(h, l)| (h.clone(), l.cap))
            .collect();
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_teach_the_most_that_worked() {
        let h = HostLimits::default();
        assert_eq!(h.refused("x.org", 1), None, "one refusal can be a hiccup");
        assert_eq!(h.cap("x.org"), None);
        assert_eq!(h.refused("x.org", 2), Some(2));
        assert_eq!(h.refused("x.org", 1), Some(2));
        assert_eq!(h.cap("x.org"), Some(2));
        h.working("x.org", 3);
        h.working("x.org", 1);
        assert_eq!(h.cap("x.org"), Some(3));
        h.working("y.org", 5);
        assert_eq!(h.cap("y.org"), None, "nothing refused there: no cap");
        assert_eq!(h.cap("other.org"), None);
        assert_eq!(h.learned(), vec![("x.org".to_owned(), 3)]);
    }
}
