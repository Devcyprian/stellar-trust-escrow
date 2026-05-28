//! # Multi-Token Staking and Gas Rebates for Governance
//!
//! Allows stakers to deposit native tokens or partner stablecoins into the
//! governance staking pool. Voting power is calculated using per-token weight
//! factors. Gas spent by active stakers when submitting governance votes is
//! tracked, and monthly XLM rebates are distributed to reimburse costs.
//!
//! ## Storage Keys
//! - `Admin`              – contract admin
//! - `TokenConfig(addr)`  – weight factor for a supported token (basis points)
//! - `Stake(addr, token)` – staked amount for (staker, token)
//! - `GasUsed(addr)`      – cumulative gas units tracked for a staker
//! - `RebatePool`         – XLM balance available for rebates
//! - `LastRebate(addr)`   – ledger timestamp of last rebate distribution
//!
//! ## Events
//! - `stk_dep`   – tokens deposited
//! - `stk_wdw`   – tokens withdrawn
//! - `gas_rec`   – gas usage recorded
//! - `reb_dist`  – rebate distributed to staker

use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, token, Address, Env,
};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Seconds in ~30 days (used as minimum rebate interval).
pub const REBATE_INTERVAL_SECS: u64 = 30 * 24 * 3600;
/// XLM rebate per gas unit (in stroops, 1 XLM = 10_000_000 stroops).
pub const REBATE_RATE_PER_GAS: i128 = 100;

// ── Storage keys ──────────────────────────────────────────────────────────────

#[contracttype]
pub enum StakeKey {
    Admin,
    /// Weight factor (bps) for a supported token.
    TokenConfig(Address),
    /// Staked amount: (staker, token).
    Stake(Address, Address),
    /// Cumulative gas units recorded for a staker.
    GasUsed(Address),
    /// XLM available for rebate distribution.
    RebatePool,
    /// Ledger timestamp of last rebate for a staker.
    LastRebate(Address),
}

// ── Errors ────────────────────────────────────────────────────────────────────

#[soroban_sdk::contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum StakeError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    UnsupportedToken = 4,
    InvalidAmount = 5,
    InsufficientStake = 6,
    RebateTooSoon = 7,
    InsufficientRebatePool = 8,
    NoGasToRebate = 9,
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct StakingContract;

#[contractimpl]
impl StakingContract {
    // ── Initialization ────────────────────────────────────────────────────────

    /// Initialize the staking contract.
    pub fn initialize(env: Env, admin: Address) -> Result<(), StakeError> {
        if env.storage().instance().has(&StakeKey::Admin) {
            return Err(StakeError::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&StakeKey::Admin, &admin);
        env.storage().instance().set(&StakeKey::RebatePool, &0i128);
        Ok(())
    }

    /// Register a supported token with its voting-power weight (basis points).
    /// e.g. native XLM = 10_000 (1x), stablecoin = 8_000 (0.8x).
    pub fn add_token(env: Env, admin: Address, token: Address, weight_bps: u32) -> Result<(), StakeError> {
        admin.require_auth();
        Self::assert_admin(&env, &admin)?;
        if weight_bps == 0 || weight_bps > 20_000 {
            return Err(StakeError::InvalidAmount);
        }
        env.storage()
            .instance()
            .set(&StakeKey::TokenConfig(token), &weight_bps);
        Ok(())
    }

    // ── Staking ───────────────────────────────────────────────────────────────

    /// Deposit `amount` of `token` into the staking pool.
    pub fn deposit(env: Env, staker: Address, token: Address, amount: i128) -> Result<(), StakeError> {
        staker.require_auth();
        if amount <= 0 {
            return Err(StakeError::InvalidAmount);
        }
        Self::assert_supported_token(&env, &token)?;

        // Transfer tokens from staker to contract
        token::Client::new(&env, &token).transfer(
            &staker,
            &env.current_contract_address(),
            &amount,
        );

        let key = StakeKey::Stake(staker.clone(), token.clone());
        let current: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        env.storage().persistent().set(&key, &(current + amount));

        env.events()
            .publish((symbol_short!("stk_dep"), staker.clone()), (token, amount));
        Ok(())
    }

    /// Withdraw `amount` of `token` from the staking pool.
    pub fn withdraw(env: Env, staker: Address, token: Address, amount: i128) -> Result<(), StakeError> {
        staker.require_auth();
        if amount <= 0 {
            return Err(StakeError::InvalidAmount);
        }
        Self::assert_supported_token(&env, &token)?;

        let key = StakeKey::Stake(staker.clone(), token.clone());
        let current: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        if current < amount {
            return Err(StakeError::InsufficientStake);
        }

        env.storage().persistent().set(&key, &(current - amount));

        token::Client::new(&env, &token).transfer(
            &env.current_contract_address(),
            &staker,
            &amount,
        );

        env.events()
            .publish((symbol_short!("stk_wdw"), staker.clone()), (token, amount));
        Ok(())
    }

    // ── Voting power ──────────────────────────────────────────────────────────

    /// Calculate voting power for a staker across all provided tokens.
    /// voting_power = Σ (stake_i * weight_bps_i / 10_000)
    pub fn voting_power(
        env: Env,
        staker: Address,
        tokens: soroban_sdk::Vec<Address>,
    ) -> i128 {
        let mut power: i128 = 0;
        for i in 0..tokens.len() {
            let t = tokens.get(i).unwrap();
            let weight_bps: u32 = env
                .storage()
                .instance()
                .get(&StakeKey::TokenConfig(t.clone()))
                .unwrap_or(0);
            if weight_bps == 0 {
                continue;
            }
            let stake: i128 = env
                .storage()
                .persistent()
                .get(&StakeKey::Stake(staker.clone(), t))
                .unwrap_or(0);
            power = power.saturating_add(
                stake.saturating_mul(weight_bps as i128) / 10_000,
            );
        }
        power
    }

    // ── Gas tracking ──────────────────────────────────────────────────────────

    /// Record gas units consumed by a staker when submitting a governance vote.
    /// Only callable by admin (governance contract).
    pub fn record_gas(env: Env, admin: Address, staker: Address, gas_units: u64) -> Result<(), StakeError> {
        admin.require_auth();
        Self::assert_admin(&env, &admin)?;

        let key = StakeKey::GasUsed(staker.clone());
        let current: u64 = env.storage().persistent().get(&key).unwrap_or(0);
        env.storage()
            .persistent()
            .set(&key, &(current + gas_units));

        env.events()
            .publish((symbol_short!("gas_rec"), staker), (gas_units,));
        Ok(())
    }

    // ── Rebate distribution ───────────────────────────────────────────────────

    /// Fund the rebate pool with XLM.
    pub fn fund_rebate_pool(env: Env, funder: Address, xlm_token: Address, amount: i128) -> Result<(), StakeError> {
        funder.require_auth();
        if amount <= 0 {
            return Err(StakeError::InvalidAmount);
        }
        token::Client::new(&env, &xlm_token).transfer(
            &funder,
            &env.current_contract_address(),
            &amount,
        );
        let current: i128 = env
            .storage()
            .instance()
            .get(&StakeKey::RebatePool)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&StakeKey::RebatePool, &(current + amount));
        Ok(())
    }

    /// Distribute monthly XLM rebate to a staker based on recorded gas usage.
    /// rebate = gas_used * REBATE_RATE_PER_GAS
    pub fn distribute_rebate(
        env: Env,
        staker: Address,
        xlm_token: Address,
    ) -> Result<i128, StakeError> {
        staker.require_auth();

        let now = env.ledger().timestamp();
        let last: u64 = env
            .storage()
            .persistent()
            .get(&StakeKey::LastRebate(staker.clone()))
            .unwrap_or(0);

        if now < last + REBATE_INTERVAL_SECS {
            return Err(StakeError::RebateTooSoon);
        }

        let gas: u64 = env
            .storage()
            .persistent()
            .get(&StakeKey::GasUsed(staker.clone()))
            .unwrap_or(0);
        if gas == 0 {
            return Err(StakeError::NoGasToRebate);
        }

        let rebate = (gas as i128).saturating_mul(REBATE_RATE_PER_GAS);

        let pool: i128 = env
            .storage()
            .instance()
            .get(&StakeKey::RebatePool)
            .unwrap_or(0);
        if pool < rebate {
            return Err(StakeError::InsufficientRebatePool);
        }

        // Deduct from pool, reset gas counter, update last rebate timestamp
        env.storage()
            .instance()
            .set(&StakeKey::RebatePool, &(pool - rebate));
        env.storage()
            .persistent()
            .set(&StakeKey::GasUsed(staker.clone()), &0u64);
        env.storage()
            .persistent()
            .set(&StakeKey::LastRebate(staker.clone()), &now);

        token::Client::new(&env, &xlm_token).transfer(
            &env.current_contract_address(),
            &staker,
            &rebate,
        );

        env.events()
            .publish((symbol_short!("reb_dist"), staker), (rebate,));
        Ok(rebate)
    }

    // ── View helpers ──────────────────────────────────────────────────────────

    pub fn stake_of(env: Env, staker: Address, token: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&StakeKey::Stake(staker, token))
            .unwrap_or(0)
    }

    pub fn gas_used(env: Env, staker: Address) -> u64 {
        env.storage()
            .persistent()
            .get(&StakeKey::GasUsed(staker))
            .unwrap_or(0)
    }

    pub fn rebate_pool(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&StakeKey::RebatePool)
            .unwrap_or(0)
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    fn assert_admin(env: &Env, caller: &Address) -> Result<(), StakeError> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&StakeKey::Admin)
            .ok_or(StakeError::NotInitialized)?;
        if *caller != admin {
            return Err(StakeError::Unauthorized);
        }
        Ok(())
    }

    fn assert_supported_token(env: &Env, token: &Address) -> Result<(), StakeError> {
        if !env
            .storage()
            .instance()
            .has(&StakeKey::TokenConfig(token.clone()))
        {
            return Err(StakeError::UnsupportedToken);
        }
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, vec, Env};

    fn setup() -> (Env, Address, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let token_a = Address::generate(&env);
        let token_b = Address::generate(&env);
        let id = env.register_contract(None, StakingContract);
        let client = StakingContractClient::new(&env, &id);
        client.initialize(&admin).unwrap();
        // native-like token: 10_000 bps (1x), stablecoin: 8_000 bps (0.8x)
        client.add_token(&admin, &token_a, &10_000).unwrap();
        client.add_token(&admin, &token_b, &8_000).unwrap();
        (env, admin, token_a, token_b)
    }

    #[test]
    fn test_deposit_and_stake_balance() {
        let (env, _admin, token_a, _token_b) = setup();
        let id = env.register_contract(None, StakingContract);
        let client = StakingContractClient::new(&env, &id);
        let staker = Address::generate(&env);
        client.deposit(&staker, &token_a, &1000).unwrap();
        assert_eq!(client.stake_of(&staker, &token_a), 1000);
    }

    #[test]
    fn test_withdraw_reduces_stake() {
        let (env, _admin, token_a, _token_b) = setup();
        let id = env.register_contract(None, StakingContract);
        let client = StakingContractClient::new(&env, &id);
        let staker = Address::generate(&env);
        client.deposit(&staker, &token_a, &1000).unwrap();
        client.withdraw(&staker, &token_a, &400).unwrap();
        assert_eq!(client.stake_of(&staker, &token_a), 600);
    }

    #[test]
    fn test_voting_power_multi_token() {
        let (env, _admin, token_a, token_b) = setup();
        let id = env.register_contract(None, StakingContract);
        let client = StakingContractClient::new(&env, &id);
        let staker = Address::generate(&env);
        // 1000 * 10_000/10_000 = 1000
        client.deposit(&staker, &token_a, &1000).unwrap();
        // 1000 * 8_000/10_000 = 800
        client.deposit(&staker, &token_b, &1000).unwrap();
        let tokens = vec![&env, token_a, token_b];
        assert_eq!(client.voting_power(&staker, &tokens), 1800);
    }

    #[test]
    fn test_gas_recording_and_rebate() {
        let (env, admin, _token_a, _token_b) = setup();
        let id = env.register_contract(None, StakingContract);
        let client = StakingContractClient::new(&env, &id);
        let staker = Address::generate(&env);
        let xlm = Address::generate(&env);

        client.record_gas(&admin, &staker, &500).unwrap();
        assert_eq!(client.gas_used(&staker), 500);

        // Fund rebate pool
        client.fund_rebate_pool(&admin, &xlm, &1_000_000).unwrap();

        // Distribute rebate: 500 * 100 = 50_000
        let rebate = client.distribute_rebate(&staker, &xlm).unwrap();
        assert_eq!(rebate, 50_000);
        assert_eq!(client.gas_used(&staker), 0);
        assert_eq!(client.rebate_pool(), 950_000);
    }

    #[test]
    fn test_rebate_too_soon_rejected() {
        let (env, admin, _token_a, _token_b) = setup();
        let id = env.register_contract(None, StakingContract);
        let client = StakingContractClient::new(&env, &id);
        let staker = Address::generate(&env);
        let xlm = Address::generate(&env);

        client.record_gas(&admin, &staker, &100).unwrap();
        client.fund_rebate_pool(&admin, &xlm, &1_000_000).unwrap();
        client.distribute_rebate(&staker, &xlm).unwrap();

        // Immediate second claim should fail
        client.record_gas(&admin, &staker, &100).unwrap();
        assert_eq!(
            client.distribute_rebate(&staker, &xlm),
            Err(StakeError::RebateTooSoon)
        );
    }

    #[test]
    fn test_unsupported_token_rejected() {
        let (env, _admin, _token_a, _token_b) = setup();
        let id = env.register_contract(None, StakingContract);
        let client = StakingContractClient::new(&env, &id);
        let staker = Address::generate(&env);
        let unknown = Address::generate(&env);
        assert_eq!(
            client.deposit(&staker, &unknown, &100),
            Err(StakeError::UnsupportedToken)
        );
    }

    #[test]
    fn test_insufficient_stake_withdrawal_rejected() {
        let (env, _admin, token_a, _token_b) = setup();
        let id = env.register_contract(None, StakingContract);
        let client = StakingContractClient::new(&env, &id);
        let staker = Address::generate(&env);
        client.deposit(&staker, &token_a, &100).unwrap();
        assert_eq!(
            client.withdraw(&staker, &token_a, &200),
            Err(StakeError::InsufficientStake)
        );
    }
}
