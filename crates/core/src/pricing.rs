use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tariff {
    pub effective_at_ms: i64,
    pub microcurrency_per_kwh: i64,
    pub currency: String,
}

pub fn parse_price(price: &str) -> Result<i64> {
    let (whole, fraction) = price.split_once('.').unwrap_or((price, ""));
    ensure!(
        !whole.is_empty()
            && whole.bytes().all(|b| b.is_ascii_digit())
            && fraction.len() <= 6
            && fraction.bytes().all(|b| b.is_ascii_digit()),
        "price must be a nonnegative decimal with at most six fractional digits"
    );
    let whole: i64 = whole.parse()?;
    ensure!(whole <= 1_000_000, "price exceeds supported range");
    let frac: i64 = if fraction.is_empty() {
        0
    } else {
        fraction.parse()?
    };
    Ok(whole * 1_000_000 + frac * 10_i64.pow(6 - fraction.len() as u32))
}
