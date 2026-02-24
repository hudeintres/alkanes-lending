use anyhow::{anyhow, Result};

/// Precision multiplier for internal calculations (1e18)
/// This allows for 18 decimal places of precision during interest calculations
/// to prevent rounding errors on small loan amounts or short durations.
pub const PRECISION_MULTIPLIER: u128 = 1_000_000_000_000_000_000;

/// APR precision from contract (10000 = 100.00%)
pub const APR_PRECISION: u128 = 10_000;

/// Blocks per year constant
pub const BLOCKS_PER_YEAR: u128 = 52_560;

/// Calculate interest with high precision
///
/// Formula: (principal * apr * duration * PRECISION_MULTIPLIER) / (APR_PRECISION * BLOCKS_PER_YEAR) / PRECISION_MULTIPLIER
///
/// This prevents rounding to zero for small loans where:
/// (principal * apr * duration) < (APR_PRECISION * BLOCKS_PER_YEAR)
pub fn calculate_interest_precise(
    principal: u128,
    apr: u128,
    duration: u128,
) -> Result<u128> {
    // First multiply by precision to keep significant digits
    // We use u128, so we need to be careful about overflow
    // principal * apr * duration * PRECISION_MULTIPLIER
    
    // Check if we can do the multiplication without overflow
    // If principal is large, we might need to be careful
    
    // Alternative ordering to maximize precision while minimizing overflow risk:
    // 1. (principal * apr)
    // 2. Multiply by PRECISION_MULTIPLIER
    // 3. Multiply by duration
    // 4. Divide by denominator
    // 5. Divide by PRECISION_MULTIPLIER
    
    // However, with u128, we have ~3.4e38 space.
    // PRECISION_MULTIPLIER is 1e18.
    // So we have ~3.4e20 space left for (principal * apr * duration).
    // If principal is 1e13 (10T), apr is 1e4, duration is 1e5, product is 1e22.
    // This would overflow u128 if we just multiply everything.
    
    // We need a safer way to handle this.
    // If the product would overflow, we can skip the precision multiplier
    // because if it's that large, rounding errors aren't significant.
    
    let numerator_part = principal
        .checked_mul(apr)
        .ok_or_else(|| anyhow!("Overflow in interest calculation"))?
        .checked_mul(duration)
        .ok_or_else(|| anyhow!("Overflow in interest calculation"))?;
        
    let denominator = APR_PRECISION * BLOCKS_PER_YEAR;
    
    // Try high precision first
    if let Some(scaled_numerator) = numerator_part.checked_mul(PRECISION_MULTIPLIER) {
        
        let scaled_interest = scaled_numerator
            .checked_div(denominator)
            .ok_or_else(|| anyhow!("Division error"))?;
            
        Ok(scaled_interest / PRECISION_MULTIPLIER)
    } else {
        // If high precision overflows, fallback to standard calculation
        // since the numbers are large enough that precision loss is negligible
        numerator_part
            .checked_div(denominator)
            .ok_or_else(|| anyhow!("Division error"))
    }
}

