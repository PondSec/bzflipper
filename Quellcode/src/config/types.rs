use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Serde helpers that serialize `None` as `""` and deserialize `""` as `None`.
/// This ensures optional string config fields always appear in the saved TOML file
/// so users can see and edit them without needing to know the field names.
mod opt_string_as_empty {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(value: &Option<String>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(value.as_deref().unwrap_or(""))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(if s.is_empty() { None } else { Some(s) })
    }
}

/// Serde helpers that serialize `None` as `0.0` and deserialize `0.0` (or any non-positive value) as `None`.
/// Used for `multi_switch_time` so the field appears in config.toml with a clear "disabled" value.
/// Note: negative values are also treated as `None` (disabled) since negative hours make no sense.
mod opt_f64_as_zero {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(value: &Option<f64>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_f64(value.unwrap_or(0.0))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let f = f64::deserialize(deserializer)?;
        Ok(if f <= 0.0 { None } else { Some(f) })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Ingame Minecraft username(s). Supports multiple comma-separated accounts:
    /// `ingame_name = "Account1"` for a single account, or
    /// `ingame_name = "Account1,Account2"` for automatic switching.
    #[serde(default)]
    pub ingame_name: Option<String>,

    /// Time in hours after which the bot switches to the next account in `ingame_name`.
    /// Only used when multiple accounts are specified. E.g. `multi_switch_time = 12.0`
    /// means switch accounts every 12 hours. Set to `0` to disable automatic switching.
    #[serde(default, with = "opt_f64_as_zero")]
    pub multi_switch_time: Option<f64>,

    #[serde(default = "default_websocket_url")]
    pub websocket_url: String,

    #[serde(default = "default_web_gui_port")]
    pub web_gui_port: u16,

    /// Minimum delay between consecutive queued commands in milliseconds.
    /// Prevents back-to-back Hypixel interactions from overlapping.
    /// Default: 500ms.
    #[serde(default = "default_command_delay_ms")]
    pub command_delay_ms: u64,

    #[serde(default = "default_bed_spam_click_delay")]
    pub bed_spam_click_delay: u64,

    #[serde(default)]
    pub bed_multiple_clicks_delay: u64,

    /// How many ms before the COFL `purchaseAt` deadline to start clicking (default: 30).
    /// Only used when `freemoney = true`. Without freemoney, bed spam starts immediately
    /// using `bed_spam_click_delay` and this value is ignored.
    #[serde(default = "default_bed_pre_click_ms")]
    pub bed_pre_click_ms: u64,

    #[serde(default = "default_bazaar_order_check_interval_seconds")]
    pub bazaar_order_check_interval_seconds: u64,

    #[serde(
        default = "default_bazaar_order_cancel_minutes_per_million",
        alias = "bazaar_order_cancel_minutes"
    )]
    pub bazaar_order_cancel_minutes_per_million: u64,

    /// Bazaar sell tax rate as a percentage (e.g. 1.25 = 1.25%).
    /// Hypixel applies 1.25% by default. The Bazaar Flipper perk from the
    /// Community Shop reduces it by up to 0.25% (two levels × 0.125%).
    /// Set to 1.0 if you have the max perk level.
    #[serde(default = "default_bazaar_tax_rate")]
    pub bazaar_tax_rate: f64,

    /// Percent of current purse that must stay untouched by local Bazaar BUY orders.
    #[serde(default = "default_purse_reserve_percent")]
    pub purse_reserve_percent: f64,

    /// Enable local Bazaar scanning using the public Hypixel Bazaar API.
    /// When enabled, the bot periodically ranks products by spread, volume,
    /// and profit, then queues the best BUY order. COFL Bazaar flips remain
    /// enabled separately.
    #[serde(default = "default_true")]
    pub enable_local_bazaar_scanner: bool,

    /// Seconds between local Bazaar scans.
    #[serde(default = "default_local_bazaar_scan_interval_seconds")]
    pub local_bazaar_scan_interval_seconds: u64,

    /// Minimum expected profit per item after Bazaar tax.
    #[serde(
        default = "default_local_bazaar_min_profit_per_unit",
        alias = "minProfitBazaar"
    )]
    pub local_bazaar_min_profit_per_unit: f64,

    /// Backward-compatible alias-style field name users asked for.
    #[serde(
        default = "default_local_bazaar_min_total_profit",
        alias = "local_bazaar_min_profit"
    )]
    pub min_profit_bazaar: f64,

    /// Minimum expected total profit for the full order after Bazaar tax.
    #[serde(default = "default_local_bazaar_min_total_profit")]
    pub local_bazaar_min_total_profit: f64,

    /// Minimum profit margin percentage after Bazaar tax.
    #[serde(default = "default_local_bazaar_min_margin_percent")]
    pub local_bazaar_min_margin_percent: f64,

    /// Maximum profit margin percentage. Very high margins are usually stale,
    /// manipulated, or too illiquid to fill reliably.
    #[serde(default = "default_local_bazaar_max_margin_percent")]
    pub local_bazaar_max_margin_percent: f64,

    /// Minimum active buy/sell order volume required by Hypixel quick_status.
    #[serde(default = "default_local_bazaar_min_volume")]
    pub local_bazaar_min_buy_volume: u64,
    #[serde(default = "default_local_bazaar_min_volume")]
    pub local_bazaar_min_sell_volume: u64,

    /// Minimum active order count on both Bazaar sides.
    #[serde(default = "default_local_bazaar_min_order_count")]
    pub local_bazaar_min_order_count: u64,

    /// Minimum 7-day moved volume required.
    #[serde(default = "default_local_bazaar_min_moving_week")]
    pub local_bazaar_min_moving_week: u64,

    /// Maximum coins to put into one local Bazaar BUY order.
    /// 0 means use volume/max amount only.
    #[serde(default = "default_local_bazaar_max_order_value")]
    pub local_bazaar_max_order_value: u64,

    /// Maximum item amount for one local Bazaar BUY order.
    #[serde(default = "default_local_bazaar_max_amount")]
    pub local_bazaar_max_amount: u64,

    /// Price step used to beat the current top order/offer.
    #[serde(default = "default_local_bazaar_price_undercut")]
    pub local_bazaar_price_undercut: f64,

    /// Maximum number of active local-scanner Bazaar BUY orders to keep open.
    #[serde(default = "default_local_bazaar_max_concurrent_orders")]
    pub local_bazaar_max_concurrent_orders: usize,

    /// Desired local Bazaar profit per hour. Used to reject flips that are too
    /// slow relative to configured volume and target profit.
    #[serde(default = "default_local_bazaar_target_profit_per_hour")]
    pub local_bazaar_target_profit_per_hour: f64,

    /// Capital model used by the local Bazaar allocation algorithm.
    #[serde(default = "default_local_bazaar_total_capital")]
    pub local_bazaar_total_capital: u64,
    #[serde(default = "default_local_bazaar_active_capital_ratio")]
    pub local_bazaar_active_capital_ratio: f64,
    #[serde(default = "default_local_bazaar_reserve_ratio")]
    pub local_bazaar_reserve_ratio: f64,
    #[serde(default = "default_local_bazaar_max_items")]
    pub local_bazaar_max_items: usize,
    #[serde(default = "default_local_bazaar_max_capital_per_item")]
    pub local_bazaar_max_capital_per_item: u64,
    #[serde(default = "default_local_bazaar_min_roi_percent")]
    pub local_bazaar_min_roi_percent: f64,
    #[serde(default = "default_local_bazaar_target_roi_percent")]
    pub local_bazaar_target_roi_percent: f64,
    #[serde(default = "default_local_bazaar_min_volume_value_hour")]
    pub local_bazaar_min_volume_value_hour: f64,
    #[serde(default = "default_local_bazaar_preferred_volume_value_hour")]
    pub local_bazaar_preferred_volume_value_hour: f64,
    #[serde(default = "default_local_bazaar_market_participation_rate")]
    pub local_bazaar_market_participation_rate: f64,
    #[serde(default = "default_local_bazaar_conservative_market_participation_rate")]
    pub local_bazaar_conservative_market_participation_rate: f64,
    #[serde(default = "default_local_bazaar_history_window_minutes")]
    pub local_bazaar_history_window_minutes: u64,
    #[serde(default = "default_local_bazaar_min_free_inventory_slots")]
    pub local_bazaar_min_free_inventory_slots: u64,
    #[serde(default = "default_local_bazaar_max_pending_buy_stacks")]
    pub local_bazaar_max_pending_buy_stacks: u64,
    #[serde(default = "default_local_bazaar_buy_sell_balance_limit")]
    pub local_bazaar_buy_sell_balance_limit: f64,
    #[serde(default = "default_local_bazaar_min_reprice_profit_improvement")]
    pub local_bazaar_min_reprice_profit_improvement: f64,
    #[serde(default = "default_local_bazaar_min_reprice_interval_seconds")]
    pub local_bazaar_min_reprice_interval_seconds: u64,
    #[serde(default = "default_local_bazaar_max_reprices_per_item_per_hour")]
    pub local_bazaar_max_reprices_per_item_per_hour: u64,
    #[serde(default = "default_local_bazaar_reprice_cooldown_seconds")]
    pub local_bazaar_reprice_cooldown_seconds: u64,

    /// Prefer/allow classic high-liquidity potato book flips.
    #[serde(default = "default_true", alias = "classic_potato_book_flips")]
    pub enable_classic_potato_book_flips: bool,

    /// Delay in milliseconds between consecutive auction listing commands
    /// (SellToAuction). Prevents Hypixel from kicking the bot with
    /// "Sending packets too fast!" during bulk listings. Default: 1500ms.
    #[serde(default = "default_auction_listing_delay_ms")]
    pub auction_listing_delay_ms: u64,

    /// Enable Bazaar flipping intake and local Bazaar scanner.
    #[serde(default = "default_true")]
    pub enable_bazaar_flips: bool,

    /// Enable Auction House flipping intake.
    #[serde(default = "default_true")]
    pub enable_ah_flips: bool,

    #[serde(default)]
    pub bed_spam: bool,

    /// Enable fast-buy skip-click on predicted Confirm Purchase window.
    /// When true, the bot pre-clicks slot 11 (confirm) in the same TCP burst as
    /// the buy-click, saving one round-trip to the server.
    #[serde(default, alias = "fastbuy")]
    pub skip: bool,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freemoney: Option<bool>,

    #[serde(default = "default_true")]
    pub use_cofl_chat: bool,

    #[serde(default)]
    pub auto_cookie: u64,

    #[serde(default = "default_true")]
    pub enable_console_input: bool,

    #[serde(default = "default_auction_duration_hours")]
    pub auction_duration_hours: u64,

    /// Maximum number of flip items allowed in inventory at once.
    /// Sent to COFL on startup via `/cofl set maxitemsininventory`.
    /// Default: 12.
    #[serde(default = "default_max_items_in_inventory")]
    pub max_items_in_inventory: u64,

    /// Enable proxy for both the Minecraft and WebSocket connections.
    #[serde(default)]
    pub proxy_enabled: bool,

    /// Proxy server address in `host:port` format, e.g. `"121.124.241.211:3313"`.
    /// Only used when `proxy_enabled = true`. Leave empty to disable.
    #[serde(default, with = "opt_string_as_empty")]
    pub proxy_address: Option<String>,

    /// Proxy credentials in `username:password` format, e.g. `"myuser:mypassword"`.
    /// Leave empty if the proxy requires no authentication.
    #[serde(default, with = "opt_string_as_empty")]
    pub proxy_credentials: Option<String>,

    #[serde(default)]
    /// Discord webhook URL for notifications.
    /// `None` = not yet configured (prompts on next startup).
    /// `Some("")` = explicitly disabled (no further prompts).
    /// `Some(url)` = active webhook.
    pub webhook_url: Option<String>,

    /// Separate Discord webhook URL for bazaar-specific notifications
    /// (order placed, collected, cancelled). Leave empty to use the regular
    /// `webhook_url` for all notifications.
    #[serde(default, with = "opt_string_as_empty")]
    pub bazaar_webhook_url: Option<String>,

    /// Discord user ID for pinging on legendary/divine flips and bans.
    /// Leave empty to disable pings.
    #[serde(default, with = "opt_string_as_empty")]
    pub discord_id: Option<String>,

    /// Password to protect the web control panel. Leave empty to disable authentication.
    #[serde(default, with = "opt_string_as_empty")]
    pub web_gui_password: Option<String>,

    /// Hypixel API key for fetching active auctions. Obtain one from https://developer.hypixel.net/
    /// Leave empty to use the Coflnet API as a fallback.
    #[serde(default, with = "opt_string_as_empty")]
    pub hypixel_api_key: Option<String>,

    /// Whether to share legendary/divine flip purchases to the public Discord channel.
    /// Defaults to true. Set to false to opt out.
    #[serde(default = "default_true")]
    pub share_legendary_flips: bool,

    /// Whether to anonymize the username in the web GUI panel and profit summary webhooks.
    /// When true, account names and avatars in the web panel are hidden, and the IGN is
    /// replaced with random characters in webhooks.  Defaults to false.
    /// **Deprecated**: This config value is ignored. Anonymization is now a session-only
    /// toggle in the web panel that always defaults to OFF on page load.
    #[serde(default)]
    pub anonymize_webhook_name: bool,

    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub sessions: HashMap<String, CoflSession>,

    // ── Humanization / Rest Breaks ──────────────────────────────
    /// Enable periodic "human-like" rest breaks where the macro disconnects
    /// for a randomized period before reconnecting. Does NOT reset the
    /// account-switching session timer. Default: false.
    #[serde(default)]
    pub humanization_enabled: bool,

    /// Minimum time between rest breaks in minutes. Default: 45.
    #[serde(default = "default_humanization_min_interval_minutes")]
    pub humanization_min_interval_minutes: u64,

    /// Maximum time between rest breaks in minutes. Default: 120.
    #[serde(default = "default_humanization_max_interval_minutes")]
    pub humanization_max_interval_minutes: u64,

    /// Minimum rest break duration in minutes. Default: 2.
    #[serde(default = "default_humanization_min_break_minutes")]
    pub humanization_min_break_minutes: u64,

    /// Maximum rest break duration in minutes. Default: 10.
    #[serde(default = "default_humanization_max_break_minutes")]
    pub humanization_max_break_minutes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoflSession {
    pub id: String,
    pub expires: DateTime<Utc>,
}

// Default values
fn default_websocket_url() -> String {
    "wss://sky.coflnet.com/modsocket".to_string()
}

fn default_web_gui_port() -> u16 {
    8080
}

fn default_command_delay_ms() -> u64 {
    500
}

fn default_bed_spam_click_delay() -> u64 {
    100
}

fn default_bed_pre_click_ms() -> u64 {
    30
}

fn default_bazaar_order_check_interval_seconds() -> u64 {
    60
}

fn default_bazaar_order_cancel_minutes_per_million() -> u64 {
    1
}

fn default_bazaar_tax_rate() -> f64 {
    1.25
}

fn default_purse_reserve_percent() -> f64 {
    22.0
}

fn default_local_bazaar_scan_interval_seconds() -> u64 {
    60
}

fn default_local_bazaar_min_profit_per_unit() -> f64 {
    5.0
}

fn default_local_bazaar_min_total_profit() -> f64 {
    15_000.0
}

fn default_local_bazaar_min_margin_percent() -> f64 {
    1.25
}

fn default_local_bazaar_max_margin_percent() -> f64 {
    32.0
}

fn default_local_bazaar_min_volume() -> u64 {
    5_000
}

fn default_local_bazaar_min_order_count() -> u64 {
    20
}

fn default_local_bazaar_min_moving_week() -> u64 {
    250_000
}

fn default_local_bazaar_max_order_value() -> u64 {
    5_000_000
}

fn default_local_bazaar_max_amount() -> u64 {
    71_680
}

fn default_local_bazaar_price_undercut() -> f64 {
    0.1
}

fn default_local_bazaar_max_concurrent_orders() -> usize {
    6
}

fn default_local_bazaar_target_profit_per_hour() -> f64 {
    2_000_000.0
}

fn default_local_bazaar_total_capital() -> u64 {
    30_000_000
}

fn default_local_bazaar_active_capital_ratio() -> f64 {
    0.78
}

fn default_local_bazaar_reserve_ratio() -> f64 {
    0.22
}

fn default_local_bazaar_max_items() -> usize {
    8
}

fn default_local_bazaar_max_capital_per_item() -> u64 {
    5_000_000
}

fn default_local_bazaar_min_roi_percent() -> f64 {
    1.0
}

fn default_local_bazaar_target_roi_percent() -> f64 {
    2.0
}

fn default_local_bazaar_min_volume_value_hour() -> f64 {
    5_000_000.0
}

fn default_local_bazaar_preferred_volume_value_hour() -> f64 {
    20_000_000.0
}

fn default_local_bazaar_market_participation_rate() -> f64 {
    0.12
}

fn default_local_bazaar_conservative_market_participation_rate() -> f64 {
    0.06
}

fn default_local_bazaar_history_window_minutes() -> u64 {
    60
}

fn default_local_bazaar_min_free_inventory_slots() -> u64 {
    10
}

fn default_local_bazaar_max_pending_buy_stacks() -> u64 {
    6
}

fn default_local_bazaar_buy_sell_balance_limit() -> f64 {
    1.10
}

fn default_local_bazaar_min_reprice_profit_improvement() -> f64 {
    50_000.0
}
fn default_local_bazaar_min_reprice_interval_seconds() -> u64 {
    180
}
fn default_local_bazaar_max_reprices_per_item_per_hour() -> u64 {
    4
}
fn default_local_bazaar_reprice_cooldown_seconds() -> u64 {
    300
}

fn default_auction_listing_delay_ms() -> u64 {
    1500
}

fn default_auction_duration_hours() -> u64 {
    24
}

fn default_max_items_in_inventory() -> u64 {
    12
}

fn default_true() -> bool {
    true
}

fn default_humanization_min_interval_minutes() -> u64 {
    45
}

fn default_humanization_max_interval_minutes() -> u64 {
    120
}

fn default_humanization_min_break_minutes() -> u64 {
    2
}

fn default_humanization_max_break_minutes() -> u64 {
    10
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ingame_name: None,
            multi_switch_time: None,
            websocket_url: default_websocket_url(),
            web_gui_port: default_web_gui_port(),
            command_delay_ms: default_command_delay_ms(),
            bed_spam_click_delay: default_bed_spam_click_delay(),
            bed_multiple_clicks_delay: 0,
            bed_pre_click_ms: default_bed_pre_click_ms(),
            bazaar_order_check_interval_seconds: default_bazaar_order_check_interval_seconds(),
            bazaar_order_cancel_minutes_per_million:
                default_bazaar_order_cancel_minutes_per_million(),
            bazaar_tax_rate: default_bazaar_tax_rate(),
            purse_reserve_percent: default_purse_reserve_percent(),
            enable_local_bazaar_scanner: true,
            local_bazaar_scan_interval_seconds: default_local_bazaar_scan_interval_seconds(),
            local_bazaar_min_profit_per_unit: default_local_bazaar_min_profit_per_unit(),
            min_profit_bazaar: default_local_bazaar_min_total_profit(),
            local_bazaar_min_total_profit: default_local_bazaar_min_total_profit(),
            local_bazaar_min_margin_percent: default_local_bazaar_min_margin_percent(),
            local_bazaar_max_margin_percent: default_local_bazaar_max_margin_percent(),
            local_bazaar_min_buy_volume: default_local_bazaar_min_volume(),
            local_bazaar_min_sell_volume: default_local_bazaar_min_volume(),
            local_bazaar_min_order_count: default_local_bazaar_min_order_count(),
            local_bazaar_min_moving_week: default_local_bazaar_min_moving_week(),
            local_bazaar_max_order_value: default_local_bazaar_max_order_value(),
            local_bazaar_max_amount: default_local_bazaar_max_amount(),
            local_bazaar_price_undercut: default_local_bazaar_price_undercut(),
            local_bazaar_max_concurrent_orders: default_local_bazaar_max_concurrent_orders(),
            local_bazaar_target_profit_per_hour: default_local_bazaar_target_profit_per_hour(),
            local_bazaar_total_capital: default_local_bazaar_total_capital(),
            local_bazaar_active_capital_ratio: default_local_bazaar_active_capital_ratio(),
            local_bazaar_reserve_ratio: default_local_bazaar_reserve_ratio(),
            local_bazaar_max_items: default_local_bazaar_max_items(),
            local_bazaar_max_capital_per_item: default_local_bazaar_max_capital_per_item(),
            local_bazaar_min_roi_percent: default_local_bazaar_min_roi_percent(),
            local_bazaar_target_roi_percent: default_local_bazaar_target_roi_percent(),
            local_bazaar_min_volume_value_hour: default_local_bazaar_min_volume_value_hour(),
            local_bazaar_preferred_volume_value_hour:
                default_local_bazaar_preferred_volume_value_hour(),
            local_bazaar_market_participation_rate: default_local_bazaar_market_participation_rate(
            ),
            local_bazaar_conservative_market_participation_rate:
                default_local_bazaar_conservative_market_participation_rate(),
            local_bazaar_history_window_minutes: default_local_bazaar_history_window_minutes(),
            local_bazaar_min_free_inventory_slots: default_local_bazaar_min_free_inventory_slots(),
            local_bazaar_max_pending_buy_stacks: default_local_bazaar_max_pending_buy_stacks(),
            local_bazaar_buy_sell_balance_limit: default_local_bazaar_buy_sell_balance_limit(),
            local_bazaar_min_reprice_profit_improvement:
                default_local_bazaar_min_reprice_profit_improvement(),
            local_bazaar_min_reprice_interval_seconds:
                default_local_bazaar_min_reprice_interval_seconds(),
            local_bazaar_max_reprices_per_item_per_hour:
                default_local_bazaar_max_reprices_per_item_per_hour(),
            local_bazaar_reprice_cooldown_seconds: default_local_bazaar_reprice_cooldown_seconds(),
            enable_classic_potato_book_flips: true,
            auction_listing_delay_ms: default_auction_listing_delay_ms(),
            enable_bazaar_flips: true,
            enable_ah_flips: true,
            bed_spam: false,
            skip: false,
            freemoney: None,
            use_cofl_chat: true,
            auto_cookie: 0,
            enable_console_input: true,
            auction_duration_hours: default_auction_duration_hours(),
            max_items_in_inventory: default_max_items_in_inventory(),
            proxy_enabled: false,
            proxy_address: None,
            proxy_credentials: None,
            webhook_url: None,
            bazaar_webhook_url: None,
            discord_id: None,
            web_gui_password: None,
            hypixel_api_key: None,
            share_legendary_flips: true,
            anonymize_webhook_name: false,
            sessions: HashMap::new(),
            humanization_enabled: false,
            humanization_min_interval_minutes: default_humanization_min_interval_minutes(),
            humanization_max_interval_minutes: default_humanization_max_interval_minutes(),
            humanization_min_break_minutes: default_humanization_min_break_minutes(),
            humanization_max_break_minutes: default_humanization_max_break_minutes(),
        }
    }
}

impl Config {
    pub fn freemoney_enabled(&self) -> bool {
        self.freemoney.unwrap_or(false)
    }

    pub fn skip_enabled(&self) -> bool {
        self.skip
    }

    /// Returns the webhook URL only if it is non-empty.
    pub fn active_webhook_url(&self) -> Option<&str> {
        self.webhook_url.as_deref().filter(|u| !u.is_empty())
    }

    /// Returns the bazaar-specific webhook URL if set, otherwise falls back
    /// to the regular `webhook_url`. Returns `None` if neither is configured.
    pub fn active_bazaar_webhook_url(&self) -> Option<&str> {
        self.bazaar_webhook_url
            .as_deref()
            .filter(|u| !u.is_empty())
            .or_else(|| self.active_webhook_url())
    }

    /// Returns the Discord user ID only if it is non-empty.
    pub fn active_discord_id(&self) -> Option<&str> {
        self.discord_id.as_deref().filter(|id| !id.is_empty())
    }

    /// Returns all ingame names parsed from the (comma-separated) `ingame_name` field.
    /// `"Account1,Account2"` → `["Account1", "Account2"]`
    pub fn ingame_names(&self) -> Vec<String> {
        match &self.ingame_name {
            None => vec![],
            Some(s) => s
                .split(',')
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
                .collect(),
        }
    }

    /// Returns the proxy username parsed from `proxy_credentials` (`"user:pass"` → `"user"`).
    pub fn proxy_username(&self) -> Option<&str> {
        let creds = self.proxy_credentials.as_deref()?;
        // splitn(2, ':').next() always returns Some for non-empty iterators
        Some(creds.splitn(2, ':').next().unwrap())
    }

    /// Returns the proxy password parsed from `proxy_credentials` (`"user:pass"` → `"pass"`).
    pub fn proxy_password(&self) -> Option<&str> {
        let creds = self.proxy_credentials.as_deref()?;
        let colon_pos = creds.find(':')?;
        Some(&creds[colon_pos + 1..])
    }
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn default_config_omits_freemoney() {
        let toml =
            toml::to_string_pretty(&Config::default()).expect("default config should serialize");
        assert!(!toml.contains("freemoney"));
    }

    #[test]
    fn manual_freemoney_true_enables_flag() {
        let config: Config = toml::from_str("freemoney = true").expect("config should parse");
        assert!(config.freemoney_enabled());
    }

    #[test]
    fn parses_bed_spam_click_delay() {
        let config: Config =
            toml::from_str("bed_spam_click_delay = 125").expect("config should parse");
        assert_eq!(config.bed_spam_click_delay, 125);
    }

    #[test]
    fn default_bed_pre_click_ms() {
        let config = Config::default();
        assert_eq!(config.bed_pre_click_ms, 30);
    }

    #[test]
    fn parses_bed_pre_click_ms() {
        let config: Config = toml::from_str("bed_pre_click_ms = 300").expect("config should parse");
        assert_eq!(config.bed_pre_click_ms, 300);
    }

    #[test]
    fn single_ingame_name() {
        let config: Config =
            toml::from_str(r#"ingame_name = "Player1""#).expect("config should parse");
        assert_eq!(config.ingame_names(), vec!["Player1"]);
    }

    #[test]
    fn multiple_ingame_names() {
        let config: Config = toml::from_str(r#"ingame_name = "Player1,Player2,Player3""#)
            .expect("config should parse");
        assert_eq!(config.ingame_names(), vec!["Player1", "Player2", "Player3"]);
    }

    #[test]
    fn multiple_ingame_names_with_spaces() {
        let config: Config = toml::from_str(r#"ingame_name = "Player1, Player2 , Player3""#)
            .expect("config should parse");
        assert_eq!(config.ingame_names(), vec!["Player1", "Player2", "Player3"]);
    }

    #[test]
    fn no_ingame_name() {
        let config = Config::default();
        assert!(config.ingame_names().is_empty());
    }

    #[test]
    fn parses_multi_switch_time() {
        let config: Config =
            toml::from_str("multi_switch_time = 12.0").expect("config should parse");
        assert_eq!(config.multi_switch_time, Some(12.0));
    }

    #[test]
    fn multi_switch_time_zero_is_none() {
        let config: Config =
            toml::from_str("multi_switch_time = 0.0").expect("config should parse");
        assert_eq!(config.multi_switch_time, None);
    }

    #[test]
    fn multi_switch_time_default_serializes_as_zero() {
        let toml =
            toml::to_string_pretty(&Config::default()).expect("default config should serialize");
        assert!(toml.contains("multi_switch_time = 0.0"));
    }

    #[test]
    fn proxy_credentials_parsing() {
        let config: Config = toml::from_str(r#"proxy_credentials = "myuser:mypassword""#)
            .expect("config should parse");
        assert_eq!(config.proxy_username(), Some("myuser"));
        assert_eq!(config.proxy_password(), Some("mypassword"));
    }

    #[test]
    fn proxy_credentials_password_with_colon() {
        let config: Config =
            toml::from_str(r#"proxy_credentials = "user:pass:word""#).expect("config should parse");
        assert_eq!(config.proxy_username(), Some("user"));
        assert_eq!(config.proxy_password(), Some("pass:word"));
    }

    #[test]
    fn proxy_empty_string_is_none() {
        let config: Config = toml::from_str(r#"proxy_address = """#).expect("config should parse");
        assert_eq!(config.proxy_address, None);
    }

    #[test]
    fn web_gui_password_empty_string_is_none() {
        let config: Config =
            toml::from_str(r#"web_gui_password = """#).expect("config should parse");
        assert_eq!(config.web_gui_password, None);
    }

    #[test]
    fn optional_fields_appear_in_default_config() {
        let toml =
            toml::to_string_pretty(&Config::default()).expect("default config should serialize");
        assert!(
            toml.contains("web_gui_password"),
            "web_gui_password should appear in default config"
        );
        assert!(
            toml.contains("proxy_address"),
            "proxy_address should appear in default config"
        );
        assert!(
            toml.contains("proxy_credentials"),
            "proxy_credentials should appear in default config"
        );
        assert!(
            toml.contains("multi_switch_time"),
            "multi_switch_time should appear in default config"
        );
        assert!(
            toml.contains("discord_id"),
            "discord_id should appear in default config"
        );
    }

    #[test]
    fn proxy_fields_use_new_names() {
        let config: Config = toml::from_str(
            r#"
proxy_enabled = true
proxy_address = "121.124.241.211:3313"
proxy_credentials = "myuser:mypassword"
"#,
        )
        .expect("config should parse");
        assert!(config.proxy_enabled);
        assert_eq!(
            config.proxy_address.as_deref(),
            Some("121.124.241.211:3313")
        );
        assert_eq!(config.proxy_username(), Some("myuser"));
        assert_eq!(config.proxy_password(), Some("mypassword"));
    }

    #[test]
    fn default_config_has_no_skip_field() {
        let toml =
            toml::to_string_pretty(&Config::default()).expect("default config should serialize");
        assert!(!toml.contains("[skip]"));
        assert!(
            !toml
                .lines()
                .any(|line| line.trim_start().starts_with("min_profit ="))
        );
    }

    #[test]
    fn discord_id_empty_string_is_none() {
        let config: Config = toml::from_str(r#"discord_id = """#).expect("config should parse");
        assert_eq!(config.discord_id, None);
        assert_eq!(config.active_discord_id(), None);
    }

    #[test]
    fn discord_id_parses_and_returns_active() {
        let config: Config =
            toml::from_str(r#"discord_id = "123456789012345678""#).expect("config should parse");
        assert_eq!(config.active_discord_id(), Some("123456789012345678"));
    }

    #[test]
    fn skip_defaults_to_false() {
        let config = Config::default();
        assert!(!config.skip_enabled());
    }

    #[test]
    fn parses_skip_true() {
        let config: Config = toml::from_str("skip = true").expect("config should parse");
        assert!(config.skip_enabled());
    }

    #[test]
    fn parses_skip_false() {
        let config: Config = toml::from_str("skip = false").expect("config should parse");
        assert!(!config.skip_enabled());
    }

    #[test]
    fn fastbuy_alias_still_works() {
        let config: Config = toml::from_str("fastbuy = true").expect("config should parse");
        assert!(config.skip_enabled());
    }

    #[test]
    fn skip_appears_in_default_config() {
        let toml =
            toml::to_string_pretty(&Config::default()).expect("default config should serialize");
        assert!(
            toml.contains("skip = false"),
            "skip should appear in default config"
        );
    }

    #[test]
    fn bazaar_webhook_url_defaults_to_none() {
        let config: Config = toml::from_str("").expect("config should parse");
        assert_eq!(config.bazaar_webhook_url, None);
        assert_eq!(config.active_bazaar_webhook_url(), None);
    }

    #[test]
    fn bazaar_webhook_url_falls_back_to_regular() {
        let config: Config =
            toml::from_str(r#"webhook_url = "https://discord.com/api/webhooks/main""#)
                .expect("config should parse");
        assert_eq!(
            config.active_bazaar_webhook_url(),
            Some("https://discord.com/api/webhooks/main")
        );
    }

    #[test]
    fn bazaar_webhook_url_overrides_regular() {
        let config: Config = toml::from_str(
            r#"webhook_url = "https://discord.com/api/webhooks/main"
bazaar_webhook_url = "https://discord.com/api/webhooks/bazaar""#,
        )
        .expect("config should parse");
        assert_eq!(
            config.active_bazaar_webhook_url(),
            Some("https://discord.com/api/webhooks/bazaar")
        );
        // Regular webhook is unchanged
        assert_eq!(
            config.active_webhook_url(),
            Some("https://discord.com/api/webhooks/main")
        );
    }

    #[test]
    fn bazaar_webhook_url_empty_string_falls_back() {
        let config: Config = toml::from_str(
            r#"webhook_url = "https://discord.com/api/webhooks/main"
bazaar_webhook_url = """#,
        )
        .expect("config should parse");
        assert_eq!(
            config.active_bazaar_webhook_url(),
            Some("https://discord.com/api/webhooks/main")
        );
    }
}
