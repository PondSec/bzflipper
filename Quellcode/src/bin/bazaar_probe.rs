use purse_pilot::bazaar_scanner::{fetch_best_flips, LocalBazaarScanConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("purse_pilot::bazaar_scanner=debug")
        .try_init();

    let config = LocalBazaarScanConfig {
        min_profit_per_unit: 5.0,
        min_total_profit: 15_000.0,
        min_margin_percent: 1.25,
        max_margin_percent: 32.0,
        min_buy_volume: 5_000,
        min_sell_volume: 5_000,
        min_order_count: 20,
        min_moving_week: 250_000,
        max_order_value: 5_000_000,
        max_amount: 71_680,
        price_undercut: 0.1,
        bazaar_tax_rate: 1.25,
        max_concurrent_orders: 6,
        target_profit_per_hour: 2_000_000.0,
        enable_classic_potato_book_flips: true,
        total_capital: 30_000_000,
        active_capital_ratio: 0.78,
        reserve_ratio: 0.22,
        max_items: 8,
        max_capital_per_item: 5_000_000,
        min_roi_percent: 1.0,
        target_roi_percent: 2.0,
        min_volume_value_hour: 5_000_000.0,
        preferred_volume_value_hour: 20_000_000.0,
        market_participation_rate: 0.12,
        conservative_market_participation_rate: 0.06,
        history_window_minutes: 60,
        inventory_free_slots: 30,
        min_free_inventory_slots: 10,
        active_buy_order_count: 0,
        active_sell_order_count: 0,
        inventory_sellable_stacks: 0,
        max_pending_buy_stacks: 6,
        buy_sell_balance_limit: 1.10,
        total_cost_lot_value: 0.0,
        open_buy_capital: 0.0,
        open_sell_value: 0.0,
        max_cost_lot_capital_ratio: 0.35,
        max_open_buy_capital_ratio: 0.35,
        per_item_exposure_cap: 3_000_000,
        min_reprice_profit_improvement: 75_000.0,
        min_reprice_interval_seconds: 240,
        max_reprices_per_item_per_hour: 3,
        reprice_cooldown_seconds: 360,
    };

    let flips = fetch_best_flips(&config, 8).await?;
    let total_risk_adjusted: f64 = flips
        .iter()
        .map(|f| f.risk_adjusted_expected_profit_per_hour)
        .sum();
    let total_expected: f64 = flips.iter().map(|f| f.expected_profit_per_hour).sum();
    println!(
        "candidates={} expected_profit_h={:.0} risk_adjusted_profit_h={:.0}",
        flips.len(),
        total_expected,
        total_risk_adjusted
    );
    for (idx, flip) in flips.iter().enumerate() {
        println!(
            "#{:02} {} amount={} buy={:.1} sell={:.1} profit_unit={:.1} roi={:.2}% vol_value_h={:.0} cycles_h={:.2} cap={:.0} expected_h={:.0} risk_adj_h={:.0} risk={:?} recommendation={:?} notes={}",
            idx + 1,
            flip.product_id,
            flip.amount,
            flip.target_buy_price,
            flip.target_sell_price,
            flip.profit_per_unit,
            flip.roi_after_tax * 100.0,
            flip.volume_value_hour,
            flip.estimated_cycles_per_hour,
            flip.allocated_capital,
            flip.expected_profit_per_hour,
            flip.risk_adjusted_expected_profit_per_hour,
            flip.risk_level,
            flip.recommendation,
            flip.risk_notes.join("|")
        );
    }

    Ok(())
}
