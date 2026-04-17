/// Jump consistent hash — assigns a doc_id to a shard uniformly.
/// From "A Fast, Minimal Memory, Consistent Hash Algorithm" by Lamping & Veach.
pub fn jump_consistent_hash(key: u64, num_buckets: u32) -> u32 {
    let mut k = key;
    let mut b: i64 = -1;
    let mut j: i64 = 0;
    while j < num_buckets as i64 {
        b = j;
        k = k.wrapping_mul(2862933555777941757).wrapping_add(1);
        j = ((b + 1) as f64 * (1u64 << 31) as f64
            / ((k >> 33) + 1) as f64) as i64;
    }
    b as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_deterministic() {
        assert_eq!(jump_consistent_hash(42, 4), jump_consistent_hash(42, 4));
    }

    #[test]
    fn test_within_range() {
        for i in 0..1000u64 {
            let shard = jump_consistent_hash(i, 4);
            assert!(shard < 4, "shard={shard}");
        }
    }

    #[test]
    fn test_uniform_distribution() {
        let num_shards = 4u32;
        let mut counts: HashMap<u32, u32> = HashMap::new();
        let n = 10_000u64;
        for i in 0..n {
            *counts.entry(jump_consistent_hash(i, num_shards)).or_default() += 1;
        }
        let expected = n as f64 / num_shards as f64;
        for (shard, count) in &counts {
            let deviation = (*count as f64 - expected).abs() / expected;
            assert!(deviation < 0.05, "shard {shard} count {count} deviates {deviation:.2} from expected {expected}");
        }
    }

    #[test]
    fn test_single_shard() {
        for i in 0..100u64 {
            assert_eq!(jump_consistent_hash(i, 1), 0);
        }
    }
}
