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