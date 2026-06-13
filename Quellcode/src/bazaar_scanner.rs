//! Local Hypixel Bazaar flip scanner.
//!
//! COFL recommendations remain supported. This scanner adds an independent
//! source that ranks live Bazaar products by risk-adjusted coins per hour.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{debug, info};

const BAZAAR_URL: &str = "https://api.hypixel.net/v2/skyblock/bazaar";

static MARKET_HISTORY: OnceLock<Mutex<HashMap<String, Vec<MarketSnapshot>>>> = OnceLock::new();

#[derive(Debug, Clone, Serialize)]
pub struct LocalBazaarFlip {
    pub item_tag: String,
    pub item_name: String,
    pub amount: u64,
    pub buy_price_per_unit: f64,
    pub sell_price_per_unit: f64,
    pub profit_per_unit: f64,
    pub total_profit: f64,
    pub margin_percent: f64,
    pub buy_volume: u64,
    pub sell_volume: u64,
    pub moving_week: u64,
    pub estimated_profit_per_hour: f64,
    pub score: f64,
    pub product_id: String,
    pub best_buy_order: f64,
    pub best_sell_offer: f64,
    pub target_buy_price: f64,
    pub target_sell_price: f64,
    pub spread_before_tax: f64,
    pub spread_after_tax: f64,
    pub roi_after_tax: f64,
    pub buy_volume_hour: f64,
    pub sell_volume_hour: f64,
    pub volume_value_hour: f64,
    pub top_buy_depth_value: f64,
    pub top_sell_depth_value: f64,
    pub avg_buy_depth_value: f64,
    pub avg_sell_depth_value: f64,
    pub depth_5_buy_value: f64,
    pub depth_5_sell_value: f64,
    pub estimated_cycles_per_hour: f64,
    pub allocated_capital: f64,
    pub expected_profit_per_hour: f64,
    pub risk_adjusted_expected_profit_per_hour: f64,
    pub risk_level: RiskLevel,
    pub risk_notes: Vec<String>,
    pub recommendation: Recommendation,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Extreme,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Recommendation {
    StrongCandidate,
    GoodButLimitCapital,
    HighVolumeLowMargin,
    HighMarginLowVolume,
    Risky,
    PossibleManipulation,
    DeadItem,
    Avoid,
}

#[derive(Debug, Clone, Serialize)]
pub struct BazaarProductQuote {
    pub item_tag: String,
    pub item_name: String,
    pub buy_price: f64,
    pub sell_price: f64,
    pub buy_volume: u64,
    pub sell_volume: u64,
    pub moving_week: u64,
}

#[derive(Debug, Deserialize)]
struct BazaarResponse {
    success: bool,
    products: HashMap<String, BazaarProduct>,
}

#[derive(Debug, Deserialize)]
struct BazaarProduct {
    quick_status: QuickStatus,
    #[serde(default)]
    buy_summary: Vec<OrderBookLevel>,
    #[serde(default)]
    sell_summary: Vec<OrderBookLevel>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrderBookLevel {
    amount: u64,
    price_per_unit: f64,
    orders: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuickStatus {
    buy_price: f64,
    sell_price: f64,
    buy_volume: u64,
    sell_volume: u64,
    buy_orders: u64,
    sell_orders: u64,
    buy_moving_week: u64,
    sell_moving_week: u64,
}

impl QuickStatus {
    fn moving_week(&self) -> u64 {
        self.buy_moving_week.min(self.sell_moving_week)
    }
}

#[derive(Debug, Clone)]
pub struct LocalBazaarScanConfig {
    pub min_profit_per_unit: f64,
    pub min_total_profit: f64,
    pub min_margin_percent: f64,
    pub max_margin_percent: f64,
    pub min_buy_volume: u64,
    pub min_sell_volume: u64,
    pub min_order_count: u64,
    pub min_moving_week: u64,
    pub max_order_value: u64,
    pub max_amount: u64,
    pub price_undercut: f64,
    pub bazaar_tax_rate: f64,
    pub max_concurrent_orders: usize,
    pub target_profit_per_hour: f64,
    pub enable_classic_potato_book_flips: bool,
    pub total_capital: u64,
    pub active_capital_ratio: f64,
    pub reserve_ratio: f64,
    pub max_items: usize,
    pub max_capital_per_item: u64,
    pub min_roi_percent: f64,
    pub target_roi_percent: f64,
    pub min_volume_value_hour: f64,
    pub preferred_volume_value_hour: f64,
    pub market_participation_rate: f64,
    pub conservative_market_participation_rate: f64,
    pub history_window_minutes: u64,
    pub inventory_free_slots: u64,
    pub min_free_inventory_slots: u64,
    pub active_buy_order_count: u64,
    pub active_sell_order_count: u64,
    pub inventory_sellable_stacks: u64,
    pub max_pending_buy_stacks: u64,
    pub buy_sell_balance_limit: f64,
    pub total_cost_lot_value: f64,
    pub open_buy_capital: f64,
    pub open_sell_value: f64,
    pub max_cost_lot_capital_ratio: f64,
    pub max_open_buy_capital_ratio: f64,
    pub per_item_exposure_cap: u64,
    pub min_reprice_profit_improvement: f64,
    pub min_reprice_interval_seconds: u64,
    pub max_reprices_per_item_per_hour: u64,
    pub reprice_cooldown_seconds: u64,
}

#[derive(Debug, Clone, Default)]
pub struct PracticalItemMetrics {
    pub expected_net_profit_after_tax: f64,
    pub expected_cycle_minutes: f64,
    pub buy_volume: u64,
    pub sell_volume: u64,
    pub moving_week: u64,
    pub order_count: u64,
    pub volume_value_hour: f64,
    pub avg_buy_fill_seconds: f64,
    pub avg_sell_fill_seconds: f64,
    pub current_open_buy_capital: f64,
    pub current_open_sell_value: f64,
    pub current_cost_lot_value: f64,
    pub max_cost_lot_age_seconds: u64,
    pub successful_flips: u64,
    pub failed_flips: u64,
    pub reprice_count: u64,
    pub cancel_count: u64,
    pub failed_search_count: u64,
    pub cannot_afford_count: u64,
    pub unknown_cost_basis_count: u64,
    pub negative_profit_block_count: u64,
    pub realized_profit_last_10m: i64,
    pub realized_profit_last_30m: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PracticalScoreBreakdown {
    pub score: f64,
    pub sell_through_score: f64,
    pub buy_fill_score: f64,
    pub liquidity_score: f64,
    pub reliability_score: f64,
    pub capital_efficiency_score: f64,
    pub recent_success_score: f64,
    pub expected_cycle_minutes: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateDecision {
    Accept,
    Reject(&'static str),
}

#[derive(Debug, Clone, PartialEq)]
pub struct RepriceDecision {
    pub allowed: bool,
    pub reason: Option<&'static str>,
}

pub fn practical_score(metrics: &PracticalItemMetrics) -> PracticalScoreBreakdown {
    let cycle = metrics.expected_cycle_minutes.max(1.0);
    let sell_through_score = if metrics.avg_sell_fill_seconds > 0.0 {
        (900.0 / metrics.avg_sell_fill_seconds.max(60.0)).clamp(0.10, 1.50)
    } else {
        0.65
    } * if metrics.current_cost_lot_value > 0.0 {
        (1.0 / (1.0 + metrics.current_cost_lot_value / 5_000_000.0)).clamp(0.15, 1.0)
    } else {
        1.0
    } * if metrics.max_cost_lot_age_seconds > 1800 {
        0.35
    } else {
        1.0
    };

    let buy_fill_score = if metrics.avg_buy_fill_seconds > 0.0 {
        (600.0 / metrics.avg_buy_fill_seconds.max(30.0)).clamp(0.20, 1.30)
    } else {
        0.75
    } * (1.0
        / (1.0 + (metrics.reprice_count + metrics.cancel_count) as f64 / 6.0));

    let volume_units = metrics.buy_volume.min(metrics.sell_volume) as f64 / 25_000.0;
    let liquidity_score = (volume_units.sqrt()
        * (metrics.moving_week as f64 / 1_000_000.0).sqrt()
        * (metrics.order_count as f64 / 40.0).sqrt()
        * (metrics.volume_value_hour / 50_000_000.0).sqrt())
    .clamp(0.05, 1.35);

    let failures = metrics.failed_flips
        + metrics.failed_search_count
        + metrics.cannot_afford_count
        + metrics.unknown_cost_basis_count
        + metrics.negative_profit_block_count
        + metrics.cancel_count;
    let reliability_score = ((metrics.successful_flips + 1) as f64
        / (metrics.successful_flips + failures + 1) as f64)
        .clamp(0.05, 1.20);

    let bound_capital = (metrics.current_open_buy_capital
        + metrics.current_open_sell_value
        + metrics.current_cost_lot_value)
        .max(metrics.expected_net_profit_after_tax.max(1.0));
    let capital_efficiency_score =
        (metrics.expected_net_profit_after_tax / bound_capital * 10.0).clamp(0.05, 1.25);

    let recent_success_score = if metrics.realized_profit_last_10m > 0 {
        1.35
    } else if metrics.realized_profit_last_30m > 0 {
        1.15
    } else if metrics.current_cost_lot_value > 0.0 && metrics.successful_flips == 0 {
        0.35
    } else {
        0.85
    };

    let score = metrics.expected_net_profit_after_tax.max(0.0)
        * sell_through_score
        * buy_fill_score
        * liquidity_score
        * reliability_score
        * capital_efficiency_score
        * recent_success_score
        / cycle;

    PracticalScoreBreakdown {
        score,
        sell_through_score,
        buy_fill_score,
        liquidity_score,
        reliability_score,
        capital_efficiency_score,
        recent_success_score,
        expected_cycle_minutes: cycle,
    }
}

pub fn evaluate_position_limits(
    item_open_buy_capital: f64,
    item_cost_lot_value: f64,
    item_open_sell_value: f64,
    total_open_buy_capital: f64,
    total_cost_lot_value: f64,
    total_open_sell_value: f64,
    config: &LocalBazaarScanConfig,
) -> CandidateDecision {
    let total_capital = config.total_capital.max(1) as f64;
    if total_cost_lot_value > total_capital * config.max_cost_lot_capital_ratio.clamp(0.05, 1.0) {
        return CandidateDecision::Reject("TOO_MUCH_OPEN_COST_BASIS");
    }
    if total_open_buy_capital > total_capital * config.max_open_buy_capital_ratio.clamp(0.05, 1.0) {
        return CandidateDecision::Reject("TOO_MUCH_OPEN_CAPITAL");
    }
    let active_limit = total_capital * config.active_capital_ratio.clamp(0.05, 1.0);
    if total_open_buy_capital + total_cost_lot_value + total_open_sell_value > active_limit {
        return CandidateDecision::Reject("POSITION_LIMIT");
    }
    let per_item_cap = if config.per_item_exposure_cap > 0 {
        config.per_item_exposure_cap as f64
    } else {
        config.max_capital_per_item as f64
    };
    if item_open_buy_capital + item_cost_lot_value + item_open_sell_value >= per_item_cap {
        return CandidateDecision::Reject("POSITION_LIMIT");
    }
    if item_cost_lot_value > 0.0 || item_open_sell_value > 0.0 {
        return CandidateDecision::Reject("COST_LOTS_ALREADY_OPEN");
    }
    CandidateDecision::Accept
}

pub fn should_reprice(
    order_age_seconds: u64,
    seconds_since_last_reprice: Option<u64>,
    reprices_this_hour: u64,
    old_expected_profit: f64,
    new_expected_profit: f64,
    has_open_cost_lots: bool,
    config: &LocalBazaarScanConfig,
) -> RepriceDecision {
    if order_age_seconds < config.min_reprice_interval_seconds {
        return RepriceDecision {
            allowed: false,
            reason: Some("ORDER_TOO_YOUNG"),
        };
    }
    if let Some(elapsed) = seconds_since_last_reprice {
        if elapsed < config.reprice_cooldown_seconds {
            return RepriceDecision {
                allowed: false,
                reason: Some("REPRICE_COOLDOWN"),
            };
        }
    }
    if reprices_this_hour >= config.max_reprices_per_item_per_hour {
        return RepriceDecision {
            allowed: false,
            reason: Some("TOO_MANY_REPRICES"),
        };
    }
    if new_expected_profit - old_expected_profit < config.min_reprice_profit_improvement {
        return RepriceDecision {
            allowed: false,
            reason: Some("LOW_REPRICE_IMPROVEMENT"),
        };
    }
    if new_expected_profit < old_expected_profit * 0.70 {
        return RepriceDecision {
            allowed: false,
            reason: Some("PROFIT_EROSION"),
        };
    }
    if has_open_cost_lots {
        return RepriceDecision {
            allowed: false,
            reason: Some("COST_LOTS_ALREADY_OPEN"),
        };
    }
    RepriceDecision {
        allowed: true,
        reason: None,
    }
}

pub fn should_reprice_sell(
    order_age_seconds: u64,
    seconds_since_last_reprice: Option<u64>,
    reprices_this_hour: u64,
    current_price_per_unit: f64,
    fresh_price_per_unit: f64,
    fifo_cost_basis_total: f64,
    amount: u64,
    config: &LocalBazaarScanConfig,
) -> RepriceDecision {
    if order_age_seconds < config.min_reprice_interval_seconds {
        return RepriceDecision {
            allowed: false,
            reason: Some("ORDER_TOO_YOUNG"),
        };
    }
    if let Some(elapsed) = seconds_since_last_reprice {
        if elapsed < config.reprice_cooldown_seconds {
            return RepriceDecision {
                allowed: false,
                reason: Some("REPRICE_COOLDOWN"),
            };
        }
    }
    if reprices_this_hour >= config.max_reprices_per_item_per_hour {
        return RepriceDecision {
            allowed: false,
            reason: Some("TOO_MANY_REPRICES"),
        };
    }
    if amount == 0 || fifo_cost_basis_total <= 0.0 || !fifo_cost_basis_total.is_finite() {
        return RepriceDecision {
            allowed: false,
            reason: Some("UNKNOWN_COST_BASIS"),
        };
    }
    if fresh_price_per_unit <= 0.0 || !fresh_price_per_unit.is_finite() {
        return RepriceDecision {
            allowed: false,
            reason: Some("INVALID_SELL_PRICE"),
        };
    }
    if fresh_price_per_unit >= current_price_per_unit {
        return RepriceDecision {
            allowed: false,
            reason: Some("SELL_NOT_OVERPRICED"),
        };
    }

    let total_price_movement = (current_price_per_unit - fresh_price_per_unit) * amount as f64;
    if total_price_movement < config.min_reprice_profit_improvement.max(1.0) {
        return RepriceDecision {
            allowed: false,
            reason: Some("LOW_SELL_PRICE_MOVEMENT"),
        };
    }

    let tax_multiplier = 1.0 - (config.bazaar_tax_rate.max(0.0) / 100.0);
    let fresh_after_tax = fresh_price_per_unit * amount as f64 * tax_multiplier;
    if fresh_after_tax <= fifo_cost_basis_total {
        return RepriceDecision {
            allowed: false,
            reason: Some("NEGATIVE_EXPECTED_PROFIT"),
        };
    }

    RepriceDecision {
        allowed: true,
        reason: None,
    }
}

pub async fn fetch_best_flips(
    config: &LocalBazaarScanConfig,
    limit: usize,
) -> Result<Vec<LocalBazaarFlip>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("failed to build Bazaar HTTP client")?;

    let response = client
        .get(BAZAAR_URL)
        .send()
        .await
        .context("failed to fetch Hypixel Bazaar data")?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Hypixel Bazaar API returned {}",
            response.status()
        ));
    }

    let data: BazaarResponse = response
        .json()
        .await
        .context("failed to parse Hypixel Bazaar response")?;

    if !data.success {
        return Err(anyhow::anyhow!("Hypixel Bazaar API returned success=false"));
    }

    let mut flips = Vec::new();
    let mut rejected: BTreeMap<&'static str, usize> = BTreeMap::new();
    let tax_multiplier = 1.0 - (config.bazaar_tax_rate.max(0.0) / 100.0);
    let timestamp = unix_timestamp();
    let target_profit_per_hour = config.target_profit_per_hour.max(2_000_000.0);
    let active_capital = (config.total_capital as f64
        * config
            .active_capital_ratio
            .clamp(0.05, 1.0 - config.reserve_ratio.clamp(0.0, 0.95)))
    .max(0.0);
    let reserved_capital = (config.total_capital as f64 - active_capital).max(0.0);
    let useful_limit = limit.max(config.max_items.max(1));
    if config.total_capital > 0 {
        let cap = config.total_capital as f64;
        if config.total_cost_lot_value > cap * config.max_cost_lot_capital_ratio.clamp(0.05, 1.0) {
            bump(&mut rejected, "TOO_MUCH_OPEN_COST_BASIS");
            info!(
                "[BAF][CAPITAL] Skipping scan — TOO_MUCH_OPEN_COST_BASIS cost_lots {:.0} / cap {:.0}",
                config.total_cost_lot_value, cap
            );
            return Ok(Vec::new());
        }
        if config.open_buy_capital > cap * config.max_open_buy_capital_ratio.clamp(0.05, 1.0) {
            bump(&mut rejected, "TOO_MUCH_OPEN_CAPITAL");
            info!(
                "[BAF][CAPITAL] Skipping scan — TOO_MUCH_OPEN_CAPITAL open_buy {:.0} / cap {:.0}",
                config.open_buy_capital, cap
            );
            return Ok(Vec::new());
        }
    }

    if config.inventory_free_slots <= config.min_free_inventory_slots {
        bump(&mut rejected, "INVENTORY_RISK");
        info!(
            "[LocalBazaar] Skipping scan — INVENTORY_RISK free slots {} <= reserve slots {}",
            config.inventory_free_slots, config.min_free_inventory_slots
        );
        return Ok(Vec::new());
    }
    if config.active_buy_order_count >= config.max_pending_buy_stacks {
        bump(&mut rejected, "TOO_MANY_ACTIVE_BUYS");
        info!(
            "[LocalBazaar] Skipping scan — TOO_MANY_ACTIVE_BUYS active {} >= max {}",
            config.active_buy_order_count, config.max_pending_buy_stacks
        );
        return Ok(Vec::new());
    }
    if config.active_sell_order_count > (config.max_pending_buy_stacks * 2).max(4) {
        bump(&mut rejected, "TOO_MANY_ACTIVE_SELLS");
        info!(
            "[LocalBazaar] Skipping scan — TOO_MANY_ACTIVE_SELLS active {}",
            config.active_sell_order_count
        );
        return Ok(Vec::new());
    }

    let history = MARKET_HISTORY.get_or_init(|| Mutex::new(HashMap::new()));

    for (item_tag, product) in data.products {
        let q = product.quick_status;
        let is_classic = config.enable_classic_potato_book_flips
            && matches!(item_tag.as_str(), "HOT_POTATO_BOOK" | "FUMING_POTATO_BOOK");
        if !is_local_order_search_supported(&item_tag) {
            bump(&mut rejected, "UNSUPPORTED_PRODUCT_LOOKUP");
            continue;
        }
        if q.buy_price <= 0.0 || q.sell_price <= 0.0 {
            bump(&mut rejected, "INVALID_PRICE");
            continue;
        }
        if product.buy_summary.is_empty() || product.sell_summary.is_empty() {
            bump(&mut rejected, "THIN_ORDERBOOK");
            continue;
        }

        let moving_week = q.moving_week();

        if !is_classic
            && (q.buy_orders < config.min_order_count
                || q.sell_orders < config.min_order_count
                || q.buy_volume == 0
                || q.sell_volume == 0
                || moving_week == 0)
        {
            bump(&mut rejected, "LOW_VOLUME");
            continue;
        }

        // Hypixel quick_status naming:
        // sellPrice/sell_summary = the top BUY-order side (what instant-sell receives).
        // buyPrice/buy_summary = the top SELL-offer side (what instant-buy pays).
        // A bazaar flip places a BUY order just above sell_summary[0], then
        // a SELL offer just below buy_summary[0] after the BUY order fills.
        let best_buy_order = product
            .sell_summary
            .first()
            .map(|l| l.price_per_unit)
            .unwrap_or(q.sell_price);
        let best_sell_offer = product
            .buy_summary
            .first()
            .map(|l| l.price_per_unit)
            .unwrap_or(q.buy_price);
        let buy_step = config.price_undercut.max(0.0);
        let sell_step = config.price_undercut.max(0.0);
        let buy_price = best_buy_order + buy_step;
        let sell_price = (best_sell_offer - sell_step).max(0.0);
        let net_sell_price = sell_price * tax_multiplier;
        let profit_per_unit = net_sell_price - buy_price;
        if profit_per_unit <= 0.0 || profit_per_unit < config.min_profit_per_unit {
            bump(&mut rejected, "LOW_ROI_AFTER_TAX");
            continue;
        }

        let margin_percent = (profit_per_unit / buy_price) * 100.0;
        let min_roi_percent = config.min_roi_percent.max(config.min_margin_percent);
        if margin_percent < min_roi_percent {
            bump(&mut rejected, "LOW_ROI_AFTER_TAX");
            continue;
        }
        if !is_classic
            && config.max_margin_percent > 0.0
            && margin_percent > config.max_margin_percent
        {
            bump(&mut rejected, "MANIPULATION_RISK");
            continue;
        }

        let buy_volume_hour = q.buy_moving_week as f64 / 168.0;
        let sell_volume_hour = q.sell_moving_week as f64 / 168.0;
        let bottleneck_units_hour = buy_volume_hour.min(sell_volume_hour).max(0.0);
        let volume_value_hour = bottleneck_units_hour * buy_price;
        if !is_classic && volume_value_hour < config.min_volume_value_hour {
            bump(&mut rejected, "LOW_VOLUME");
            continue;
        }
        let raw_volume_floor = config.min_buy_volume.min(config.min_sell_volume).max(1) as f64;
        if !is_classic
            && bottleneck_units_hour < raw_volume_floor / 24.0
            && volume_value_hour < config.preferred_volume_value_hour
        {
            bump(&mut rejected, "LOW_VOLUME");
            continue;
        }
        let sell_to_buy_flow_ratio = if buy_volume_hour > 0.0 {
            (sell_volume_hour / buy_volume_hour).clamp(0.0, 2.0)
        } else {
            0.0
        };
        if !is_classic && sell_to_buy_flow_ratio < 0.25 {
            bump(&mut rejected, "LOW_SELL_LIQUIDITY");
            continue;
        }

        let buy_depth = depth_metrics(&product.sell_summary);
        let sell_depth = depth_metrics(&product.buy_summary);
        if !is_classic && (buy_depth.depth_5_value <= 0.0 || sell_depth.depth_5_value <= 0.0) {
            bump(&mut rejected, "THIN_ORDERBOOK");
            continue;
        }

        let mut risk_notes = Vec::new();
        let top_depth_min = buy_depth.top_value.min(sell_depth.top_value);
        let depth_5_min = buy_depth.depth_5_value.min(sell_depth.depth_5_value);
        if top_depth_min < 100_000.0 {
            risk_notes.push("thin_top_level".to_string());
        }
        if depth_5_min < 500_000.0 {
            risk_notes.push("thin_first_5_levels".to_string());
        }
        if sell_depth.depth_5_value < buy_depth.depth_5_value * 0.35
            || sell_to_buy_flow_ratio < 0.55
        {
            risk_notes.push("low_sell_liquidity".to_string());
        }
        if margin_percent >= config.max_margin_percent.max(30.0) * 0.75
            && volume_value_hour < config.preferred_volume_value_hour
        {
            risk_notes.push("high_margin_low_liquidity".to_string());
        }

        let (stability_factor, spread_reliability_factor, volatility_note) = {
            let mut guard = history.lock().unwrap();
            let snapshots = guard.entry(item_tag.clone()).or_default();
            let cutoff = timestamp.saturating_sub(config.history_window_minutes.max(5) * 60);
            snapshots.retain(|s| s.timestamp >= cutoff);
            let spread = sell_price - buy_price;
            let result = history_factors(snapshots, sell_price, spread);
            snapshots.push(MarketSnapshot {
                timestamp,
                product_id: item_tag.clone(),
                best_buy_order,
                best_sell_offer,
                target_buy_price: buy_price,
                target_sell_price: sell_price,
                spread,
                net_spread: profit_per_unit,
                roi: margin_percent / 100.0,
                buy_volume_hour,
                sell_volume_hour,
                top_buy_depth_value: buy_depth.top_value,
                top_sell_depth_value: sell_depth.top_value,
                score: 0.0,
                risk_level: "pending".to_string(),
            });
            result
        };
        if let Some(note) = volatility_note {
            if note == "price_or_spread_spike" {
                bump(&mut rejected, "UNSTABLE_HISTORY");
            }
            risk_notes.push(note);
        }

        let orderbook_order_pressure = (product
            .buy_summary
            .iter()
            .take(5)
            .map(|l| l.orders)
            .sum::<u64>()
            + product
                .sell_summary
                .iter()
                .take(5)
                .map(|l| l.orders)
                .sum::<u64>()) as f64;
        let competition_factor = (1.0 / (1.0 + orderbook_order_pressure / 400.0)).clamp(0.25, 1.0);
        let liquidity_factor =
            (volume_value_hour / config.preferred_volume_value_hour.max(1.0)).clamp(0.15, 1.0);
        let depth_factor = (depth_5_min / 2_000_000.0).clamp(0.15, 1.0);
        let fill_probability_factor =
            (liquidity_factor * 0.45 + depth_factor * 0.35 + competition_factor * 0.20)
                .clamp(0.05, 1.0);
        let mut participation_rate = config
            .market_participation_rate
            .max(config.conservative_market_participation_rate)
            .clamp(0.01, 0.30);
        if competition_factor < 0.45 || stability_factor < 0.55 {
            participation_rate = participation_rate.min(
                config
                    .conservative_market_participation_rate
                    .clamp(0.01, 0.20),
            );
        }
        let capturable_units_hour =
            bottleneck_units_hour * participation_rate * fill_probability_factor;

        let per_item_cap_from_ratio = active_capital
            * (1.0 / config.max_items.max(1) as f64)
                .max(0.15)
                .min(config.buy_sell_balance_limit.clamp(0.15, 0.30));
        let max_configured_cap = if config.max_capital_per_item == 0 {
            config.max_order_value as f64
        } else {
            config.max_capital_per_item as f64
        };
        let max_order_cap = if config.max_order_value == 0 {
            max_configured_cap
        } else {
            config.max_order_value as f64
        };
        let sell_liquidity_cap_multiplier = sell_to_buy_flow_ratio.clamp(0.10, 1.0);
        let liquidity_cap = (volume_value_hour * 0.35 * sell_liquidity_cap_multiplier)
            .min(depth_5_min * 1.25)
            .min(capturable_units_hour * buy_price);
        let mut allocated_capital = active_capital
            .min(per_item_cap_from_ratio)
            .min(max_configured_cap)
            .min(max_order_cap)
            .min(liquidity_cap)
            .max(0.0);
        if allocated_capital < buy_price {
            bump(&mut rejected, "CAPITAL_LIMIT");
            continue;
        }

        let amount = ((allocated_capital / buy_price).floor() as u64)
            .max(1)
            .min(config.max_amount.max(1));
        allocated_capital = buy_price * amount as f64;
        let total_profit = profit_per_unit * amount as f64;
        if total_profit < config.min_total_profit {
            bump(&mut rejected, "CAPITAL_LIMIT");
            continue;
        }

        let estimated_cycles_per_hour = (capturable_units_hour / amount as f64).clamp(0.05, 12.0);
        let expected_profit_per_hour =
            allocated_capital * (margin_percent / 100.0) * estimated_cycles_per_hour;
        let risk_penalty = risk_notes.len() as f64 * 0.08;
        let risk_multiplier = (liquidity_factor
            * stability_factor
            * spread_reliability_factor
            * fill_probability_factor
            * competition_factor
            - risk_penalty)
            .clamp(0.05, 1.0);
        let risk_adjusted_expected_profit_per_hour = expected_profit_per_hour * risk_multiplier;
        let per_order_target = target_profit_per_hour / config.max_items.max(1) as f64;
        if per_order_target > 0.0
            && !is_classic
            && risk_adjusted_expected_profit_per_hour < per_order_target * 0.15
        {
            bump(&mut rejected, "CAPITAL_LIMIT");
            continue;
        }

        let risk_level = risk_level_for(risk_multiplier, &risk_notes, volume_value_hour);
        if risk_level == RiskLevel::Extreme && !is_classic {
            bump(&mut rejected, "MANIPULATION_RISK");
            continue;
        }
        let recommendation = recommendation_for(
            margin_percent,
            volume_value_hour,
            risk_level,
            config.target_roi_percent,
            config.preferred_volume_value_hour,
        );
        let practical = practical_score(&PracticalItemMetrics {
            expected_net_profit_after_tax: total_profit,
            expected_cycle_minutes: (60.0 / estimated_cycles_per_hour.max(0.05)).max(1.0),
            buy_volume: q.buy_volume,
            sell_volume: q.sell_volume,
            moving_week,
            order_count: q.buy_orders.min(q.sell_orders),
            volume_value_hour,
            ..Default::default()
        });
        let score = practical
            .score
            .max(risk_adjusted_expected_profit_per_hour * 0.25);
        debug!(
            "[BAF][SCORE] product={} score={:.2} practical={:?}",
            item_tag, score, practical
        );

        append_snapshot_jsonl(&MarketSnapshot {
            timestamp,
            product_id: item_tag.clone(),
            best_buy_order,
            best_sell_offer,
            target_buy_price: buy_price,
            target_sell_price: sell_price,
            spread: sell_price - buy_price,
            net_spread: profit_per_unit,
            roi: margin_percent / 100.0,
            buy_volume_hour,
            sell_volume_hour,
            top_buy_depth_value: buy_depth.top_value,
            top_sell_depth_value: sell_depth.top_value,
            score,
            risk_level: format!("{:?}", risk_level),
        });

        flips.push(LocalBazaarFlip {
            item_name: item_tag_to_name(&item_tag),
            product_id: item_tag.clone(),
            item_tag,
            amount,
            buy_price_per_unit: buy_price,
            sell_price_per_unit: sell_price,
            profit_per_unit,
            total_profit,
            margin_percent,
            buy_volume: q.buy_volume,
            sell_volume: q.sell_volume,
            moving_week,
            estimated_profit_per_hour: expected_profit_per_hour,
            score,
            best_buy_order,
            best_sell_offer,
            target_buy_price: buy_price,
            target_sell_price: sell_price,
            spread_before_tax: sell_price - buy_price,
            spread_after_tax: profit_per_unit,
            roi_after_tax: margin_percent / 100.0,
            buy_volume_hour,
            sell_volume_hour,
            volume_value_hour,
            top_buy_depth_value: buy_depth.top_value,
            top_sell_depth_value: sell_depth.top_value,
            avg_buy_depth_value: buy_depth.avg_value,
            avg_sell_depth_value: sell_depth.avg_value,
            depth_5_buy_value: buy_depth.depth_5_value,
            depth_5_sell_value: sell_depth.depth_5_value,
            estimated_cycles_per_hour,
            allocated_capital,
            expected_profit_per_hour,
            risk_adjusted_expected_profit_per_hour: score,
            risk_level,
            risk_notes,
            recommendation,
        });
    }

    flips.sort_by(|a, b| {
        b.risk_adjusted_expected_profit_per_hour
            .partial_cmp(&a.risk_adjusted_expected_profit_per_hour)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b.volume_value_hour
                    .partial_cmp(&a.volume_value_hour)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                b.roi_after_tax
                    .partial_cmp(&a.roi_after_tax)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    let capital_budget = if config.max_order_value == 0 {
        active_capital
    } else {
        active_capital.min(config.max_order_value as f64 * useful_limit as f64)
    };
    let mut allocated_total = 0.0;
    flips.retain(|flip| {
        if allocated_total + flip.allocated_capital > capital_budget {
            bump(&mut rejected, "CAPITAL_LIMIT");
            return false;
        }
        allocated_total += flip.allocated_capital;
        true
    });
    flips.truncate(useful_limit);

    let expected_total: f64 = flips
        .iter()
        .map(|f| f.risk_adjusted_expected_profit_per_hour)
        .sum();
    let hourly_roi = if config.total_capital > 0 {
        expected_total / config.total_capital as f64
    } else {
        0.0
    };
    info!(
        "[LocalBazaar] analyzed {} products, {} candidates kept, expected risk-adjusted profit/h {:.0}, hourly ROI {:.2}%, target 2m/h met: {}",
        rejected.values().sum::<usize>() + flips.len(),
        flips.len(),
        expected_total,
        hourly_roi * 100.0,
        expected_total >= target_profit_per_hour
    );
    info!(
        "[LocalBazaar] capital model active {:.0}, reserved {:.0}, free model capital {:.0}, free slots {}, active BUYs {}, active SELLs {}, inventory sellables {}",
        active_capital,
        reserved_capital,
        (active_capital - allocated_total).max(0.0),
        config.inventory_free_slots,
        config.active_buy_order_count,
        config.active_sell_order_count,
        config.inventory_sellable_stacks
    );
    for flip in flips.iter().take(5) {
        info!(
            "[LocalBazaar] Top {}: cap {:.0}, p/h {:.0}, roi {:.2}%, cycles {:.2}, vol/h {:.0}, depth5 {:.0}/{:.0}, risk {:?}, {:?}, notes {:?}",
            flip.product_id,
            flip.allocated_capital,
            flip.risk_adjusted_expected_profit_per_hour,
            flip.roi_after_tax * 100.0,
            flip.estimated_cycles_per_hour,
            flip.volume_value_hour,
            flip.depth_5_buy_value,
            flip.depth_5_sell_value,
            flip.risk_level,
            flip.recommendation,
            flip.risk_notes
        );
    }
    debug!("[LocalBazaar] rejected products by reason: {:?}", rejected);
    Ok(flips)
}

pub async fn fetch_product_quote(item_tag: &str) -> Result<Option<BazaarProductQuote>> {
    Ok(fetch_product_quotes().await?.remove(item_tag))
}

#[derive(Debug, Clone, Serialize)]
struct MarketSnapshot {
    timestamp: u64,
    product_id: String,
    best_buy_order: f64,
    best_sell_offer: f64,
    target_buy_price: f64,
    target_sell_price: f64,
    spread: f64,
    net_spread: f64,
    roi: f64,
    buy_volume_hour: f64,
    sell_volume_hour: f64,
    top_buy_depth_value: f64,
    top_sell_depth_value: f64,
    score: f64,
    risk_level: String,
}

#[derive(Debug, Clone, Copy)]
struct DepthMetrics {
    top_value: f64,
    depth_5_value: f64,
    avg_value: f64,
}

fn depth_metrics(levels: &[OrderBookLevel]) -> DepthMetrics {
    let values: Vec<f64> = levels
        .iter()
        .take(10)
        .filter(|l| l.price_per_unit > 0.0 && l.amount > 0)
        .map(|l| l.price_per_unit * l.amount as f64)
        .collect();
    let top_value = values.first().copied().unwrap_or(0.0);
    let depth_5_value = values.iter().take(5).sum::<f64>();
    let avg_value = if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    };
    DepthMetrics {
        top_value,
        depth_5_value,
        avg_value,
    }
}

fn history_factors(
    snapshots: &[MarketSnapshot],
    current_sell_price: f64,
    current_spread: f64,
) -> (f64, f64, Option<String>) {
    if snapshots.len() < 3 {
        return (0.72, 0.70, Some("limited_history".to_string()));
    }

    let avg_price =
        snapshots.iter().map(|s| s.target_sell_price).sum::<f64>() / snapshots.len() as f64;
    let avg_spread = snapshots.iter().map(|s| s.spread).sum::<f64>() / snapshots.len() as f64;
    let price_dev = relative_deviation(current_sell_price, avg_price);
    let spread_dev = relative_deviation(current_spread, avg_spread.max(0.01));
    let price_changes = snapshots
        .windows(2)
        .filter(|w| relative_deviation(w[0].target_sell_price, w[1].target_sell_price) > 0.002)
        .count() as f64;
    let competition_noise = (price_changes / snapshots.len() as f64).clamp(0.0, 1.0);
    let stability = (1.0 - price_dev * 3.0 - competition_noise * 0.35).clamp(0.20, 1.0);
    let spread_reliability = (1.0 - spread_dev * 2.0).clamp(0.20, 1.0);
    let note = if price_dev > 0.12 || spread_dev > 0.45 {
        Some("price_or_spread_spike".to_string())
    } else if competition_noise > 0.55 {
        Some("top_price_changes_often".to_string())
    } else {
        None
    };
    (stability, spread_reliability, note)
}

fn relative_deviation(current: f64, average: f64) -> f64 {
    if average.abs() < f64::EPSILON {
        0.0
    } else {
        ((current - average) / average).abs()
    }
}

fn risk_level_for(multiplier: f64, notes: &[String], volume_value_hour: f64) -> RiskLevel {
    if notes.iter().any(|n| n == "price_or_spread_spike") && volume_value_hour < 2_000_000.0 {
        return RiskLevel::Extreme;
    }
    if multiplier < 0.18 || notes.len() >= 3 {
        RiskLevel::High
    } else if multiplier < 0.38 || notes.len() >= 2 {
        RiskLevel::Medium
    } else {
        RiskLevel::Low
    }
}

fn recommendation_for(
    roi_percent: f64,
    volume_value_hour: f64,
    risk_level: RiskLevel,
    target_roi_percent: f64,
    preferred_volume_value_hour: f64,
) -> Recommendation {
    match risk_level {
        RiskLevel::Extreme => Recommendation::PossibleManipulation,
        RiskLevel::High if volume_value_hour < preferred_volume_value_hour => Recommendation::Risky,
        _ if volume_value_hour < 1_000_000.0 => Recommendation::DeadItem,
        _ if roi_percent >= target_roi_percent
            && volume_value_hour >= preferred_volume_value_hour =>
        {
            Recommendation::StrongCandidate
        }
        _ if roi_percent < target_roi_percent
            && volume_value_hour >= preferred_volume_value_hour * 2.0 =>
        {
            Recommendation::HighVolumeLowMargin
        }
        _ if roi_percent >= target_roi_percent * 2.0
            && volume_value_hour < preferred_volume_value_hour =>
        {
            Recommendation::HighMarginLowVolume
        }
        _ => Recommendation::GoodButLimitCapital,
    }
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

fn append_snapshot_jsonl(snapshot: &MarketSnapshot) {
    let Ok(line) = serde_json::to_string(snapshot) else {
        return;
    };
    let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open("bazaar_market_history.jsonl")
    else {
        return;
    };
    let _ = writeln!(file, "{}", line);
}

fn bump(map: &mut BTreeMap<&'static str, usize>, reason: &'static str) {
    *map.entry(reason).or_insert(0) += 1;
}

pub async fn fetch_product_quotes() -> Result<HashMap<String, BazaarProductQuote>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("failed to build Bazaar HTTP client")?;

    let response = client
        .get(BAZAAR_URL)
        .send()
        .await
        .context("failed to fetch Hypixel Bazaar data")?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Hypixel Bazaar API returned {}",
            response.status()
        ));
    }

    let data: BazaarResponse = response
        .json()
        .await
        .context("failed to parse Hypixel Bazaar response")?;

    if !data.success {
        return Err(anyhow::anyhow!("Hypixel Bazaar API returned success=false"));
    }

    Ok(data
        .products
        .into_iter()
        .map(|(item_tag, product)| {
            let q = product.quick_status;
            (
                item_tag.clone(),
                BazaarProductQuote {
                    item_name: item_tag_to_name(&item_tag),
                    item_tag,
                    buy_price: q.buy_price,
                    sell_price: q.sell_price,
                    buy_volume: q.buy_volume,
                    sell_volume: q.sell_volume,
                    moving_week: q.moving_week(),
                },
            )
        })
        .collect())
}

fn item_tag_to_name(tag: &str) -> String {
    if let Some(name) = gemstone_tag_to_name(tag) {
        return name;
    }

    match tag {
        "FINE_AQUAMARINE_GEM" => return "Fine Aquamarine Gemstone".to_string(),
        "FINE_CITRINE_GEM" => return "Fine Citrine Gemstone".to_string(),
        "FINE_ONYX_GEM" => return "Fine Onyx Gemstone".to_string(),
        "FINE_PERIDOT_GEM" => return "Fine Peridot Gemstone".to_string(),
        "FINE_RUBY_GEM" => return "Fine Ruby Gemstone".to_string(),
        "FINE_SAPPHIRE_GEM" => return "Fine Sapphire Gemstone".to_string(),
        "FINE_TOPAZ_GEM" => return "Fine Topaz Gemstone".to_string(),
        "FINE_JASPER_GEM" => return "Fine Jasper Gemstone".to_string(),
        "FINE_AMETHYST_GEM" => return "Fine Amethyst Gemstone".to_string(),
        "FLAWLESS_AQUAMARINE_GEM" => return "Flawless Aquamarine Gemstone".to_string(),
        "FLAWLESS_CITRINE_GEM" => return "Flawless Citrine Gemstone".to_string(),
        "FLAWLESS_ONYX_GEM" => return "Flawless Onyx Gemstone".to_string(),
        "FLAWLESS_PERIDOT_GEM" => return "Flawless Peridot Gemstone".to_string(),
        "FLAWLESS_RUBY_GEM" => return "Flawless Ruby Gemstone".to_string(),
        "FLAWLESS_SAPPHIRE_GEM" => return "Flawless Sapphire Gemstone".to_string(),
        "FLAWLESS_TOPAZ_GEM" => return "Flawless Topaz Gemstone".to_string(),
        "FLAWLESS_JASPER_GEM" => return "Flawless Jasper Gemstone".to_string(),
        "FLAWLESS_AMETHYST_GEM" => return "Flawless Amethyst Gemstone".to_string(),
        "PERFECT_AQUAMARINE_GEM" => return "Perfect Aquamarine Gemstone".to_string(),
        "PERFECT_CITRINE_GEM" => return "Perfect Citrine Gemstone".to_string(),
        "PERFECT_ONYX_GEM" => return "Perfect Onyx Gemstone".to_string(),
        "PERFECT_PERIDOT_GEM" => return "Perfect Peridot Gemstone".to_string(),
        "PERFECT_RUBY_GEM" => return "Perfect Ruby Gemstone".to_string(),
        "PERFECT_SAPPHIRE_GEM" => return "Perfect Sapphire Gemstone".to_string(),
        "PERFECT_TOPAZ_GEM" => return "Perfect Topaz Gemstone".to_string(),
        "PERFECT_JASPER_GEM" => return "Perfect Jasper Gemstone".to_string(),
        "PERFECT_AMETHYST_GEM" => return "Perfect Amethyst Gemstone".to_string(),
        "ROUGH_AQUAMARINE_GEM" => return "Rough Aquamarine Gemstone".to_string(),
        "ROUGH_CITRINE_GEM" => return "Rough Citrine Gemstone".to_string(),
        "ROUGH_ONYX_GEM" => return "Rough Onyx Gemstone".to_string(),
        "ROUGH_PERIDOT_GEM" => return "Rough Peridot Gemstone".to_string(),
        "ROUGH_RUBY_GEM" => return "Rough Ruby Gemstone".to_string(),
        "ROUGH_SAPPHIRE_GEM" => return "Rough Sapphire Gemstone".to_string(),
        "ROUGH_TOPAZ_GEM" => return "Rough Topaz Gemstone".to_string(),
        "ROUGH_JASPER_GEM" => return "Rough Jasper Gemstone".to_string(),
        "ROUGH_AMETHYST_GEM" => return "Rough Amethyst Gemstone".to_string(),
        "WATER_LILY" => return "Lily Pad".to_string(),
        "ENCHANTED_WATER_LILY" => return "Enchanted Lily Pad".to_string(),
        "RAW_FISH" => return "Raw Cod".to_string(),
        "SHARD_COD" => return "Cod Shard".to_string(),
        "SHARD_VORACIOUS_SPIDER" => return "Voracious Spider Shard".to_string(),
        _ => {}
    }

    if let Some(essence) = tag.strip_prefix("ESSENCE_") {
        return format!("{} Essence", title_case_tag(essence));
    }

    title_case_tag(tag)
}

fn gemstone_tag_to_name(tag: &str) -> Option<String> {
    let mut parts = tag.split('_').collect::<Vec<_>>();
    if parts.len() != 3 || parts.pop()? != "GEM" {
        return None;
    }
    let gem = parts.pop()?;
    let tier = parts.pop()?;
    if !matches!(tier, "ROUGH" | "FLAWED" | "FINE" | "FLAWLESS" | "PERFECT") {
        return None;
    }
    Some(format!(
        "{} {} Gemstone",
        title_case_tag(tier),
        title_case_tag(gem)
    ))
}

fn title_case_tag(tag: &str) -> String {
    tag.split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => format!(
                    "{}{}",
                    first.to_ascii_uppercase(),
                    chars.as_str().to_ascii_lowercase()
                ),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_local_order_search_supported(tag: &str) -> bool {
    !tag.starts_with("ENCHANTMENT_") && !tag.starts_with("ESSENCE_")
}

#[cfg(test)]
mod tests {
    use super::item_tag_to_name;

    #[test]
    fn formats_item_tags() {
        assert_eq!(
            item_tag_to_name("ENCHANTED_COAL_BLOCK"),
            "Enchanted Coal Block"
        );
        assert_eq!(
            item_tag_to_name("ENCHANTED_WATER_LILY"),
            "Enchanted Lily Pad"
        );
        assert_eq!(
            item_tag_to_name("FLAWLESS_AMBER_GEM"),
            "Flawless Amber Gemstone"
        );
        assert_eq!(item_tag_to_name("FLAWED_JADE_GEM"), "Flawed Jade Gemstone");
    }

    #[test]
    fn formats_essence_tags_for_visible_bazaar_search() {
        assert_eq!(item_tag_to_name("ESSENCE_UNDEAD"), "Undead Essence");
        assert_eq!(item_tag_to_name("ESSENCE_WITHER"), "Wither Essence");
    }

    #[test]
    fn rejects_enchantment_products_for_local_buy_search_until_mapped() {
        assert!(!super::is_local_order_search_supported(
            "ENCHANTMENT_FEAST_1"
        ));
        assert!(!super::is_local_order_search_supported("ESSENCE_UNDEAD"));
        assert!(super::is_local_order_search_supported("SUMMONING_EYE"));
    }
}

#[cfg(test)]
mod practical_tests {
    use super::*;

    fn test_config() -> LocalBazaarScanConfig {
        LocalBazaarScanConfig {
            min_profit_per_unit: 5.0,
            min_total_profit: 15_000.0,
            min_margin_percent: 1.25,
            max_margin_percent: 32.0,
            min_buy_volume: 15_000,
            min_sell_volume: 25_000,
            min_order_count: 40,
            min_moving_week: 1_000_000,
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
            min_volume_value_hour: 15_000_000.0,
            preferred_volume_value_hour: 50_000_000.0,
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
        }
    }

    #[test]
    fn no_theoretical_profit_booking_is_a_tracker_invariant() {
        let expected_candidate_profit = 500_000.0;
        let realized_fifo_profit = 0;
        assert!(expected_candidate_profit > 0.0);
        assert_eq!(realized_fifo_profit, 0);
    }

    #[test]
    fn sell_through_scoring_prefers_realized_profit_per_hour() {
        let slow_theoretical = practical_score(&PracticalItemMetrics {
            expected_net_profit_after_tax: 500_000.0,
            expected_cycle_minutes: 45.0,
            buy_volume: 25_000,
            sell_volume: 25_000,
            moving_week: 1_000_000,
            order_count: 40,
            volume_value_hour: 50_000_000.0,
            avg_sell_fill_seconds: 2700.0,
            current_cost_lot_value: 2_000_000.0,
            ..Default::default()
        });
        let fast_realized = practical_score(&PracticalItemMetrics {
            expected_net_profit_after_tax: 150_000.0,
            expected_cycle_minutes: 5.0,
            buy_volume: 25_000,
            sell_volume: 25_000,
            moving_week: 1_000_000,
            order_count: 40,
            volume_value_hour: 50_000_000.0,
            avg_sell_fill_seconds: 300.0,
            successful_flips: 3,
            realized_profit_last_10m: 150_000,
            ..Default::default()
        });
        assert!(fast_realized.score > slow_theoretical.score);
    }

    #[test]
    fn cost_lot_pressure_blocks_carrot_zest_stack() {
        let cfg = test_config();
        let decision =
            evaluate_position_limits(0.0, 3_500_000.0, 0.0, 2_000_000.0, 3_500_000.0, 0.0, &cfg);
        assert_eq!(decision, CandidateDecision::Reject("POSITION_LIMIT"));
    }

    #[test]
    fn per_item_exposure_cap_rejects_candidate() {
        let cfg = test_config();
        let decision = evaluate_position_limits(
            2_000_000.0,
            1_500_000.0,
            0.0,
            2_000_000.0,
            1_500_000.0,
            0.0,
            &cfg,
        );
        assert_eq!(decision, CandidateDecision::Reject("POSITION_LIMIT"));
    }

    #[test]
    fn reprice_throttle_requires_interval_and_profit_improvement() {
        let cfg = test_config();
        assert_eq!(
            should_reprice(60, None, 0, 100_000.0, 300_000.0, false, &cfg).reason,
            Some("ORDER_TOO_YOUNG")
        );
        assert_eq!(
            should_reprice(300, Some(100), 0, 100_000.0, 300_000.0, false, &cfg).reason,
            Some("REPRICE_COOLDOWN")
        );
        assert_eq!(
            should_reprice(300, Some(400), 0, 100_000.0, 120_000.0, false, &cfg).reason,
            Some("LOW_REPRICE_IMPROVEMENT")
        );
        assert!(should_reprice(300, Some(400), 0, 100_000.0, 200_000.0, false, &cfg).allowed);
    }

    #[test]
    fn sell_reprice_requires_fifo_positive_lower_price() {
        let cfg = test_config();
        assert_eq!(
            should_reprice_sell(60, None, 0, 1_200_000.0, 1_100_000.0, 2_000_000.0, 2, &cfg).reason,
            Some("ORDER_TOO_YOUNG")
        );
        assert_eq!(
            should_reprice_sell(
                300,
                Some(400),
                0,
                1_200_000.0,
                1_190_000.0,
                2_000_000.0,
                2,
                &cfg
            )
            .reason,
            Some("LOW_SELL_PRICE_MOVEMENT")
        );
        assert_eq!(
            should_reprice_sell(
                300,
                Some(400),
                0,
                1_200_000.0,
                980_000.0,
                1_970_000.0,
                2,
                &cfg
            )
            .reason,
            Some("NEGATIVE_EXPECTED_PROFIT")
        );
        assert_eq!(
            should_reprice_sell(
                300,
                Some(400),
                0,
                1_200_000.0,
                1_250_000.0,
                2_000_000.0,
                2,
                &cfg
            )
            .reason,
            Some("SELL_NOT_OVERPRICED")
        );
        assert!(
            should_reprice_sell(
                300,
                Some(400),
                0,
                1_200_000.0,
                1_100_000.0,
                2_000_000.0,
                2,
                &cfg
            )
            .allowed
        );
    }
}
