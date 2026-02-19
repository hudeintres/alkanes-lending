//! Lending contract test helpers
//!
//! Reusable building blocks for lending contract integration tests.
//! Each helper encapsulates a logical operation (deploy, init, take, repay, etc.)
//! so tests read as a sequence of high-level steps.

#![allow(dead_code)]

use crate::tests::helper::common::calculate_repayment_amount;
use crate::tests::std::lending_contract_build;

use alkanes::indexer::index_block;
use alkanes::precompiled::{alkanes_std_auth_token_build, alkanes_std_owned_token_build};
use alkanes::tests::helpers::{self as alkane_helpers, BinaryAndCellpack};
use alkanes_support::constants::AUTH_TOKEN_FACTORY_ID;
use alkanes_support::trace::TraceResponse;
use alkanes_support::{cellpack::Cellpack, id::AlkaneId};
use anyhow::Result;
use bitcoin::blockdata::transaction::OutPoint;
use bitcoin::{Block, ScriptBuf, Sequence, TxIn, Witness};
use protorune::test_helpers::create_block_with_coinbase_tx;
use protorune_support::protostone::ProtostoneEdict;

// ============================================================================
// Constants
// ============================================================================

/// Default test loan parameters
pub const COLLATERAL_AMOUNT: u128 = 1_000_000_000; // 1 billion units
pub const LOAN_AMOUNT: u128 = 500_000_000; // 500 million units
pub const DURATION_BLOCKS: u128 = 5256; // ~1 month (1/10th of a year)
pub const APR_500_BPS: u128 = 500; // 5.00% APR

/// Initial token supply for test tokens
pub const INIT_TOKEN_SUPPLY: u128 = 10_000_000_000_000; // 10 trillion

/// First block height used for deployment
pub const DEPLOY_HEIGHT: u32 = 840_000;

// ============================================================================
// Deployment IDs
// ============================================================================

/// Deployment IDs produced by [`deploy_lending_with_tokens`].
pub struct LendingDeploymentIds {
    pub lending_contract: AlkaneId,
    pub collateral_token: AlkaneId,
    pub loan_token: AlkaneId,
}

// ============================================================================
// Loan term parameters
// ============================================================================

/// Parameters that define a loan offer.
/// Passed to [`init_loan_offer`] so tests can override defaults.
pub struct LoanTerms {
    pub collateral_token: AlkaneId,
    pub collateral_amount: u128,
    pub loan_token: AlkaneId,
    pub loan_amount: u128,
    pub duration_blocks: u128,
    pub apr: u128,
}

impl LoanTerms {
    /// Build default terms from deployment IDs using the module-level constants.
    pub fn default_from(ids: &LendingDeploymentIds) -> Self {
        Self {
            collateral_token: ids.collateral_token.clone(),
            collateral_amount: COLLATERAL_AMOUNT,
            loan_token: ids.loan_token.clone(),
            loan_amount: LOAN_AMOUNT,
            duration_blocks: DURATION_BLOCKS,
            apr: APR_500_BPS,
        }
    }
}

// ============================================================================
// Low-level helpers
// ============================================================================

/// Create a [`TxIn`] that spends vout 0 of the last transaction in `block`.
pub fn txin_from_last_tx(block: &Block) -> TxIn {
    let outpoint = OutPoint {
        txid: block.txdata.last().unwrap().compute_txid(),
        vout: 0,
    };
    TxIn {
        previous_output: outpoint,
        script_sig: ScriptBuf::new(),
        sequence: Sequence::MAX,
        witness: Witness::new(),
    }
}

/// Create a block, add a cellpack transaction with edicts, index it, and return it.
///
/// This is the most common pattern in the tests: build a new block at `height`,
/// attach a transaction that spends vout 0 of the last tx in `prev_block`,
/// include the given `cellpack` and `edicts`, then index.
pub fn execute_cellpack_with_edicts(
    prev_block: &Block,
    height: u32,
    cellpack: Cellpack,
    edicts: Vec<ProtostoneEdict>,
) -> Result<Block> {
    let txin = txin_from_last_tx(prev_block);
    let mut block = create_block_with_coinbase_tx(height);
    block.txdata.push(
        alkane_helpers::create_multiple_cellpack_with_witness_and_txins_edicts(
            vec![cellpack],
            vec![txin],
            false,
            edicts,
        ),
    );
    index_block(&block, height)?;
    Ok(block)
}

/// Execute a cellpack from a default (empty) outpoint — no real token balance.
/// Used for calls that are expected to revert.
pub fn execute_cellpack_no_balance(
    height: u32,
    cellpack: Cellpack,
) -> Result<Block> {
    let mut block = create_block_with_coinbase_tx(height);
    block.txdata.push(
        alkane_helpers::create_multiple_cellpack_with_witness_and_in(
            Witness::new(),
            vec![cellpack],
            OutPoint::default(),
            false,
        ),
    );
    index_block(&block, height)?;
    Ok(block)
}

/// Execute a cellpack where the token input is split via an Edict so that only
/// `token_amount` of `token_id` reaches the contract call. Remaining tokens go
/// to a separate output. Returns the indexed block.
pub fn execute_cellpack_with_split(
    prev_block: &Block,
    height: u32,
    cellpack: Cellpack,
    token_id: AlkaneId,
    token_amount: u128,
) -> Result<Block> {
    let outpoint = OutPoint {
        txid: prev_block.txdata.last().unwrap().compute_txid(),
        vout: 0,
    };
    let mut block = create_block_with_coinbase_tx(height);
    block.txdata.push(
        alkane_helpers::create_multiple_cellpack_with_witness_and_in_with_edicts_and_leftovers(
            Witness::new(),
            vec![
                alkane_helpers::CellpackOrEdict::Edict(vec![ProtostoneEdict {
                    id: token_id.into(),
                    amount: token_amount,
                    output: 0,
                }]),
                alkane_helpers::CellpackOrEdict::Cellpack(cellpack),
            ],
            outpoint,
            false,
            true,
        ),
    );
    index_block(&block, height)?;
    Ok(block)
}

/// Get the protostone vout for `assert_revert_context` on a standard
/// 2-output transaction (txout + OP_RETURN). The single protostone is at vout 3.
pub const PROTOSTONE_VOUT: u32 = 3;

/// Get the protostone vout for the cellpack in a split transaction
/// (3 outputs + edict protostone + cellpack protostone). The cellpack is at vout 5.
pub const SPLIT_CELLPACK_VOUT: u32 = 5;

/// Build an [`OutPoint`] pointing to the protostone of the last tx in `block`.
pub fn protostone_outpoint(block: &Block, vout: u32) -> OutPoint {
    OutPoint {
        txid: block.txdata.last().unwrap().compute_txid(),
        vout,
    }
}

/// Assert that the last tx in `block` reverted at the standard protostone vout
/// with a message containing `expected_msg`.
pub fn assert_revert(block: &Block, expected_msg: &str) -> Result<()> {
    alkane_helpers::assert_revert_context(
        &protostone_outpoint(block, PROTOSTONE_VOUT),
        expected_msg,
    )
}

/// Assert revert for a split-transaction (cellpack protostone at vout 5).
pub fn assert_revert_split(block: &Block, expected_msg: &str) -> Result<()> {
    alkane_helpers::assert_revert_context(
        &protostone_outpoint(block, SPLIT_CELLPACK_VOUT),
        expected_msg,
    )
}

// ============================================================================
// High-level lending operations
// ============================================================================

/// Deploy lending contract, auth-token factory, and two test tokens
/// (collateral + loan). Returns the genesis block and deployment IDs.
pub fn deploy_lending_with_tokens() -> Result<(Block, LendingDeploymentIds)> {
    alkane_helpers::clear();

    let cellpack_pairs: Vec<BinaryAndCellpack> = vec![
        // Auth token factory at reserved factory ID
        BinaryAndCellpack {
            binary: alkanes_std_auth_token_build::get_bytes(),
            cellpack: Cellpack {
                target: AlkaneId {
                    block: 3,
                    tx: AUTH_TOKEN_FACTORY_ID,
                },
                inputs: vec![100],
            },
        },
        // Lending contract → sequence 1
        BinaryAndCellpack {
            binary: lending_contract_build::get_bytes(),
            cellpack: Cellpack {
                target: AlkaneId { block: 1, tx: 0 },
                inputs: vec![99],
            },
        },
        // Collateral token → sequence 2 (auth at 3)
        BinaryAndCellpack {
            binary: alkanes_std_owned_token_build::get_bytes(),
            cellpack: Cellpack {
                target: AlkaneId { block: 1, tx: 0 },
                inputs: vec![0, 1, INIT_TOKEN_SUPPLY],
            },
        },
        // Loan token → sequence 4 (auth at 5)
        BinaryAndCellpack {
            binary: alkanes_std_owned_token_build::get_bytes(),
            cellpack: Cellpack {
                target: AlkaneId { block: 1, tx: 0 },
                inputs: vec![0, 1, INIT_TOKEN_SUPPLY],
            },
        },
    ];

    let test_block = alkane_helpers::init_with_cellpack_pairs(cellpack_pairs);
    index_block(&test_block, DEPLOY_HEIGHT)?;

    let ids = LendingDeploymentIds {
        lending_contract: AlkaneId { block: 2, tx: 1 },
        collateral_token: AlkaneId { block: 2, tx: 2 },
        loan_token: AlkaneId { block: 2, tx: 4 },
    };

    Ok((test_block, ids))
}

/// Creditor creates a loan offer (opcode 0).
///
/// Sends `terms.loan_amount` of loan tokens to the contract and receives an
/// auth token back. Returns the indexed block.
pub fn init_loan_offer(
    prev_block: &Block,
    height: u32,
    lending_id: &AlkaneId,
    terms: &LoanTerms,
) -> Result<Block> {
    let cellpack = Cellpack {
        target: lending_id.clone(),
        inputs: vec![
            0,
            terms.collateral_token.block,
            terms.collateral_token.tx,
            terms.collateral_amount,
            terms.loan_token.block,
            terms.loan_token.tx,
            terms.loan_amount,
            terms.duration_blocks,
            terms.apr,
        ],
    };
    let edicts = vec![ProtostoneEdict {
        id: terms.loan_token.clone().into(),
        amount: terms.loan_amount,
        output: 0,
    }];
    execute_cellpack_with_edicts(prev_block, height, cellpack, edicts)
}

/// Debitor takes the loan by providing collateral (opcode 1).
///
/// Sends `terms.collateral_amount` of collateral tokens and receives the loan
/// tokens. Returns the indexed block.
pub fn take_loan(
    prev_block: &Block,
    height: u32,
    lending_id: &AlkaneId,
    terms: &LoanTerms,
) -> Result<Block> {
    let cellpack = Cellpack {
        target: lending_id.clone(),
        inputs: vec![1],
    };
    let edicts = vec![ProtostoneEdict {
        id: terms.collateral_token.clone().into(),
        amount: terms.collateral_amount,
        output: 0,
    }];
    execute_cellpack_with_edicts(prev_block, height, cellpack, edicts)
}

/// Debitor repays the loan (opcode 2).
///
/// Sends the full repayment amount (principal + interest) in loan tokens.
/// Returns the indexed block.
pub fn repay_loan(
    prev_block: &Block,
    height: u32,
    lending_id: &AlkaneId,
    terms: &LoanTerms,
) -> Result<Block> {
    let repayment_amount =
        calculate_repayment_amount(terms.loan_amount, terms.apr, terms.duration_blocks);
    let cellpack = Cellpack {
        target: lending_id.clone(),
        inputs: vec![2],
    };
    let edicts = vec![ProtostoneEdict {
        id: terms.loan_token.clone().into(),
        amount: repayment_amount,
        output: 0,
    }];
    execute_cellpack_with_edicts(prev_block, height, cellpack, edicts)
}

/// Creditor claims repayment after loan is repaid (opcode 5).
///
/// Sends the auth token (1 unit of lending contract's self-token) to prove
/// ownership. Returns the indexed block.
pub fn claim_repayment(
    prev_block: &Block,
    height: u32,
    lending_id: &AlkaneId,
) -> Result<Block> {
    let cellpack = Cellpack {
        target: lending_id.clone(),
        inputs: vec![5],
    };
    let edicts = vec![ProtostoneEdict {
        id: lending_id.clone().into(),
        amount: 1,
        output: 0,
    }];
    execute_cellpack_with_edicts(prev_block, height, cellpack, edicts)
}

/// Creditor claims collateral after loan default (opcode 3).
///
/// Sends the auth token to prove ownership. Returns the indexed block.
pub fn claim_defaulted_collateral(
    prev_block: &Block,
    height: u32,
    lending_id: &AlkaneId,
) -> Result<Block> {
    let cellpack = Cellpack {
        target: lending_id.clone(),
        inputs: vec![3],
    };
    let edicts = vec![ProtostoneEdict {
        id: lending_id.clone().into(),
        amount: 1,
        output: 0,
    }];
    execute_cellpack_with_edicts(prev_block, height, cellpack, edicts)
}

/// Creditor cancels the loan offer (opcode 4).
///
/// Sends the auth token to prove ownership. Returns the indexed block.
pub fn cancel_loan_offer(
    prev_block: &Block,
    height: u32,
    lending_id: &AlkaneId,
) -> Result<Block> {
    let cellpack = Cellpack {
        target: lending_id.clone(),
        inputs: vec![4],
    };
    let edicts = vec![ProtostoneEdict {
        id: lending_id.clone().into(),
        amount: 1,
        output: 0,
    }];
    execute_cellpack_with_edicts(prev_block, height, cellpack, edicts)
}

// ============================================================================
// Composite setup helpers
// ============================================================================

/// Deploy + init loan offer. Returns the block after init and the IDs.
pub fn setup_to_waiting_state() -> Result<(Block, LendingDeploymentIds)> {
    let (deploy_block, ids) = deploy_lending_with_tokens()?;
    let terms = LoanTerms::default_from(&ids);
    let init_block = init_loan_offer(&deploy_block, DEPLOY_HEIGHT + 1, &ids.lending_contract, &terms)?;
    Ok((init_block, ids))
}

/// Deploy + init + take. Returns the block after take and the IDs.
/// State is `STATE_LOAN_ACTIVE`.
pub fn setup_to_active_state() -> Result<(Block, LendingDeploymentIds)> {
    let (init_block, ids) = setup_to_waiting_state()?;
    let terms = LoanTerms::default_from(&ids);
    let take_block = take_loan(&init_block, DEPLOY_HEIGHT + 2, &ids.lending_contract, &terms)?;
    Ok((take_block, ids))
}

/// Deploy + init + take + repay. Returns the block after repay and the IDs.
/// State is `STATE_LOAN_REPAID`.
pub fn setup_to_repaid_state() -> Result<(Block, LendingDeploymentIds)> {
    let (take_block, ids) = setup_to_active_state()?;
    let terms = LoanTerms::default_from(&ids);
    let repay_block = repay_loan(&take_block, DEPLOY_HEIGHT + 3, &ids.lending_contract, &terms)?;
    Ok((repay_block, ids))
}

// ============================================================================
// View-call helpers
// ============================================================================

/// Call a view function (opcode 90–100) on the lending contract.
///
/// View calls don't need token transfers, so we use a default outpoint.
/// Returns the indexed block (the trace can be inspected for response data).
pub fn call_view(
    height: u32,
    lending_id: &AlkaneId,
    opcode: u128,
) -> Result<Block> {
    let cellpack = Cellpack {
        target: lending_id.clone(),
        inputs: vec![opcode],
    };
    execute_cellpack_no_balance(height, cellpack)
}

/// Extract the response data bytes from a successful (non-reverting) call.
///
/// Uses `assert_return_context` to verify the call succeeded and returns
/// the raw `data` bytes from the `ExtendedCallResponse`.
pub fn get_response_data(block: &Block) -> Result<Vec<u8>> {
    let outpoint = protostone_outpoint(block, PROTOSTONE_VOUT);
    let data = alkane_helpers::assert_return_context(&outpoint, |trace_response: TraceResponse| {
        Ok(trace_response.inner.data.clone())
    })?;
    Ok(data)
}

/// Call a view function and return its response data bytes.
pub fn call_view_and_get_data(
    height: u32,
    lending_id: &AlkaneId,
    opcode: u128,
) -> Result<Vec<u8>> {
    let block = call_view(height, lending_id, opcode)?;
    get_response_data(&block)
}

/// Parse a single `u128` value from little-endian response data.
pub fn parse_u128(data: &[u8]) -> u128 {
    assert!(data.len() >= 16, "Expected at least 16 bytes, got {}", data.len());
    u128::from_le_bytes(data[..16].try_into().unwrap())
}

/// Decoded loan details returned by opcode 90.
#[derive(Debug)]
pub struct LoanDetails {
    pub state: u128,
    pub collateral_token: Option<AlkaneId>,
    pub collateral_amount: Option<u128>,
    pub loan_token: Option<AlkaneId>,
    pub loan_amount: Option<u128>,
    pub duration_blocks: Option<u128>,
    pub apr: Option<u128>,
    pub repayment_deadline: Option<u128>,
    pub loan_start_block: Option<u128>,
}

/// Parse the binary blob returned by `GetLoanDetails` (opcode 90).
///
/// Layout (all little-endian u128):
/// - state (always present)
/// - If state != 0 (UNINITIALIZED):
///   - collateral_token.block, collateral_token.tx
///   - collateral_amount
///   - loan_token.block, loan_token.tx
///   - loan_amount
///   - duration_blocks
///   - apr
/// - If state == 2 (LOAN_ACTIVE):
///   - repayment_deadline
///   - loan_start_block
pub fn parse_loan_details(data: &[u8]) -> LoanDetails {
    let state = parse_u128(&data[0..16]);
    if state == 0 {
        return LoanDetails {
            state,
            collateral_token: None,
            collateral_amount: None,
            loan_token: None,
            loan_amount: None,
            duration_blocks: None,
            apr: None,
            repayment_deadline: None,
            loan_start_block: None,
        };
    }

    let mut offset = 16;
    let ct_block = parse_u128(&data[offset..offset + 16]); offset += 16;
    let ct_tx = parse_u128(&data[offset..offset + 16]); offset += 16;
    let collateral_amount = parse_u128(&data[offset..offset + 16]); offset += 16;
    let lt_block = parse_u128(&data[offset..offset + 16]); offset += 16;
    let lt_tx = parse_u128(&data[offset..offset + 16]); offset += 16;
    let loan_amount = parse_u128(&data[offset..offset + 16]); offset += 16;
    let duration_blocks = parse_u128(&data[offset..offset + 16]); offset += 16;
    let apr = parse_u128(&data[offset..offset + 16]); offset += 16;

    let (repayment_deadline, loan_start_block) = if state == 2 && data.len() >= offset + 32 {
        let deadline = parse_u128(&data[offset..offset + 16]); offset += 16;
        let start = parse_u128(&data[offset..offset + 16]);
        (Some(deadline), Some(start))
    } else {
        (None, None)
    };

    LoanDetails {
        state,
        collateral_token: Some(AlkaneId { block: ct_block, tx: ct_tx }),
        collateral_amount: Some(collateral_amount),
        loan_token: Some(AlkaneId { block: lt_block, tx: lt_tx }),
        loan_amount: Some(loan_amount),
        duration_blocks: Some(duration_blocks),
        apr: Some(apr),
        repayment_deadline,
        loan_start_block,
    }
}
