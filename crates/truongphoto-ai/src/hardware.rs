use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};
use truongphoto_core::{AiTier, HardwareInfo};

const GIB: u64 = 1024 * 1024 * 1024;

/// Maps total system RAM bytes to recommended `(vision_tier, chat_tier)`:
/// - `< 16 GiB` -> `(Tier1, Tier1)`
/// - `16 GiB .. < 32 GiB` -> `(Tier2, Tier2)`
/// - `>= 32 GiB` -> `(Tier3, Tier3)`
pub const fn recommend_tiers_for_ram(total_ram_bytes: u64) -> (AiTier, AiTier) {
    if total_ram_bytes < 16 * GIB {
        (AiTier::Tier1, AiTier::Tier1)
    } else if total_ram_bytes < 32 * GIB {
        (AiTier::Tier2, AiTier::Tier2)
    } else {
        (AiTier::Tier3, AiTier::Tier3)
    }
}

/// Detects host RAM and CPU core count via `sysinfo` and computes recommended AI tiers.
pub fn detect_hardware_info() -> HardwareInfo {
    let sys = System::new_with_specifics(
        RefreshKind::nothing()
            .with_memory(MemoryRefreshKind::everything())
            .with_cpu(CpuRefreshKind::nothing()),
    );

    let total_ram_bytes = sys.total_memory();
    let cpu_cores = {
        let detected = sys.cpus().len();
        if detected > 0 {
            detected
        } else {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
        }
    };

    let (recommended_vision_tier, recommended_chat_tier) = recommend_tiers_for_ram(total_ram_bytes);

    HardwareInfo {
        total_ram_bytes,
        cpu_cores,
        recommended_vision_tier,
        recommended_chat_tier,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recommend_tiers_matches_ram_thresholds() {
        assert_eq!(
            recommend_tiers_for_ram(8 * GIB),
            (AiTier::Tier1, AiTier::Tier1)
        );
        assert_eq!(
            recommend_tiers_for_ram(16 * GIB - 1),
            (AiTier::Tier1, AiTier::Tier1)
        );
        assert_eq!(
            recommend_tiers_for_ram(16 * GIB),
            (AiTier::Tier2, AiTier::Tier2)
        );
        assert_eq!(
            recommend_tiers_for_ram(24 * GIB),
            (AiTier::Tier2, AiTier::Tier2)
        );
        assert_eq!(
            recommend_tiers_for_ram(32 * GIB),
            (AiTier::Tier3, AiTier::Tier3)
        );
        assert_eq!(
            recommend_tiers_for_ram(64 * GIB),
            (AiTier::Tier3, AiTier::Tier3)
        );
    }

    #[test]
    fn detect_hardware_info_returns_nonzero_cpu_and_ram() {
        let info = detect_hardware_info();
        assert!(info.total_ram_bytes > 0);
        assert!(info.cpu_cores >= 1);
    }
}
