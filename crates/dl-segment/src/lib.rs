//! 分段模型。不变量（DESIGN §5.3）：任意时刻
//! {已完成区间} ∪ {进行中区间（以连续写位置为界）} 构成 [0, total) 的不交覆盖。

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Segment {
    /// 含。
    pub start: u64,
    /// 不含。
    pub end: u64,
    /// 从 start 起连续已写字节。
    pub written: u64,
}

impl Segment {
    pub fn len(&self) -> u64 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.end == self.start
    }

    pub fn cursor(&self) -> u64 {
        self.start + self.written
    }

    pub fn done(&self) -> bool {
        self.written >= self.len()
    }

    pub fn remaining(&self) -> u64 {
        self.len().saturating_sub(self.written)
    }
}

/// 最小段长：≥256KB 的文件即可多线程（aria2 min-split-size 同款思路，取更激进值）
const MIN_SEG_BYTES: u64 = 256 * 1024;

/// 均匀切分 [0, total)；每段至少 1MiB（太小则收缩段数），单段文件自然只有一段。
pub fn plan(total: u64, parts: usize) -> Vec<Segment> {
    if total == 0 {
        return vec![Segment {
            start: 0,
            end: 0,
            written: 0,
        }];
    }
    let mut parts = parts.max(1).min(total as usize);
    while parts > 1 && (total / parts as u64) < MIN_SEG_BYTES {
        parts -= 1;
    }
    let chunk = total / parts as u64;
    (0..parts)
        .map(|i| {
            let start = i as u64 * chunk;
            let end = if i as u64 + 1 == parts as u64 {
                total
            } else {
                start + chunk
            };
            Segment {
                start,
                end,
                written: 0,
            }
        })
        .collect()
}

/// 覆盖完整性检查：排序后区间无缝无重叠覆盖 [0, total)，且全部写满。
/// 进入 Done 的强制前置条件（DESIGN §5.3 校验集）。
pub fn full_coverage(segs: &[Segment], total: u64) -> bool {
    if segs.is_empty() {
        return total == 0;
    }
    let mut sorted: Vec<&Segment> = segs.iter().collect();
    sorted.sort_by_key(|s| s.start);
    let mut cursor = 0u64;
    for s in sorted {
        if s.start != cursor || !s.done() {
            return false;
        }
        cursor = s.end;
    }
    cursor == total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_partitions_exactly() {
        for &(total, parts) in &[(100u64, 8usize), (1024 * 1024 * 100, 8), (7, 8), (0, 4)] {
            let segs = plan(total, parts);
            let covered: u64 = segs.iter().map(|s| s.len()).sum();
            assert_eq!(covered, total, "total={total} parts={parts}");
            let mut sorted = segs.clone();
            sorted.sort_by_key(|s| s.start);
            let mut cursor = 0;
            for s in &sorted {
                assert_eq!(s.start, cursor);
                cursor = s.end;
            }
            assert_eq!(cursor, total);
        }
    }

    #[test]
    fn plan_shrinks_for_small_files() {
        // 500KB：500/2=250KB < 256KB → 1 段
        assert_eq!(plan(500 * 1024, 8).len(), 1);
        // 1MB：每段 256KB 恰好达标 → 4 段
        assert_eq!(plan(1024 * 1024, 8).len(), 4);
        // 10MB / 4MB 均保持 8 段
        assert_eq!(plan(10 * 1024 * 1024, 8).len(), 8);
        assert_eq!(plan(4 * 1024 * 1024, 8).len(), 8);
    }

    #[test]
    fn full_coverage_detects_holes_and_overlap() {
        let done = |s: u64, e: u64| Segment {
            start: s,
            end: e,
            written: e - s,
        };
        assert!(full_coverage(&[done(0, 50), done(50, 100)], 100));
        assert!(!full_coverage(&[done(0, 50), done(60, 100)], 100)); // 空洞
        assert!(!full_coverage(&[done(0, 60), done(50, 100)], 100)); // 重叠
        assert!(!full_coverage(
            &[Segment {
                start: 0,
                end: 100,
                written: 99
            }],
            100
        )); // 未写满
        assert!(full_coverage(&[], 0));
    }
}
