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
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, warn};

/// File name for persisted orders (stored next to the executable / in the logs dir).
const ORDERS_FILE: &str = "bazaar_orders.json";
/// File name for persisted FIFO buy-cost lots.
const BUY_COSTS_FILE: &str = "bazaar_buy_cost_lots.json";
/// Legacy pre-lot buy-cost file. Never migrated automatically because it can
/// contain stale weighted-average costs from older sessions.
const LEGACY_BUY_COSTS_FILE: &str = "bazaar_buy_costs.json";

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

/// One realized BUY fill used as cost basis for a future SELL collection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BuyCostLot {
    /// Display item name as seen when the BUY order was collected.
    pub item: String,
    /// Remaining units in this lot.
    pub amount: u64,
    /// BUY price per unit in coins.
    pub unit_cost: f64,
    /// Remaining total cost (`amount * unit_cost`) in coins.
    pub total_cost: f64,
    /// Unix timestamp (seconds) when the BUY collection was recorded.
    pub timestamp: u64,
    /// Optional order id for future integrations. Current in-game events do not expose one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
}

/// Cost basis consumed for one SELL collection.
#[derive(Debug, Clone, PartialEq)]
pub struct ConsumedLotCosts {
    pub amount: u64,
    pub total_cost: f64,
    pub weighted_unit_cost: f64,
    pub lots_consumed: usize,
}

/// Reason why a SELL collection could not be matched to known FIFO BUY lots.
#[derive(Debug, Clone, PartialEq)]
pub enum UnknownCostBasis {
    NoLots,
    InsufficientLots { requested: u64, available: u64 },
    ZeroAmount,
}

/// Thread-safe tracker for active bazaar orders.
#[derive(Clone)]
pub struct BazaarOrderTracker {
    orders: Arc<RwLock<Vec<TrackedBazaarOrder>>>,
    /// FIFO buy-cost lots keyed by normalized item name. Each collected BUY
    /// creates one lot; each collected SELL consumes lots from the front.
    buy_cost_lots: Arc<RwLock<HashMap<String, VecDeque<BuyCostLot>>>>,
    /// Per-item profit data from `/cofl bz l` output.
    /// Maps normalized item name → (total_profit, flip_count).
    /// Used as a fallback when local buy-cost tracking has no data for a sell.
    bz_list_profits: Arc<RwLock<HashMap<String, (i64, u32)>>>,
    /// Count of SELL collections that were intentionally not booked because
    /// matching FIFO BUY lots were missing/insufficient.
    unknown_cost_basis_sells: Arc<AtomicU64>,
}

impl BazaarOrderTracker {
    pub fn new() -> Self {
        let tracker = Self {
            orders: Arc::new(RwLock::new(Vec::new())),
            buy_cost_lots: Arc::new(RwLock::new(HashMap::new())),
            bz_list_profits: Arc::new(RwLock::new(HashMap::new())),
            unknown_cost_basis_sells: Arc::new(AtomicU64::new(0)),
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
            unknown_cost_basis_sells: Arc::new(AtomicU64::new(0)),
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
        self.orders.write().push(TrackedBazaarOrder {
            item_name,
            amount,
            price_per_unit,
            is_buy_order,
            status: "open".to_string(),
            placed_at: now,
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

    /// Return a snapshot of all tracked orders.
    pub fn get_orders(&self) -> Vec<TrackedBazaarOrder> {
        self.orders.read().clone()
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
        for (name, is_buy, _, _) in ingame_orders {
            *ingame_counts
                .entry((normalize_for_match(name), *is_buy))
                .or_insert(0) += 1;
        }
        let mut orders = self.orders.write();
        let original_len = orders.len();
        // Track how many of each (item, side) we have already kept so we
        // don't exceed the in-game count.
        let mut kept_counts: std::collections::HashMap<(String, bool), usize> =
            std::collections::HashMap::new();
        orders.retain(|o| {
            let key = (normalize_for_match(&o.item_name), o.is_buy_order);
            let allowed = ingame_counts.get(&key).copied().unwrap_or(0);
            let kept = kept_counts.entry(key).or_insert(0);
            if *kept < allowed {
                *kept += 1;
                true
            } else {
                false
            }
        });
        let removed = original_len - orders.len();

        // Add in-game orders that aren't already tracked.
        // Iterate over unique keys to avoid duplicate additions.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut added = 0usize;

        // Build a map from (normalized_name, is_buy) → Vec<(amount, price)>
        // so we can pick the correct data for each missing order.
        let mut ingame_data: std::collections::HashMap<(String, bool), Vec<(u64, f64)>> =
            std::collections::HashMap::new();
        for (name, is_buy, amount, price) in ingame_orders {
            ingame_data
                .entry((normalize_for_match(name), *is_buy))
                .or_default()
                .push((*amount, *price));
        }

        for (key, data_entries) in &ingame_data {
            let tracked = kept_counts.get(key).copied().unwrap_or(0);
            let needed = data_entries.len();
            for idx in tracked..needed {
                let (amount, price) = data_entries[idx];
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
        if removed > 0 || added > 0 {
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

    /// Record a collected buy order as its own FIFO cost lot.
    ///
    /// Older builds collapsed costs into a single weighted-average entry per
    /// item, which allowed stale costs from previous flips to be mixed into new
    /// sells. Keeping immutable lots makes each realized SELL consume exactly
    /// the currently matching BUY fills.
    pub fn record_buy_cost(&self, item_name: &str, price_per_unit: f64, amount: u64) {
        if amount == 0 {
            debug!(
                "[BazaarTracker] Ignoring zero-amount buy-cost lot for {}",
                item_name
            );
            return;
        }
        let key = normalize_for_match(item_name);
        let lot = BuyCostLot {
            item: item_name.to_string(),
            amount,
            unit_cost: price_per_unit,
            total_cost: price_per_unit * amount as f64,
            timestamp: current_unix_secs(),
            order_id: None,
        };
        self.buy_cost_lots
            .write()
            .entry(key)
            .or_default()
            .push_back(lot);
        self.save_buy_costs_to_disk();
    }

    /// Consume FIFO buy-cost lots for a SELL collection.
    ///
    /// The method is all-or-nothing: if there are no lots, or if the known lots
    /// do not cover the full sold amount, no lot is mutated and callers must
    /// mark the SELL as `UNKNOWN_COST_BASIS` instead of booking profit/loss.
    pub fn consume_buy_lots_fifo(
        &self,
        item_name: &str,
        amount: u64,
    ) -> Result<ConsumedLotCosts, UnknownCostBasis> {
        if amount == 0 {
            return Err(UnknownCostBasis::ZeroAmount);
        }

        let key = normalize_for_match(item_name);
        let mut lots_by_item = self.buy_cost_lots.write();
        let lots = lots_by_item.get_mut(&key).ok_or(UnknownCostBasis::NoLots)?;
        if lots.is_empty() {
            return Err(UnknownCostBasis::NoLots);
        }

        let available = lots.iter().map(|lot| lot.amount).sum::<u64>();
        if available < amount {
            return Err(UnknownCostBasis::InsufficientLots {
                requested: amount,
                available,
            });
        }

        let mut remaining = amount;
        let mut total_cost = 0.0;
        let mut lots_consumed = 0usize;

        while remaining > 0 {
            let front = lots.front_mut().expect("availability checked above");
            let take = remaining.min(front.amount);
            total_cost += front.unit_cost * take as f64;
            front.amount -= take;
            front.total_cost = front.unit_cost * front.amount as f64;
            remaining -= take;
            lots_consumed += 1;

            if front.amount == 0 {
                lots.pop_front();
            }
        }

        if lots.is_empty() {
            lots_by_item.remove(&key);
        }
        drop(lots_by_item);
        self.save_buy_costs_to_disk();

        Ok(ConsumedLotCosts {
            amount,
            total_cost,
            weighted_unit_cost: total_cost / amount as f64,
            lots_consumed,
        })
    }

    /// Return all FIFO buy-cost lots for an item. Used by tests and diagnostics.
    pub fn buy_cost_lots_for_item(&self, item_name: &str) -> Vec<BuyCostLot> {
        self.buy_cost_lots
            .read()
            .get(&normalize_for_match(item_name))
            .map(|lots| lots.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Return true if any cost lots are currently persisted in memory.
    pub fn has_buy_cost_lots(&self) -> bool {
        self.buy_cost_lots
            .read()
            .values()
            .any(|lots| !lots.is_empty())
    }

    /// Record that a SELL collection was skipped because its cost basis was unknown.
    pub fn record_unknown_cost_basis_sell(&self) {
        self.unknown_cost_basis_sells
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Number of skipped SELL collections with unknown cost basis this session.
    pub fn unknown_cost_basis_sell_count(&self) -> u64 {
        self.unknown_cost_basis_sells.load(Ordering::Relaxed)
    }

    /// Clear all buy-cost lots if doing so is safe for startup reset.
    ///
    /// Reset is allowed only when the tracker has no open/filled orders and the
    /// caller has verified there are no inventory positions that could belong
    /// to pending Bazaar flips.
    pub fn reset_buy_cost_lots_if_safe(
        &self,
        inventory_empty: bool,
    ) -> Result<usize, &'static str> {
        if !self.orders.read().is_empty() {
            return Err("open_or_filled_orders_exist");
        }
        if !inventory_empty {
            return Err("inventory_not_empty");
        }
        let mut lots = self.buy_cost_lots.write();
        let removed = lots.values().map(VecDeque::len).sum();
        lots.clear();
        drop(lots);
        self.save_buy_costs_to_disk();
        Ok(removed)
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

    fn save_buy_costs_to_disk(&self) {
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
            let path = Self::persistence_dir().join(BUY_COSTS_FILE);
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
                Err(e) => warn!("[BazaarTracker] Failed to serialize buy costs: {}", e),
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
        let costs_path = Self::persistence_dir().join(BUY_COSTS_FILE);
        if costs_path.exists() {
            match std::fs::read_to_string(&costs_path) {
                Ok(json) => match serde_json::from_str::<HashMap<String, Vec<BuyCostLot>>>(&json) {
                    Ok(costs) => {
                        let normalized: HashMap<String, VecDeque<BuyCostLot>> = costs
                            .into_iter()
                            .map(|(item, lots)| (normalize_for_match(&item), lots.into()))
                            .collect();
                        debug!(
                            "[BazaarTracker] Loaded {} buy-cost lot item groups from disk",
                            normalized.len()
                        );
                        *self.buy_cost_lots.write() = normalized;
                    }
                    Err(e) => warn!(
                        "[BazaarTracker] Failed to parse {}: {}",
                        costs_path.display(),
                        e
                    ),
                },
                Err(e) => warn!(
                    "[BazaarTracker] Failed to read {}: {}",
                    costs_path.display(),
                    e
                ),
            }
        }

        let legacy_costs_path = Self::persistence_dir().join(LEGACY_BUY_COSTS_FILE);
        if legacy_costs_path.exists() && !costs_path.exists() {
            warn!(
                "[BazaarTracker] Found legacy {} but did not migrate it; old weighted-average costs are ignored to avoid mixing stale cost basis into new sells",
                legacy_costs_path.display()
            );
        }
    }
}

fn current_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn normalize_for_match(name: &str) -> String {
    name.to_lowercase().trim().to_string()
}

/// Public wrapper for `normalize_for_match` — used by `ManageOrders` targeted cancel.
pub fn normalize_for_match_pub(name: &str) -> String {
    normalize_for_match(name)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn record_buy_cost_creates_one_lot() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.record_buy_cost("Enchanted Coal Block", 500.0, 10);

        let lots = tracker.buy_cost_lots_for_item("Enchanted Coal Block");
        assert_eq!(lots.len(), 1);
        assert_eq!(lots[0].item, "Enchanted Coal Block");
        assert_eq!(lots[0].amount, 10);
        assert_eq!(lots[0].unit_cost, 500.0);
        assert_eq!(lots[0].total_cost, 5_000.0);
    }

    #[test]
    fn consume_buy_lots_case_insensitive() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.record_buy_cost("Enchanted Coal Block", 500.0, 10);
        assert!(
            tracker
                .consume_buy_lots_fifo("enchanted coal block", 10)
                .is_ok()
        );
    }

    #[test]
    fn consume_buy_lots_returns_unknown_when_missing() {
        let tracker = BazaarOrderTracker::new_in_memory();
        assert_eq!(
            tracker.consume_buy_lots_fifo("Nonexistent", 1),
            Err(UnknownCostBasis::NoLots)
        );
    }

    #[test]
    fn sell_profit_from_fifo_buy_cost() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.record_buy_cost("Coal", 500.0, 10);

        let sell_ppu = 600.0;
        let sell_amount = 10u64;
        let consumed = tracker.consume_buy_lots_fifo("Coal", sell_amount).unwrap();
        let profit = (sell_ppu * sell_amount as f64) - consumed.total_cost;
        assert_eq!(profit, 1000.0);
        assert!(tracker.buy_cost_lots_for_item("Coal").is_empty());
    }

    #[test]
    fn multiple_buy_orders_are_consumed_fifo() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.record_buy_cost("Coal", 500.0, 10);
        tracker.record_buy_cost("Coal", 700.0, 10);

        let consumed = tracker.consume_buy_lots_fifo("Coal", 15).unwrap();
        assert_eq!(consumed.amount, 15);
        assert_eq!(consumed.total_cost, 8_500.0);
        assert!((consumed.weighted_unit_cost - 566.6666667).abs() < 0.01);

        let remaining = tracker.buy_cost_lots_for_item("Coal");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].amount, 5);
        assert_eq!(remaining[0].unit_cost, 700.0);
        assert_eq!(remaining[0].total_cost, 3_500.0);
    }

    #[test]
    fn consume_buy_lots_is_all_or_nothing_when_insufficient() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.record_buy_cost("Diamond", 500.0, 10);

        assert_eq!(
            tracker.consume_buy_lots_fifo("Diamond", 11),
            Err(UnknownCostBasis::InsufficientLots {
                requested: 11,
                available: 10,
            })
        );

        let remaining = tracker.buy_cost_lots_for_item("Diamond");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].amount, 10);
    }

    #[test]
    fn reset_buy_cost_lots_requires_no_orders_and_empty_inventory() {
        let tracker = BazaarOrderTracker::new_in_memory();
        tracker.record_buy_cost("Diamond", 500.0, 10);
        assert_eq!(
            tracker.reset_buy_cost_lots_if_safe(false),
            Err("inventory_not_empty")
        );
        assert!(tracker.has_buy_cost_lots());

        tracker.add_order("Diamond".into(), 10, 500.0, true);
        assert_eq!(
            tracker.reset_buy_cost_lots_if_safe(true),
            Err("open_or_filled_orders_exist")
        );
        tracker.remove_order("Diamond", true);

        assert_eq!(tracker.reset_buy_cost_lots_if_safe(true), Ok(1));
        assert!(!tracker.has_buy_cost_lots());
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
        assert!(
            remaining
                .iter()
                .any(|o| o.item_name == "Coal" && o.is_buy_order)
        );
        assert!(
            remaining
                .iter()
                .any(|o| o.item_name == "Diamond" && !o.is_buy_order)
        );
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
}
