//! Common helper functions for lending contract tests

#![allow(dead_code)]

/// APR precision constant (matches contract)
pub const APR_PRECISION: u128 = 10000;

/// Blocks per year approximation (matches contract)
pub const BLOCKS_PER_YEAR: u128 = 52560;

/// Calculate expected repayment amount (principal + interest)
/// Matches the contract's calculation logic
pub fn calculate_repayment_amount(
    principal: u128,
    apr: u128,
    duration_blocks: u128,
) -> u128 {
    let interest = principal * apr * duration_blocks / (APR_PRECISION * BLOCKS_PER_YEAR);
    principal + interest
}
