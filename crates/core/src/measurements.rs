pub fn imbalance(currents: &[i64; 6]) -> (i64, u32) {
    let total: i64 = currents.iter().sum();
    if total <= 0 {
        return (0, 0);
    }
    let spread = currents.iter().max().unwrap() - currents.iter().min().unwrap();
    let max = currents
        .iter()
        .map(|c| (c * 6 - total).abs())
        .max()
        .unwrap();
    (spread, (max * 100 / total) as u32)
}
