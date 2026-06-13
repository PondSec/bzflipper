//! Tracks active bazaar orders for the web panel.
//!
//! Orders are added on [`BazaarOrderPlaced`] events and removed when
//! [`BazaarOrderCollected`] or [`BazaarOrderCancelled`] events fire.
//!
//! Orders and buy costs are persisted to disk so profit tracking survives
//! across bot restarts.

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

/// File name for persisted orders (stored next to the executable / in the logs dir).
const ORDERS_FILE: &str = "bazaar_orders.json";
/// Legacy weighted-average buy-cost file.  It is intentionally ignored for FIFO profit.
const LEGACY_BUY_COSTS_FILE: &str = "bazaar_buy_costs.json";
/// File name for persisted FIFO buy-cost lots.
const BUY_COST_LOTS_FILE: &str = "bazaar_buy_cost_lots.json";
/// File name for per-item runtime performance and cooldown state.
const ITEM_PERFORMANCE_FILE: &str = "bazaar_item_performance.json";
/// Planned SELL order escrows are only a short bridge until the in-game order
/// appears in Manage Orders. A longer lifetime can pin FIFO lots after a failed
/// command and stall the whole sell-first lifecycle.
const PLANNED_LOCAL_SELL_ORDER_TTL_SECONDS: u64 = 120;
const PRODUCT_LOOKUP_FAILURE_COOLDOWN_SECONDS: u64 = 120;

/// A single tracked bazaar order visible on the web panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackedBazaarOrder {
    pub item_name: String,
    pub amount: u64,
    pub price_per_unit: f64,
    pub is_buy_order: bool,
    /// `"open"` or `"filled"`.
    pub status: String,
    /// Unix timestamp (seconds) when the order was placed.
    pub placed_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BuyCostLot {
    pub price_per_unit: f64,
    pub amount: u64,
    pub collected_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BuyCostLotUsage {
    pub price_per_unit: f64,
    pub amount: u64,
    pub total_cost: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SellBlockReason {
    NegativeExpectedProfit,
    UnknownCostBasis,
    InvalidSell,
}

impl SellBlockReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NegativeExpectedProfit => "NEGATIVE_EXPECTED_PROFIT",
            Self::UnknownCostBasis => "UNKNOWN_COST_BASIS",
            Self::InvalidSell => "INVALID_SELL",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SellProfitCheck {
    pub allowed: bool,
    pub reason: Option<SellBlockReason>,
    pub expected_sell_after_tax: f64,
    pub fifo_cost_basis_total: f64,
    pub amount: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ItemPerformance {
    pub item_name: String,
    pub product_id: String,
    pub buy_orders_placed: u64,
    pub buy_orders_filled: u64,
    pub buy_orders_cancelled: u64,
    pub sell_orders_placed: u64,
    pub sell_orders_filled: u64,
    pub sell_orders_cancelled: u64,
    pub successful_flips: u64,
    pub failed_flips: u64,
    pub realized_profit_total: i64,
    pub realized_profit_last_10m: i64,
    pub realized_profit_last_30m: i64,
    pub realized_profit_last_60m: i64,
    pub avg_realized_profit_per_flip: f64,
    pub avg_realized_profit_per_hour: f64,
    pub avg_buy_fill_seconds: f64,
    pub avg_sell_fill_seconds: f64,
    pub avg_total_cycle_seconds: f64,
    pub avg_hold_seconds: f64,
    pub current_open_buy_capital: f64,
    pub current_open_sell_value: f64,
    pub current_cost_lot_value: f64,
    pub max_cost_lot_age_seconds: u64,
    pub reprice_count: u64,
    pub cancel_count: u64,
    pub failed_search_count: u64,
    pub cannot_afford_count: u64,
    pub unknown_cost_basis_count: u64,
    pub negative_profit_block_count: u64,
    pub last_success_timestamp: Option<u64>,
    pub last_failure_timestamp: Option<u64>,
    pub cooldown_until: Option<u64>,
    pub block_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CostBasisStatus {
    Known,
    PartialKnownCostBasis,
    UnknownCostBasis,
}

impl CostBasisStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Known => "KNOWN",
            Self::PartialKnownCostBasis => "PARTIAL_KNOWN_COST_BASIS",
            Self::UnknownCostBasis => "UNKNOWN_COST_BASIS",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SellProfitAudit {
    pub item_name: String,
    pub sold_amount: u64,
    pub claimed_coins_after_tax: f64,
    pub gross_list_value: Option<f64>,
    pub lots_used: Vec<BuyCostLotUsage>,
    pub cost_basis_total: f64,
    pub cost_basis_status: CostBasisStatus,
    pub realized_profit: i64,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BazaarProfitAuditSnapshot {
    pub current_fifo_realized_profit_total: i64,
    pub unknown_cost_basis_sell_total_count: u64,
    pub ignored_legacy_profit_total: i64,
    pub open_buy_capital: f64,
    pub open_sell_value: f64,
    pub active_buy_orders: usize,
    pub active_sell_orders: usize,
    pub remaining_cost_lots: HashMap<String, Vec<BuyCostLot>>,
    pub remaining_cost_lot_value: f64,
    pub estimated_sell_value_after_tax: f64,
    pub estimated_unrealized_profit: f64,
    pub stale_buy_orders: usize,
    pub stale_sell_orders: usize,
    pub items_waiting_for_sell: Vec<String>,
    pub blocked_items_waiting_for_sell: Vec<String>,
    pub blocked_cost_lot_value: f64,
    pub actionable_cost_lot_value: f64,
    pub last_sell_audit_at: Option<u64>,
    pub web_graph_source: String,
    pub last_sell_audit: Option<SellProfitAudit>,
    pub item_performance: HashMap<String, ItemPerformance>,
    pub cleanup_state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct BazaarProfitAuditState {
    current_fifo_realized_profit_total: i64,
    unknown_cost_basis_sell_total_count: u64,
    ignored_legacy_profit_total: i64,
    last_sell_audit: Option<SellProfitAudit>,
    last_sell_audit_at: Option<u64>,
}

/// Thread-safe tracker for active bazaar orders.
#[derive(Clone)]
pub struct BazaarOrderTracker {
    orders: Arc<RwLock<Vec<TrackedBazaarOrder>>>,
    /// FIFO buy-cost lots collected locally in this session/process.
    buy_cost_lots: Arc<RwLock<HashMap<String, VecDeque<BuyCostLot>>>>,
    /// Per-item profit data from `/cofl bz l` output. Stored only for diagnostics;
    /// it is never used as a graph/profit fallback.
    bz_list_profits: Arc<RwLock<HashMap<String, (i64, u32)>>>,
    profit_audit: Arc<RwLock<BazaarProfitAuditState>>,
    /// SELL order targets created by the local Hypixel Bazaar scanner.
    /// Maps normalized item name → (target_sell_price_per_unit, item_tag, planned_amount, recorded_at).
    planned_local_sells: Arc<RwLock<HashMap<String, (f64, Option<String>, Option<u64>, u64)>>>,
    /// SELL fills that were already accounted locally but may still appear as
    /// the original amount in the next Manage Orders snapshot.
    recently_collected_sell_fills: Arc<RwLock<HashMap<String, u64>>>,
    item_performance: Arc<RwLock<HashMap<String, ItemPerformance>>>,
    end_phase_state: Arc<RwLock<String>>,
}

impl BazaarOrderTracker {
    pub fn new() -> Self {
        let tracker = Self {
            orders: Arc::new(RwLock::new(Vec::new())),
            buy_cost_lots: Arc::new(RwLock::new(HashMap::new())),
            bz_list_profits: Arc::new(RwLock::new(HashMap::new())),
            profit_audit: Arc::new(RwLock::new(BazaarProfitAuditState::default())),
            planned_local_sells: Arc::new(RwLock::new(HashMap::new())),
            recently_collected_sell_fills: Arc::new(RwLock::new(HashMap::new())),
            item_performance: Arc::new(RwLock::new(HashMap::new())),
            end_phase_state: Arc::new(RwLock::new("IDLE".to_string())),
        };
        tracker.load_from_disk();
        tracker
    }

    /// Create a tracker that does NOT load from / save to disk.
    /// Used in unit tests to avoid cross-test interference.
    #[cfg(test)]
    pub fn new_in_memory() -> Self {
        Self {
            orders: Arc::new(RwLock::new(Vec::new())),
            buy_cost_lots: Arc::new(RwLock::new(HashMap::new())),
            bz_list_profits: Arc::new(RwLock::new(HashMap::new())),
            profit_audit: Arc::new(RwLock::new(BazaarProfitAuditState::default())),
            planned_local_sells: Arc::new(RwLock::new(HashMap::new())),
            recently_collected_sell_fills: Arc::new(RwLock::new(HashMap::new())),
            item_performance: Arc::new(RwLock::new(HashMap::new())),
            end_phase_state: Arc::new(RwLock::new("IDLE".to_string())),
        }
    }

    /// Record a newly placed bazaar order.
    pub fn add_order(
        &self,
        item_name: String,
        amount: u64,
        price_per_unit: f64,
        is_buy_order: bool,
    ) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let item_for_perf = item_name.clone();
        self.orders.write().push(TrackedBazaarOrder {
            item_name,
            amount,
            price_per_unit,
            is_buy_order,
            status: "open".to_string(),
            placed_at: now,
        });
        self.update_performance(&item_for_perf, |p| {
            if is_buy_order {
                p.buy_orders_placed += 1;
            } else {
                p.sell_orders_placed += 1;
            }
        });
        self.save_orders_to_disk();
    }

    /// Mark the most recent matching open order as `"filled"`.
    pub fn mark_filled(&self, item_name: &str, is_buy_order: bool) {
        let mut orders = self.orders.write();
        if let Some(order) = orders.iter_mut().rev().find(|o| {
            o.status == "open"
                && o.is_buy_order == is_buy_order
                && normalize_for_match(&o.item_name) == normalize_for_match(item_name)
        }) {
            order.status = "filled".to_string();
        }
        drop(orders);
        self.save_orders_to_disk();
    }

    /// Remove a matching order (on collect or cancel) and return its data
    /// so the caller can use price/amount for profit calculation.
    pub fn remove_order(&self, item_name: &str, is_buy_order: bool) -> Option<TrackedBazaarOrder> {
        let mut orders = self.orders.write();
        let result = if let Some(pos) = orders.iter().rposition(|o| {
            (o.status == "open" || o.status == "filled")
                && o.is_buy_order == is_buy_order
                && normalize_for_match(&o.item_name) == normalize_for_match(item_name)
        }) {
            Some(orders.remove(pos))
        } else {
            None
        };
        drop(orders);
        self.save_orders_to_disk();
        result
    }

    /// Remove the oldest matching open/filled order. ManageOrders cancels stale
    /// orders by walking the in-game order list; when multiple same-item orders
    /// exist, removing the newest tracker entry leaves the stale one behind and
    /// causes cancel/relist churn.
    pub fn remove_oldest_order(
        &self,
        item_name: &str,
        is_buy_order: bool,
    ) -> Option<TrackedBazaarOrder> {
        let mut orders = self.orders.write();
        let target = normalize_for_match(item_name);
        let result = orders
            .iter()
            .enumerate()
            .filter(|(_, o)| {
                (o.status == "open" || o.status == "filled")
                    && o.is_buy_order == is_buy_order
                    && normalize_for_match(&o.item_name) == target
            })
            .min_by_key(|(_, o)| o.placed_at)
            .map(|(idx, _)| idx)
            .map(|pos| orders.remove(pos));
        drop(orders);
        self.save_orders_to_disk();
        result
    }

    /// Remove the collected portion of an order and keep any unfilled remainder
    /// tracked as open. Bazaar orders can be partially claimable before the
    /// whole order has filled; removing the full order at that point
    /// underreports locked capital and breaks the buy/sell lifecycle.
    pub fn remove_or_reduce_order_on_collect(
        &self,
        item_name: &str,
        is_buy_order: bool,
        claimed_amount: Option<u64>,
    ) -> Option<TrackedBazaarOrder> {
        let mut orders = self.orders.write();
        let result = if let Some(pos) = orders.iter().rposition(|o| {
            (o.status == "open" || o.status == "filled")
                && o.is_buy_order == is_buy_order
                && normalize_for_match(&o.item_name) == normalize_for_match(item_name)
        }) {
            let tracked_amount = orders[pos].amount;
            let collected_amount = claimed_amount
                .filter(|amount| *amount > 0)
                .unwrap_or(tracked_amount)
                .min(tracked_amount);
            let mut collected = orders[pos].clone();
            collected.amount = collected_amount;

            if collected_amount < tracked_amount {
                orders[pos].amount = tracked_amount - collected_amount;
                orders[pos].status = "open".to_string();
            } else {
                orders.remove(pos);
            }
            if !is_buy_order && collected_amount > 0 {
                *self
                    .recently_collected_sell_fills
                    .write()
                    .entry(normalize_for_match(item_name))
                    .or_insert(0) += collected_amount;
            }
            Some(collected)
        } else {
            None
        };
        drop(orders);
        self.save_orders_to_disk();
        result
    }

    /// Return a snapshot of all tracked orders.
    pub fn get_orders(&self) -> Vec<TrackedBazaarOrder> {
        self.orders.read().clone()
    }

    pub fn filled_order_count(&self, is_buy_order: bool) -> usize {
        self.orders
            .read()
            .iter()
            .filter(|o| o.is_buy_order == is_buy_order && o.status == "filled")
            .count()
    }

    pub fn stale_order_count(&self, is_buy_order: bool, max_age_secs: u64) -> usize {
        let now = Self::now_secs();
        self.orders
            .read()
            .iter()
            .filter(|o| {
                o.is_buy_order == is_buy_order
                    && (o.status == "open" || o.status == "filled")
                    && now.saturating_sub(o.placed_at) >= max_age_secs
            })
            .count()
    }

    pub fn oldest_stale_order(
        &self,
        is_buy_order: bool,
        max_age_secs: u64,
    ) -> Option<TrackedBazaarOrder> {
        let now = Self::now_secs();
        self.orders
            .read()
            .iter()
            .filter(|o| {
                o.is_buy_order == is_buy_order
                    && (o.status == "open" || o.status == "filled")
                    && now.saturating_sub(o.placed_at) >= max_age_secs
            })
            .min_by_key(|o| o.placed_at)
            .cloned()
    }

    /// Remove all tracked orders and persist.  Used on startup to get a clean
    /// view since the in-game ManageOrders cycle will cancel everything.
    pub fn clear_all_orders(&self) -> usize {
        let mut orders = self.orders.write();
        let removed = orders.len();
        orders.clear();
        drop(orders);
        self.save_orders_to_disk();
        removed
    }

    /// Returns `true` if at least one tracked order has status `"filled"`.
    /// Used by the periodic ManageOrders timer to skip GUI cycles when there
    /// is nothing to collect.
    pub fn has_filled_orders(&self) -> bool {
        self.orders.read().iter().any(|o| o.status == "filled")
    }

    /// Remove orders older than `max_age_secs` seconds.
    /// Returns the number of stale orders removed.
    pub fn remove_stale_orders(&self, max_age_secs: u64) -> usize {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut orders = self.orders.write();
        let original_len = orders.len();
        orders.retain(|o| now.saturating_sub(o.placed_at) < max_age_secs);
        let removed = original_len - orders.len();
        drop(orders);
        if removed > 0 {
            self.save_orders_to_disk();
        }
        removed
    }

    /// Reconcile the tracker with the orders currently visible in-game.
    ///
    /// `ingame_orders` is the list of `(item_name, is_buy_order, amount, price_per_unit)`
    /// tuples taken from the Bazaar Orders window during a ManageOrders cycle.
    /// Any tracked order whose item+type does **not** appear in this list is
    /// removed so the web panel stays in sync with the actual in-game state.
    /// Sell orders backed by FIFO CostLots are preserved across missing
    /// snapshots because listed items are held in Bazaar escrow until claim or
    /// cancel confirms their final state.
    ///
    /// Orders visible in-game but NOT yet tracked (e.g. placed before the bot
    /// started, or placed manually) are added as new entries so the web panel
    /// shows all active orders from startup.
    ///
    /// Duplicate same-item orders are handled by counting occurrences: if the
    /// in-game window shows 2 "Coal" buy orders, at most 2 tracked "Coal" buy
    /// orders are kept.
    ///
    /// Returns the number of stale tracker entries removed.
    pub fn reconcile_with_ingame(&self, ingame_orders: &[(String, bool, u64, f64)]) -> usize {
        // Build a count map: (normalized_name, is_buy) → how many in-game.
        let mut ingame_counts: std::collections::HashMap<(String, bool), usize> =
            std::collections::HashMap::new();
        let mut ingame_details: std::collections::HashMap<(String, bool), Vec<(u64, f64)>> =
            std::collections::HashMap::new();
        for (name, is_buy, amount, _) in ingame_orders {
            if !*is_buy && *amount == 0 {
                debug!(
                    "[BazaarTracker] Ignoring zero-amount in-game SELL order for {} during reconcile",
                    name
                );
                continue;
            }
            *ingame_counts
                .entry((normalize_for_match(name), *is_buy))
                .or_insert(0) += 1;
        }
        for (name, is_buy, amount, price_per_unit) in ingame_orders {
            if !*is_buy && *amount == 0 {
                debug!(
                    "[BazaarTracker] Ignoring zero-amount in-game SELL order for {} during reconcile",
                    name
                );
                continue;
            }
            ingame_details
                .entry((normalize_for_match(name), *is_buy))
                .or_default()
                .push((*amount, *price_per_unit));
        }
        let fifo_amount_by_item: std::collections::HashMap<String, u64> = self
            .buy_cost_lots
            .read()
            .iter()
            .map(|(item, lots)| (item.clone(), lots.iter().map(|lot| lot.amount).sum::<u64>()))
            .collect();
        let mut unaccounted_buy_fills_by_item = fifo_amount_by_item.clone();
        let now = Self::now_secs();
        let mut orders = self.orders.write();
        let mut recently_collected_sell_fills = self.recently_collected_sell_fills.write();
        let original_len = orders.len();
        // Track how many of each (item, side) we have already kept so we
        // don't exceed the in-game count.
        let mut kept_counts: std::collections::HashMap<(String, bool), usize> =
            std::collections::HashMap::new();
        let mut preserved_fifo_sell_orders = 0usize;
        orders.retain_mut(|o| {
            let key = (normalize_for_match(&o.item_name), o.is_buy_order);
            let allowed = ingame_counts.get(&key).copied().unwrap_or(0);
            let kept = kept_counts.entry(key.clone()).or_insert(0);
            if *kept < allowed {
                if let Some((amount, price_per_unit)) = ingame_details
                    .get(&key)
                    .and_then(|rows| rows.get(*kept))
                    .copied()
                {
                    let mut reconciled_amount = amount;
                    if o.is_buy_order {
                        if let Some(already_collected) =
                            unaccounted_buy_fills_by_item.get_mut(&key.0)
                        {
                            let offset = (*already_collected).min(reconciled_amount);
                            if offset > 0 {
                                reconciled_amount = reconciled_amount.saturating_sub(offset);
                                *already_collected -= offset;
                                debug!(
                                    "[BAF][PARTIAL_BUY_RECONCILE] item={} ingame_amount={} already_collected_fifo_amount={} open_amount={}",
                                    o.item_name, amount, offset, reconciled_amount
                                );
                            }
                        }
                    } else if let Some(already_collected) =
                        recently_collected_sell_fills.get_mut(&key.0)
                    {
                        if reconciled_amount > o.amount {
                            let overreported_amount = reconciled_amount - o.amount;
                            let offset = (*already_collected).min(overreported_amount);
                            if offset > 0 {
                                reconciled_amount = reconciled_amount.saturating_sub(offset);
                                *already_collected -= offset;
                                debug!(
                                    "[BAF][PARTIAL_SELL_RECONCILE] item={} ingame_amount={} tracked_open_amount={} already_collected_sell_amount={} open_amount={}",
                                    o.item_name, amount, o.amount, offset, reconciled_amount
                                );
                            }
                        } else {
                            *already_collected = 0;
                        }
                    }
                    if reconciled_amount > 0 {
                        o.amount = reconciled_amount;
                    }
                    if price_per_unit > 0.0 {
                        if (o.price_per_unit - price_per_unit).abs() > 0.01 {
                            o.placed_at = now;
                        }
                        o.price_per_unit = price_per_unit;
                    }
                }
                *kept += 1;
                true
            } else if !o.is_buy_order && fifo_amount_by_item.get(&key.0).copied().unwrap_or(0) > 0 {
                // A just-listed sell offer moves items into Bazaar escrow, so a
                // transient or incomplete Manage Orders snapshot must not make
                // us drop the sell-side bridge needed for FIFO profit claiming.
                preserved_fifo_sell_orders += 1;
                *kept += 1;
                true
            } else {
                false
            }
        });
        let removed = original_len - orders.len();

        // Add in-game orders that aren't already tracked.
        // Iterate over unique keys to avoid duplicate additions.
        let mut added = 0usize;

        // Build a map from (normalized_name, is_buy) → Vec<(amount, price)>
        // so we can pick the correct data for each missing order.
        let mut ingame_data: std::collections::HashMap<(String, bool), Vec<(u64, f64)>> =
            std::collections::HashMap::new();
        for (name, is_buy, amount, price) in ingame_orders {
            if !*is_buy && *amount == 0 {
                continue;
            }
            ingame_data
                .entry((normalize_for_match(name), *is_buy))
                .or_default()
                .push((*amount, *price));
        }

        for (key, data_entries) in &ingame_data {
            let tracked = kept_counts.get(key).copied().unwrap_or(0);
            let needed = data_entries.len();
            for idx in tracked..needed {
                let (mut amount, price) = data_entries[idx];
                if key.1 {
                    if let Some(already_collected) = unaccounted_buy_fills_by_item.get_mut(&key.0) {
                        let offset = (*already_collected).min(amount);
                        if offset > 0 {
                            amount = amount.saturating_sub(offset);
                            *already_collected -= offset;
                            debug!(
                                "[BAF][PARTIAL_BUY_RECONCILE] item={} ingame_amount={} already_collected_fifo_amount={} open_amount={}",
                                key.0, data_entries[idx].0, offset, amount
                            );
                        }
                    }
                } else if let Some(already_collected) =
                    recently_collected_sell_fills.get_mut(&key.0)
                {
                    let offset = (*already_collected).min(amount);
                    if offset > 0 {
                        amount = amount.saturating_sub(offset);
                        *already_collected -= offset;
                        debug!(
                            "[BAF][PARTIAL_SELL_RECONCILE] item={} ingame_amount={} already_collected_sell_amount={} open_amount={}",
                            key.0, data_entries[idx].0, offset, amount
                        );
                    }
                }
                if amount == 0 {
                    continue;
                }
                // Use title case for the item name from the first matching ingame order
                let display_name = ingame_orders
                    .iter()
                    .find(|(n, b, _, _)| normalize_for_match(n) == key.0 && *b == key.1)
                    .map(|(n, _, _, _)| n.clone())
                    .unwrap_or_else(|| key.0.clone());
                orders.push(TrackedBazaarOrder {
                    item_name: display_name,
                    amount,
                    price_per_unit: price,
                    is_buy_order: key.1,
                    status: "open".to_string(),
                    placed_at: now,
                });
                added += 1;
            }
            *kept_counts.entry(key.clone()).or_insert(0) = needed;
        }

        drop(orders);
        recently_collected_sell_fills.retain(|_, amount| *amount > 0);
        drop(recently_collected_sell_fills);
        if removed > 0 || added > 0 {
            if preserved_fifo_sell_orders > 0 {
                warn!(
                    "[BAF][SELL_ESCROW_GUARD] preserved {} sell order(s) with FIFO CostLots despite missing in-game snapshot",
                    preserved_fifo_sell_orders
                );
            }
            if added > 0 {
                debug!(
                    "[BazaarTracker] Added {} in-game orders not previously tracked",
                    added
                );
            }
            self.save_orders_to_disk();
        }
        removed
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    fn update_performance<F>(&self, item_name: &str, f: F)
    where
        F: FnOnce(&mut ItemPerformance),
    {
        let key = normalize_for_match(item_name);
        {
            let mut map = self.item_performance.write();
            let perf = map.entry(key.clone()).or_insert_with(|| ItemPerformance {
                item_name: item_name.to_string(),
                product_id: key.clone(),
                ..Default::default()
            });
            if perf.item_name.is_empty() {
                perf.item_name = item_name.to_string();
            }
            f(perf);
        }
        self.save_item_performance_to_disk();
    }

    pub fn set_end_phase_state(&self, state: impl Into<String>) {
        *self.end_phase_state.write() = state.into();
    }

    pub fn end_phase_state(&self) -> String {
        self.end_phase_state.read().clone()
    }

    pub fn remaining_cost_lot_value_for(&self, item_name: &str) -> f64 {
        let key = normalize_for_match(item_name);
        self.buy_cost_lots
            .read()
            .get(&key)
            .map(|lots| {
                lots.iter()
                    .map(|lot| lot.price_per_unit * lot.amount as f64)
                    .sum()
            })
            .unwrap_or(0.0)
    }

    pub fn open_order_value_for(&self, item_name: &str, is_buy_order: bool) -> f64 {
        let key = normalize_for_match(item_name);
        self.orders
            .read()
            .iter()
            .filter(|o| {
                o.is_buy_order == is_buy_order
                    && (o.status == "open" || o.status == "filled")
                    && normalize_for_match(&o.item_name) == key
            })
            .map(|o| o.price_per_unit * o.amount as f64)
            .sum()
    }

    pub fn item_cooldown_reason(&self, item_name: &str) -> Option<(u64, String)> {
        let key = normalize_for_match(item_name);
        let now = Self::now_secs();
        let map = self.item_performance.read();
        let perf = map.get(&key)?;
        let until = perf.cooldown_until?;
        let reason = perf
            .block_reason
            .clone()
            .unwrap_or_else(|| "ITEM_COOLDOWN".to_string());
        let effective_until = if reason == "PRODUCT_LOOKUP_FAILED" {
            perf.last_failure_timestamp
                .map(|ts| ts + PRODUCT_LOOKUP_FAILURE_COOLDOWN_SECONDS)
                .unwrap_or(until)
                .min(until)
        } else {
            until
        };
        if effective_until <= now {
            return None;
        }
        Some((effective_until, reason))
    }

    pub fn active_buy_order_on_cooldown(&self) -> Option<(String, String, u64)> {
        let orders = self.orders.read();
        for order in orders.iter().filter(|order| {
            order.is_buy_order && (order.status == "open" || order.status == "filled")
        }) {
            if let Some((until, reason)) = self.item_cooldown_reason(&order.item_name) {
                return Some((order.item_name.clone(), reason, until));
            }
        }
        None
    }

    pub fn cooldown_item(&self, item_name: &str, reason: &str, cooldown_seconds: u64) {
        let cooldown_seconds = cooldown_seconds.max(60);
        let normalized_reason = reason.trim().to_ascii_uppercase();
        self.update_performance(item_name, |p| {
            let now = Self::now_secs();
            let until = now.saturating_add(cooldown_seconds);
            p.cooldown_until = Some(p.cooldown_until.unwrap_or(0).max(until));
            p.block_reason = Some(normalized_reason.clone());
            p.last_failure_timestamp = Some(now);
        });
        warn!(
            "[BAF][ITEM_COOLDOWN] item={} reason={} action=manual_cooldown cooldown_seconds={}",
            item_name, normalized_reason, cooldown_seconds
        );
    }

    pub fn record_product_lookup_failed(&self, item_name: &str, reason: &str) {
        self.planned_local_sells
            .write()
            .remove(&normalize_for_match(item_name));
        let normalized_reason = if reason.trim().is_empty() {
            "PRODUCT_LOOKUP_FAILED"
        } else {
            reason.trim()
        };
        let now = Self::now_secs();
        self.update_performance(item_name, |p| {
            p.failed_search_count += 1;
            p.failed_flips += 1;
            p.last_failure_timestamp = Some(now);
            p.cooldown_until = Some(now + PRODUCT_LOOKUP_FAILURE_COOLDOWN_SECONDS);
            p.block_reason = Some(normalized_reason.to_string());
        });
        warn!(
            "[BAF][ITEM_COOLDOWN] item={} reason={} action=product_lookup_failed cooldown_seconds={}",
            item_name, normalized_reason, PRODUCT_LOOKUP_FAILURE_COOLDOWN_SECONDS
        );
    }

    pub fn record_reprice(&self, item_name: &str) {
        self.update_performance(item_name, |p| {
            p.reprice_count += 1;
        });
    }

    pub fn peek_fifo_cost_basis(&self, item_name: &str, amount: u64) -> Option<f64> {
        if amount == 0 {
            return None;
        }
        let key = normalize_for_match(item_name);
        let lots = self.buy_cost_lots.read();
        let mut remaining = amount;
        let mut total = 0.0;
        for lot in lots.get(&key)? {
            let used = remaining.min(lot.amount);
            total += lot.price_per_unit * used as f64;
            remaining -= used;
            if remaining == 0 {
                return Some(total);
            }
        }
        None
    }

    pub fn validate_sell_before_order(
        &self,
        item_name: &str,
        amount: u64,
        sell_price_per_unit: f64,
        bazaar_tax_rate: f64,
    ) -> SellProfitCheck {
        let tax_multiplier = 1.0 - (bazaar_tax_rate.max(0.0) / 100.0);
        let expected_sell_after_tax = sell_price_per_unit * amount as f64 * tax_multiplier;
        if amount == 0 || sell_price_per_unit <= 0.0 {
            return SellProfitCheck {
                allowed: false,
                reason: Some(SellBlockReason::InvalidSell),
                expected_sell_after_tax,
                fifo_cost_basis_total: 0.0,
                amount,
            };
        }
        let Some(cost_basis) = self.peek_fifo_cost_basis(item_name, amount) else {
            self.update_performance(item_name, |p| {
                p.unknown_cost_basis_count += 1;
                p.last_failure_timestamp = Some(Self::now_secs());
                p.block_reason = Some(SellBlockReason::UnknownCostBasis.as_str().to_string());
            });
            return SellProfitCheck {
                allowed: false,
                reason: Some(SellBlockReason::UnknownCostBasis),
                expected_sell_after_tax,
                fifo_cost_basis_total: 0.0,
                amount,
            };
        };
        if expected_sell_after_tax <= cost_basis {
            self.update_performance(item_name, |p| {
                p.negative_profit_block_count += 1;
                p.failed_flips += 1;
                p.last_failure_timestamp = Some(Self::now_secs());
                p.cooldown_until = Some(Self::now_secs() + 900);
                p.block_reason = Some(SellBlockReason::NegativeExpectedProfit.as_str().to_string());
            });
            info!(
                "[BAF][SELL_BLOCKED_NEGATIVE] item={} amount={} expected_after_tax={:.1} fifo_cost_basis={:.1} reason=NEGATIVE_EXPECTED_PROFIT",
                item_name, amount, expected_sell_after_tax, cost_basis
            );
            return SellProfitCheck {
                allowed: false,
                reason: Some(SellBlockReason::NegativeExpectedProfit),
                expected_sell_after_tax,
                fifo_cost_basis_total: cost_basis,
                amount,
            };
        }
        SellProfitCheck {
            allowed: true,
            reason: None,
            expected_sell_after_tax,
            fifo_cost_basis_total: cost_basis,
            amount,
        }
    }

    /// Record a collected buy order as an individual FIFO cost lot.
    ///
    /// Legacy weighted-average data from `bazaar_buy_costs.json` is not used
    /// by this path; only locally observed collected BUY lots can become cost
    /// basis for realized SELL profit.
    pub fn record_buy_cost(&self, item_name: &str, price_per_unit: f64, amount: u64) {
        if amount == 0 || price_per_unit <= 0.0 {
            warn!(
                "[BazaarProfit] Ignoring invalid BUY lot for {} — amount={}, ppu={:.2}",
                item_name, amount, price_per_unit
            );
            return;
        }
        let key = normalize_for_match(item_name);
        let lot = BuyCostLot {
            price_per_unit,
            amount,
            collected_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        };
        self.buy_cost_lots
            .write()
            .entry(key)
            .or_default()
            .push_back(lot);
        self.save_buy_cost_lots_to_disk();
    }

    /// Consume known FIFO lots for a SELL collection and return a complete audit row.
    /// Mixed claims can contain more sold items than locally known BUY lots when
    /// old inventory and tracked flips are collected together. In that case only
    /// the known FIFO-backed portion is realized; the unknown remainder is
    /// audited but never counted as profit.
    pub fn account_sell_collect(
        &self,
        item_name: &str,
        sold_amount: u64,
        claimed_coins_after_tax: f64,
        gross_list_value: Option<f64>,
    ) -> SellProfitAudit {
        let key = normalize_for_match(item_name);
        self.planned_local_sells.write().remove(&key);
        let available: u64 = self
            .buy_cost_lots
            .read()
            .get(&key)
            .map(|lots| lots.iter().map(|lot| lot.amount).sum())
            .unwrap_or(0);

        if sold_amount == 0
            || available == 0
            || claimed_coins_after_tax <= 0.0
            || !claimed_coins_after_tax.is_finite()
            || gross_list_value.is_none()
        {
            let reason = if sold_amount == 0 {
                "sold_amount_zero".to_string()
            } else if claimed_coins_after_tax <= 0.0
                || !claimed_coins_after_tax.is_finite()
                || gross_list_value.is_none()
            {
                "missing_known_sell_claim_value".to_string()
            } else {
                format!(
                    "insufficient_fifo_lots: needed {}, available {}",
                    sold_amount, available
                )
            };
            let audit = SellProfitAudit {
                item_name: item_name.to_string(),
                sold_amount,
                claimed_coins_after_tax,
                gross_list_value,
                lots_used: Vec::new(),
                cost_basis_total: 0.0,
                cost_basis_status: CostBasisStatus::UnknownCostBasis,
                realized_profit: 0,
                reason: Some(reason),
            };
            let mut state = self.profit_audit.write();
            state.unknown_cost_basis_sell_total_count += 1;
            state.last_sell_audit = Some(audit.clone());
            state.last_sell_audit_at = Some(Self::now_secs());
            drop(state);
            self.update_performance(item_name, |p| {
                p.unknown_cost_basis_count += 1;
                p.failed_flips += 1;
                p.last_failure_timestamp = Some(Self::now_secs());
                p.block_reason = audit.reason.clone();
            });
            return audit;
        }

        let known_fifo_amount = sold_amount.min(available);
        let unknown_fifo_amount = sold_amount.saturating_sub(known_fifo_amount);
        let known_ratio = known_fifo_amount as f64 / sold_amount.max(1) as f64;
        let known_claimed_coins_after_tax = claimed_coins_after_tax * known_ratio;
        let known_gross_list_value = gross_list_value.map(|value| value * known_ratio);

        let mut remaining = known_fifo_amount;
        let mut lots_used = Vec::new();
        let mut cost_basis_total = 0.0;
        {
            let mut all_lots = self.buy_cost_lots.write();
            let lots = all_lots.entry(key.clone()).or_default();
            while remaining > 0 {
                let mut lot = lots.pop_front().expect("availability checked above");
                let used_amount = remaining.min(lot.amount);
                let total_cost = lot.price_per_unit * used_amount as f64;
                lots_used.push(BuyCostLotUsage {
                    price_per_unit: lot.price_per_unit,
                    amount: used_amount,
                    total_cost,
                });
                cost_basis_total += total_cost;
                remaining -= used_amount;
                lot.amount -= used_amount;
                if lot.amount > 0 {
                    lots.push_front(lot);
                }
            }
            if lots.is_empty() {
                all_lots.remove(&key);
            }
        }
        self.save_buy_cost_lots_to_disk();

        let realized_profit = (known_claimed_coins_after_tax - cost_basis_total).round() as i64;
        let remaining_cost_lot_value_after = self.remaining_cost_lot_value_for(item_name);
        let active_sell_value_after = self.open_order_value_for(item_name, false);
        let active_buy_value_after = self.open_order_value_for(item_name, true);
        let can_clear_item_cooldown = remaining_cost_lot_value_after <= 1.0
            && active_sell_value_after <= 1.0
            && active_buy_value_after <= 1.0;
        let cost_basis_status = if unknown_fifo_amount > 0 {
            CostBasisStatus::PartialKnownCostBasis
        } else {
            CostBasisStatus::Known
        };
        let reason = if unknown_fifo_amount > 0 {
            Some(format!(
                "partial_known_cost_basis: sold {}, known {}, unknown {}",
                sold_amount, known_fifo_amount, unknown_fifo_amount
            ))
        } else {
            None
        };
        let audit = SellProfitAudit {
            item_name: item_name.to_string(),
            sold_amount: known_fifo_amount,
            claimed_coins_after_tax: known_claimed_coins_after_tax,
            gross_list_value: known_gross_list_value,
            lots_used,
            cost_basis_total,
            cost_basis_status,
            realized_profit,
            reason,
        };
        let mut state = self.profit_audit.write();
        state.current_fifo_realized_profit_total += realized_profit;
        if unknown_fifo_amount > 0 {
            state.unknown_cost_basis_sell_total_count += 1;
        }
        state.last_sell_audit = Some(audit.clone());
        state.last_sell_audit_at = Some(Self::now_secs());
        drop(state);
        self.update_performance(item_name, |p| {
            p.sell_orders_filled += 1;
            p.realized_profit_total += realized_profit;
            if unknown_fifo_amount > 0 {
                p.unknown_cost_basis_count += 1;
                p.failed_flips += 1;
                p.last_failure_timestamp = Some(Self::now_secs());
            }
            if realized_profit > 0 {
                p.successful_flips += 1;
                p.last_success_timestamp = Some(Self::now_secs());
                if can_clear_item_cooldown {
                    p.cooldown_until = None;
                    p.block_reason = None;
                } else {
                    debug!(
                        "[BAF][ITEM_COOLDOWN] item={} action=keep_after_partial_sell remaining_cost_lots={:.0} active_sell={:.0} active_buy={:.0}",
                        item_name,
                        remaining_cost_lot_value_after,
                        active_sell_value_after,
                        active_buy_value_after
                    );
                }
            } else {
                p.failed_flips += 1;
                p.last_failure_timestamp = Some(Self::now_secs());
            }
            let flips = p.successful_flips.max(1) as f64;
            p.avg_realized_profit_per_flip = p.realized_profit_total as f64 / flips;
        });
        info!(
            "[BAF][REALIZED_PROFIT] item={} amount={} cost_basis={:.1} claimed_after_tax={:.1} realized_profit={} status={} reason={}",
            item_name,
            known_fifo_amount,
            cost_basis_total,
            known_claimed_coins_after_tax,
            realized_profit,
            audit.cost_basis_status.as_str(),
            audit.reason.as_deref().unwrap_or("none")
        );
        audit
    }

    /// Compatibility helper for older tests/callers: consume an entire local
    /// FIFO balance and return its weighted average. Production SELL accounting
    /// must use `account_sell_collect` so partial fills do not corrupt lots.
    #[cfg(test)]
    pub fn take_buy_cost(&self, item_name: &str) -> Option<(f64, u64)> {
        let key = normalize_for_match(item_name);
        let lots = self.buy_cost_lots.write().remove(&key)?;
        let amount: u64 = lots.iter().map(|lot| lot.amount).sum();
        if amount == 0 {
            return None;
        }
        let total: f64 = lots
            .iter()
            .map(|lot| lot.price_per_unit * lot.amount as f64)
            .sum();
        Some((total / amount as f64, amount))
    }

    pub fn record_ignored_legacy_profit(&self, profit: i64, source: &str) {
        self.profit_audit.write().ignored_legacy_profit_total += profit;
        debug!(
            "[BazaarProfitAudit] Ignored legacy/external BZ profit from {}: {} coins",
            source, profit
        );
    }

    pub fn profit_audit_snapshot(&self) -> BazaarProfitAuditSnapshot {
        let state = self.profit_audit.read().clone();
        let now = Self::now_secs();
        let stale_after = 20 * 60;
        let orders = self.orders.read();
        let active_orders: Vec<&TrackedBazaarOrder> = orders
            .iter()
            .filter(|o| o.status == "open" || o.status == "filled")
            .collect();
        let open_buy_capital: f64 = active_orders
            .iter()
            .filter(|o| o.is_buy_order)
            .map(|o| o.price_per_unit * o.amount as f64)
            .sum();
        let open_sell_value: f64 = active_orders
            .iter()
            .filter(|o| !o.is_buy_order)
            .map(|o| o.price_per_unit * o.amount as f64)
            .sum();
        let active_buy_orders = active_orders.iter().filter(|o| o.is_buy_order).count();
        let active_sell_orders = active_orders.iter().filter(|o| !o.is_buy_order).count();
        let stale_buy_orders = active_orders
            .iter()
            .filter(|o| o.is_buy_order && now.saturating_sub(o.placed_at) >= stale_after)
            .count();
        let stale_sell_orders = active_orders
            .iter()
            .filter(|o| !o.is_buy_order && now.saturating_sub(o.placed_at) >= stale_after)
            .count();
        let tax_multiplier = 1.0 - 0.0125;
        let estimated_sell_value_after_tax = open_sell_value * tax_multiplier;
        let lots_guard = self.buy_cost_lots.read();
        let mut remaining_cost_lots = HashMap::new();
        let mut remaining_cost_lot_value = 0.0;
        let mut actionable_cost_lot_value = 0.0;
        let mut blocked_cost_lot_value = 0.0;
        let mut items_waiting_for_sell = Vec::new();
        let mut blocked_items_waiting_for_sell = Vec::new();
        let mut item_performance = self.item_performance.read().clone();
        for (item, lots) in lots_guard.iter() {
            let lot_vec: Vec<BuyCostLot> = lots.iter().cloned().collect();
            let lot_amount: u64 = lots.iter().map(|lot| lot.amount).sum();
            let item_value: f64 = lots
                .iter()
                .map(|lot| lot.price_per_unit * lot.amount as f64)
                .sum();
            remaining_cost_lot_value += item_value;
            let active_sell_amount: u64 = active_orders
                .iter()
                .filter(|o| !o.is_buy_order && normalize_for_match(&o.item_name) == *item)
                .map(|o| o.amount)
                .sum();
            if item_value > 0.0 && active_sell_amount < lot_amount {
                let blocked_by_cooldown = item_performance
                    .get(item)
                    .and_then(|perf| perf.cooldown_until.map(|until| (until, perf)))
                    .map(|(until, perf)| {
                        until > now
                            && perf
                                .block_reason
                                .as_deref()
                                .map(|reason| {
                                    matches!(
                                        reason,
                                        "NEGATIVE_EXPECTED_PROFIT"
                                            | "PRODUCT_LOOKUP_FAILED"
                                            | "COST_LOT_NOT_IN_INVENTORY"
                                    )
                                })
                                .unwrap_or(false)
                    })
                    .unwrap_or(false);
                if blocked_by_cooldown {
                    blocked_items_waiting_for_sell.push(item.clone());
                    blocked_cost_lot_value += item_value;
                } else {
                    items_waiting_for_sell.push(item.clone());
                    actionable_cost_lot_value += item_value;
                }
            }
            remaining_cost_lots.insert(item.clone(), lot_vec);
        }
        for perf in item_performance.values_mut() {
            perf.current_open_buy_capital = active_orders
                .iter()
                .filter(|o| {
                    o.is_buy_order
                        && normalize_for_match(&o.item_name) == normalize_for_match(&perf.item_name)
                })
                .map(|o| o.price_per_unit * o.amount as f64)
                .sum();
            perf.current_open_sell_value = active_orders
                .iter()
                .filter(|o| {
                    !o.is_buy_order
                        && normalize_for_match(&o.item_name) == normalize_for_match(&perf.item_name)
                })
                .map(|o| o.price_per_unit * o.amount as f64)
                .sum();
            perf.current_cost_lot_value = 0.0;
            perf.max_cost_lot_age_seconds = 0;
            if let Some(lots) = lots_guard.get(&normalize_for_match(&perf.item_name)) {
                perf.current_cost_lot_value = lots
                    .iter()
                    .map(|lot| lot.price_per_unit * lot.amount as f64)
                    .sum();
                perf.max_cost_lot_age_seconds = lots
                    .iter()
                    .map(|lot| now.saturating_sub(lot.collected_at))
                    .max()
                    .unwrap_or(0);
            }
        }
        BazaarProfitAuditSnapshot {
            current_fifo_realized_profit_total: state.current_fifo_realized_profit_total,
            unknown_cost_basis_sell_total_count: state.unknown_cost_basis_sell_total_count,
            ignored_legacy_profit_total: state.ignored_legacy_profit_total,
            open_buy_capital,
            open_sell_value,
            active_buy_orders,
            active_sell_orders,
            remaining_cost_lots,
            remaining_cost_lot_value,
            estimated_sell_value_after_tax,
            estimated_unrealized_profit: estimated_sell_value_after_tax - remaining_cost_lot_value,
            stale_buy_orders,
            stale_sell_orders,
            items_waiting_for_sell,
            blocked_items_waiting_for_sell,
            blocked_cost_lot_value,
            actionable_cost_lot_value,
            last_sell_audit_at: state.last_sell_audit_at,
            web_graph_source: "local_fifo_known_cost_basis_only".to_string(),
            last_sell_audit: state.last_sell_audit,
            item_performance,
            cleanup_state: self.end_phase_state(),
        }
    }

    /// Replace the per-item profit map with data parsed from `/cofl bz l`.
    /// Called after collecting all flip lines from a single `/cofl bz l` response.
    pub fn set_bz_list_profits(&self, items: HashMap<String, (i64, u32)>) {
        let normalized: HashMap<String, (i64, u32)> = items
            .into_iter()
            .map(|(k, v)| (normalize_for_match(&k), v))
            .collect();
        *self.bz_list_profits.write() = normalized;
    }

    /// Return the total profit for an item from the latest `/cofl bz l` data.
    /// Used as a fallback when local buy-cost tracking has no data for a sell.
    /// Returns the profit exactly as shown in the `/cofl bz l` list for that item.
    pub fn get_bz_list_profit(&self, item_name: &str) -> Option<i64> {
        let key = normalize_for_match(item_name);
        let data = self.bz_list_profits.read();
        data.get(&key).map(|(total, _count)| *total)
    }

    /// Remember the target sell price for a locally scanned BUY order.
    pub fn record_planned_local_sell(
        &self,
        item_name: &str,
        sell_price_per_unit: f64,
        item_tag: Option<String>,
    ) {
        self.planned_local_sells.write().insert(
            normalize_for_match(item_name),
            (sell_price_per_unit, item_tag, None, Self::now_secs()),
        );
    }

    /// Remember a local SELL order amount before the GUI confirmation arrives.
    pub fn record_planned_local_sell_amount(
        &self,
        item_name: &str,
        sell_price_per_unit: f64,
        item_tag: Option<String>,
        amount: u64,
    ) {
        self.planned_local_sells.write().insert(
            normalize_for_match(item_name),
            (
                sell_price_per_unit,
                item_tag,
                Some(amount),
                Self::now_secs(),
            ),
        );
    }

    /// Return the remembered local SELL amount without consuming the planned sell.
    pub fn planned_local_sell_amount(&self, item_name: &str) -> Option<u64> {
        let key = normalize_for_match(item_name);
        let now = Self::now_secs();
        let mut planned = self.planned_local_sells.write();
        let Some((_, _, amount, recorded_at)) = planned.get(&key).cloned() else {
            return None;
        };
        if amount.is_some()
            && now.saturating_sub(recorded_at) > PLANNED_LOCAL_SELL_ORDER_TTL_SECONDS
        {
            planned.remove(&key);
            warn!(
                "[BAF][PLANNED_SELL_EXPIRED] item={} planned_amount={} age={}s action=clear_stale_sell_escrow",
                item_name,
                amount.unwrap_or(0),
                now.saturating_sub(recorded_at)
            );
            return None;
        }
        amount
    }

    pub fn planned_local_sell(
        &self,
        item_name: &str,
    ) -> Option<(f64, Option<String>, Option<u64>)> {
        let key = normalize_for_match(item_name);
        let now = Self::now_secs();
        let mut planned = self.planned_local_sells.write();
        let Some((price, tag, amount, recorded_at)) = planned.get(&key).cloned() else {
            return None;
        };
        if amount.is_some()
            && now.saturating_sub(recorded_at) > PLANNED_LOCAL_SELL_ORDER_TTL_SECONDS
        {
            planned.remove(&key);
            warn!(
                "[BAF][PLANNED_SELL_EXPIRED] item={} planned_amount={} age={}s action=clear_stale_sell_escrow",
                item_name,
                amount.unwrap_or(0),
                now.saturating_sub(recorded_at)
            );
            return None;
        }
        Some((price, tag, amount))
    }

    pub fn remaining_cost_lot_amount_for(&self, item_name: &str) -> u64 {
        self.buy_cost_lots
            .read()
            .get(&normalize_for_match(item_name))
            .map(|lots| lots.iter().map(|lot| lot.amount).sum())
            .unwrap_or(0)
    }

    pub fn active_sell_amount_for(&self, item_name: &str) -> u64 {
        let normalized = normalize_for_match(item_name);
        self.orders
            .read()
            .iter()
            .filter(|order| {
                !order.is_buy_order
                    && (order.status == "open" || order.status == "filled")
                    && normalize_for_match(&order.item_name) == normalized
            })
            .map(|order| order.amount)
            .sum()
    }

    pub fn active_sell_order_count_for(&self, item_name: &str) -> usize {
        let normalized = normalize_for_match(item_name);
        self.orders
            .read()
            .iter()
            .filter(|order| {
                !order.is_buy_order
                    && (order.status == "open" || order.status == "filled")
                    && normalize_for_match(&order.item_name) == normalized
            })
            .count()
    }

    pub fn duplicate_active_sell_order_target(&self) -> Option<(String, usize, u64)> {
        let mut grouped: HashMap<String, (String, usize, u64, u64)> = HashMap::new();
        for order in self.orders.read().iter().filter(|order| {
            !order.is_buy_order
                && order.amount > 0
                && (order.status == "open" || order.status == "filled")
        }) {
            let normalized = normalize_for_match(&order.item_name);
            let entry = grouped.entry(normalized).or_insert((
                order.item_name.clone(),
                0,
                0,
                order.placed_at,
            ));
            entry.1 += 1;
            entry.2 = entry.2.saturating_add(order.amount);
            if order.placed_at <= entry.3 {
                entry.0 = order.item_name.clone();
                entry.3 = order.placed_at;
            }
        }

        grouped
            .into_values()
            .filter(|(_, count, _, _)| *count > 1)
            .max_by(|a, b| {
                a.1.cmp(&b.1)
                    .then_with(|| a.2.cmp(&b.2))
                    .then_with(|| b.3.cmp(&a.3))
            })
            .map(|(item_name, count, amount, _)| (item_name, count, amount))
    }

    pub fn mark_cost_lots_missing_from_inventory(
        &self,
        item_name: &str,
        observed_inventory_amount: u64,
    ) -> Option<(u64, f64)> {
        self.mark_cost_lot_excess_missing_from_inventory(
            item_name,
            observed_inventory_amount,
            observed_inventory_amount,
        )
    }

    pub fn mark_cost_lot_excess_missing_from_inventory(
        &self,
        item_name: &str,
        keep_amount: u64,
        observed_inventory_amount: u64,
    ) -> Option<(u64, f64)> {
        let key = normalize_for_match(item_name);
        let mut map = self.buy_cost_lots.write();
        let lots = map.get_mut(&key)?;
        let total_amount: u64 = lots.iter().map(|lot| lot.amount).sum();
        let mut amount_to_remove = total_amount.saturating_sub(keep_amount);
        if amount_to_remove == 0 {
            return None;
        }

        let mut removed_amount = 0u64;
        let mut removed_value = 0.0;
        while amount_to_remove > 0 {
            let Some(mut lot) = lots.pop_back() else {
                break;
            };
            if lot.amount <= amount_to_remove {
                removed_amount += lot.amount;
                removed_value += lot.price_per_unit * lot.amount as f64;
                amount_to_remove -= lot.amount;
            } else {
                removed_amount += amount_to_remove;
                removed_value += lot.price_per_unit * amount_to_remove as f64;
                lot.amount -= amount_to_remove;
                lots.push_back(lot);
                amount_to_remove = 0;
            }
        }
        if lots.is_empty() {
            map.remove(&key);
            self.planned_local_sells.write().remove(&key);
        }
        drop(map);

        self.save_buy_cost_lots_to_disk();
        self.update_performance(item_name, |p| {
            p.failed_flips += removed_amount.max(1);
            p.last_failure_timestamp = Some(Self::now_secs());
            p.cooldown_until = Some(Self::now_secs() + 1800);
            p.block_reason = Some("COST_LOT_NOT_IN_INVENTORY".to_string());
        });
        warn!(
            "[BAF][COST_LOT_MISSING_INVENTORY] item={} removed_fifo_amount={} removed_fifo_value={:.1} kept_fifo_amount={} observed_inventory_amount={} action=removed_missing_fifo_excess",
            item_name, removed_amount, removed_value, keep_amount, observed_inventory_amount
        );
        Some((removed_amount, removed_value))
    }

    /// Consume the target sell price after the corresponding BUY order is collected.
    pub fn take_planned_local_sell(&self, item_name: &str) -> Option<(f64, Option<String>)> {
        self.planned_local_sells
            .write()
            .remove(&normalize_for_match(item_name))
            .map(|(price, tag, _, _)| (price, tag))
    }

    // ── Persistence helpers ──

    fn persistence_dir() -> std::path::PathBuf {
        crate::logging::get_logs_dir()
    }

    fn save_orders_to_disk(&self) {
        #[cfg(test)]
        return;
        #[cfg(not(test))]
        {
            let orders = self.orders.read().clone();
            let path = Self::persistence_dir().join(ORDERS_FILE);
            if let Err(e) = std::fs::create_dir_all(Self::persistence_dir()) {
                warn!("[BazaarTracker] Failed to create persistence dir: {}", e);
                return;
            }
            match serde_json::to_string(&orders) {
                Ok(json) => {
                    if let Err(e) = std::fs::write(&path, json) {
                        warn!("[BazaarTracker] Failed to write {}: {}", path.display(), e);
                    }
                }
                Err(e) => warn!("[BazaarTracker] Failed to serialize orders: {}", e),
            }
        }
    }

    fn save_buy_cost_lots_to_disk(&self) {
        #[cfg(test)]
        return;
        #[cfg(not(test))]
        {
            let costs: HashMap<String, Vec<BuyCostLot>> = self
                .buy_cost_lots
                .read()
                .iter()
                .map(|(item, lots)| (item.clone(), lots.iter().cloned().collect()))
                .collect();
            let path = Self::persistence_dir().join(BUY_COST_LOTS_FILE);
            if let Err(e) = std::fs::create_dir_all(Self::persistence_dir()) {
                warn!("[BazaarTracker] Failed to create persistence dir: {}", e);
                return;
            }
            match serde_json::to_string(&costs) {
                Ok(json) => {
                    if let Err(e) = std::fs::write(&path, json) {
                        warn!("[BazaarTracker] Failed to write {}: {}", path.display(), e);
                    }
                }
                Err(e) => warn!(
                    "[BazaarTracker] Failed to serialize FIFO buy-cost lots: {}",
                    e
                ),
            }
        }
    }

    fn save_item_performance_to_disk(&self) {
        #[cfg(test)]
        return;
        #[cfg(not(test))]
        {
            let performance = self.item_performance.read().clone();
            let path = Self::persistence_dir().join(ITEM_PERFORMANCE_FILE);
            if let Err(e) = std::fs::create_dir_all(Self::persistence_dir()) {
                warn!("[BazaarTracker] Failed to create persistence dir: {}", e);
                return;
            }
            match serde_json::to_string(&performance) {
                Ok(json) => {
                    if let Err(e) = std::fs::write(&path, json) {
                        warn!("[BazaarTracker] Failed to write {}: {}", path.display(), e);
                    }
                }
                Err(e) => warn!(
                    "[BazaarTracker] Failed to serialize item performance: {}",
                    e
                ),
            }
        }
    }

    fn load_from_disk(&self) {
        let orders_path = Self::persistence_dir().join(ORDERS_FILE);
        if orders_path.exists() {
            match std::fs::read_to_string(&orders_path) {
                Ok(json) => match serde_json::from_str::<Vec<TrackedBazaarOrder>>(&json) {
                    Ok(orders) => {
                        debug!("[BazaarTracker] Loaded {} orders from disk", orders.len());
                        *self.orders.write() = orders;
                    }
                    Err(e) => warn!(
                        "[BazaarTracker] Failed to parse {}: {}",
                        orders_path.display(),
                        e
                    ),
                },
                Err(e) => warn!(
                    "[BazaarTracker] Failed to read {}: {}",
                    orders_path.display(),
                    e
                ),
            }
        }
        let legacy_costs_path = Self::persistence_dir().join(LEGACY_BUY_COSTS_FILE);
        if legacy_costs_path.exists() {
            warn!(
                "[BazaarTracker] Ignoring legacy weighted-average buy-cost file {} for FIFO profit",
                legacy_costs_path.display()
            );
        }

        let lots_path = Self::persistence_dir().join(BUY_COST_LOTS_FILE);
        if lots_path.exists() {
            match std::fs::read_to_string(&lots_path) {
                Ok(json) => match serde_json::from_str::<HashMap<String, Vec<BuyCostLot>>>(&json) {
                    Ok(costs) => {
                        debug!(
                            "[BazaarTracker] Loaded FIFO buy-cost lots for {} items",
                            costs.len()
                        );
                        let mut normalized_costs: HashMap<String, VecDeque<BuyCostLot>> =
                            HashMap::new();
                        for (item, lots) in costs {
                            normalized_costs
                                .entry(normalize_for_match(&item))
                                .or_default()
                                .extend(lots);
                        }
                        *self.buy_cost_lots.write() = normalized_costs;
                    }
                    Err(e) => warn!(
                        "[BazaarTracker] Failed to parse {}: {}",
                        lots_path.display(),
                        e
                    ),
                },
                Err(e) => warn!(
                    "[BazaarTracker] Failed to read {}: {}",
                    lots_path.display(),
                    e
                ),
            }
        }

        let perf_path = Self::persistence_dir().join(ITEM_PERFORMANCE_FILE);
        if perf_path.exists() {
            match std::fs::read_to_string(&perf_path) {
                Ok(json) => match serde_json::from_str::<HashMap<String, ItemPerformance>>(&json) {
                    Ok(performance) => {
                        let normalized: HashMap<String, ItemPerformance> = performance
                            .into_values()
                            .map(|mut perf| {
                                let key = if perf.item_name.trim().is_empty() {
                                    normalize_for_match(&perf.product_id)
                                } else {
                                    normalize_for_match(&perf.item_name)
                                };
                                if perf.product_id.trim().is_empty() {
                                    perf.product_id = key.clone();
                                }
                                (key, perf)
                            })
                            .collect();
                        debug!(
                            "[BazaarTracker] Loaded item performance for {} items",
                            normalized.len()
                        );
                        *self.item_performance.write() = normalized;
                    }
                    Err(e) => warn!(
                        "[BazaarTracker] Failed to parse {}: {}",
                        perf_path.display(),
                        e
                    ),
                },
                Err(e) => warn!(
                    "[BazaarTracker] Failed to read {}: {}",
                    perf_path.display(),
                    e
                ),
            }
        }
    }
}

fn normalize_for_match(name: &str) -> String {
    let mut without_formatting = String::with_capacity(name.len());
    let mut chars = name.trim().chars();
    while let Some(ch) = chars.next() {
        if ch == '§' {
            let _ = chars.next();
            continue;
        }
        without_formatting.push(ch);
    }
    let trimmed = without_formatting.trim();
    let identity_start = trimmed
        .char_indices()
        .find(|(_, ch)| ch.is_alphanumeric())
        .map(|(idx, _)| idx)
        .unwrap_or(0);

    let normalized = trimmed[identity_start..]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();

    const GEM_TIERS: &[&str] = &["rough", "flawed", "fine", "flawless", "perfect"];
    const GEM_TYPES: &[&str] = &[
        "amber",
        "amethyst",
        "aquamarine",
        "citrine",
        "jade",
        "jasper",
        "onyx",
        "opal",
        "peridot",
        "ruby",
        "sapphire",
        "topaz",
    ];
    for tier in GEM_TIERS {
        for gem in GEM_TYPES {
            if normalized == format!("{tier} {gem} gem") {
                return format!("{tier} {gem} gemstone");
            }
        }
    }

    normalized
}

/// Public wrapper for `normalize_for_match` — used by `ManageOrders` targeted cancel.
pub fn normalize_for_match_pub(name: &str) -> String {
    normalize_for_match(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_bazaar_display_prefixes_for_exposure_matching() {
        assert_eq!(
            normalize_for_match("✎ Flawless Sapphire Gemstone"),
            normalize_for_match("Flawless Sapphire Gemstone")
        );
        assert_eq!(
            normalize_for_match("⸕ Flawless Amber Gemstone"),
            normalize_for_match("Flawless Amber Gemstone")
        );
        assert_eq!(
            normalize_for_match("§a✎ Flawless Sapphire Gemstone"),
            "flawless sapphire gemstone"
        );
        assert_eq!(
            normalize_for_match("⸕ Flawless Amber Gem"),
            normalize_for_match("Flawless Amber Gemstone")
        );
    }

    #[test]
    fn partial_bazaar_collect_reduces_order_instead_of_removing_it() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.add_order("Carrot Zest".into(), 6, 556_808.9, true);

        let collected = tracker
            .remove_or_reduce_order_on_collect("Carrot Zest", true, Some(2))
            .expect("collected portion should be returned");

        assert_eq!(collected.amount, 2);
        let orders = tracker.get_orders();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].amount, 4);
        assert_eq!(orders[0].status, "open");
        assert!(orders[0].is_buy_order);
    }

    #[test]
    fn reconcile_does_not_restore_partially_collected_buy_amount() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.add_order("Enchanted Titanium".into(), 161, 21_723.5, true);
        let collected = tracker
            .remove_or_reduce_order_on_collect("Enchanted Titanium", true, Some(11))
            .expect("partial collect should return collected amount");
        assert_eq!(collected.amount, 11);
        tracker.record_buy_cost("Enchanted Titanium", 21_723.5, 11);

        let ingame = vec![("Enchanted Titanium".to_string(), true, 161, 21_723.5)];
        let removed = tracker.reconcile_with_ingame(&ingame);
        assert_eq!(removed, 0);
        let orders = tracker.get_orders();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].amount, 150);
    }

    #[test]
    fn reconcile_does_not_restore_partially_collected_sell_amount() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.record_buy_cost("Flawless Amber Gemstone", 2_850_000.6, 3);
        tracker.add_order("Flawless Amber Gemstone".into(), 3, 3_193_804.4, false);
        let collected = tracker
            .remove_or_reduce_order_on_collect("Flawless Amber Gemstone", false, Some(2))
            .expect("partial sell collect should return collected amount");
        assert_eq!(collected.amount, 2);
        let audit = tracker.account_sell_collect(
            "Flawless Amber Gemstone",
            2,
            6_307_763.69,
            Some(6_387_608.8),
        );
        assert_eq!(audit.cost_basis_status, CostBasisStatus::Known);

        let ingame = vec![(
            "⸕ Flawless Amber Gemstone".to_string(),
            false,
            3,
            3_193_804.4,
        )];
        let removed = tracker.reconcile_with_ingame(&ingame);
        assert_eq!(removed, 0);
        let orders = tracker.get_orders();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].amount, 1);
        assert!(!orders[0].is_buy_order);
        assert_eq!(
            tracker.remaining_cost_lot_amount_for("Flawless Amber Gemstone"),
            1
        );
    }

    #[test]
    fn cancel_removes_oldest_same_item_order_to_avoid_relist_churn() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.add_order("Carrot Zest".into(), 2, 618_010.9, false);
        tracker.add_order("Carrot Zest".into(), 2, 618_010.4, false);

        let cancelled = tracker
            .remove_oldest_order("Carrot Zest", false)
            .expect("oldest sell should be removed");

        assert_eq!(cancelled.price_per_unit, 618_010.9);
        let orders = tracker.get_orders();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].price_per_unit, 618_010.4);
        assert!(!orders[0].is_buy_order);
    }

    #[test]
    fn active_sell_count_and_duplicate_target_group_same_item_orders() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.add_order("Enchanted Lily Pad".into(), 64, 31_168.6, false);
        tracker.add_order("Enchanted Lily Pad".into(), 116, 31_299.8, false);
        tracker.add_order("Kismet Feather".into(), 2, 1_512_000.0, true);

        assert_eq!(tracker.active_sell_order_count_for("enchanted lily pad"), 2);
        assert_eq!(tracker.active_sell_amount_for("Enchanted Lily Pad"), 180);

        let duplicate = tracker
            .duplicate_active_sell_order_target()
            .expect("duplicate sell item should be detected");
        assert_eq!(normalize_for_match(&duplicate.0), "enchanted lily pad");
        assert_eq!(duplicate.1, 2);
        assert_eq!(duplicate.2, 180);
    }

    #[test]
    fn crystalized_moonlight_fifo_profit_is_positive() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.record_buy_cost("Crystalized Moonlight", 460_003.1, 10);

        let audit = tracker.account_sell_collect(
            "Crystalized Moonlight",
            10,
            5_065_283.0,
            Some(5_129_400.0),
        );

        assert_eq!(audit.cost_basis_status, CostBasisStatus::Known);
        assert_eq!(audit.cost_basis_total.round() as i64, 4_600_031);
        assert_eq!(audit.realized_profit, 465_252);
        assert_eq!(
            tracker
                .profit_audit_snapshot()
                .current_fifo_realized_profit_total,
            465_252
        );
    }

    #[test]
    fn mixed_known_and_unknown_sell_realizes_only_known_fifo_profit() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.record_buy_cost("Crystalized Moonlight", 460_003.1, 10);

        let audit = tracker.account_sell_collect(
            "Crystalized Moonlight",
            17,
            8_610_000.0,
            Some(8_720_000.0),
        );

        assert_eq!(
            audit.cost_basis_status,
            CostBasisStatus::PartialKnownCostBasis
        );
        assert_eq!(audit.sold_amount, 10);
        assert_eq!(audit.cost_basis_total.round() as i64, 4_600_031);
        assert_eq!(audit.realized_profit, 464_675);
        let snapshot = tracker.profit_audit_snapshot();
        assert_eq!(snapshot.current_fifo_realized_profit_total, 464_675);
        assert_eq!(snapshot.unknown_cost_basis_sell_total_count, 1);
        assert!(!snapshot
            .remaining_cost_lots
            .contains_key("crystalized moonlight"));
        assert_eq!(
            audit.reason.as_deref(),
            Some("partial_known_cost_basis: sold 17, known 10, unknown 7")
        );
    }

    #[test]
    fn busted_belt_buckle_mixed_sell_consumes_known_remainder_without_counting_unknown() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.record_buy_cost("Busted Belt Buckle", 266_805.5, 5);

        let audit = tracker.account_sell_collect(
            "Busted Belt Buckle",
            14,
            4_147_410.1375,
            Some(4_199_909.0),
        );

        assert_eq!(
            audit.cost_basis_status,
            CostBasisStatus::PartialKnownCostBasis
        );
        assert_eq!(audit.sold_amount, 5);
        assert_eq!(audit.cost_basis_total, 1_334_027.5);
        assert_eq!(audit.realized_profit, 147_190);
        assert_eq!(
            audit.reason.as_deref(),
            Some("partial_known_cost_basis: sold 14, known 5, unknown 9")
        );
        let snapshot = tracker.profit_audit_snapshot();
        assert_eq!(snapshot.current_fifo_realized_profit_total, 147_190);
        assert_eq!(snapshot.unknown_cost_basis_sell_total_count, 1);
        assert!(!snapshot
            .remaining_cost_lots
            .contains_key("busted belt buckle"));
    }

    #[test]
    fn sell_without_known_claim_value_does_not_consume_fifo_or_count_profit() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.record_buy_cost("Melon Juice", 162_566.1, 21);

        let audit = tracker.account_sell_collect("Melon Juice", 21, 0.0, None);

        assert_eq!(audit.cost_basis_status, CostBasisStatus::UnknownCostBasis);
        assert_eq!(audit.realized_profit, 0);
        assert_eq!(
            audit.reason.as_deref(),
            Some("missing_known_sell_claim_value")
        );
        let snapshot = tracker.profit_audit_snapshot();
        assert_eq!(snapshot.current_fifo_realized_profit_total, 0);
        assert_eq!(snapshot.unknown_cost_basis_sell_total_count, 1);
        assert_eq!(snapshot.remaining_cost_lots["melon juice"][0].amount, 21);
    }

    #[test]
    fn sell_collect_clears_stale_planned_sell_guard_even_when_unknown() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.record_planned_local_sell_amount("Busted Belt Buckle", 299_993.5, None, 13);

        let audit = tracker.account_sell_collect("Busted Belt Buckle", 14, 0.0, None);

        assert_eq!(audit.cost_basis_status, CostBasisStatus::UnknownCostBasis);
        assert!(tracker
            .planned_local_sell_amount("Busted Belt Buckle")
            .is_none());
    }

    #[test]
    fn stale_planned_sell_order_escrow_expires_but_buy_target_price_survives() {
        let tracker = BazaarOrderTracker::new_in_memory();
        let stale_at =
            BazaarOrderTracker::now_secs().saturating_sub(PLANNED_LOCAL_SELL_ORDER_TTL_SECONDS + 1);
        tracker.planned_local_sells.write().insert(
            normalize_for_match("Busted Belt Buckle"),
            (299_993.5, None, Some(5), stale_at),
        );
        tracker.planned_local_sells.write().insert(
            normalize_for_match("Flawless Amber Gemstone"),
            (
                3_193_811.6,
                Some("PERFECT_AMBER_GEM".to_string()),
                None,
                stale_at,
            ),
        );

        assert!(tracker
            .planned_local_sell_amount("Busted Belt Buckle")
            .is_none());
        let planned_target = tracker
            .planned_local_sell("Flawless Amber Gemstone")
            .expect("buy target price should not expire");
        assert_eq!(planned_target.0, 3_193_811.6);
        assert!(planned_target.2.is_none());
    }

    #[test]
    fn legacy_bz_list_profit_is_diagnostic_only() {
        let tracker = BazaarOrderTracker::new_in_memory();
        let mut legacy = HashMap::new();
        legacy.insert("Crystalized Moonlight".to_string(), (-5_830_000, 1));
        tracker.set_bz_list_profits(legacy);
        tracker.record_ignored_legacy_profit(-5_830_000, "/cofl bz l");

        let audit = tracker.account_sell_collect(
            "Crystalized Moonlight",
            17,
            8_610_000.0,
            Some(8_720_000.0),
        );

        let snapshot = tracker.profit_audit_snapshot();
        assert_eq!(audit.cost_basis_status, CostBasisStatus::UnknownCostBasis);
        assert_eq!(snapshot.current_fifo_realized_profit_total, 0);
        assert_eq!(snapshot.ignored_legacy_profit_total, -5_830_000);
    }

    #[test]
    fn partial_fill_leaves_remaining_fifo_lot() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.record_buy_cost("Coal", 100.0, 10);

        let audit = tracker.account_sell_collect("Coal", 4, 500.0, Some(500.0));

        assert_eq!(audit.cost_basis_status, CostBasisStatus::Known);
        assert_eq!(audit.cost_basis_total, 400.0);
        let snapshot = tracker.profit_audit_snapshot();
        assert_eq!(snapshot.remaining_cost_lots["coal"][0].amount, 6);
        assert_eq!(
            snapshot.remaining_cost_lots["coal"][0].price_per_unit,
            100.0
        );
    }

    #[test]
    fn multiple_lots_are_consumed_fifo() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.record_buy_cost("Diamond", 100.0, 5);
        tracker.record_buy_cost("Diamond", 120.0, 5);

        let audit = tracker.account_sell_collect("Diamond", 8, 1_000.0, Some(1_000.0));

        assert_eq!(audit.cost_basis_status, CostBasisStatus::Known);
        assert_eq!(audit.cost_basis_total, 860.0);
        assert_eq!(audit.lots_used[0].amount, 5);
        assert_eq!(audit.lots_used[0].price_per_unit, 100.0);
        assert_eq!(audit.lots_used[1].amount, 3);
        assert_eq!(audit.lots_used[1].price_per_unit, 120.0);
        let snapshot = tracker.profit_audit_snapshot();
        assert_eq!(snapshot.remaining_cost_lots["diamond"][0].amount, 2);
        assert_eq!(
            snapshot.remaining_cost_lots["diamond"][0].price_per_unit,
            120.0
        );
    }

    #[test]
    fn audit_snapshot_documents_graph_source() {
        let tracker = BazaarOrderTracker::new_in_memory();
        let snapshot = tracker.profit_audit_snapshot();
        assert_eq!(
            snapshot.web_graph_source,
            "local_fifo_known_cost_basis_only"
        );
        assert_eq!(snapshot.current_fifo_realized_profit_total, 0);
    }

    #[test]
    fn add_and_remove_order() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.add_order("Enchanted Coal Block".into(), 4, 30100.0, false);
        assert_eq!(tracker.get_orders().len(), 1);

        let removed = tracker.remove_order("Enchanted Coal Block", false);
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().amount, 4);
        assert_eq!(tracker.get_orders().len(), 0);
    }

    #[test]
    fn mark_filled() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.add_order("Diamond".into(), 64, 100.0, true);
        assert!(!tracker.has_filled_orders());
        tracker.mark_filled("Diamond", true);
        assert_eq!(tracker.get_orders()[0].status, "filled");
        assert!(tracker.has_filled_orders());
    }

    #[test]
    fn has_filled_orders_empty() {
        let tracker = BazaarOrderTracker::new_in_memory();
        assert!(!tracker.has_filled_orders());
    }

    #[test]
    fn has_filled_orders_cleared_on_remove() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.add_order("Coal".into(), 10, 500.0, true);
        tracker.mark_filled("Coal", true);
        assert!(tracker.has_filled_orders());
        tracker.remove_order("Coal", true);
        assert!(!tracker.has_filled_orders());
    }

    #[test]
    fn remove_filled_order() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.add_order("Diamond".into(), 64, 100.0, true);
        tracker.mark_filled("Diamond", true);
        let removed = tracker.remove_order("Diamond", true);
        assert!(removed.is_some());
        assert_eq!(tracker.get_orders().len(), 0);
    }

    #[test]
    fn case_insensitive_match() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.add_order("Enchanted Coal Block".into(), 4, 30100.0, false);
        let removed = tracker.remove_order("enchanted coal block", false);
        assert!(removed.is_some());
        assert_eq!(tracker.get_orders().len(), 0);
    }

    #[test]
    fn remove_returns_none_for_missing() {
        let tracker = BazaarOrderTracker::new_in_memory();
        assert!(tracker.remove_order("Nonexistent", true).is_none());
    }

    #[test]
    fn remove_stale_orders() {
        let tracker = BazaarOrderTracker::new_in_memory();
        // Manually insert an order with a very old timestamp
        {
            let mut orders = tracker.orders.write();
            orders.push(TrackedBazaarOrder {
                item_name: "Old Item".into(),
                amount: 10,
                price_per_unit: 100.0,
                is_buy_order: true,
                status: "open".into(),
                placed_at: 1000, // ancient timestamp
            });
        }
        // Also add a fresh order normally
        tracker.add_order("Fresh Item".into(), 5, 200.0, false);

        let removed = tracker.remove_stale_orders(3600); // 1 hour max age
        assert_eq!(removed, 1);
        let remaining = tracker.get_orders();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].item_name, "Fresh Item");
    }

    #[test]
    fn profit_calculation_from_removed_orders() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.add_order("Coal".into(), 10, 500.0, true);
        tracker.add_order("Coal".into(), 10, 600.0, false);

        let buy = tracker.remove_order("Coal", true).unwrap();
        let sell = tracker.remove_order("Coal", false).unwrap();
        let profit =
            (sell.price_per_unit * sell.amount as f64) - (buy.price_per_unit * buy.amount as f64);
        assert_eq!(profit, 1000.0);
    }

    #[test]
    fn record_and_take_buy_cost() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.record_buy_cost("Enchanted Coal Block", 500.0, 10);

        let cost = tracker.take_buy_cost("Enchanted Coal Block");
        assert!(cost.is_some());
        let (ppu, amt) = cost.unwrap();
        assert_eq!(ppu, 500.0);
        assert_eq!(amt, 10);

        // Second take returns None (consumed).
        assert!(tracker.take_buy_cost("Enchanted Coal Block").is_none());
    }

    #[test]
    fn take_buy_cost_case_insensitive() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.record_buy_cost("Enchanted Coal Block", 500.0, 10);
        assert!(tracker.take_buy_cost("enchanted coal block").is_some());
    }

    #[test]
    fn take_buy_cost_returns_none_when_missing() {
        let tracker = BazaarOrderTracker::new_in_memory();
        assert!(tracker.take_buy_cost("Nonexistent").is_none());
    }

    #[test]
    fn sell_profit_from_recorded_buy_cost() {
        let tracker = BazaarOrderTracker::new_in_memory();
        // Simulate buy order collected: 10x Coal @ 500 coins/unit
        tracker.record_buy_cost("Coal", 500.0, 10);
        // Simulate sell offer collected: 10x Coal @ 600 coins/unit
        let sell_ppu = 600.0;
        let sell_amount = 10u64;
        let (buy_ppu, buy_amount) = tracker.take_buy_cost("Coal").unwrap();
        let profit = (sell_ppu * sell_amount as f64) - (buy_ppu * buy_amount as f64);
        assert_eq!(profit, 1000.0);
    }

    #[test]
    fn multiple_buy_orders_accumulate_cost() {
        let tracker = BazaarOrderTracker::new_in_memory();
        // Two buy orders for the same item collected before the sell
        tracker.record_buy_cost("Coal", 500.0, 10);
        tracker.record_buy_cost("Coal", 500.0, 10);
        // Sell 20x Coal @ 600 coins/unit
        let (buy_ppu, buy_amount) = tracker.take_buy_cost("Coal").unwrap();
        assert_eq!(buy_amount, 20);
        assert!((buy_ppu - 500.0).abs() < 0.01);
        let sell_total = 600.0 * 20.0;
        let buy_total = buy_ppu * buy_amount as f64;
        let profit = sell_total - buy_total;
        // Expected: (600*20) - (500*20) = 12000 - 10000 = 2000
        assert_eq!(profit, 2000.0);
    }

    #[test]
    fn multiple_buy_orders_weighted_average() {
        let tracker = BazaarOrderTracker::new_in_memory();
        // Buy 10 @ 500/unit, then 10 @ 510/unit
        tracker.record_buy_cost("Diamond", 500.0, 10);
        tracker.record_buy_cost("Diamond", 510.0, 10);
        let (buy_ppu, buy_amount) = tracker.take_buy_cost("Diamond").unwrap();
        assert_eq!(buy_amount, 20);
        // Weighted avg = (500*10 + 510*10) / 20 = 10100 / 20 = 505
        assert!((buy_ppu - 505.0).abs() < 0.01);
    }

    #[test]
    fn reconcile_removes_stale_orders() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.add_order("Coal".into(), 10, 500.0, true);
        tracker.add_order("Diamond".into(), 5, 1000.0, false);
        tracker.add_order("Iron Ingot".into(), 64, 50.0, true);
        assert_eq!(tracker.get_orders().len(), 3);

        // In-game only has Coal BUY and Diamond SELL — Iron Ingot is stale
        let ingame = vec![
            ("Coal".to_string(), true, 10, 500.0),
            ("Diamond".to_string(), false, 5, 1000.0),
        ];
        let removed = tracker.reconcile_with_ingame(&ingame);
        assert_eq!(removed, 1);
        let remaining = tracker.get_orders();
        assert_eq!(remaining.len(), 2);
        assert!(remaining
            .iter()
            .any(|o| o.item_name == "Coal" && o.is_buy_order));
        assert!(remaining
            .iter()
            .any(|o| o.item_name == "Diamond" && !o.is_buy_order));
    }

    #[test]
    fn reconcile_case_insensitive() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.add_order("Enchanted Coal Block".into(), 4, 30100.0, false);
        let ingame = vec![("enchanted coal block".to_string(), false, 4, 30100.0)];
        let removed = tracker.reconcile_with_ingame(&ingame);
        assert_eq!(removed, 0);
        assert_eq!(tracker.get_orders().len(), 1);
    }

    #[test]
    fn reconcile_price_change_refreshes_order_age() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.add_order("Flawless Sapphire Gemstone".into(), 1, 3_027_535.9, false);
        tracker.orders.write()[0].placed_at = 100;

        let ingame = vec![(
            "✎ Flawless Sapphire Gemstone".to_string(),
            false,
            1,
            3_027_533.9,
        )];
        let removed = tracker.reconcile_with_ingame(&ingame);
        assert_eq!(removed, 0);
        let orders = tracker.get_orders();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].price_per_unit, 3_027_533.9);
        assert!(orders[0].placed_at > 100);
    }

    #[test]
    fn reconcile_duplicate_same_item_orders() {
        let tracker = BazaarOrderTracker::new_in_memory();
        // Tracker has 3 "Coal" buy orders
        tracker.add_order("Coal".into(), 10, 500.0, true);
        tracker.add_order("Coal".into(), 20, 510.0, true);
        tracker.add_order("Coal".into(), 30, 520.0, true);
        assert_eq!(tracker.get_orders().len(), 3);

        // In-game only has 1 "Coal" buy order (2 were cancelled externally)
        let ingame = vec![("Coal".to_string(), true, 10, 500.0)];
        let removed = tracker.reconcile_with_ingame(&ingame);
        assert_eq!(removed, 2);
        assert_eq!(tracker.get_orders().len(), 1);
    }

    #[test]
    fn reconcile_keeps_correct_count_of_duplicates() {
        let tracker = BazaarOrderTracker::new_in_memory();
        // Tracker has 2 "Coal" buy orders and 1 "Diamond" sell order
        tracker.add_order("Coal".into(), 10, 500.0, true);
        tracker.add_order("Coal".into(), 20, 510.0, true);
        tracker.add_order("Diamond".into(), 5, 1000.0, false);
        assert_eq!(tracker.get_orders().len(), 3);

        // In-game has 2 "Coal" buy orders and 1 "Diamond" sell order
        let ingame = vec![
            ("Coal".to_string(), true, 10, 500.0),
            ("Coal".to_string(), true, 20, 510.0),
            ("Diamond".to_string(), false, 5, 1000.0),
        ];
        let removed = tracker.reconcile_with_ingame(&ingame);
        assert_eq!(removed, 0);
        assert_eq!(tracker.get_orders().len(), 3);
    }

    #[test]
    fn reconcile_adds_new_orders_with_correct_data() {
        let tracker = BazaarOrderTracker::new_in_memory();
        // Empty tracker, in-game has 2 orders
        let ingame = vec![
            ("Coal".to_string(), true, 64, 500.0),
            ("Diamond".to_string(), false, 10, 1200.5),
        ];
        let removed = tracker.reconcile_with_ingame(&ingame);
        assert_eq!(removed, 0);
        let orders = tracker.get_orders();
        assert_eq!(orders.len(), 2);

        let coal = orders.iter().find(|o| o.item_name == "Coal").unwrap();
        assert_eq!(coal.amount, 64);
        assert!((coal.price_per_unit - 500.0).abs() < 0.01);
        assert!(coal.is_buy_order);

        let diamond = orders.iter().find(|o| o.item_name == "Diamond").unwrap();
        assert_eq!(diamond.amount, 10);
        assert!((diamond.price_per_unit - 1200.5).abs() < 0.01);
        assert!(!diamond.is_buy_order);
    }

    #[test]
    fn reconcile_preserves_fifo_backed_sell_missing_from_snapshot() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.record_buy_cost("Melon Juice", 162_566.1, 21);
        tracker.add_order("Melon Juice".into(), 21, 189_835.3, false);

        let removed = tracker.reconcile_with_ingame(&[]);

        assert_eq!(removed, 0);
        let snapshot = tracker.profit_audit_snapshot();
        assert_eq!(snapshot.active_sell_orders, 1);
        assert!(snapshot.items_waiting_for_sell.is_empty());
        assert_eq!(snapshot.remaining_cost_lots["melon juice"][0].amount, 21);
    }

    #[test]
    fn reconcile_removes_unbacked_sell_missing_from_snapshot() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.add_order("Crystalized Moonlight".into(), 6, 720_000.0, false);

        let removed = tracker.reconcile_with_ingame(&[]);

        assert_eq!(removed, 1);
        assert!(tracker.get_orders().is_empty());
    }

    #[test]
    fn product_lookup_failure_clears_planned_sell_and_cooldowns_item() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.record_planned_local_sell(
            "Enchanted Water Lily",
            32_599.9,
            Some("ENCHANTED_WATER_LILY".to_string()),
        );

        tracker.record_product_lookup_failed("Enchanted Water Lily", "PRODUCT_LOOKUP_FAILED");

        assert!(tracker.planned_local_sell("Enchanted Water Lily").is_none());
        let snapshot = tracker.profit_audit_snapshot();
        let perf = &snapshot.item_performance["enchanted water lily"];
        assert_eq!(perf.failed_search_count, 1);
        assert_eq!(perf.failed_flips, 1);
        assert_eq!(perf.block_reason.as_deref(), Some("PRODUCT_LOOKUP_FAILED"));
        assert!(tracker
            .item_cooldown_reason("Enchanted Water Lily")
            .is_some());
    }

    #[test]
    fn successful_fifo_sell_clears_previous_item_cooldown() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.record_product_lookup_failed("Kismet Feather", "PRODUCT_LOOKUP_FAILED");
        assert!(tracker.item_cooldown_reason("Kismet Feather").is_some());

        tracker.record_buy_cost("Kismet Feather", 1_450_672.7, 2);
        let audit =
            tracker.account_sell_collect("Kismet Feather", 2, 3_119_218.0, Some(3_154_708.0));

        assert_eq!(audit.cost_basis_status, CostBasisStatus::Known);
        assert!(audit.realized_profit > 0);
        assert!(tracker.item_cooldown_reason("Kismet Feather").is_none());
        let snapshot = tracker.profit_audit_snapshot();
        let perf = &snapshot.item_performance["kismet feather"];
        assert_eq!(perf.block_reason, None);
        assert_eq!(perf.cooldown_until, None);
        assert_eq!(perf.successful_flips, 1);
    }

    #[test]
    fn partial_fifo_sell_keeps_cooldown_while_exposure_remains() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.record_buy_cost("Flawless Amber Gemstone", 2_850_000.6, 3);
        tracker.add_order("Flawless Amber Gemstone".into(), 3, 3_193_804.4, false);
        tracker.cooldown_item("Flawless Amber Gemstone", "CAPITAL_BLOCKER", 1_800);
        assert!(tracker
            .item_cooldown_reason("Flawless Amber Gemstone")
            .is_some());

        tracker.remove_or_reduce_order_on_collect("Flawless Amber Gemstone", false, Some(2));
        let audit = tracker.account_sell_collect(
            "Flawless Amber Gemstone",
            2,
            6_307_763.69,
            Some(6_387_608.8),
        );

        assert_eq!(audit.cost_basis_status, CostBasisStatus::Known);
        assert!(audit.realized_profit > 0);
        let (_, reason) = tracker
            .item_cooldown_reason("Flawless Amber Gemstone")
            .expect("cooldown should remain while one listed sell is still open");
        assert_eq!(reason, "CAPITAL_BLOCKER");
    }

    #[test]
    fn manual_item_cooldown_sets_reason_and_blocks_candidate() {
        let tracker = BazaarOrderTracker::new_in_memory();

        tracker.cooldown_item("Flawless Amber Gemstone", "capital_blocker", 1_800);

        let Some((until, reason)) = tracker.item_cooldown_reason("Flawless Amber Gemstone") else {
            panic!("cooldown should be active");
        };
        assert_eq!(reason, "CAPITAL_BLOCKER");
        assert!(until > BazaarOrderTracker::now_secs());
    }

    #[test]
    fn bz_list_profit_single_flip() {
        let tracker = BazaarOrderTracker::new_in_memory();
        let mut items = HashMap::new();
        items.insert("Worm Membrane".to_string(), (100_000i64, 1u32));
        tracker.set_bz_list_profits(items);
        assert_eq!(tracker.get_bz_list_profit("Worm Membrane"), Some(100_000));
    }

    #[test]
    fn bz_list_profit_multiple_flips_returns_total() {
        let tracker = BazaarOrderTracker::new_in_memory();
        let mut items = HashMap::new();
        items.insert("Worm Membrane".to_string(), (741_000i64, 7u32));
        tracker.set_bz_list_profits(items);
        // Returns total profit, not per-flip average
        assert_eq!(tracker.get_bz_list_profit("Worm Membrane"), Some(741_000));
    }

    #[test]
    fn bz_list_profit_case_insensitive() {
        let tracker = BazaarOrderTracker::new_in_memory();
        let mut items = HashMap::new();
        items.insert("Enchanted Coal Block".to_string(), (50_000i64, 2u32));
        tracker.set_bz_list_profits(items);
        assert_eq!(
            tracker.get_bz_list_profit("enchanted coal block"),
            Some(50_000)
        );
    }

    #[test]
    fn bz_list_profit_missing_item() {
        let tracker = BazaarOrderTracker::new_in_memory();
        assert!(tracker.get_bz_list_profit("Nonexistent").is_none());
    }

    #[test]
    fn bz_list_profit_zero_count_still_returns_total() {
        let tracker = BazaarOrderTracker::new_in_memory();
        let mut items = HashMap::new();
        items.insert("Coal".to_string(), (50_000i64, 0u32));
        tracker.set_bz_list_profits(items);
        assert_eq!(tracker.get_bz_list_profit("Coal"), Some(50_000));
    }

    #[test]
    fn bz_list_profits_replaced_on_new_set() {
        let tracker = BazaarOrderTracker::new_in_memory();
        let mut items1 = HashMap::new();
        items1.insert("Coal".to_string(), (10_000i64, 1u32));
        tracker.set_bz_list_profits(items1);
        assert!(tracker.get_bz_list_profit("Coal").is_some());

        // Second set replaces all data
        let mut items2 = HashMap::new();
        items2.insert("Diamond".to_string(), (20_000i64, 2u32));
        tracker.set_bz_list_profits(items2);
        assert!(tracker.get_bz_list_profit("Coal").is_none());
        assert_eq!(tracker.get_bz_list_profit("Diamond"), Some(20_000));
    }
    #[test]
    fn fifo_frogcoin_profit_matches_real_claim() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.record_buy_cost("Frogcoin", 367_843.9, 2);

        let audit = tracker.account_sell_collect("Frogcoin", 2, 955_952.3375, Some(967_040.8481));

        assert_eq!(audit.cost_basis_status, CostBasisStatus::Known);
        assert_eq!(audit.cost_basis_total, 735_687.8);
        assert_eq!(audit.realized_profit, 220_265);
    }

    #[test]
    fn negative_designer_coffee_beans_sell_is_blocked_without_consuming_fifo() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.record_buy_cost("Designer Coffee Beans", 254_005.2, 2);

        let check =
            tracker.validate_sell_before_order("Designer Coffee Beans", 2, 229_996.900, 1.25);

        assert!(!check.allowed);
        assert_eq!(check.reason, Some(SellBlockReason::NegativeExpectedProfit));
        assert!((check.fifo_cost_basis_total - 508_010.4).abs() < 0.01);
        assert!((check.expected_sell_after_tax - 454_243.8775).abs() < 0.01);
        let snapshot = tracker.profit_audit_snapshot();
        assert_eq!(snapshot.current_fifo_realized_profit_total, 0);
        assert_eq!(
            snapshot.remaining_cost_lots["designer coffee beans"][0].amount,
            2
        );
        assert_eq!(
            snapshot.item_performance["designer coffee beans"].negative_profit_block_count,
            1
        );
    }

    #[test]
    fn negative_sell_cost_lot_is_blocked_not_actionable_backlog() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.record_buy_cost("Summoning Eye", 1_578_979.1, 1);

        let check = tracker.validate_sell_before_order("Summoning Eye", 1, 1_400_000.0, 1.25);
        assert!(!check.allowed);
        assert_eq!(check.reason, Some(SellBlockReason::NegativeExpectedProfit));

        let snapshot = tracker.profit_audit_snapshot();
        assert!(snapshot.items_waiting_for_sell.is_empty());
        assert_eq!(
            snapshot.blocked_items_waiting_for_sell,
            vec!["summoning eye".to_string()]
        );
        assert_eq!(snapshot.actionable_cost_lot_value, 0.0);
        assert!(snapshot.blocked_cost_lot_value > 0.0);
        assert_eq!(snapshot.remaining_cost_lots["summoning eye"][0].amount, 1);
    }

    #[test]
    fn api_snapshot_open_sell_value_is_nonzero_for_active_sell_order() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.add_order("Frogcoin".into(), 2, 500_000.0, false);

        let snapshot = tracker.profit_audit_snapshot();

        assert_eq!(snapshot.active_sell_orders, 1);
        assert!(snapshot.open_sell_value > 0.0);
    }

    #[test]
    fn legacy_ignored_total_never_changes_fifo_bz_total() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.record_ignored_legacy_profit(18_984_775, "/cofl bz l");

        let snapshot = tracker.profit_audit_snapshot();

        assert_eq!(snapshot.ignored_legacy_profit_total, 18_984_775);
        assert_eq!(snapshot.current_fifo_realized_profit_total, 0);
        assert_eq!(
            snapshot.web_graph_source,
            "local_fifo_known_cost_basis_only"
        );
    }
}
