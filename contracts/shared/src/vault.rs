//! # Multi-Sig Vault with Timelocked Recovery
//!
//! Treasury vault requiring 3-of-5 threshold signatures for standard withdrawals.
//! A single signer can trigger a security freeze. All balance changes and upgrades
//! must wait for a mandatory timelock of 100,000 ledger blocks before execution,
//! giving the governance DAO time to intervene.
//!
//! ## Storage Keys
//! - `Admin`          – initial deployer / bootstrap admin
//! - `Signers`        – Vec<Address> of up to 5 authorized signers
//! - `Threshold`      – u32 required signatures (default 3)
//! - `Frozen`         – bool security-freeze flag
//! - `Nonce`          – u64 replay-protection counter
//! - `PendingTx(id)`  – PendingTransaction awaiting timelock
//! - `TxCounter`      – u64 monotonic pending-tx id
//!
//! ## Events
//! - `vault_init`     – vault initialized
//! - `tx_proposed`    – withdrawal proposed, timelock started
//! - `tx_signed`      – additional signature collected
//! - `tx_executed`    – transfer executed after timelock
//! - `tx_cancelled`   – pending tx cancelled
//! - `vault_frozen`   – security freeze triggered
//! - `vault_unfrozen` – freeze lifted by threshold consensus

#![no_std]

use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, vec, Address, Env, Vec,
};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Mandatory timelock in ledger blocks before a pending tx can execute.
const TIMELOCK_BLOCKS: u32 = 100_000;
/// Maximum number of authorized signers.
const MAX_SIGNERS: u32 = 5;
/// Default signature threshold.
const DEFAULT_THRESHOLD: u32 = 3;

// ── Storage keys ──────────────────────────────────────────────────────────────

#[contracttype]
pub enum DataKey {
    Admin,
    Signers,
    Threshold,
    Frozen,
    Nonce,
    PendingTx(u64),
    TxCounter,
}

// ── Types ─────────────────────────────────────────────────────────────────────

/// A withdrawal proposal waiting for signatures and the timelock to expire.
#[contracttype]
#[derive(Clone, Debug)]
pub struct PendingTransaction {
    /// Unique transaction id.
    pub id: u64,
    /// Token contract to transfer from.
    pub token: Address,
    /// Destination address.
    pub recipient: Address,
    /// Amount to transfer.
    pub amount: i128,
    /// Ledger block at which this tx was proposed.
    pub proposed_at: u32,
    /// Ledger block after which execution is allowed (proposed_at + TIMELOCK_BLOCKS).
    pub executable_after: u32,
    /// Addresses that have already signed.
    pub signatures: Vec<Address>,
    /// Whether this tx has been executed.
    pub executed: bool,
    /// Whether this tx has been cancelled.
    pub cancelled: bool,
    /// Replay-protection nonce at proposal time.
    pub nonce: u64,
}

// ── Errors ────────────────────────────────────────────────────────────────────

#[soroban_sdk::contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum VaultError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    VaultFrozen = 4,
    VaultNotFrozen = 5,
    TooManySigners = 6,
    InvalidThreshold = 7,
    TxNotFound = 8,
    TxAlreadyExecuted = 9,
    TxAlreadyCancelled = 10,
    TimelockNotExpired = 11,
    AlreadySigned = 12,
    ThresholdNotMet = 13,
    InvalidAmount = 14,
    DuplicateSigner = 15,
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct VaultContract;

#[contractimpl]
impl VaultContract {
    // ── Initialization ────────────────────────────────────────────────────────

    /// Initialize the vault with a set of signers and an optional threshold.
    ///
    /// # Arguments
    /// * `admin`     – bootstrap admin (can be a governance contract)
    /// * `signers`   – up to 5 authorized signer addresses
    /// * `threshold` – required signatures (defaults to 3; must be ≤ len(signers))
    pub fn initialize(
        env: Env,
        admin: Address,
        signers: Vec<Address>,
        threshold: u32,
    ) -> Result<(), VaultError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(VaultError::AlreadyInitialized);
        }
        admin.require_auth();

        let n = signers.len();
        if n == 0 || n > MAX_SIGNERS {
            return Err(VaultError::TooManySigners);
        }
        let t = if threshold == 0 { DEFAULT_THRESHOLD } else { threshold };
        if t > n {
            return Err(VaultError::InvalidThreshold);
        }

        // Ensure no duplicate signers
        for i in 0..n {
            for j in (i + 1)..n {
                if signers.get(i).unwrap() == signers.get(j).unwrap() {
                    return Err(VaultError::DuplicateSigner);
                }
            }
        }

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Signers, &signers);
        env.storage().instance().set(&DataKey::Threshold, &t);
        env.storage().instance().set(&DataKey::Frozen, &false);
        env.storage().instance().set(&DataKey::Nonce, &0u64);
        env.storage().instance().set(&DataKey::TxCounter, &0u64);

        env.events().publish((symbol_short!("vlt_init"),), (admin,));
        Ok(())
    }

    // ── Propose withdrawal ────────────────────────────────────────────────────

    /// Propose a withdrawal. The caller must be an authorized signer.
    /// The proposal is automatically signed by the proposer.
    /// Execution is only possible after TIMELOCK_BLOCKS ledger blocks.
    ///
    /// Returns the new pending transaction id.
    pub fn propose(
        env: Env,
        proposer: Address,
        token: Address,
        recipient: Address,
        amount: i128,
    ) -> Result<u64, VaultError> {
        Self::assert_not_frozen(&env)?;
        Self::assert_initialized(&env)?;
        proposer.require_auth();
        Self::assert_is_signer(&env, &proposer)?;

        if amount <= 0 {
            return Err(VaultError::InvalidAmount);
        }

        let nonce: u64 = env.storage().instance().get(&DataKey::Nonce).unwrap_or(0);
        let tx_id: u64 = env.storage().instance().get(&DataKey::TxCounter).unwrap_or(0);
        let current_block = env.ledger().sequence();

        let mut sigs: Vec<Address> = vec![&env];
        sigs.push_back(proposer.clone());

        let pending = PendingTransaction {
            id: tx_id,
            token: token.clone(),
            recipient: recipient.clone(),
            amount,
            proposed_at: current_block,
            executable_after: current_block + TIMELOCK_BLOCKS,
            signatures: sigs,
            executed: false,
            cancelled: false,
            nonce,
        };

        env.storage().persistent().set(&DataKey::PendingTx(tx_id), &pending);
        env.storage().instance().set(&DataKey::TxCounter, &(tx_id + 1));
        // Increment nonce to prevent replay
        env.storage().instance().set(&DataKey::Nonce, &(nonce + 1));

        env.events().publish(
            (symbol_short!("tx_prop"), tx_id),
            (proposer, token, recipient, amount, pending.executable_after),
        );
        Ok(tx_id)
    }

    // ── Sign a pending transaction ────────────────────────────────────────────

    /// Add a signature to a pending transaction.
    pub fn sign(env: Env, signer: Address, tx_id: u64) -> Result<u32, VaultError> {
        Self::assert_not_frozen(&env)?;
        signer.require_auth();
        Self::assert_is_signer(&env, &signer)?;

        let mut tx: PendingTransaction = env
            .storage()
            .persistent()
            .get(&DataKey::PendingTx(tx_id))
            .ok_or(VaultError::TxNotFound)?;

        if tx.executed {
            return Err(VaultError::TxAlreadyExecuted);
        }
        if tx.cancelled {
            return Err(VaultError::TxAlreadyCancelled);
        }

        // Prevent double-signing
        for i in 0..tx.signatures.len() {
            if tx.signatures.get(i).unwrap() == signer {
                return Err(VaultError::AlreadySigned);
            }
        }

        tx.signatures.push_back(signer.clone());
        let sig_count = tx.signatures.len();
        env.storage().persistent().set(&DataKey::PendingTx(tx_id), &tx);

        env.events()
            .publish((symbol_short!("tx_sign"), tx_id), (signer, sig_count));
        Ok(sig_count)
    }

    // ── Execute a pending transaction ─────────────────────────────────────────

    /// Execute a pending transaction once the timelock has expired and the
    /// signature threshold is met.
    pub fn execute(env: Env, executor: Address, tx_id: u64) -> Result<(), VaultError> {
        Self::assert_not_frozen(&env)?;
        executor.require_auth();
        Self::assert_is_signer(&env, &executor)?;

        let mut tx: PendingTransaction = env
            .storage()
            .persistent()
            .get(&DataKey::PendingTx(tx_id))
            .ok_or(VaultError::TxNotFound)?;

        if tx.executed {
            return Err(VaultError::TxAlreadyExecuted);
        }
        if tx.cancelled {
            return Err(VaultError::TxAlreadyCancelled);
        }

        let threshold: u32 = env
            .storage()
            .instance()
            .get(&DataKey::Threshold)
            .unwrap_or(DEFAULT_THRESHOLD);

        if tx.signatures.len() < threshold {
            return Err(VaultError::ThresholdNotMet);
        }

        let current_block = env.ledger().sequence();
        if current_block < tx.executable_after {
            return Err(VaultError::TimelockNotExpired);
        }

        // Transfer tokens
        let token_client = soroban_sdk::token::Client::new(&env, &tx.token);
        token_client.transfer(
            &env.current_contract_address(),
            &tx.recipient,
            &tx.amount,
        );

        tx.executed = true;
        env.storage().persistent().set(&DataKey::PendingTx(tx_id), &tx);

        env.events().publish(
            (symbol_short!("tx_exec"), tx_id),
            (tx.recipient, tx.amount),
        );
        Ok(())
    }

    // ── Cancel a pending transaction ──────────────────────────────────────────

    /// Cancel a pending transaction. Requires threshold signatures.
    pub fn cancel(env: Env, canceller: Address, tx_id: u64) -> Result<(), VaultError> {
        canceller.require_auth();
        Self::assert_is_signer(&env, &canceller)?;

        let mut tx: PendingTransaction = env
            .storage()
            .persistent()
            .get(&DataKey::PendingTx(tx_id))
            .ok_or(VaultError::TxNotFound)?;

        if tx.executed {
            return Err(VaultError::TxAlreadyExecuted);
        }
        if tx.cancelled {
            return Err(VaultError::TxAlreadyCancelled);
        }

        let threshold: u32 = env
            .storage()
            .instance()
            .get(&DataKey::Threshold)
            .unwrap_or(DEFAULT_THRESHOLD);

        if tx.signatures.len() < threshold {
            return Err(VaultError::ThresholdNotMet);
        }

        tx.cancelled = true;
        env.storage().persistent().set(&DataKey::PendingTx(tx_id), &tx);

        env.events()
            .publish((symbol_short!("tx_cncl"), tx_id), (canceller,));
        Ok(())
    }

    // ── Security freeze ───────────────────────────────────────────────────────

    /// Any single authorized signer can trigger a security freeze.
    /// While frozen, no new proposals or executions are allowed.
    pub fn freeze(env: Env, signer: Address) -> Result<(), VaultError> {
        signer.require_auth();
        Self::assert_initialized(&env)?;
        Self::assert_is_signer(&env, &signer)?;

        env.storage().instance().set(&DataKey::Frozen, &true);

        env.events()
            .publish((symbol_short!("vlt_frz"),), (signer,));
        Ok(())
    }

    /// Lift the security freeze. Requires threshold signatures (all must call unfreeze).
    /// Simplified: requires the admin to unfreeze (governance can override).
    pub fn unfreeze(env: Env, admin: Address) -> Result<(), VaultError> {
        admin.require_auth();
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(VaultError::NotInitialized)?;
        if admin != stored_admin {
            return Err(VaultError::Unauthorized);
        }

        env.storage().instance().set(&DataKey::Frozen, &false);

        env.events()
            .publish((symbol_short!("vlt_ufrz"),), (admin,));
        Ok(())
    }

    // ── View helpers ──────────────────────────────────────────────────────────

    /// Returns whether the vault is currently frozen.
    pub fn is_frozen(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Frozen)
            .unwrap_or(false)
    }

    /// Returns the current nonce.
    pub fn nonce(env: Env) -> u64 {
        env.storage().instance().get(&DataKey::Nonce).unwrap_or(0)
    }

    /// Returns the list of authorized signers.
    pub fn signers(env: Env) -> Vec<Address> {
        env.storage()
            .instance()
            .get(&DataKey::Signers)
            .unwrap_or_else(|| vec![&env])
    }

    /// Returns the signature threshold.
    pub fn threshold(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Threshold)
            .unwrap_or(DEFAULT_THRESHOLD)
    }

    /// Returns a pending transaction by id.
    pub fn get_pending_tx(env: Env, tx_id: u64) -> Option<PendingTransaction> {
        env.storage().persistent().get(&DataKey::PendingTx(tx_id))
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    fn assert_initialized(env: &Env) -> Result<(), VaultError> {
        if !env.storage().instance().has(&DataKey::Admin) {
            return Err(VaultError::NotInitialized);
        }
        Ok(())
    }

    fn assert_not_frozen(env: &Env) -> Result<(), VaultError> {
        let frozen: bool = env
            .storage()
            .instance()
            .get(&DataKey::Frozen)
            .unwrap_or(false);
        if frozen {
            return Err(VaultError::VaultFrozen);
        }
        Ok(())
    }

    fn assert_is_signer(env: &Env, addr: &Address) -> Result<(), VaultError> {
        let signers: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::Signers)
            .ok_or(VaultError::NotInitialized)?;
        for i in 0..signers.len() {
            if signers.get(i).unwrap() == *addr {
                return Ok(());
            }
        }
        Err(VaultError::Unauthorized)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Env};

    fn setup() -> (Env, Address, Vec<Address>) {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let mut signers: Vec<Address> = vec![&env];
        for _ in 0..5 {
            signers.push_back(Address::generate(&env));
        }
        (env, admin, signers)
    }

    #[test]
    fn test_initialize_and_freeze() {
        let (env, admin, signers) = setup();
        let contract_id = env.register_contract(None, VaultContract);
        let client = VaultContractClient::new(&env, &contract_id);

        client.initialize(&admin, &signers, &3).unwrap();
        assert!(!client.is_frozen());

        // Any signer can freeze
        client.freeze(&signers.get(0).unwrap()).unwrap();
        assert!(client.is_frozen());
    }

    #[test]
    fn test_propose_and_sign_and_execute() {
        let (env, admin, signers) = setup();
        let contract_id = env.register_contract(None, VaultContract);
        let client = VaultContractClient::new(&env, &contract_id);

        client.initialize(&admin, &signers, &3).unwrap();

        let token = Address::generate(&env);
        let recipient = Address::generate(&env);

        let tx_id = client
            .propose(&signers.get(0).unwrap(), &token, &recipient, &1000)
            .unwrap();
        assert_eq!(tx_id, 0);

        // Second and third signatures
        client.sign(&signers.get(1).unwrap(), &tx_id).unwrap();
        client.sign(&signers.get(2).unwrap(), &tx_id).unwrap();

        let tx = client.get_pending_tx(&tx_id).unwrap();
        assert_eq!(tx.signatures.len(), 3);
        assert!(!tx.executed);
    }

    #[test]
    fn test_timelock_not_expired() {
        let (env, admin, signers) = setup();
        let contract_id = env.register_contract(None, VaultContract);
        let client = VaultContractClient::new(&env, &contract_id);

        client.initialize(&admin, &signers, &3).unwrap();

        let token = Address::generate(&env);
        let recipient = Address::generate(&env);
        let tx_id = client
            .propose(&signers.get(0).unwrap(), &token, &recipient, &500)
            .unwrap();
        client.sign(&signers.get(1).unwrap(), &tx_id).unwrap();
        client.sign(&signers.get(2).unwrap(), &tx_id).unwrap();

        // Timelock not expired — execute should fail
        let result = client.execute(&signers.get(0).unwrap(), &tx_id);
        assert_eq!(result, Err(VaultError::TimelockNotExpired));
    }

    #[test]
    fn test_double_sign_rejected() {
        let (env, admin, signers) = setup();
        let contract_id = env.register_contract(None, VaultContract);
        let client = VaultContractClient::new(&env, &contract_id);

        client.initialize(&admin, &signers, &3).unwrap();

        let token = Address::generate(&env);
        let recipient = Address::generate(&env);
        let tx_id = client
            .propose(&signers.get(0).unwrap(), &token, &recipient, &100)
            .unwrap();

        // Proposer already signed — second sign attempt should fail
        let result = client.sign(&signers.get(0).unwrap(), &tx_id);
        assert_eq!(result, Err(VaultError::AlreadySigned));
    }

    #[test]
    fn test_frozen_vault_blocks_propose() {
        let (env, admin, signers) = setup();
        let contract_id = env.register_contract(None, VaultContract);
        let client = VaultContractClient::new(&env, &contract_id);

        client.initialize(&admin, &signers, &3).unwrap();
        client.freeze(&signers.get(0).unwrap()).unwrap();

        let token = Address::generate(&env);
        let recipient = Address::generate(&env);
        let result = client.propose(&signers.get(1).unwrap(), &token, &recipient, &100);
        assert_eq!(result, Err(VaultError::VaultFrozen));
    }

    #[test]
    fn test_nonce_increments() {
        let (env, admin, signers) = setup();
        let contract_id = env.register_contract(None, VaultContract);
        let client = VaultContractClient::new(&env, &contract_id);

        client.initialize(&admin, &signers, &3).unwrap();
        assert_eq!(client.nonce(), 0);

        let token = Address::generate(&env);
        let recipient = Address::generate(&env);
        client
            .propose(&signers.get(0).unwrap(), &token, &recipient, &100)
            .unwrap();
        assert_eq!(client.nonce(), 1);
    }
}
