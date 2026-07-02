CREATE TABLE IF NOT EXISTS aave_gho_tokens (
	id INTEGER NOT NULL, 
	token_id INTEGER NOT NULL, 
	v_token_id INTEGER, 
	v_gho_discount_rate_strategy VARCHAR(42), 
	v_gho_discount_token VARCHAR(42), 
	PRIMARY KEY (id), 
	FOREIGN KEY(token_id) REFERENCES erc20_tokens (id), 
	FOREIGN KEY(v_token_id) REFERENCES erc20_tokens (id)
);
CREATE TABLE IF NOT EXISTS aave_v3_asset_configs (
	id INTEGER NOT NULL, 
	asset_id INTEGER NOT NULL, 
	ltv INTEGER NOT NULL, 
	liquidation_threshold INTEGER NOT NULL, 
	liquidation_bonus INTEGER NOT NULL, 
	e_mode_category_id INTEGER, 
	borrowing_enabled BOOLEAN NOT NULL, 
	stable_borrowing_enabled BOOLEAN NOT NULL, 
	flash_loan_enabled BOOLEAN NOT NULL, 
	isolation_mode BOOLEAN NOT NULL, 
	borrowable_in_isolation BOOLEAN NOT NULL, 
	debt_ceiling VARCHAR(78), 
	PRIMARY KEY (id), 
	FOREIGN KEY(asset_id) REFERENCES aave_v3_assets (id)
);
CREATE TABLE IF NOT EXISTS aave_v3_assets (
	id INTEGER NOT NULL, 
	market_id INTEGER NOT NULL, 
	underlying_asset_id INTEGER NOT NULL, 
	a_token_id INTEGER NOT NULL, 
	a_token_revision INTEGER NOT NULL, 
	v_token_id INTEGER NOT NULL, 
	v_token_revision INTEGER NOT NULL, 
	e_mode_category_id INTEGER, 
	price_source VARCHAR(42), 
	last_update_block INTEGER, 
	liquidity_index VARCHAR(78) NOT NULL, 
	liquidity_rate VARCHAR(78) NOT NULL, 
	borrow_index VARCHAR(78) NOT NULL, 
	borrow_rate VARCHAR(78) NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(market_id) REFERENCES aave_v3_markets (id), 
	FOREIGN KEY(underlying_asset_id) REFERENCES erc20_tokens (id), 
	FOREIGN KEY(a_token_id) REFERENCES erc20_tokens (id), 
	FOREIGN KEY(v_token_id) REFERENCES erc20_tokens (id), 
	FOREIGN KEY(e_mode_category_id) REFERENCES aave_v3_emode_categories (id)
);
CREATE TABLE IF NOT EXISTS aave_v3_collateral_positions (
	id INTEGER NOT NULL, 
	user_id INTEGER NOT NULL, 
	asset_id INTEGER NOT NULL, 
	balance VARCHAR(78) NOT NULL, 
	last_index VARCHAR(78), 
	PRIMARY KEY (id), 
	FOREIGN KEY(user_id) REFERENCES aave_v3_users (id), 
	FOREIGN KEY(asset_id) REFERENCES aave_v3_assets (id)
);
CREATE TABLE IF NOT EXISTS aave_v3_contracts (
	id INTEGER NOT NULL, 
	market_id INTEGER NOT NULL, 
	name TEXT NOT NULL, 
	address VARCHAR(42) NOT NULL, 
	revision INTEGER, 
	PRIMARY KEY (id), 
	FOREIGN KEY(market_id) REFERENCES aave_v3_markets (id)
);
CREATE TABLE IF NOT EXISTS aave_v3_debt_positions (
	id INTEGER NOT NULL, 
	user_id INTEGER NOT NULL, 
	asset_id INTEGER NOT NULL, 
	balance VARCHAR(78) NOT NULL, 
	last_index VARCHAR(78), 
	PRIMARY KEY (id), 
	FOREIGN KEY(user_id) REFERENCES aave_v3_users (id), 
	FOREIGN KEY(asset_id) REFERENCES aave_v3_assets (id)
);
CREATE TABLE IF NOT EXISTS aave_v3_emode_categories (
	id INTEGER NOT NULL, 
	market_id INTEGER NOT NULL, 
	category_id INTEGER NOT NULL, 
	label TEXT, 
	ltv INTEGER NOT NULL, 
	liquidation_threshold INTEGER NOT NULL, 
	liquidation_bonus INTEGER NOT NULL, 
	price_source VARCHAR(42), 
	PRIMARY KEY (id), 
	FOREIGN KEY(market_id) REFERENCES aave_v3_markets (id)
);
CREATE TABLE IF NOT EXISTS aave_v3_markets (
	id INTEGER NOT NULL, 
	chain_id INTEGER NOT NULL, 
	name TEXT NOT NULL, 
	active BOOLEAN NOT NULL, 
	last_update_block INTEGER, 
	PRIMARY KEY (id)
);
CREATE TABLE IF NOT EXISTS aave_v3_user_collateral_configs (
	id INTEGER NOT NULL, 
	user_id INTEGER NOT NULL, 
	asset_id INTEGER NOT NULL, 
	enabled BOOLEAN NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(user_id) REFERENCES aave_v3_users (id), 
	FOREIGN KEY(asset_id) REFERENCES aave_v3_assets (id)
);
CREATE TABLE IF NOT EXISTS aave_v3_users (
	id INTEGER NOT NULL, 
	market_id INTEGER NOT NULL, 
	address VARCHAR(42) NOT NULL, 
	e_mode INTEGER NOT NULL, 
	gho_discount INTEGER NOT NULL, 
	stk_aave_balance VARCHAR(78), 
	isolation_mode_collateral_asset_id INTEGER, 
	isolation_mode_debt VARCHAR(78) NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(market_id) REFERENCES aave_v3_markets (id), 
	FOREIGN KEY(isolation_mode_collateral_asset_id) REFERENCES aave_v3_assets (id)
);
CREATE TABLE IF NOT EXISTS aerodrome_v2_pools (
	stable BOOLEAN NOT NULL, 
	pool_id INTEGER NOT NULL, 
	fee_token0 INTEGER NOT NULL, 
	fee_token1 INTEGER NOT NULL, 
	fee_denominator INTEGER NOT NULL, 
	PRIMARY KEY (pool_id), 
	FOREIGN KEY(pool_id) REFERENCES pools (id)
);
CREATE TABLE IF NOT EXISTS aerodrome_v3_pools (
	pool_id INTEGER NOT NULL, 
	tick_spacing INTEGER NOT NULL, 
	liquidity_update_block INTEGER, 
	liquidity_update_log_index INTEGER, 
	fee_token0 INTEGER NOT NULL, 
	fee_token1 INTEGER NOT NULL, 
	fee_denominator INTEGER NOT NULL, 
	PRIMARY KEY (pool_id), 
	FOREIGN KEY(pool_id) REFERENCES pools (id)
);
CREATE TABLE IF NOT EXISTS camelot_v2_pools (
	pool_id INTEGER NOT NULL, 
	fee_token0 INTEGER NOT NULL, 
	fee_token1 INTEGER NOT NULL, 
	fee_denominator INTEGER NOT NULL, 
	PRIMARY KEY (pool_id), 
	FOREIGN KEY(pool_id) REFERENCES pools (id)
);
CREATE TABLE IF NOT EXISTS erc20_tokens (
	id INTEGER NOT NULL, 
	chain INTEGER NOT NULL, 
	address VARCHAR(42) NOT NULL, 
	name TEXT, 
	symbol TEXT, 
	decimals INTEGER, 
	PRIMARY KEY (id)
);
CREATE TABLE IF NOT EXISTS exchanges (
	id INTEGER NOT NULL, 
	chain_id INTEGER NOT NULL, 
	name TEXT NOT NULL, 
	active BOOLEAN NOT NULL, 
	last_update_block INTEGER, 
	factory VARCHAR(42) NOT NULL, 
	deployer VARCHAR(42), 
	PRIMARY KEY (id)
);
CREATE TABLE IF NOT EXISTS initialization_maps (
	id INTEGER NOT NULL, 
	pool_id INTEGER NOT NULL, 
	word INTEGER NOT NULL, 
	bitmap VARCHAR(78) NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(pool_id) REFERENCES pools (id)
);
CREATE TABLE IF NOT EXISTS liquidity_positions (
	id INTEGER NOT NULL, 
	pool_id INTEGER NOT NULL, 
	tick INTEGER NOT NULL, 
	liquidity_net VARCHAR(78) NOT NULL, 
	liquidity_gross VARCHAR(78) NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(pool_id) REFERENCES pools (id)
);
CREATE TABLE IF NOT EXISTS managed_pool_initialization_maps (
	id INTEGER NOT NULL, 
	managed_pool_id INTEGER NOT NULL, 
	word INTEGER NOT NULL, 
	bitmap VARCHAR(78) NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(managed_pool_id) REFERENCES managed_pools (id)
);
CREATE TABLE IF NOT EXISTS managed_pool_liquidity_positions (
	id INTEGER NOT NULL, 
	managed_pool_id INTEGER NOT NULL, 
	tick INTEGER NOT NULL, 
	liquidity_net VARCHAR(78) NOT NULL, 
	liquidity_gross VARCHAR(78) NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(managed_pool_id) REFERENCES managed_pools (id)
);
CREATE TABLE IF NOT EXISTS managed_pools (
	id INTEGER NOT NULL, 
	kind TEXT NOT NULL, 
	manager_id INTEGER NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(manager_id) REFERENCES pool_managers (id)
);
CREATE TABLE IF NOT EXISTS pancakeswap_v2_pools (
	pool_id INTEGER NOT NULL, 
	fee_token0 INTEGER NOT NULL, 
	fee_token1 INTEGER NOT NULL, 
	fee_denominator INTEGER NOT NULL, 
	PRIMARY KEY (pool_id), 
	FOREIGN KEY(pool_id) REFERENCES pools (id)
);
CREATE TABLE IF NOT EXISTS pancakeswap_v3_pools (
	pool_id INTEGER NOT NULL, 
	tick_spacing INTEGER NOT NULL, 
	liquidity_update_block INTEGER, 
	liquidity_update_log_index INTEGER, 
	fee_token0 INTEGER NOT NULL, 
	fee_token1 INTEGER NOT NULL, 
	fee_denominator INTEGER NOT NULL, 
	PRIMARY KEY (pool_id), 
	FOREIGN KEY(pool_id) REFERENCES pools (id)
);
CREATE TABLE IF NOT EXISTS pool_managers (
	id INTEGER NOT NULL, 
	address VARCHAR(42) NOT NULL, 
	chain INTEGER NOT NULL, 
	kind TEXT NOT NULL, 
	state_view VARCHAR(42), 
	exchange_id INTEGER NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(exchange_id) REFERENCES exchanges (id)
);
CREATE TABLE IF NOT EXISTS pools (
	id INTEGER NOT NULL, 
	address VARCHAR(42) NOT NULL, 
	chain INTEGER NOT NULL, 
	kind TEXT NOT NULL, 
	token0_id INTEGER NOT NULL, 
	token1_id INTEGER NOT NULL, 
	exchange_id INTEGER NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(token0_id) REFERENCES erc20_tokens (id), 
	FOREIGN KEY(token1_id) REFERENCES erc20_tokens (id), 
	FOREIGN KEY(exchange_id) REFERENCES exchanges (id)
);
CREATE TABLE IF NOT EXISTS sushiswap_v2_pools (
	pool_id INTEGER NOT NULL, 
	fee_token0 INTEGER NOT NULL, 
	fee_token1 INTEGER NOT NULL, 
	fee_denominator INTEGER NOT NULL, 
	PRIMARY KEY (pool_id), 
	FOREIGN KEY(pool_id) REFERENCES pools (id)
);
CREATE TABLE IF NOT EXISTS sushiswap_v3_pools (
	pool_id INTEGER NOT NULL, 
	tick_spacing INTEGER NOT NULL, 
	liquidity_update_block INTEGER, 
	liquidity_update_log_index INTEGER, 
	fee_token0 INTEGER NOT NULL, 
	fee_token1 INTEGER NOT NULL, 
	fee_denominator INTEGER NOT NULL, 
	PRIMARY KEY (pool_id), 
	FOREIGN KEY(pool_id) REFERENCES pools (id)
);
CREATE TABLE IF NOT EXISTS swapbased_v2_pools (
	pool_id INTEGER NOT NULL, 
	fee_token0 INTEGER NOT NULL, 
	fee_token1 INTEGER NOT NULL, 
	fee_denominator INTEGER NOT NULL, 
	PRIMARY KEY (pool_id), 
	FOREIGN KEY(pool_id) REFERENCES pools (id)
);
CREATE TABLE IF NOT EXISTS uniswap_v2_pools (
	pool_id INTEGER NOT NULL, 
	fee_token0 INTEGER NOT NULL, 
	fee_token1 INTEGER NOT NULL, 
	fee_denominator INTEGER NOT NULL, 
	PRIMARY KEY (pool_id), 
	FOREIGN KEY(pool_id) REFERENCES pools (id)
);
CREATE TABLE IF NOT EXISTS uniswap_v3_pools (
	pool_id INTEGER NOT NULL, 
	tick_spacing INTEGER NOT NULL, 
	liquidity_update_block INTEGER, 
	liquidity_update_log_index INTEGER, 
	fee_token0 INTEGER NOT NULL, 
	fee_token1 INTEGER NOT NULL, 
	fee_denominator INTEGER NOT NULL, 
	PRIMARY KEY (pool_id), 
	FOREIGN KEY(pool_id) REFERENCES pools (id)
);
CREATE TABLE IF NOT EXISTS uniswap_v4_pools (
	managed_pool_id INTEGER NOT NULL, 
	pool_hash VARCHAR(66) NOT NULL, 
	hooks VARCHAR(42) NOT NULL, 
	currency0_id INTEGER NOT NULL, 
	currency1_id INTEGER NOT NULL, 
	fee_currency0 INTEGER NOT NULL, 
	fee_currency1 INTEGER NOT NULL, 
	fee_denominator INTEGER NOT NULL, 
	tick_spacing INTEGER NOT NULL, 
	liquidity_update_block INTEGER, 
	liquidity_update_log_index INTEGER, 
	PRIMARY KEY (managed_pool_id), 
	FOREIGN KEY(managed_pool_id) REFERENCES managed_pools (id), 
	FOREIGN KEY(currency0_id) REFERENCES erc20_tokens (id), 
	FOREIGN KEY(currency1_id) REFERENCES erc20_tokens (id)
);

CREATE UNIQUE INDEX ix_aave_asset_config_asset ON aave_v3_asset_configs (asset_id);
CREATE UNIQUE INDEX ix_aave_assets_underlying_asset_market ON aave_v3_assets (underlying_asset_id, market_id);
CREATE UNIQUE INDEX ix_aave_collateral_position_user_asset ON aave_v3_collateral_positions (user_id, asset_id);
CREATE UNIQUE INDEX ix_aave_debt_position_user_asset ON aave_v3_debt_positions (user_id, asset_id);
CREATE UNIQUE INDEX ix_aave_emode_category_market_cat ON aave_v3_emode_categories (market_id, category_id);
CREATE INDEX ix_aave_gho_tokens_token_id ON aave_gho_tokens (token_id);
CREATE INDEX ix_aave_gho_tokens_v_token_id ON aave_gho_tokens (v_token_id);
CREATE INDEX ix_aave_user_collateral_config_enabled ON aave_v3_user_collateral_configs (enabled);
CREATE UNIQUE INDEX ix_aave_user_collateral_config_user_asset ON aave_v3_user_collateral_configs (user_id, asset_id);
CREATE UNIQUE INDEX ix_aave_users_address_market ON aave_v3_users (address, market_id);
CREATE INDEX ix_aave_v3_asset_configs_asset_id ON aave_v3_asset_configs (asset_id);
CREATE INDEX ix_aave_v3_assets_a_token_id ON aave_v3_assets (a_token_id);
CREATE INDEX ix_aave_v3_assets_e_mode_category_id ON aave_v3_assets (e_mode_category_id);
CREATE INDEX ix_aave_v3_assets_market_id ON aave_v3_assets (market_id);
CREATE INDEX ix_aave_v3_assets_underlying_asset_id ON aave_v3_assets (underlying_asset_id);
CREATE INDEX ix_aave_v3_assets_v_token_id ON aave_v3_assets (v_token_id);
CREATE INDEX ix_aave_v3_collateral_positions_asset_id ON aave_v3_collateral_positions (asset_id);
CREATE INDEX ix_aave_v3_collateral_positions_user_id ON aave_v3_collateral_positions (user_id);
CREATE INDEX ix_aave_v3_contracts_market_id ON aave_v3_contracts (market_id);
CREATE INDEX ix_aave_v3_debt_positions_asset_id ON aave_v3_debt_positions (asset_id);
CREATE INDEX ix_aave_v3_debt_positions_user_id ON aave_v3_debt_positions (user_id);
CREATE INDEX ix_aave_v3_emode_categories_market_id ON aave_v3_emode_categories (market_id);
CREATE INDEX ix_aave_v3_user_collateral_configs_asset_id ON aave_v3_user_collateral_configs (asset_id);
CREATE INDEX ix_aave_v3_user_collateral_configs_user_id ON aave_v3_user_collateral_configs (user_id);
CREATE INDEX ix_aave_v3_users_isolation_mode_collateral_asset_id ON aave_v3_users (isolation_mode_collateral_asset_id);
CREATE INDEX ix_aave_v3_users_market_id ON aave_v3_users (market_id);
CREATE UNIQUE INDEX ix_erc20_tokens_address_chain ON erc20_tokens (address, chain);
CREATE INDEX ix_erc20_tokens_chain ON erc20_tokens (chain);
CREATE UNIQUE INDEX ix_initialization_maps_pool_id_word ON initialization_maps (pool_id, word);
CREATE UNIQUE INDEX ix_liquidity_pool_address_chain ON pools (address, chain);
CREATE INDEX ix_liquidity_pools_token_ids ON pools (token0_id, token1_id);
CREATE UNIQUE INDEX ix_liquidity_positions_pool_id_tick ON liquidity_positions (pool_id, tick);
CREATE INDEX ix_managed_pool_initialization_maps_managed_pool_id ON managed_pool_initialization_maps (managed_pool_id);
CREATE UNIQUE INDEX ix_managed_pool_initialization_maps_pool_id_word ON managed_pool_initialization_maps (managed_pool_id, word);
CREATE INDEX ix_managed_pool_liquidity_positions_managed_pool_id ON managed_pool_liquidity_positions (managed_pool_id);
CREATE UNIQUE INDEX ix_managed_pool_liquidity_positions_pool_id_tick ON managed_pool_liquidity_positions (managed_pool_id, tick);
CREATE INDEX ix_managed_pools_manager_id ON managed_pools (manager_id);
CREATE UNIQUE INDEX ix_pool_manager_address_chain ON pool_managers (address, chain);
CREATE INDEX ix_pools_token0_id ON pools (token0_id);
CREATE INDEX ix_pools_token1_id ON pools (token1_id);
CREATE INDEX ix_uniswap_v4_pools_currency0_id ON uniswap_v4_pools (currency0_id);
CREATE INDEX ix_uniswap_v4_pools_currency1_id ON uniswap_v4_pools (currency1_id);
CREATE INDEX ix_uniswap_v4_pools_pool_hash ON uniswap_v4_pools (pool_hash);
CREATE INDEX ix_uniswap_v4_pools_token_ids ON uniswap_v4_pools (currency0_id, currency1_id);
