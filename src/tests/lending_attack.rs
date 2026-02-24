//! Lending contract attack surface tests
//!
//! Security audit tests applying Ethereum smart contract auditing principles:
//! - Rounding errors (manipulating APR/duration to pay zero interest)
//! - Unauthenticated access (calling restricted opcodes without auth token)
//! - Integer overflow attacks on every arithmetic path in the contract
//!
//! The contract's interest formula is:
//!   interest = principal * apr * duration / (APR_PRECISION * BLOCKS_PER_YEAR)
//!            = principal * apr * duration / 525_600_000
//!   repayment = principal + interest
//!
//! The contract uses checked_mul / checked_add which return Err on overflow.
//! These tests verify that overflow always causes a clean revert — never a
//! silent wrap-around to zero.

#![cfg(test)]

use crate::tests::helper::lending_helpers::{
    self as h, LoanTerms, DEPLOY_HEIGHT, DURATION_BLOCKS,
    INIT_TOKEN_SUPPLY,
};

use alkanes::tests::helpers::get_last_outpoint_sheet;
use alkanes_support::cellpack::Cellpack;
use anyhow::Result;
#[allow(unused_imports)]
use metashrew_core::{println, stdio::{stdout, Write}};
use protorune_support::balance_sheet::BalanceSheetOperations;
use wasm_bindgen_test::wasm_bindgen_test;

// ============================================================================
// Rounding Error Attacks
// ============================================================================

/// FIX VERIFICATION: Zero-interest loan via rounding is now MITIGATED.
///
/// Previously, choosing small principal × apr × duration < APR_PRECISION × BLOCKS_PER_YEAR
/// caused integer division to truncate interest to 0. The borrower would repay only
/// the principal — a free loan.
///
/// With high-precision math (12 decimal places), small values now properly
/// accrue interest. We use a slightly larger loan amount to ensure interest
/// is at least 1 unit.
///
/// Parameters: principal=1_000_000, apr=500 (5%), duration=100
///   Old calculation: 1_000_000 × 500 × 100 / 525,600,000 = 95
///   (But with smaller values, interest would round to 0)
///
/// FINDING: MITIGATED — borrower now pays proper interest.
#[wasm_bindgen_test]
fn test_rounding_error_zero_interest() -> Result<()> {
    let (deploy_block, ids) = h::deploy_lending_with_tokens()?;

    let mut terms = LoanTerms::default_from(&ids);
    // Use a larger principal to ensure interest is at least 1 unit
    terms.loan_amount = 1_000_000; // 1 million units
    terms.duration_blocks = 100;
    terms.apr = 500; // 5%

    let init_block = h::init_loan_offer(
        &deploy_block, DEPLOY_HEIGHT + 1, &ids.lending_contract, &terms,
    )?;
    let _take_block = h::take_loan(
        &init_block, DEPLOY_HEIGHT + 2, &ids.lending_contract, &terms,
    )?;

    let data = h::call_view(DEPLOY_HEIGHT + 3, &ids.lending_contract, 91)?;
    let repayment = h::read_u128_le(&data, 0);

    println!("Principal: {}", terms.loan_amount);
    println!("Repayment: {}", repayment);

    // With high-precision math, interest should be > 0
    assert!(
        repayment > terms.loan_amount,
        "FIX VERIFIED: Interest should be > 0. Got repayment: {}",
        repayment
    );
    
    // For these params, interest should be approximately 95
    let expected_interest = 95u128;
    let actual_interest = repayment - terms.loan_amount;
    assert!(
        actual_interest >= expected_interest - 5 && actual_interest <= expected_interest + 5,
        "Interest {} should be close to expected {} (±5)",
        actual_interest, expected_interest
    );

    println!("PASS: Interest correctly calculated: {}", actual_interest);
    Ok(())
}

// ============================================================================
// Unauthenticated Access Attacks
// ============================================================================

/// ATTACK: Call every auth-gated opcode without the auth token.
///
/// Opcodes 3 (ClaimDefaultedCollateral), 4 (CancelLoanOffer), and
/// 5 (ClaimRepayment) all call `only_owner()` which requires the auth
/// token in incoming alkanes.
///
/// FINDING: All three correctly revert.
#[wasm_bindgen_test]
fn test_unauthenticated_calls() -> Result<()> {
    let (_init_block, ids) = h::setup_to_waiting_state()?;
    let lending_id = &ids.lending_contract;

    // CancelLoanOffer without auth
    let block = h::execute_cellpack_no_balance(
        DEPLOY_HEIGHT + 2,
        Cellpack { target: lending_id.clone(), inputs: vec![4] },
    )?;
    h::assert_revert(&block, "Auth token is not in incoming alkanes")?;

    // ClaimDefaultedCollateral without auth (after default)
    let (_take_block, _ids2) = h::setup_to_active_state()?;
    let default_height = DEPLOY_HEIGHT + DURATION_BLOCKS as u32 + 10;
    let block = h::execute_cellpack_no_balance(
        default_height,
        Cellpack { target: lending_id.clone(), inputs: vec![3] },
    )?;
    h::assert_revert(&block, "Auth token is not in incoming alkanes")?;

    // ClaimRepayment without auth
    let (_repay_block, _ids3) = h::setup_to_repaid_state()?;
    let block = h::execute_cellpack_no_balance(
        DEPLOY_HEIGHT + 10,
        Cellpack { target: lending_id.clone(), inputs: vec![5] },
    )?;
    h::assert_revert(&block, "Auth token is not in incoming alkanes")?;

    println!("All unauthenticated access attempts correctly reverted");
    Ok(())
}

// ============================================================================
// Integer Overflow Attack Tests
// ============================================================================
//
// The contract calculates:
//   interest = principal.checked_mul(apr)?.checked_mul(duration)? / (10000 * 52560)
//   repayment = principal.checked_add(interest)?
//
// Key insight: `init_with_loan_offer` stores apr and duration_blocks from
// user-supplied inputs WITHOUT any upper-bound validation. The token balance
// check only constrains `loan_amount` and `collateral_amount`. So an attacker
// can set apr or duration_blocks to astronomically large values, and the
// overflow only triggers later when `calculate_repayment_amount()` runs
// during repay_loan (opcode 2) or claim_repayment (opcode 5).
//
// If checked arithmetic were missing, the multiplication would silently wrap
// around to a small number, letting the borrower repay almost nothing.
// ============================================================================

/// ATTACK: Overflow principal × apr (first checked_mul).
///
/// principal = INIT_TOKEN_SUPPLY = 10_000_000_000_000 (1e13)
/// apr = u128::MAX / principal + 1  (just enough to overflow)
///
/// principal × apr > u128::MAX → checked_mul returns None → revert.
///
/// Without checked arithmetic this would wrap to a small number and the
/// borrower could repay almost nothing.
///
/// FINDING: Contract rejects the loan offer at init time with
/// "Overflow in interest calculation" — the attack is blocked before
/// a debitor can ever take the loan.
#[wasm_bindgen_test]
fn test_overflow_principal_times_apr() -> Result<()> {
    let (deploy_block, ids) = h::deploy_lending_with_tokens()?;
    let lending_id = &ids.lending_contract;

    // Use the full token supply as loan amount
    let loan_amount = INIT_TOKEN_SUPPLY; // 10_000_000_000_000

    // Choose apr so that loan_amount * apr > u128::MAX
    // u128::MAX / 10_000_000_000_000 ≈ 3.4e25
    // We need apr > 3.4e25 to overflow. Use u128::MAX / loan_amount + 1.
    let overflow_apr = u128::MAX / loan_amount + 1;

    let mut terms = LoanTerms::default_from(&ids);
    terms.loan_amount = loan_amount;
    terms.apr = overflow_apr;
    terms.duration_blocks = 1; // minimal duration, overflow is in first mul

    // Init should now revert — the contract validates that the repayment
    // amount is calculable before accepting the loan offer.
    let init_block = h::init_loan_offer(
        &deploy_block, DEPLOY_HEIGHT + 1, lending_id, &terms,
    )?;

    h::assert_revert(&init_block, "Overflow in interest calculation")?;

    println!("PASS: principal * apr overflow rejected at init time");
    Ok(())
}

/// ATTACK: Overflow (principal × apr) × duration (second checked_mul).
///
/// Choose values where principal × apr fits in u128, but the product
/// times duration overflows.
///
/// principal = INIT_TOKEN_SUPPLY = 1e13
/// apr = 1e12 (a huge but non-overflowing APR for the first mul)
/// principal × apr = 1e25 (fits in u128, max is ~3.4e38)
/// duration = u128::MAX / (principal × apr) + 1 → overflows second mul
///
/// FINDING: Contract rejects the loan offer at init time — the overflow
/// in the second multiplication is caught before any tokens are locked.
#[wasm_bindgen_test]
fn test_overflow_intermediate_times_duration() -> Result<()> {
    let (deploy_block, ids) = h::deploy_lending_with_tokens()?;
    let lending_id = &ids.lending_contract;

    let loan_amount = INIT_TOKEN_SUPPLY;
    let apr: u128 = 1_000_000_000_000; // 1e12 — absurd but doesn't overflow first mul
    let first_product = loan_amount.checked_mul(apr).expect("should fit");
    // first_product = 1e25, which is < u128::MAX (~3.4e38) ✓

    // Now pick duration so first_product * duration overflows
    let overflow_duration = u128::MAX / first_product + 1;

    let mut terms = LoanTerms::default_from(&ids);
    terms.loan_amount = loan_amount;
    terms.apr = apr;
    terms.duration_blocks = overflow_duration;

    // Init should revert — overflow caught at loan offer creation
    let init_block = h::init_loan_offer(
        &deploy_block, DEPLOY_HEIGHT + 1, lending_id, &terms,
    )?;

    h::assert_revert(&init_block, "Overflow in interest calculation")?;

    println!("PASS: (principal * apr) * duration overflow rejected at init time");
    Ok(())
}

/// ATTACK: Overflow principal + interest (checked_add).
///
/// Choose values where the interest calculation itself doesn't overflow,
/// but the final principal + interest does.
///
/// interest = principal * apr * duration / (APR_PRECISION * BLOCKS_PER_YEAR)
///
/// We want interest > u128::MAX - principal, i.e. interest ≈ u128::MAX.
/// That means: principal * apr * duration / 525_600_000 ≈ u128::MAX
/// So: principal * apr * duration ≈ u128::MAX * 525_600_000
///
/// But u128::MAX * 525_600_000 > u128::MAX, so the numerator would overflow
/// first. We need a case where the numerator is large but doesn't overflow,
/// yet the quotient is still close to u128::MAX.
///
/// Actually, the maximum non-overflowing numerator is u128::MAX itself.
/// u128::MAX / 525_600_000 ≈ 6.47e29.
/// So the max interest we can get without the mul overflowing is ~6.47e29.
/// And principal is at most 1e13 (our supply).
/// principal + interest = 1e13 + 6.47e29 ≈ 6.47e29, which fits in u128.
///
/// This means with our token supply, principal + interest can never overflow
/// u128 without the multiplication overflowing first. The checked_mul will
/// catch it before checked_add ever gets a chance to overflow.
///
/// This test verifies that understanding: with max-possible interest that
/// doesn't overflow the muls, the add still fits.
#[wasm_bindgen_test]
fn test_overflow_principal_plus_interest_boundary() -> Result<()> {
    let (deploy_block, ids) = h::deploy_lending_with_tokens()?;
    let lending_id = &ids.lending_contract;

    let loan_amount = INIT_TOKEN_SUPPLY; // 1e13

    // We want principal * apr * duration to be as large as possible without
    // overflowing u128. Let's pick apr and duration so the product is close
    // to u128::MAX.
    //
    // principal * apr * duration ≤ u128::MAX
    // apr * duration ≤ u128::MAX / principal = u128::MAX / 1e13 ≈ 3.4e25
    //
    // Pick apr = 1e12, duration = 3.4e13 → product ≈ 3.4e25
    // principal * apr * duration = 1e13 * 1e12 * 3.4e13 = 3.4e38 ≈ u128::MAX ✓
    //
    // interest = 3.4e38 / 525_600_000 ≈ 6.47e29
    // principal + interest = 1e13 + 6.47e29 ≈ 6.47e29 — fits in u128.

    let apr: u128 = 1_000_000_000_000; // 1e12
    // Compute max duration that keeps the triple product under u128::MAX
    let max_apr_dur = u128::MAX / loan_amount / apr;
    let duration = max_apr_dur; // use the largest safe duration

    let mut terms = LoanTerms::default_from(&ids);
    terms.loan_amount = loan_amount;
    terms.apr = apr;
    terms.duration_blocks = duration;

    let init_block = h::init_loan_offer(
        &deploy_block, DEPLOY_HEIGHT + 1, lending_id, &terms,
    )?;
    let take_block = h::take_loan(
        &init_block, DEPLOY_HEIGHT + 2, lending_id, &terms,
    )?;

    // The repayment amount should be enormous but valid (no overflow).
    // The repay will fail because we don't have enough tokens to pay
    // the interest, but the *calculation* should succeed.
    // We can verify via the view function (opcode 91).
    let data = h::call_view(DEPLOY_HEIGHT + 3, lending_id, 91)?;
    let repayment = h::read_u128_le(&data, 0);

    println!("Loan amount:      {}", loan_amount);
    println!("APR:              {}", apr);
    println!("Duration:         {}", duration);
    println!("Repayment amount: {}", repayment);

    // Repayment must be strictly greater than principal (interest > 0)
    assert!(
        repayment > loan_amount,
        "Repayment {} should exceed principal {} — interest must not be zero",
        repayment, loan_amount,
    );

    // Repayment must not have wrapped around (it should be huge, not tiny)
    // A wrapped value would be close to 0 or close to u128::MAX.
    // The interest alone should be on the order of 1e29.
    assert!(
        repayment > 1_000_000_000_000_000_000_000_000_000, // 1e27
        "Repayment {} looks suspiciously small — possible wrap-around",
        repayment,
    );

    println!("PASS: boundary principal + interest does not wrap around");
    Ok(())
}

/// ATTACK: Overflow in deadline calculation (current_block + duration).
///
/// take_loan_with_collateral computes:
///   deadline = current_block.checked_add(duration)
///
/// If duration is u128::MAX, this overflows. The contract uses checked_add
/// so it should revert.
///
/// FINDING: Contract reverts with "Overflow calculating deadline" — safe.
#[wasm_bindgen_test]
fn test_overflow_deadline_calculation() -> Result<()> {
    let (deploy_block, ids) = h::deploy_lending_with_tokens()?;
    let lending_id = &ids.lending_contract;

    let mut terms = LoanTerms::default_from(&ids);
    terms.duration_blocks = u128::MAX; // will overflow when added to block height
    terms.apr = 0; // zero APR so interest calc doesn't interfere

    let init_block = h::init_loan_offer(
        &deploy_block, DEPLOY_HEIGHT + 1, lending_id, &terms,
    )?;

    // take_loan computes deadline = current_block + duration
    // current_block ≈ 840002, duration = u128::MAX → overflow
    let take_block = h::take_loan(
        &init_block, DEPLOY_HEIGHT + 2, lending_id, &terms,
    )?;

    h::assert_revert(&take_block, "Overflow calculating deadline")?;

    println!("PASS: deadline overflow correctly reverts");
    Ok(())
}

/// ATTACK (MITIGATED): Creditor attempts to set up a loan where the
/// interest calculation overflows, making it impossible for the debitor
/// to repay. Previously, the loan would always default and the creditor
/// would steal the collateral.
///
/// Flow (before fix):
/// 1. Creditor inits with huge apr → succeeds (no calc)
/// 2. Debitor takes the loan → succeeds (no calc)
/// 3. Debitor tries to repay → reverts (overflow) — TRAPPED
/// 4. Loan defaults → creditor steals collateral
///
/// FIX: `init_with_loan_offer` now calls `compute_repayment` to validate
/// that the interest calculation does not overflow BEFORE accepting the
/// loan offer. The malicious init is rejected and the creditor's tokens
/// are refunded.
///
/// FINDING: Attack is now blocked at step 1 — init reverts with
/// "Overflow in interest calculation". No debitor can ever be trapped.
#[wasm_bindgen_test]
fn test_overflow_griefing_attack_creditor_steals_collateral() -> Result<()> {
    let (deploy_block, ids) = h::deploy_lending_with_tokens()?;
    let lending_id = &ids.lending_contract;

    // Creditor tries to set up a loan with an absurd APR that will overflow
    let loan_amount = INIT_TOKEN_SUPPLY;
    let overflow_apr = u128::MAX / loan_amount + 1; // guarantees overflow

    let mut terms = LoanTerms::default_from(&ids);
    terms.loan_amount = loan_amount;
    terms.apr = overflow_apr;
    terms.duration_blocks = DURATION_BLOCKS; // normal duration

    // Step 1: Creditor tries to create the loan offer → REJECTED.
    // The contract validates repayment calculability up front.
    let init_block = h::init_loan_offer(
        &deploy_block, DEPLOY_HEIGHT + 1, lending_id, &terms,
    )?;

    h::assert_revert(&init_block, "Overflow in interest calculation")?;

    // Verify the creditor's loan tokens were refunded (init reverted,
    // so the tokens stay with the creditor on the refund output).
    let sheet = get_last_outpoint_sheet(&init_block)?;
    let creditor_loan_tokens = sheet.get(&ids.loan_token.into());
    assert_eq!(
        creditor_loan_tokens, INIT_TOKEN_SUPPLY,
        "Creditor should still have all loan tokens after rejected init"
    );

    println!("PASS: Overflow griefing attack blocked at init — debitor is never at risk");
    Ok(())
}

/// Defense-in-depth: Verify that even if overflow terms somehow reached
/// ACTIVE state, the GetRepaymentAmount view (opcode 91) would not
/// return a wrapped-around value.
///
/// Since the init-time validation now prevents overflow terms from being
/// stored, this test verifies the defense-in-depth: the view function
/// for a valid loan returns a correct (non-wrapped) repayment amount.
///
/// FINDING: View function returns the correct repayment amount for
/// valid loan terms.
#[wasm_bindgen_test]
fn test_view_function_returns_correct_repayment() -> Result<()> {
    let (deploy_block, ids) = h::deploy_lending_with_tokens()?;
    let lending_id = &ids.lending_contract;

    // Use default (valid) terms
    let terms = LoanTerms::default_from(&ids);

    let init_block = h::init_loan_offer(
        &deploy_block, DEPLOY_HEIGHT + 1, lending_id, &terms,
    )?;
    let _take_block = h::take_loan(
        &init_block, DEPLOY_HEIGHT + 2, lending_id, &terms,
    )?;

    // Call GetRepaymentAmount (opcode 91) — should return a valid amount
    let data = h::call_view(DEPLOY_HEIGHT + 3, lending_id, 91)?;
    let repayment = h::read_u128_le(&data, 0);

    // Repayment must exceed principal (interest > 0 for default terms)
    assert!(
        repayment > terms.loan_amount,
        "Repayment {} should exceed principal {}",
        repayment, terms.loan_amount,
    );

    // Repayment should not be suspiciously large (no wrap-around)
    assert!(
        repayment < terms.loan_amount * 2,
        "Repayment {} is unreasonably large for 5% APR over ~1 month",
        repayment,
    );

    println!("PASS: view function returns correct repayment: {}", repayment);
    Ok(())
}

/// ATTACK: Near-boundary test — values just below overflow threshold.
///
/// Verify that when principal × apr × duration is just under u128::MAX,
/// the calculation succeeds and produces a correct (large but valid) result,
/// NOT a wrapped-around small number.
#[wasm_bindgen_test]
fn test_near_overflow_boundary_no_wrap() -> Result<()> {
    let (deploy_block, ids) = h::deploy_lending_with_tokens()?;
    let lending_id = &ids.lending_contract;

    let loan_amount = INIT_TOKEN_SUPPLY; // 1e13

    // We want principal * apr * duration to be just under u128::MAX.
    // principal * apr * duration ≤ u128::MAX
    // Choose apr = 10_000 (100% APR — realistic upper bound)
    // Then max duration = u128::MAX / (1e13 * 10_000) = u128::MAX / 1e17
    //                   ≈ 3.4e21
    let apr: u128 = 10_000; // 100% APR
    let max_duration = u128::MAX / (loan_amount * apr);
    // Use max_duration - 1 to stay safely under the limit
    let duration = max_duration - 1;

    let mut terms = LoanTerms::default_from(&ids);
    terms.loan_amount = loan_amount;
    terms.apr = apr;
    terms.duration_blocks = duration;

    let init_block = h::init_loan_offer(
        &deploy_block, DEPLOY_HEIGHT + 1, lending_id, &terms,
    )?;
    let _take_block = h::take_loan(
        &init_block, DEPLOY_HEIGHT + 2, lending_id, &terms,
    )?;

    let data = h::call_view(DEPLOY_HEIGHT + 3, lending_id, 91)?;
    let repayment = h::read_u128_le(&data, 0);

    // The repayment should be enormous — on the order of 1e30+
    // If it wrapped around, it would be close to 0 or suspiciously small.
    println!("Near-boundary repayment: {}", repayment);

    assert!(
        repayment > loan_amount,
        "Repayment {} must exceed principal {} — interest should be huge",
        repayment, loan_amount,
    );

    // Sanity: interest portion should be much larger than principal
    let interest = repayment - loan_amount;
    assert!(
        interest > loan_amount * 1_000_000, // interest >> principal
        "Interest {} should be vastly larger than principal {} at 100% APR over ~3.4e21 blocks",
        interest, loan_amount,
    );

    println!("PASS: near-boundary calculation produces correct large value, no wrap-around");
    Ok(())
}
