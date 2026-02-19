//! Lending contract integration tests
//!
//! Tests for the peer-to-peer lending protocol (Case 2 only):
//! Creditor offers loan tokens; debitor takes with collateral (2 steps)

#![cfg(test)]

use crate::tests::helper::common::calculate_repayment_amount;
use crate::tests::helper::lending_helpers::{
    self as h, LoanTerms, COLLATERAL_AMOUNT, DEPLOY_HEIGHT, INIT_TOKEN_SUPPLY, LOAN_AMOUNT,
    APR_500_BPS, DURATION_BLOCKS,
};

use alkanes::tests::helpers::get_last_outpoint_sheet;
use alkanes_support::cellpack::Cellpack;
use anyhow::Result;
#[allow(unused_imports)]
use metashrew_core::{println, stdio::{stdout, Write}};
use protorune_support::balance_sheet::BalanceSheetOperations;
use wasm_bindgen_test::wasm_bindgen_test;

/// Contract state constants (mirror contract's internal values)
const STATE_UNINITIALIZED: u128 = 0;
const STATE_WAITING_FOR_DEBITOR_TAKE: u128 = 1;
const STATE_LOAN_ACTIVE: u128 = 2;
const STATE_LOAN_REPAID: u128 = 3;
const STATE_LOAN_DEFAULTED: u128 = 4;

// ============================================================================
// Deployment Tests
// ============================================================================

/// Test deploying lending contract with test tokens.
/// Verifies auth_token_factory, owned_tokens for collateral and loan are deployed
/// and initial supplies are correct.
#[wasm_bindgen_test]
fn test_deploy_lending_with_tokens() -> Result<()> {
    let (deploy_block, ids) = h::deploy_lending_with_tokens()?;

    let sheet = get_last_outpoint_sheet(&deploy_block)?;
    let collateral_balance = sheet.get(&ids.collateral_token.into());
    let loan_balance = sheet.get(&ids.loan_token.into());

    assert_eq!(collateral_balance, INIT_TOKEN_SUPPLY, "Should have initial collateral token supply");
    assert_eq!(loan_balance, INIT_TOKEN_SUPPLY, "Should have initial loan token supply");

    println!("Lending with tokens deployment test passed");
    Ok(())
}

// ============================================================================
// Authorization Tests
// ============================================================================

/// Test that ClaimRepayment reverts if called by non-creditor (no auth token).
///
/// Reaches repaid state via the full lifecycle, then attempts ClaimRepayment
/// from a default outpoint (no auth token balance) and asserts the revert.
#[wasm_bindgen_test]
fn test_claim_repayment_non_creditor_reverts() -> Result<()> {
    let (_repay_block, ids) = h::setup_to_repaid_state()?;
    let lending_id = &ids.lending_contract;

    // Attempt ClaimRepayment from default outpoint (no auth token)
    let bad_claim = Cellpack {
        target: lending_id.clone(),
        inputs: vec![5],
    };
    let block_bad = h::execute_cellpack_no_balance(DEPLOY_HEIGHT + 4, bad_claim)?;

    h::assert_revert(&block_bad, "Auth token is not in incoming alkanes")?;

    println!("ClaimRepayment auth test executed");
    Ok(())
}

// ============================================================================
// Full Loan Lifecycle Test (Case 2 only)
// ============================================================================

/// Test Case 2 Full Lifecycle:
/// 1. Creditor creates loan offer (InitWithLoanOffer opcode 0)
/// 2. Debitor takes with collateral (TakeLoanWithCollateral opcode 1)
/// 3. Debitor repays (RepayLoan opcode 2)
/// 4. Creditor claims repayment (ClaimRepayment opcode 5)
#[wasm_bindgen_test]
fn test_case2_full_loan_lifecycle() -> Result<()> {
    let (take_block, ids) = h::setup_to_active_state()?;
    let lending_id = &ids.lending_contract;
    let terms = LoanTerms::default_from(&ids);

    let repayment_amount = calculate_repayment_amount(LOAN_AMOUNT, APR_500_BPS, DURATION_BLOCKS);
    println!("Repayment amount: {} (principal: {}, interest: {})",
             repayment_amount, LOAN_AMOUNT, repayment_amount - LOAN_AMOUNT);

    // Step 3: Repay
    let repay_block = h::repay_loan(&take_block, DEPLOY_HEIGHT + 3, lending_id, &terms)?;

    let sheet3 = get_last_outpoint_sheet(&repay_block)?;
    let collateral_after_repay = sheet3.get(&ids.collateral_token.into());
    assert_eq!(
        collateral_after_repay, INIT_TOKEN_SUPPLY,
        "Debitor should get collateral back after repayment"
    );

    // Step 4: Creditor claims repayment
    let claim_block = h::claim_repayment(&repay_block, DEPLOY_HEIGHT + 4, lending_id)?;

    let sheet4 = get_last_outpoint_sheet(&claim_block)?;
    let loan_after_claim = sheet4.get(&ids.loan_token.into());
    assert!(
        loan_after_claim >= repayment_amount,
        "Creditor should receive repayment tokens"
    );

    println!("\n=== LOAN COMPLETED SUCCESSFULLY ===");
    Ok(())
}

// ============================================================================
// Loan Default Tests
// ============================================================================

/// End-to-end test for loan default:
/// - Repay after deadline fails
/// - Claim defaulted collateral without auth fails
/// - Creditor claims collateral with auth succeeds
/// - Post-default repay fails
#[wasm_bindgen_test]
fn test_case2_loan_default_claim_collateral() -> Result<()> {
    let (take_block, ids) = h::setup_to_active_state()?;
    let lending_id = &ids.lending_contract;
    let default_height = 845_260u32;

    // Repay after deadline → should fail
    let repay_cellpack = Cellpack { target: lending_id.clone(), inputs: vec![2] };
    let block_repay_fail = h::execute_cellpack_no_balance(default_height, repay_cellpack.clone())?;
    h::assert_revert(&block_repay_fail, "Loan has defaulted - deadline passed")?;

    // ClaimDefaultedCollateral without auth → should fail
    let bad_claim = Cellpack { target: lending_id.clone(), inputs: vec![3] };
    let block_bad_claim = h::execute_cellpack_no_balance(default_height + 1, bad_claim)?;
    h::assert_revert(&block_bad_claim, "Auth token is not in incoming alkanes")?;

    // Creditor claims collateral with auth token (uses take_block outpoint chain)
    let block_claim = h::claim_defaulted_collateral(&take_block, default_height + 2, lending_id)?;

    let sheet = get_last_outpoint_sheet(&block_claim)?;
    assert_eq!(
        sheet.get(&ids.collateral_token.into()), INIT_TOKEN_SUPPLY,
        "Creditor should receive collateral on default"
    );
    assert_eq!(
        sheet.get(&ids.loan_token.into()), INIT_TOKEN_SUPPLY,
        "Debitor keeps loan tokens on default"
    );

    // Post-default repay → should fail (state=DEFAULTED)
    let block_repay_final = h::execute_cellpack_no_balance(default_height + 3, repay_cellpack)?;
    h::assert_revert(&block_repay_final, "No active loan to repay")?;

    println!("Loan default test passed");
    Ok(())
}

// ============================================================================
// Loan Offer Cancellation Tests
// ============================================================================

/// Test successful cancellation of a loan offer by the creditor.
///
/// 1. Init loan offer (deposits loan tokens, receives auth token)
/// 2. Cancel loan offer (sends auth token back, gets loan tokens refunded)
/// 3. Verify loan tokens fully refunded
#[wasm_bindgen_test]
fn test_cancel_loan_offer_success() -> Result<()> {
    let (init_block, ids) = h::setup_to_waiting_state()?;
    let lending_id = &ids.lending_contract;

    // Verify loan tokens were deposited
    let sheet1 = get_last_outpoint_sheet(&init_block)?;
    assert_eq!(
        sheet1.get(&ids.loan_token.into()),
        INIT_TOKEN_SUPPLY - LOAN_AMOUNT,
        "Creditor should have deposited loan tokens into the contract"
    );
    assert_eq!(
        sheet1.get(&(*lending_id).into()), 1,
        "Creditor should have received 1 auth token"
    );

    // Cancel
    let cancel_block = h::cancel_loan_offer(&init_block, DEPLOY_HEIGHT + 2, lending_id)?;

    // Verify refund
    let sheet2 = get_last_outpoint_sheet(&cancel_block)?;
    assert_eq!(
        sheet2.get(&ids.loan_token.into()), INIT_TOKEN_SUPPLY,
        "Creditor should get all loan tokens refunded after cancellation"
    );
    assert_eq!(
        sheet2.get(&ids.collateral_token.into()), INIT_TOKEN_SUPPLY,
        "Collateral tokens should be unchanged (never deposited)"
    );

    println!("\n=== LOAN OFFER CANCELLATION SUCCESSFUL ===");
    Ok(())
}

/// Test that cancelling a loan offer fails when the debitor has already taken.
#[wasm_bindgen_test]
fn test_cancel_loan_offer_fails_after_debitor_takes() -> Result<()> {
    let (take_block, ids) = h::setup_to_active_state()?;

    let cancel_block = h::cancel_loan_offer(&take_block, DEPLOY_HEIGHT + 3, &ids.lending_contract)?;

    h::assert_revert(&cancel_block, "Cannot cancel - loan offer not in cancellable state")?;

    println!("\n=== CANCEL CORRECTLY REJECTED — LOAN ALREADY TAKEN ===");
    Ok(())
}

// ============================================================================
// Insufficient Token Tests
// ============================================================================

/// Test that InitWithLoanOffer reverts when insufficient loan tokens are sent.
/// Uses Edict+Cellpack split to pipe only half the required amount.
#[wasm_bindgen_test]
fn test_init_insufficient_loan() -> Result<()> {
    let (deploy_block, ids) = h::deploy_lending_with_tokens()?;
    let terms = LoanTerms::default_from(&ids);
    let insufficient_amount = LOAN_AMOUNT / 2;

    let init_cellpack = Cellpack {
        target: ids.lending_contract.clone(),
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

    let block = h::execute_cellpack_with_split(
        &deploy_block,
        DEPLOY_HEIGHT + 1,
        init_cellpack,
        ids.loan_token.clone(),
        insufficient_amount,
    )?;

    h::assert_revert_split(&block, "Insufficient tokens")?;

    println!("\n=== INIT CORRECTLY REJECTED — INSUFFICIENT LOAN TOKENS ===");
    Ok(())
}

/// Test that TakeLoanWithCollateral reverts when insufficient collateral is sent.
/// Uses Edict+Cellpack split to pipe only half the required collateral.
#[wasm_bindgen_test]
fn test_take_insufficient_collateral() -> Result<()> {
    let (init_block, ids) = h::setup_to_waiting_state()?;
    let insufficient_collateral = COLLATERAL_AMOUNT / 2;

    let take_cellpack = Cellpack {
        target: ids.lending_contract.clone(),
        inputs: vec![1],
    };

    let block = h::execute_cellpack_with_split(
        &init_block,
        DEPLOY_HEIGHT + 2,
        take_cellpack,
        ids.collateral_token.clone(),
        insufficient_collateral,
    )?;

    h::assert_revert_split(&block, "Insufficient tokens")?;

    println!("\n=== TAKE CORRECTLY REJECTED — INSUFFICIENT COLLATERAL ===");
    Ok(())
}

// ============================================================================
// InitWithLoanOffer Validation Error Tests
// ============================================================================

/// Test that InitWithLoanOffer reverts when collateral_amount is zero.
#[wasm_bindgen_test]
fn test_init_collateral_amount_zero() -> Result<()> {
    let (_deploy_block, ids) = h::deploy_lending_with_tokens()?;
    let mut terms = LoanTerms::default_from(&ids);
    terms.collateral_amount = 0;

    let cellpack = h::build_init_cellpack(&ids.lending_contract, &terms);
    let block = h::execute_cellpack_no_balance(DEPLOY_HEIGHT + 1, cellpack)?;

    h::assert_revert(&block, "Collateral amount cannot be zero")?;
    println!("Init collateral_amount=0 correctly rejected");
    Ok(())
}

/// Test that InitWithLoanOffer reverts when loan_amount is zero.
#[wasm_bindgen_test]
fn test_init_loan_amount_zero() -> Result<()> {
    let (_deploy_block, ids) = h::deploy_lending_with_tokens()?;
    let mut terms = LoanTerms::default_from(&ids);
    terms.loan_amount = 0;

    let cellpack = h::build_init_cellpack(&ids.lending_contract, &terms);
    let block = h::execute_cellpack_no_balance(DEPLOY_HEIGHT + 1, cellpack)?;

    h::assert_revert(&block, "Loan amount cannot be zero")?;
    println!("Init loan_amount=0 correctly rejected");
    Ok(())
}

/// Test that InitWithLoanOffer reverts when duration_blocks is zero.
#[wasm_bindgen_test]
fn test_init_duration_zero() -> Result<()> {
    let (_deploy_block, ids) = h::deploy_lending_with_tokens()?;
    let mut terms = LoanTerms::default_from(&ids);
    terms.duration_blocks = 0;

    let cellpack = h::build_init_cellpack(&ids.lending_contract, &terms);
    let block = h::execute_cellpack_no_balance(DEPLOY_HEIGHT + 1, cellpack)?;

    h::assert_revert(&block, "Duration cannot be zero")?;
    println!("Init duration=0 correctly rejected");
    Ok(())
}

/// Test that InitWithLoanOffer reverts when collateral and loan token are the same.
#[wasm_bindgen_test]
fn test_init_same_collateral_and_loan_token() -> Result<()> {
    let (_deploy_block, ids) = h::deploy_lending_with_tokens()?;
    let mut terms = LoanTerms::default_from(&ids);
    // Set collateral token equal to loan token
    terms.collateral_token = terms.loan_token.clone();

    let cellpack = h::build_init_cellpack(&ids.lending_contract, &terms);
    let block = h::execute_cellpack_no_balance(DEPLOY_HEIGHT + 1, cellpack)?;

    h::assert_revert(&block, "Collateral and loan token cannot be the same")?;
    println!("Init same-token correctly rejected");
    Ok(())
}

/// Test that InitWithLoanOffer reverts when called a second time
/// (contract already initialized via `observe_initialization`).
#[wasm_bindgen_test]
fn test_init_already_initialized() -> Result<()> {
    let (_init_block, ids) = h::setup_to_waiting_state()?;

    // Attempt a second init — should fail at observe_initialization
    let terms = LoanTerms::default_from(&ids);
    let cellpack = h::build_init_cellpack(&ids.lending_contract, &terms);
    let block = h::execute_cellpack_no_balance(DEPLOY_HEIGHT + 2, cellpack)?;

    h::assert_revert(&block, "already initialized")?;
    println!("Init already-initialized correctly rejected");
    Ok(())
}

// ============================================================================
// View Function Tests (Opcodes 90–100)
// ============================================================================

/// Test GetState (opcode 92) returns correct state at every lifecycle stage:
/// UNINITIALIZED → WAITING → ACTIVE → REPAID
#[wasm_bindgen_test]
fn test_get_state_all_lifecycle_stages() -> Result<()> {
    // Uninitialized
    let (deploy_block, ids) = h::deploy_lending_with_tokens()?;
    let lending_id = &ids.lending_contract;

    let data = h::call_view(DEPLOY_HEIGHT + 1, lending_id, 92)?;
    let state = h::read_u128_le(&data, 0);
    assert_eq!(state, STATE_UNINITIALIZED, "State should be UNINITIALIZED after deploy");

    // Waiting
    let terms = LoanTerms::default_from(&ids);
    let init_block = h::init_loan_offer(&deploy_block, DEPLOY_HEIGHT + 2, lending_id, &terms)?;

    let data = h::call_view(DEPLOY_HEIGHT + 3, lending_id, 92)?;
    let state = h::read_u128_le(&data, 0);
    assert_eq!(state, STATE_WAITING_FOR_DEBITOR_TAKE, "State should be WAITING after init");

    // Active
    let take_block = h::take_loan(&init_block, DEPLOY_HEIGHT + 4, lending_id, &terms)?;

    let data = h::call_view(DEPLOY_HEIGHT + 5, lending_id, 92)?;
    let state = h::read_u128_le(&data, 0);
    assert_eq!(state, STATE_LOAN_ACTIVE, "State should be ACTIVE after take");

    // Repaid
    let _repay_block = h::repay_loan(&take_block, DEPLOY_HEIGHT + 6, lending_id, &terms)?;

    let data = h::call_view(DEPLOY_HEIGHT + 7, lending_id, 92)?;
    let state = h::read_u128_le(&data, 0);
    assert_eq!(state, STATE_LOAN_REPAID, "State should be REPAID after repay");

    println!("GetState lifecycle test passed");
    Ok(())
}

/// Test GetState (opcode 92) returns DEFAULTED after creditor claims collateral.
#[wasm_bindgen_test]
fn test_get_state_defaulted() -> Result<()> {
    let (take_block, ids) = h::setup_to_active_state()?;
    let lending_id = &ids.lending_contract;
    let default_height = 845_260u32;

    // Creditor claims defaulted collateral
    let _claim_block = h::claim_defaulted_collateral(&take_block, default_height, lending_id)?;

    let data = h::call_view(default_height + 1, lending_id, 92)?;
    let state = h::read_u128_le(&data, 0);
    assert_eq!(state, STATE_LOAN_DEFAULTED, "State should be DEFAULTED after collateral claim");

    println!("GetState defaulted test passed");
    Ok(())
}

/// Test GetLoanDetails (opcode 90) when contract is uninitialized.
/// Should return only the state (0) with no additional data.
#[wasm_bindgen_test]
fn test_get_loan_details_uninitialized() -> Result<()> {
    let (_deploy_block, ids) = h::deploy_lending_with_tokens()?;
    let lending_id = &ids.lending_contract;

    let data = h::call_view(DEPLOY_HEIGHT + 1, lending_id, 90)?;

    // When uninitialized, data is just the state (16 bytes)
    assert_eq!(data.len(), 16, "Uninitialized loan details should be 16 bytes (state only)");
    let state = h::read_u128_le(&data, 0);
    assert_eq!(state, STATE_UNINITIALIZED, "State should be UNINITIALIZED");

    println!("GetLoanDetails uninitialized test passed");
    Ok(())
}

/// Test GetLoanDetails (opcode 90) in WAITING state.
/// Should return state + collateral_token (block, tx) + collateral_amount +
/// loan_token (block, tx) + loan_amount + duration + APR = 9 × u128 = 144 bytes.
#[wasm_bindgen_test]
fn test_get_loan_details_waiting() -> Result<()> {
    let (_init_block, ids) = h::setup_to_waiting_state()?;
    let lending_id = &ids.lending_contract;

    let data = h::call_view(DEPLOY_HEIGHT + 2, lending_id, 90)?;

    // state + collateral_token.block + collateral_token.tx + collateral_amount
    // + loan_token.block + loan_token.tx + loan_amount + duration + apr
    // = 9 × 16 = 144 bytes
    assert_eq!(data.len(), 144, "Waiting loan details should be 144 bytes");

    let state = h::read_u128_le(&data, 0);
    assert_eq!(state, STATE_WAITING_FOR_DEBITOR_TAKE);

    let coll_block = h::read_u128_le(&data, 16);
    let coll_tx = h::read_u128_le(&data, 32);
    assert_eq!(coll_block, ids.collateral_token.block);
    assert_eq!(coll_tx, ids.collateral_token.tx);

    let coll_amount = h::read_u128_le(&data, 48);
    assert_eq!(coll_amount, COLLATERAL_AMOUNT);

    let loan_block = h::read_u128_le(&data, 64);
    let loan_tx = h::read_u128_le(&data, 80);
    assert_eq!(loan_block, ids.loan_token.block);
    assert_eq!(loan_tx, ids.loan_token.tx);

    let loan_amount = h::read_u128_le(&data, 96);
    assert_eq!(loan_amount, LOAN_AMOUNT);

    let duration = h::read_u128_le(&data, 112);
    assert_eq!(duration, DURATION_BLOCKS);

    let apr = h::read_u128_le(&data, 128);
    assert_eq!(apr, APR_500_BPS);

    println!("GetLoanDetails waiting test passed");
    Ok(())
}

/// Test GetLoanDetails (opcode 90) in ACTIVE state.
/// Should include deadline and start_block (2 extra u128 fields = 176 bytes total).
#[wasm_bindgen_test]
fn test_get_loan_details_active() -> Result<()> {
    let (_take_block, ids) = h::setup_to_active_state()?;
    let lending_id = &ids.lending_contract;

    let data = h::call_view(DEPLOY_HEIGHT + 3, lending_id, 90)?;

    // 9 base fields + deadline + start_block = 11 × 16 = 176 bytes
    assert_eq!(data.len(), 176, "Active loan details should be 176 bytes");

    let state = h::read_u128_le(&data, 0);
    assert_eq!(state, STATE_LOAN_ACTIVE);

    // Deadline: take happened at DEPLOY_HEIGHT + 2, deadline = (DEPLOY_HEIGHT+2) + DURATION_BLOCKS
    let deadline = h::read_u128_le(&data, 144);
    let expected_deadline = (DEPLOY_HEIGHT as u128 + 2) + DURATION_BLOCKS;
    assert_eq!(deadline, expected_deadline, "Deadline should be take_height + duration");

    // Start block: take happened at DEPLOY_HEIGHT + 2
    let start_block = h::read_u128_le(&data, 160);
    assert_eq!(start_block, DEPLOY_HEIGHT as u128 + 2, "Start block should be take height");

    println!("GetLoanDetails active test passed");
    Ok(())
}

/// Test GetRepaymentAmount (opcode 91) in ACTIVE state.
/// Should return the calculated repayment amount (principal + interest).
#[wasm_bindgen_test]
fn test_get_repayment_amount_active() -> Result<()> {
    let (_take_block, ids) = h::setup_to_active_state()?;
    let lending_id = &ids.lending_contract;

    let data = h::call_view(DEPLOY_HEIGHT + 3, lending_id, 91)?;

    assert_eq!(data.len(), 16, "Repayment amount should be 16 bytes (u128)");
    let returned_amount = h::read_u128_le(&data, 0);
    let expected_amount = calculate_repayment_amount(LOAN_AMOUNT, APR_500_BPS, DURATION_BLOCKS);

    assert_eq!(
        returned_amount, expected_amount,
        "Repayment amount should match calculated value"
    );
    println!("GetRepaymentAmount returned: {} (expected: {})", returned_amount, expected_amount);
    Ok(())
}

/// Test GetRepaymentAmount (opcode 91) when no active loan.
/// Should return 0.
#[wasm_bindgen_test]
fn test_get_repayment_amount_no_active_loan() -> Result<()> {
    let (_deploy_block, ids) = h::deploy_lending_with_tokens()?;
    let lending_id = &ids.lending_contract;

    // Uninitialized state
    let data = h::call_view(DEPLOY_HEIGHT + 1, lending_id, 91)?;
    let amount = h::read_u128_le(&data, 0);
    assert_eq!(amount, 0, "Repayment amount should be 0 when uninitialized");

    println!("GetRepaymentAmount no-active-loan test passed");
    Ok(())
}

/// Test GetRepaymentAmount (opcode 91) in WAITING state (not yet active).
/// Should return 0.
#[wasm_bindgen_test]
fn test_get_repayment_amount_waiting() -> Result<()> {
    let (_init_block, ids) = h::setup_to_waiting_state()?;
    let lending_id = &ids.lending_contract;

    let data = h::call_view(DEPLOY_HEIGHT + 2, lending_id, 91)?;
    let amount = h::read_u128_le(&data, 0);
    assert_eq!(amount, 0, "Repayment amount should be 0 when waiting (not active)");

    println!("GetRepaymentAmount waiting test passed");
    Ok(())
}

/// Test GetTimeRemaining (opcode 93) during active loan.
/// Should return the number of blocks until the deadline.
#[wasm_bindgen_test]
fn test_get_time_remaining_active() -> Result<()> {
    let (_take_block, ids) = h::setup_to_active_state()?;
    let lending_id = &ids.lending_contract;

    // Take happened at DEPLOY_HEIGHT + 2, deadline = (DEPLOY_HEIGHT + 2) + DURATION_BLOCKS
    // Query at DEPLOY_HEIGHT + 3, remaining = deadline - (DEPLOY_HEIGHT + 3)
    let query_height = DEPLOY_HEIGHT + 3;
    let data = h::call_view(query_height, lending_id, 93)?;
    let remaining = h::read_u128_le(&data, 0);

    let expected_deadline = (DEPLOY_HEIGHT as u128 + 2) + DURATION_BLOCKS;
    let expected_remaining = expected_deadline - query_height as u128;

    assert_eq!(
        remaining, expected_remaining,
        "Time remaining should be deadline - current_block"
    );
    println!("GetTimeRemaining returned: {} blocks (expected: {})", remaining, expected_remaining);
    Ok(())
}

/// Test GetTimeRemaining (opcode 93) when deadline has passed.
/// Should return 0.
#[wasm_bindgen_test]
fn test_get_time_remaining_expired() -> Result<()> {
    let (_take_block, ids) = h::setup_to_active_state()?;
    let lending_id = &ids.lending_contract;

    // Query well past the deadline
    let expired_height = 850_000u32;
    let data = h::call_view(expired_height, lending_id, 93)?;
    let remaining = h::read_u128_le(&data, 0);

    assert_eq!(remaining, 0, "Time remaining should be 0 after deadline");

    println!("GetTimeRemaining expired test passed");
    Ok(())
}

/// Test GetTimeRemaining (opcode 93) when no active loan.
/// Should return 0.
#[wasm_bindgen_test]
fn test_get_time_remaining_no_active_loan() -> Result<()> {
    let (_deploy_block, ids) = h::deploy_lending_with_tokens()?;
    let lending_id = &ids.lending_contract;

    let data = h::call_view(DEPLOY_HEIGHT + 1, lending_id, 93)?;
    let remaining = h::read_u128_le(&data, 0);
    assert_eq!(remaining, 0, "Time remaining should be 0 when uninitialized");

    println!("GetTimeRemaining no-active-loan test passed");
    Ok(())
}

/// Test GetName (opcode 99) and GetSymbol (opcode 100).
/// The lending contract does not set a name or symbol, so both should return
/// empty data.
#[wasm_bindgen_test]
fn test_get_name_and_symbol() -> Result<()> {
    let (_deploy_block, ids) = h::deploy_lending_with_tokens()?;
    let lending_id = &ids.lending_contract;

    let name_data = h::call_view(DEPLOY_HEIGHT + 1, lending_id, 99)?;
    let symbol_data = h::call_view(DEPLOY_HEIGHT + 2, lending_id, 100)?;

    // Contract never calls set_name_and_symbol, so both are empty
    let name = String::from_utf8(name_data.clone()).unwrap_or_default();
    let symbol = String::from_utf8(symbol_data.clone()).unwrap_or_default();

    println!("GetName returned: {:?} ({} bytes)", name, name_data.len());
    println!("GetSymbol returned: {:?} ({} bytes)", symbol, symbol_data.len());

    // Name and symbol are not set in the lending contract, so they should be empty
    assert!(name_data.is_empty() || name.is_empty(), "Name should be empty (not set by lending contract)");
    assert!(symbol_data.is_empty() || symbol.is_empty(), "Symbol should be empty (not set by lending contract)");

    println!("GetName and GetSymbol test passed");
    Ok(())
}