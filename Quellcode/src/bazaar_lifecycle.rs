use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BazaarNextAction {
    RecoverGui,
    ClaimFilledSells,
    ClaimFilledBuys,
    PlaceMissingSellOrders,
    HandleStaleSells,
    HandleStaleBuys,
    RepriceUsefulOrder,
    PlaceNewBuy,
    Idle,
}

impl BazaarNextAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RecoverGui => "RECOVER_GUI",
            Self::ClaimFilledSells => "CLAIM_FILLED_SELLS",
            Self::ClaimFilledBuys => "CLAIM_FILLED_BUYS",
            Self::PlaceMissingSellOrders => "PLACE_MISSING_SELL_ORDERS",
            Self::HandleStaleSells => "HANDLE_STALE_SELLS",
            Self::HandleStaleBuys => "HANDLE_STALE_BUYS",
            Self::RepriceUsefulOrder => "REPRICE_USEFUL_ORDER",
            Self::PlaceNewBuy => "PLACE_NEW_BUY",
            Self::Idle => "IDLE",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BazaarLifecycleConfig {
    pub max_stale_buy_orders: usize,
    pub pause_new_buys_when_stale_buy_pressure: bool,
    pub cancel_stale_buys_when_sell_queue_pending: bool,
}

impl Default for BazaarLifecycleConfig {
    fn default() -> Self {
        Self {
            max_stale_buy_orders: 2,
            pause_new_buys_when_stale_buy_pressure: true,
            cancel_stale_buys_when_sell_queue_pending: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BazaarLifecycleState {
    pub bot_can_accept_commands: bool,
    pub command_queue_busy: bool,
    pub startup_in_progress: bool,
    pub daily_sell_limit: bool,
    pub filled_sell_orders: usize,
    pub filled_buy_orders: usize,
    pub items_waiting_for_sell: usize,
    pub remaining_cost_lot_value: f64,
    pub active_sell_orders: usize,
    pub duplicate_active_sell_items: usize,
    pub stale_sell_orders: usize,
    pub stale_buy_orders: usize,
    pub useful_reprice_available: bool,
    pub sell_backlog_age_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BazaarLifecycleDecision {
    pub action: BazaarNextAction,
    pub block_new_buys: bool,
    pub flow_stall: bool,
    pub reason: String,
}

pub fn determine_next_bazaar_action(
    state: &BazaarLifecycleState,
    config: &BazaarLifecycleConfig,
) -> BazaarLifecycleDecision {
    let sell_backlog = state.items_waiting_for_sell > 0
        || (state.remaining_cost_lot_value > 0.0 && state.active_sell_orders == 0);
    let flow_stall =
        sell_backlog && state.active_sell_orders == 0 && state.sell_backlog_age_seconds >= 120;

    if state.startup_in_progress || !state.bot_can_accept_commands {
        return decision(
            BazaarNextAction::RecoverGui,
            true,
            flow_stall,
            "BOT_NOT_READY_OR_GUI_BUSY",
        );
    }
    if state.command_queue_busy {
        return decision(
            BazaarNextAction::Idle,
            sell_backlog,
            flow_stall,
            "COMMAND_QUEUE_BUSY",
        );
    }
    if state.filled_sell_orders > 0 {
        return decision(
            BazaarNextAction::ClaimFilledSells,
            true,
            flow_stall,
            "FILLED_SELL_ORDERS_WAITING",
        );
    }
    if state.filled_buy_orders > 0 {
        return decision(
            BazaarNextAction::ClaimFilledBuys,
            true,
            flow_stall,
            "FILLED_BUY_ORDERS_WAITING",
        );
    }
    if sell_backlog {
        return decision(
            BazaarNextAction::PlaceMissingSellOrders,
            true,
            flow_stall,
            "COST_LOTS_WITHOUT_ACTIVE_SELL",
        );
    }
    if state.duplicate_active_sell_items > 0 {
        return decision(
            BazaarNextAction::HandleStaleSells,
            true,
            flow_stall,
            "DUPLICATE_ACTIVE_SELL_ORDERS",
        );
    }
    if state.stale_sell_orders > 0 {
        return decision(
            BazaarNextAction::HandleStaleSells,
            true,
            flow_stall,
            "STALE_SELL_ORDERS",
        );
    }
    if state.stale_buy_orders > 0
        && (state.stale_buy_orders > config.max_stale_buy_orders
            || (config.cancel_stale_buys_when_sell_queue_pending
                && state.items_waiting_for_sell > 0)
            || config.pause_new_buys_when_stale_buy_pressure)
    {
        return decision(
            BazaarNextAction::HandleStaleBuys,
            true,
            flow_stall,
            "STALE_BUY_PRESSURE",
        );
    }
    if state.useful_reprice_available {
        return decision(
            BazaarNextAction::RepriceUsefulOrder,
            true,
            flow_stall,
            "USEFUL_REPRICE_AVAILABLE",
        );
    }
    if state.daily_sell_limit {
        return decision(BazaarNextAction::Idle, true, flow_stall, "DAILY_SELL_LIMIT");
    }
    decision(
        BazaarNextAction::PlaceNewBuy,
        false,
        false,
        "READY_FOR_NEW_BUY",
    )
}

fn decision(
    action: BazaarNextAction,
    block_new_buys: bool,
    flow_stall: bool,
    reason: &str,
) -> BazaarLifecycleDecision {
    BazaarLifecycleDecision {
        action,
        block_new_buys,
        flow_stall,
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_state() -> BazaarLifecycleState {
        BazaarLifecycleState {
            bot_can_accept_commands: true,
            command_queue_busy: false,
            startup_in_progress: false,
            daily_sell_limit: false,
            filled_sell_orders: 0,
            filled_buy_orders: 0,
            items_waiting_for_sell: 0,
            remaining_cost_lot_value: 0.0,
            active_sell_orders: 0,
            duplicate_active_sell_items: 0,
            stale_sell_orders: 0,
            stale_buy_orders: 0,
            useful_reprice_available: false,
            sell_backlog_age_seconds: 0,
        }
    }

    #[test]
    fn sell_backlog_blocks_new_buys_and_places_missing_sells() {
        let mut state = base_state();
        state.items_waiting_for_sell = 3;
        state.remaining_cost_lot_value = 3_700_000.0;
        let decision = determine_next_bazaar_action(&state, &BazaarLifecycleConfig::default());
        assert_eq!(decision.action, BazaarNextAction::PlaceMissingSellOrders);
        assert!(decision.block_new_buys);
    }

    #[test]
    fn filled_sells_have_priority_over_buys_and_backlog() {
        let mut state = base_state();
        state.filled_sell_orders = 1;
        state.filled_buy_orders = 2;
        state.items_waiting_for_sell = 1;
        let decision = determine_next_bazaar_action(&state, &BazaarLifecycleConfig::default());
        assert_eq!(decision.action, BazaarNextAction::ClaimFilledSells);
    }

    #[test]
    fn stale_buy_pressure_blocks_new_buys() {
        let mut state = base_state();
        state.stale_buy_orders = 4;
        let decision = determine_next_bazaar_action(&state, &BazaarLifecycleConfig::default());
        assert_eq!(decision.action, BazaarNextAction::HandleStaleBuys);
        assert!(decision.block_new_buys);
    }

    #[test]
    fn duplicate_active_sells_block_new_buys_for_consolidation() {
        let mut state = base_state();
        state.duplicate_active_sell_items = 1;
        let decision = determine_next_bazaar_action(&state, &BazaarLifecycleConfig::default());
        assert_eq!(decision.action, BazaarNextAction::HandleStaleSells);
        assert_eq!(decision.reason, "DUPLICATE_ACTIVE_SELL_ORDERS");
        assert!(decision.block_new_buys);
    }

    #[test]
    fn prolonged_missing_sell_state_is_a_flow_stall() {
        let mut state = base_state();
        state.remaining_cost_lot_value = 1_000_000.0;
        state.active_sell_orders = 0;
        state.sell_backlog_age_seconds = 121;
        let decision = determine_next_bazaar_action(&state, &BazaarLifecycleConfig::default());
        assert_eq!(decision.action, BazaarNextAction::PlaceMissingSellOrders);
        assert!(decision.flow_stall);
    }
}
