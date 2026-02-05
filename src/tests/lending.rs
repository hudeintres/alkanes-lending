//! Lending contract integration tests
//!
//! Tests for the peer-to-peer lending protocol with two cases:
//! - Case 1: Debitor creates loan request with collateral (3 steps)
//! - Case 2: Creditor offers loan tokens (2 steps)

#![cfg(test)]

use crate::tests::helper::common::calculate_repayment_amount;
use crate::tests::std::lending_contract_build;

use alkanes::indexer::index_block;
use alkanes::precompiled::{alkanes_std_auth_token_build, alkanes_std_owned_token_build};
use alkanes::tests::helpers::{self as alkane_helpers, BinaryAndCellpack, get_last_outpoint_sheet};
use alkanes_support::constants::AUTH_TOKEN_FACTORY_ID;
use alkanes_support::{cellpack::Cellpack, id::AlkaneId};
use anyhow::Result;
use bitcoin::blockdata::transaction::OutPoint;
use bitcoin::{Block, ScriptBuf, Sequence, Witness, TxIn};
#[allow(unused_imports)]
use metashrew_core::{println, stdio::{stdout, Write}};
use protorune::test_helpers::create_block_with_coinbase_tx;
use protorune_support::balance_sheet::BalanceSheetOperations;
use protorune_support::protostone::ProtostoneEdict;
use wasm_bindgen_test::wasm_bindgen_test;

/// Test constants
const COLLATERAL_AMOUNT: u128 = 1_000_000_000; // 1 billion units
const LOAN_AMOUNT: u128 = 500_000_000; // 500 million units
const DURATION_BLOCKS: u128 = 5256; // ~1 month (1/10th of a year)
const APR_500_BPS: u128 = 500; // 5.00% APR

/// Initial token supply for test tokens
const INIT_TOKEN_SUPPLY: u128 = 10_000_000_000_000; // 10 trillion

/// Deployment IDs for lending tests
pub struct LendingDeploymentIds {
    pub lending_contract: AlkaneId,
    pub collateral_token: AlkaneId,
    pub loan_token: AlkaneId,
}

// ============================================================================
// Deployment Helper Functions
// ============================================================================

/// Deploy lending contract and two test tokens using owned_token
/// Returns the block and deployment IDs
fn deploy_lending_with_tokens() -> Result<(Block, LendingDeploymentIds)> {
    alkane_helpers::clear();
    let block_height = 840_000;

    // First, we need to deploy auth_token_factory at the reserved factory ID
    // Then deploy owned_tokens which will use the auth_token_factory
    //
    // Owned token initialization parameters:
    // opcode 0: Initialize { auth_token_units, token_units }
    // This creates auth_token_units of auth token and token_units of the owned token
    //
    // Deployment order and sequence allocation:
    // 1. auth_token_factory at factory space (block=4, tx=AUTH_TOKEN_FACTORY_ID)
    // 2. lending_contract at sequence 1
    // 3. collateral_token at sequence 2 (creates auth token at sequence 3)
    // 4. loan_token at sequence 4 (creates auth token at sequence 5)
    
    let cellpack_pairs: Vec<BinaryAndCellpack> = vec![
        // Deploy auth token factory first (required for owned_token)
        // Uses block: 3 to target factory space, tx: AUTH_TOKEN_FACTORY_ID
        BinaryAndCellpack {
            binary: alkanes_std_auth_token_build::get_bytes(),
            cellpack: Cellpack {
                target: AlkaneId {
                    block: 3,
                    tx: AUTH_TOKEN_FACTORY_ID,
                },
                inputs: vec![100], // no-op opcode
            },
        },
        // Deploy lending contract at sequence 1
        BinaryAndCellpack {
            binary: lending_contract_build::get_bytes(),
            cellpack: Cellpack {
                target: AlkaneId { block: 1, tx: 0 },
                inputs: vec![99], // GetName - just deploy
            },
        },
        // Deploy collateral token (Token A) at sequence 2
        // owned_token: opcode 0 = Initialize { auth_token_units, token_units }
        BinaryAndCellpack {
            binary: alkanes_std_owned_token_build::get_bytes(),
            cellpack: Cellpack {
                target: AlkaneId { block: 1, tx: 0 },
                inputs: vec![
                    0,                  // opcode: Initialize
                    1,                  // auth_token_units (1 auth token)
                    INIT_TOKEN_SUPPLY,  // token_units (initial supply)
                ],
            },
        },
        // Deploy loan token (Token B) at sequence 4
        BinaryAndCellpack {
            binary: alkanes_std_owned_token_build::get_bytes(),
            cellpack: Cellpack {
                target: AlkaneId { block: 1, tx: 0 },
                inputs: vec![
                    0,                  // opcode: Initialize
                    1,                  // auth_token_units (1 auth token)
                    INIT_TOKEN_SUPPLY,  // token_units (initial supply)
                ],
            },
        },
    ];

    let test_block = alkane_helpers::init_with_cellpack_pairs(cellpack_pairs);
    index_block(&test_block, block_height)?;

    // Sequence allocation:
    // auth_token_factory goes to factory space (block=4, tx=AUTH_TOKEN_FACTORY_ID)
    // lending=1, collateralToken=2, authToken1=3, loanToken=4, authToken2=5
    let deployment_ids = LendingDeploymentIds {
        lending_contract: AlkaneId { block: 2, tx: 1 },
        collateral_token: AlkaneId { block: 2, tx: 2 },
        loan_token: AlkaneId { block: 2, tx: 4 },
    };

    Ok((test_block, deployment_ids))
}

// ============================================================================
// Deployment Tests
// ============================================================================

/// Test deploying lending contract with test tokens
/// Deploys auth_token_factory first, then owned_tokens for collateral and loan
#[wasm_bindgen_test]
fn test_deploy_lending_with_tokens() -> Result<()> {
    let (test_block, deployment_ids) = deploy_lending_with_tokens()?;
    
    println!("Lending contract deployed at: {:?}", deployment_ids.lending_contract);
    println!("Collateral token deployed at: {:?}", deployment_ids.collateral_token);
    println!("Loan token deployed at: {:?}", deployment_ids.loan_token);
    
    // Check the balance sheet at the output
    let sheet = get_last_outpoint_sheet(&test_block)?;
    
    // Print all balances at the outpoint for debugging
    println!("Balance sheet: {:?}", sheet.balances());
    
    // Verify we have the tokens
    let collateral_balance = sheet.get(&deployment_ids.collateral_token.into());
    let loan_balance = sheet.get(&deployment_ids.loan_token.into());
    
    println!("Collateral token balance: {}", collateral_balance);
    println!("Loan token balance: {}", loan_balance);
    
    // Verify tokens were minted
    assert_eq!(collateral_balance, INIT_TOKEN_SUPPLY, "Should have initial collateral token supply");
    assert_eq!(loan_balance, INIT_TOKEN_SUPPLY, "Should have initial loan token supply");
    
    println!("Lending with tokens deployment test passed");
    Ok(())
}

// ============================================================================
// Full Loan Lifecycle Test - Case 1
// ============================================================================

/// Test Case 1 Full Lifecycle: 
/// 1. Debitor creates loan request with collateral (InitWithCollateral)
/// 2. Creditor funds the loan (FundLoan)  
/// 3. Debitor claims the loan tokens (ClaimLoan)
/// 4. Debitor repays the loan with interest (RepayLoan)
/// 
/// Asserts token transfers at each step
#[wasm_bindgen_test]
fn test_case1_full_loan_lifecycle() -> Result<()> {
    // ========== SETUP ==========
    let (test_block, deployment_ids) = deploy_lending_with_tokens()?;
    
    let lending_id = deployment_ids.lending_contract;
    let collateral_token = deployment_ids.collateral_token;
    let loan_token = deployment_ids.loan_token;
    
    // Verify initial balances
    let initial_sheet = get_last_outpoint_sheet(&test_block)?;
    let initial_collateral = initial_sheet.get(&collateral_token.into());
    let initial_loan = initial_sheet.get(&loan_token.into());
    
    println!("=== INITIAL STATE ===");
    println!("Initial collateral balance: {}", initial_collateral);
    println!("Initial loan balance: {}", initial_loan);
    
    assert_eq!(initial_collateral, INIT_TOKEN_SUPPLY);
    assert_eq!(initial_loan, INIT_TOKEN_SUPPLY);
    
    // ========== STEP 1: Debitor creates loan request with collateral ==========
    println!("\n=== STEP 1: InitWithCollateral ===");
    
    let init_cellpack = Cellpack {
        target: lending_id.clone(),
        inputs: vec![
            0,                          // opcode: InitWithCollateral
            collateral_token.block,
            collateral_token.tx,
            COLLATERAL_AMOUNT,
            loan_token.block,
            loan_token.tx,
            LOAN_AMOUNT,
            DURATION_BLOCKS,
            APR_500_BPS,
        ],
    };
    
    let mut block1 = create_block_with_coinbase_tx(840_001);
    let outpoint1 = OutPoint {
        txid: test_block.txdata.last().unwrap().compute_txid(),
        vout: 0,
    };
    
    // Use alkane_helpers to create tx with edicts that send tokens to the cellpack
    let txin1 = TxIn {
        previous_output: outpoint1,
        script_sig: ScriptBuf::new(),
        sequence: Sequence::MAX,
        witness: Witness::new(),
    };
    
    block1.txdata.push(
        alkane_helpers::create_multiple_cellpack_with_witness_and_txins_edicts(
            vec![init_cellpack],
            vec![txin1],
            false,
            vec![ProtostoneEdict {
                id: collateral_token.into(),
                amount: COLLATERAL_AMOUNT,
                output: 0, // Will be remapped by the helper
            }],
        ),
    );
    
    index_block(&block1, 840_001)?;
    
    let sheet1 = get_last_outpoint_sheet(&block1)?;
    let collateral_after_init = sheet1.get(&collateral_token.into());
    let loan_after_init = sheet1.get(&loan_token.into());
    
    println!("Collateral after init: {} (deposited {})", collateral_after_init, COLLATERAL_AMOUNT);
    println!("Loan tokens after init: {}", loan_after_init);
    
    // Collateral should be reduced (deposited to contract)
    assert_eq!(
        collateral_after_init, 
        INIT_TOKEN_SUPPLY - COLLATERAL_AMOUNT,
        "Collateral should be deposited to contract"
    );
    // Loan tokens should be unchanged
    assert_eq!(loan_after_init, INIT_TOKEN_SUPPLY, "Loan tokens should be unchanged");
    
    // ========== STEP 2: Creditor funds the loan ==========
    println!("\n=== STEP 2: FundLoan ===");
    
    let fund_cellpack = Cellpack {
        target: lending_id.clone(),
        inputs: vec![2], // opcode: FundLoan
    };
    
    let mut block2 = create_block_with_coinbase_tx(840_002);
    let outpoint2 = OutPoint {
        txid: block1.txdata.last().unwrap().compute_txid(),
        vout: 0,
    };
    
    let txin2 = TxIn {
        previous_output: outpoint2,
        script_sig: ScriptBuf::new(),
        sequence: Sequence::MAX,
        witness: Witness::new(),
    };
    
    block2.txdata.push(
        alkane_helpers::create_multiple_cellpack_with_witness_and_txins_edicts(
            vec![fund_cellpack],
            vec![txin2],
            false,
            vec![ProtostoneEdict {
                id: loan_token.into(),
                amount: LOAN_AMOUNT,
                output: 0,
            }],
        ),
    );
    
    index_block(&block2, 840_002)?;
    
    let sheet2 = get_last_outpoint_sheet(&block2)?;
    let collateral_after_fund = sheet2.get(&collateral_token.into());
    let loan_after_fund = sheet2.get(&loan_token.into());
    
    println!("Collateral after fund: {}", collateral_after_fund);
    println!("Loan tokens after fund: {} (deposited {})", loan_after_fund, LOAN_AMOUNT);
    
    // Collateral should still be reduced
    assert_eq!(
        collateral_after_fund, 
        INIT_TOKEN_SUPPLY - COLLATERAL_AMOUNT,
        "Collateral should still be in contract"
    );
    // Loan tokens should be reduced (deposited to contract)
    assert_eq!(
        loan_after_fund, 
        INIT_TOKEN_SUPPLY - LOAN_AMOUNT,
        "Loan tokens should be deposited to contract"
    );
    
    // ========== STEP 3: Debitor claims the loan ==========
    println!("\n=== STEP 3: ClaimLoan ===");
    
    let claim_cellpack = Cellpack {
        target: lending_id.clone(),
        inputs: vec![3], // opcode: ClaimLoan
    };
    
    let mut block3 = create_block_with_coinbase_tx(840_003);
    let outpoint3 = OutPoint {
        txid: block2.txdata.last().unwrap().compute_txid(),
        vout: 0,
    };
    
    block3.txdata.push(
        alkane_helpers::create_multiple_cellpack_with_witness_and_in(
            Witness::new(),
            vec![claim_cellpack],
            outpoint3,
            false,
        ),
    );
    
    index_block(&block3, 840_003)?;
    
    let sheet3 = get_last_outpoint_sheet(&block3)?;
    let collateral_after_claim = sheet3.get(&collateral_token.into());
    let loan_after_claim = sheet3.get(&loan_token.into());
    
    println!("Collateral after claim: {}", collateral_after_claim);
    println!("Loan tokens after claim: {} (received {})", loan_after_claim, LOAN_AMOUNT);
    
    // Collateral should still be locked
    assert_eq!(
        collateral_after_claim, 
        INIT_TOKEN_SUPPLY - COLLATERAL_AMOUNT,
        "Collateral should still be locked in contract"
    );
    // Debitor should receive loan tokens
    assert_eq!(
        loan_after_claim, 
        INIT_TOKEN_SUPPLY,
        "Debitor should receive loan tokens back"
    );
    
    // ========== STEP 4: Debitor repays the loan ==========
    println!("\n=== STEP 4: RepayLoan ===");
    
    // Calculate repayment amount (principal + interest)
    let repayment_amount = calculate_repayment_amount(LOAN_AMOUNT, APR_500_BPS, DURATION_BLOCKS);
    println!("Repayment amount: {} (principal: {}, interest: {})", 
             repayment_amount, LOAN_AMOUNT, repayment_amount - LOAN_AMOUNT);
    
    let repay_cellpack = Cellpack {
        target: lending_id.clone(),
        inputs: vec![5], // opcode: RepayLoan
    };
    
    let mut block4 = create_block_with_coinbase_tx(840_004);
    let outpoint4 = OutPoint {
        txid: block3.txdata.last().unwrap().compute_txid(),
        vout: 0,
    };
    
    let txin4 = TxIn {
        previous_output: outpoint4,
        script_sig: ScriptBuf::new(),
        sequence: Sequence::MAX,
        witness: Witness::new(),
    };
    
    block4.txdata.push(
        alkane_helpers::create_multiple_cellpack_with_witness_and_txins_edicts(
            vec![repay_cellpack],
            vec![txin4],
            false,
            vec![ProtostoneEdict {
                id: loan_token.into(),
                amount: repayment_amount,
                output: 0,
            }],
        ),
    );
    
    index_block(&block4, 840_004)?;
    
    let sheet4 = get_last_outpoint_sheet(&block4)?;
    let collateral_after_repay = sheet4.get(&collateral_token.into());
    let loan_after_repay = sheet4.get(&loan_token.into());
    
    println!("Collateral after repay: {} (should get {} back)", collateral_after_repay, COLLATERAL_AMOUNT);
    println!("Loan tokens after repay: {}", loan_after_repay);
    
    // Debitor should get collateral back
    assert_eq!(
        collateral_after_repay, 
        INIT_TOKEN_SUPPLY,
        "Debitor should get collateral back after repayment"
    );
    
    // ========== STEP 5: Creditor claims repayment (after duration) ==========
    println!("\n=== STEP 5: ClaimRepayment ===");
    
    // Advance height past duration (start was ~840003, deadline ~841259)
    let claim_height = 840_004 + DURATION_BLOCKS + 10;
    let claim_cellpack = Cellpack {
        target: lending_id.clone(),
        inputs: vec![9], // opcode: ClaimRepayment
    };
    
    // cast to u32 for test helpers (safe as value fits)
    let claim_height_u32: u32 = claim_height.try_into().unwrap();
    let mut block5 = create_block_with_coinbase_tx(claim_height_u32);
    let outpoint5 = OutPoint {
        txid: block4.txdata.last().unwrap().compute_txid(),
        vout: 0,
    };
    
    block5.txdata.push(
        alkane_helpers::create_multiple_cellpack_with_witness_and_in(
            Witness::new(),
            vec![claim_cellpack],
            outpoint5,
            false,
        ),
    );
    
    index_block(&block5, claim_height_u32)?;
    
    let sheet5 = get_last_outpoint_sheet(&block5)?;
    let collateral_after_claim = sheet5.get(&collateral_token.into());
    let loan_after_claim = sheet5.get(&loan_token.into());
    
    println!("Collateral after claim: {}", collateral_after_claim);
    println!("Loan tokens after claim: {} (received {})", loan_after_claim, repayment_amount);
    
    // Collateral stays returned
    assert_eq!(
        collateral_after_claim, 
        INIT_TOKEN_SUPPLY,
        "Collateral remains with debitor/creditor"
    );
    // Creditor gets repayment back (loan tokens restored)
    assert_eq!(
        loan_after_claim, 
        INIT_TOKEN_SUPPLY,
        "Creditor should receive repayment after duration"
    );
    
    println!("\n=== LOAN COMPLETED SUCCESSFULLY ===");
    println!("Final collateral balance: {}", collateral_after_claim);
    println!("Final loan token balance: {}", loan_after_claim);
    
    Ok(())
}

// ============================================================================
// Full Loan Lifecycle Test - Case 2  
// ============================================================================

/// Test Case 2 Full Lifecycle:
/// 1. Creditor creates loan offer with loan tokens (InitWithLoanOffer)
/// 2. Debitor takes loan by providing collateral (TakeLoanWithCollateral)
/// 3. Debitor repays the loan with interest (RepayLoan)
///
/// Asserts token transfers at each step
#[wasm_bindgen_test]
fn test_case2_full_loan_lifecycle() -> Result<()> {
    // ========== SETUP ==========
    let (test_block, deployment_ids) = deploy_lending_with_tokens()?;
    
    let lending_id = deployment_ids.lending_contract;
    let collateral_token = deployment_ids.collateral_token;
    let loan_token = deployment_ids.loan_token;
    
    // Verify initial balances
    let initial_sheet = get_last_outpoint_sheet(&test_block)?;
    let initial_collateral = initial_sheet.get(&collateral_token.into());
    let initial_loan = initial_sheet.get(&loan_token.into());
    
    println!("=== INITIAL STATE ===");
    println!("Initial collateral balance: {}", initial_collateral);
    println!("Initial loan balance: {}", initial_loan);
    
    assert_eq!(initial_collateral, INIT_TOKEN_SUPPLY);
    assert_eq!(initial_loan, INIT_TOKEN_SUPPLY);
    
    // ========== STEP 1: Creditor creates loan offer ==========
    println!("\n=== STEP 1: InitWithLoanOffer ===");
    
    let init_cellpack = Cellpack {
        target: lending_id.clone(),
        inputs: vec![
            1,                          // opcode: InitWithLoanOffer
            collateral_token.block,
            collateral_token.tx,
            COLLATERAL_AMOUNT,
            loan_token.block,
            loan_token.tx,
            LOAN_AMOUNT,
            DURATION_BLOCKS,
            APR_500_BPS,
        ],
    };
    
    let mut block1 = create_block_with_coinbase_tx(840_001);
    let outpoint1 = OutPoint {
        txid: test_block.txdata.last().unwrap().compute_txid(),
        vout: 0,
    };
    
    let txin1 = TxIn {
        previous_output: outpoint1,
        script_sig: ScriptBuf::new(),
        sequence: Sequence::MAX,
        witness: Witness::new(),
    };
    
    block1.txdata.push(
        alkane_helpers::create_multiple_cellpack_with_witness_and_txins_edicts(
            vec![init_cellpack],
            vec![txin1],
            false,
            vec![ProtostoneEdict {
                id: loan_token.into(),
                amount: LOAN_AMOUNT,
                output: 0,
            }],
        ),
    );
    
    index_block(&block1, 840_001)?;
    
    let sheet1 = get_last_outpoint_sheet(&block1)?;
    let collateral_after_init = sheet1.get(&collateral_token.into());
    let loan_after_init = sheet1.get(&loan_token.into());
    
    println!("Collateral after init: {}", collateral_after_init);
    println!("Loan tokens after init: {} (deposited {})", loan_after_init, LOAN_AMOUNT);
    
    // Collateral should be unchanged
    assert_eq!(collateral_after_init, INIT_TOKEN_SUPPLY, "Collateral should be unchanged");
    // Loan tokens should be reduced (deposited to contract)
    assert_eq!(
        loan_after_init, 
        INIT_TOKEN_SUPPLY - LOAN_AMOUNT,
        "Loan tokens should be deposited to contract"
    );
    
    // ========== STEP 2: Debitor takes loan with collateral ==========
    println!("\n=== STEP 2: TakeLoanWithCollateral ===");
    
    let take_cellpack = Cellpack {
        target: lending_id.clone(),
        inputs: vec![4], // opcode: TakeLoanWithCollateral
    };
    
    let mut block2 = create_block_with_coinbase_tx(840_002);
    let outpoint2 = OutPoint {
        txid: block1.txdata.last().unwrap().compute_txid(),
        vout: 0,
    };
    
    let txin2 = TxIn {
        previous_output: outpoint2,
        script_sig: ScriptBuf::new(),
        sequence: Sequence::MAX,
        witness: Witness::new(),
    };
    
    block2.txdata.push(
        alkane_helpers::create_multiple_cellpack_with_witness_and_txins_edicts(
            vec![take_cellpack],
            vec![txin2],
            false,
            vec![ProtostoneEdict {
                id: collateral_token.into(),
                amount: COLLATERAL_AMOUNT,
                output: 0,
            }],
        ),
    );
    
    index_block(&block2, 840_002)?;
    
    let sheet2 = get_last_outpoint_sheet(&block2)?;
    let collateral_after_take = sheet2.get(&collateral_token.into());
    let loan_after_take = sheet2.get(&loan_token.into());
    
    println!("Collateral after take: {} (deposited {})", collateral_after_take, COLLATERAL_AMOUNT);
    println!("Loan tokens after take: {} (received {})", loan_after_take, LOAN_AMOUNT);
    
    // Collateral should be reduced (deposited to contract)
    assert_eq!(
        collateral_after_take, 
        INIT_TOKEN_SUPPLY - COLLATERAL_AMOUNT,
        "Collateral should be deposited to contract"
    );
    // Debitor should receive loan tokens immediately
    assert_eq!(
        loan_after_take, 
        INIT_TOKEN_SUPPLY,
        "Debitor should receive loan tokens"
    );
    
    // ========== STEP 3: Debitor repays the loan ==========
    println!("\n=== STEP 3: RepayLoan ===");
    
    let repayment_amount = calculate_repayment_amount(LOAN_AMOUNT, APR_500_BPS, DURATION_BLOCKS);
    println!("Repayment amount: {} (principal: {}, interest: {})", 
             repayment_amount, LOAN_AMOUNT, repayment_amount - LOAN_AMOUNT);
    
    let repay_cellpack = Cellpack {
        target: lending_id.clone(),
        inputs: vec![5], // opcode: RepayLoan
    };
    
    let mut block3 = create_block_with_coinbase_tx(840_003);
    let outpoint3 = OutPoint {
        txid: block2.txdata.last().unwrap().compute_txid(),
        vout: 0,
    };
    
    let txin3 = TxIn {
        previous_output: outpoint3,
        script_sig: ScriptBuf::new(),
        sequence: Sequence::MAX,
        witness: Witness::new(),
    };
    
    block3.txdata.push(
        alkane_helpers::create_multiple_cellpack_with_witness_and_txins_edicts(
            vec![repay_cellpack],
            vec![txin3],
            false,
            vec![ProtostoneEdict {
                id: loan_token.into(),
                amount: repayment_amount,
                output: 0,
            }],
        ),
    );
    
    index_block(&block3, 840_003)?;
    
    let sheet3 = get_last_outpoint_sheet(&block3)?;
    let collateral_after_repay = sheet3.get(&collateral_token.into());
    let loan_after_repay = sheet3.get(&loan_token.into());
    
    println!("Collateral after repay: {} (should get {} back)", collateral_after_repay, COLLATERAL_AMOUNT);
    println!("Loan tokens after repay: {}", loan_after_repay);
    
    // Debitor should get collateral back
    assert_eq!(
        collateral_after_repay, 
        INIT_TOKEN_SUPPLY,
        "Debitor should get collateral back after repayment"
    );
    
    println!("\n=== LOAN COMPLETED SUCCESSFULLY ===");
    println!("Final collateral balance: {}", collateral_after_repay);
    println!("Final loan token balance: {}", loan_after_repay);
    
    // ========== STEP 4: Creditor claims repayment (after duration) ==========
    println!("\n=== STEP 4: ClaimRepayment ===");
    
    // Advance height past duration (start was ~840002, deadline ~840002+5256)
    let claim_height = 840_003 + DURATION_BLOCKS + 10;
    let claim_cellpack = Cellpack {
        target: lending_id.clone(),
        inputs: vec![9], // opcode: ClaimRepayment
    };
    
    // cast to u32 for test helpers (safe as value fits)
    let claim_height_u32: u32 = claim_height.try_into().unwrap();
    let mut block4 = create_block_with_coinbase_tx(claim_height_u32);
    let outpoint4 = OutPoint {
        txid: block3.txdata.last().unwrap().compute_txid(),
        vout: 0,
    };
    
    block4.txdata.push(
        alkane_helpers::create_multiple_cellpack_with_witness_and_in(
            Witness::new(),
            vec![claim_cellpack],
            outpoint4,
            false,
        ),
    );
    
    index_block(&block4, claim_height_u32)?;
    
    let sheet4 = get_last_outpoint_sheet(&block4)?;
    let collateral_after_claim = sheet4.get(&collateral_token.into());
    let loan_after_claim = sheet4.get(&loan_token.into());
    
    println!("Collateral after claim: {}", collateral_after_claim);
    println!("Loan tokens after claim: {} (received {})", loan_after_claim, repayment_amount);
    
    // Collateral stays returned
    assert_eq!(
        collateral_after_claim, 
        INIT_TOKEN_SUPPLY,
        "Collateral remains with debitor/creditor"
    );
    // Creditor gets repayment back (loan tokens restored)
    assert_eq!(
        loan_after_claim, 
        INIT_TOKEN_SUPPLY,
        "Creditor should receive repayment after duration"
    );
    
    println!("\n=== LOAN COMPLETED SUCCESSFULLY ===");
    println!("Final collateral balance: {}", collateral_after_claim);
    println!("Final loan token balance: {}", loan_after_claim);
    
    Ok(())
}