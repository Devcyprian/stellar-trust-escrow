//! # Flash-Loan Prevention Dynamic Fee Multiplier
//!
//! Tracks deposit/withdrawal volumes per ledger block window. If a single
//! transaction exceeds 50% of the active pool volume within a 5-block window,
//! a 2x fee multiplier is applied automatically.
//!
//! ## Storage Keys
//! - `FeeConfig`        – base fee rate in basis points
//! - `BlockVolume(seq)` – cumulative volume recorded at ledger sequence `seq`
//! - `PoolVolume`       – current total active pool volume
//!
//! ## Events
//! - `fee_calc`  – emitted on every fee calculation with applied multiplier
//! - `vol_upd`   – emitted when block volume is updated

use soroban_sdk::{contract, contractimpl, contracttype, symbol_short, Env};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Base platform fee in basis points (e.g. 100 = 1%).
pub const BASE_FEE_BPS: u32 = 100;
/// Flash-loan multiplier applied when threshold is breached.
pub const FLASH_LOAN_MULTIPLIER: u32 = 2;
/// Window size in ledger blocks for volume tracking.
pub const VOLUME_WINDOW_BLOCKS: u32 = 5;
/// Threshold: transaction volume as a fraction of pool (50% = 5000 bps).
pub const FLASH_LOAN_THRESHOLD_BPS: u32 = 5000;

// ── Storage keys ──────────────────────────────────────────────────────────────

#[contracttype]
pub enum FeeKey {
    /// Base fee configuration (basis points).
    FeeConfig,
    /// Cumulative volume in a given ledger block window slot.
    BlockVolume(u32),
    /// Total active pool volume.
    PoolVolume,
}

// ── Types ─────────────────────────────────────────────────────────────────────

/// Result of a fee calculation.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeResult {
    /// The fee amount charged.
    pub fee: i128,
    /// The multiplier applied (1 = normal, 2 = flash-loan penalty).
    pub multiplier: u32,
    /// Whether the flash-loan threshold was breached.
    pub flash_loan_detected: bool,
}

// ── Errors ────────────────────────────────────────────────────────────────────

#[soroban_sdk::contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum FeeError {
    InvalidAmount = 1,
    InvalidFeeBps = 2,
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct FeeContract;

#[contractimpl]
impl FeeContract {
    /// Initialize fee configuration.
    pub fn init(env: Env, fee_bps: u32) -> Result<(), FeeError> {
        if fee_bps == 0 || fee_bps > 10_000 {
            return Err(FeeError::InvalidFeeBps);
        }
        env.storage().instance().set(&FeeKey::FeeConfig, &fee_bps);
        env.storage().instance().set(&FeeKey::PoolVolume, &0i128);
        Ok(())
    }

    /// Record a deposit into the pool, updating pool and block volumes.
    pub fn record_deposit(env: Env, amount: i128) -> Result<(), FeeError> {
        if amount <= 0 {
            return Err(FeeError::InvalidAmount);
        }
        Self::update_pool_volume(&env, amount);
        Self::update_block_volume(&env, amount);
        Ok(())
    }

    /// Record a withdrawal from the pool.
    pub fn record_withdrawal(env: Env, amount: i128) -> Result<(), FeeError> {
        if amount <= 0 {
            return Err(FeeError::InvalidAmount);
        }
        Self::update_pool_volume(&env, -amount);
        Self::update_block_volume(&env, amount);
        Ok(())
    }

    /// Calculate the fee for a transaction of `amount`.
    ///
    /// Applies a 2x multiplier if the transaction exceeds 50% of the active
    /// pool volume within the current 5-block window.
    pub fn calculate_fee(env: Env, amount: i128) -> Result<FeeResult, FeeError> {
        if amount <= 0 {
            return Err(FeeError::InvalidAmount);
        }

        let fee_bps: u32 = env
            .storage()
            .instance()
            .get(&FeeKey::FeeConfig)
            .unwrap_or(BASE_FEE_BPS);

        let pool_volume: i128 = env
            .storage()
            .instance()
            .get(&FeeKey::PoolVolume)
            .unwrap_or(0);

        let window_volume = Self::get_window_volume(&env);
        let flash_loan_detected = Self::is_flash_loan(&env, amount, pool_volume, window_volume);

        let multiplier = if flash_loan_detected {
            FLASH_LOAN_MULTIPLIER
        } else {
            1
        };

        // fee = amount * fee_bps * multiplier / 10_000
        let fee = amount
            .saturating_mul(fee_bps as i128)
            .saturating_mul(multiplier as i128)
            / 10_000;

        env.events().publish(
            (symbol_short!("fee_calc"),),
            (amount, fee, multiplier, flash_loan_detected, pool_volume, window_volume),
        );

        Ok(FeeResult { fee, multiplier, flash_loan_detected })
    }

    /// Returns the current pool volume.
    pub fn pool_volume(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&FeeKey::PoolVolume)
            .unwrap_or(0)
    }

    /// Returns the cumulative volume in the current 5-block window.
    pub fn window_volume(env: Env) -> i128 {
        Self::get_window_volume(&env)
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    /// Determine whether a transaction constitutes a flash-loan attack.
    ///
    /// Conditions (either triggers the multiplier):
    /// 1. `amount` > 50% of `pool_volume`
    /// 2. `window_volume + amount` > 50% of `pool_volume` (within 5-block window)
    fn is_flash_loan(env: &Env, amount: i128, pool_volume: i128, window_volume: i128) -> bool {
        if pool_volume <= 0 {
            return false;
        }
        // threshold = pool_volume * 5000 / 10_000 = pool_volume / 2
        let threshold = pool_volume
            .saturating_mul(FLASH_LOAN_THRESHOLD_BPS as i128)
            / 10_000;

        // Single tx exceeds threshold
        if amount > threshold {
            return true;
        }
        // Cumulative window volume exceeds threshold
        if window_volume.saturating_add(amount) > threshold {
            return true;
        }
        false
    }

    /// Sum volumes across the current 5-block window.
    fn get_window_volume(env: &Env) -> i128 {
        let current = env.ledger().sequence();
        let mut total: i128 = 0;
        for offset in 0..VOLUME_WINDOW_BLOCKS {
            let slot = current.saturating_sub(offset);
            let vol: i128 = env
                .storage()
                .temporary()
                .get(&FeeKey::BlockVolume(slot))
                .unwrap_or(0);
            total = total.saturating_add(vol);
        }
        total
    }

    fn update_pool_volume(env: &Env, delta: i128) {
        let current: i128 = env
            .storage()
            .instance()
            .get(&FeeKey::PoolVolume)
            .unwrap_or(0);
        let updated = current.saturating_add(delta).max(0);
        env.storage().instance().set(&FeeKey::PoolVolume, &updated);
        env.events()
            .publish((symbol_short!("vol_upd"),), (updated,));
    }

    fn update_block_volume(env: &Env, amount: i128) {
        let slot = env.ledger().sequence();
        let current: i128 = env
            .storage()
            .temporary()
            .get(&FeeKey::BlockVolume(slot))
            .unwrap_or(0);
        let updated = current.saturating_add(amount);
        // TTL: keep for VOLUME_WINDOW_BLOCKS + 1 blocks
        env.storage()
            .temporary()
            .set(&FeeKey::BlockVolume(slot), &updated);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::Env;

    fn setup() -> (Env, soroban_sdk::Address) {
        let env = Env::default();
        let id = env.register_contract(None, FeeContract);
        FeeContractClient::new(&env, &id).init(&BASE_FEE_BPS).unwrap();
        (env, id)
    }

    #[test]
    fn test_normal_fee_no_multiplier() {
        let (env, id) = setup();
        let client = FeeContractClient::new(&env, &id);
        // Pool is empty — no flash-loan detection
        let result = client.calculate_fee(&1000).unwrap();
        assert_eq!(result.multiplier, 1);
        assert!(!result.flash_loan_detected);
        // fee = 1000 * 100 / 10_000 = 10
        assert_eq!(result.fee, 10);
    }

    #[test]
    fn test_flash_loan_multiplier_applied() {
        let (env, id) = setup();
        let client = FeeContractClient::new(&env, &id);
        // Seed pool with 1000
        client.record_deposit(&1000).unwrap();
        // Transaction of 600 > 50% of 1000 → flash-loan detected
        let result = client.calculate_fee(&600).unwrap();
        assert!(result.flash_loan_detected);
        assert_eq!(result.multiplier, 2);
        // fee = 600 * 100 * 2 / 10_000 = 12
        assert_eq!(result.fee, 12);
    }

    #[test]
    fn test_below_threshold_no_multiplier() {
        let (env, id) = setup();
        let client = FeeContractClient::new(&env, &id);
        client.record_deposit(&1000).unwrap();
        // 400 < 500 (50% of 1000) → normal fee
        let result = client.calculate_fee(&400).unwrap();
        assert!(!result.flash_loan_detected);
        assert_eq!(result.multiplier, 1);
    }

    #[test]
    fn test_window_volume_accumulation_triggers_multiplier() {
        let (env, id) = setup();
        let client = FeeContractClient::new(&env, &id);
        client.record_deposit(&1000).unwrap();
        // First tx: 300 (below threshold alone)
        client.record_withdrawal(&300).unwrap();
        // Second tx: 300 — window total = 600 > 500 → flash-loan
        let result = client.calculate_fee(&300).unwrap();
        assert!(result.flash_loan_detected);
        assert_eq!(result.multiplier, 2);
    }

    #[test]
    fn test_pool_volume_tracks_deposits_and_withdrawals() {
        let (env, id) = setup();
        let client = FeeContractClient::new(&env, &id);
        client.record_deposit(&500).unwrap();
        assert_eq!(client.pool_volume(), 500);
        client.record_withdrawal(&200).unwrap();
        assert_eq!(client.pool_volume(), 300);
    }

    #[test]
    fn test_invalid_amount_rejected() {
        let (env, id) = setup();
        let client = FeeContractClient::new(&env, &id);
        assert_eq!(client.calculate_fee(&0), Err(FeeError::InvalidAmount));
        assert_eq!(client.calculate_fee(&-1), Err(FeeError::InvalidAmount));
    }
}
