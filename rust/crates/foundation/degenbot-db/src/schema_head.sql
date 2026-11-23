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
CREATE TABLE IF NOT EXISTS aave_v3_block_anchors (
	chain_id INTEGER NOT NULL,
	block_number INTEGER NOT NULL,
	block_hash VARCHAR(66) NOT NULL,
	parent_hash VARCHAR(66) NOT NULL,
	PRIMARY KEY (chain_id, block_hash),
	UNIQUE (chain_id, block_hash, block_number, parent_hash),
	CHECK (block_number >= 0),
	CHECK (substr(block_hash, 1, 2) = '0x' AND length(block_hash) = 66),
	CHECK (substr(parent_hash, 1, 2) = '0x' AND length(parent_hash) = 66),
	CHECK (block_hash <> parent_hash)
);
CREATE TABLE IF NOT EXISTS aave_v3_checkpoint_assets (
	checkpoint_id INTEGER NOT NULL,
	asset_id INTEGER NOT NULL,
	underlying_address VARCHAR(42) NOT NULL,
	a_token_address VARCHAR(42) NOT NULL,
	v_token_address VARCHAR(42) NOT NULL,
	a_token_revision INTEGER NOT NULL,
	v_token_revision INTEGER NOT NULL,
	PRIMARY KEY (checkpoint_id, asset_id),
	FOREIGN KEY(checkpoint_id) REFERENCES aave_v3_projection_checkpoints (id),
	CHECK (length(underlying_address) > 0 AND length(a_token_address) > 0 AND length(v_token_address) > 0),
	CHECK (a_token_revision >= 0 AND v_token_revision >= 0)
);
CREATE TABLE IF NOT EXISTS aave_v3_checkpoint_contracts (
	checkpoint_id INTEGER NOT NULL,
	name TEXT NOT NULL,
	address VARCHAR(42) NOT NULL,
	revision INTEGER,
	PRIMARY KEY (checkpoint_id, name),
	FOREIGN KEY(checkpoint_id) REFERENCES aave_v3_projection_checkpoints (id),
	CHECK (length(name) > 0 AND length(address) > 0),
	CHECK (revision IS NULL OR revision >= 0)
);
CREATE TABLE IF NOT EXISTS aave_v3_checkpoint_generations (
	checkpoint_id INTEGER NOT NULL,
	market_id INTEGER NOT NULL,
	generation_id INTEGER NOT NULL,
	PRIMARY KEY (checkpoint_id, generation_id),
	FOREIGN KEY(checkpoint_id, market_id) REFERENCES aave_v3_projection_checkpoints (id, market_id),
	FOREIGN KEY(market_id, generation_id) REFERENCES aave_v3_generations (market_id, id)
);
CREATE TABLE IF NOT EXISTS aave_v3_checkpoint_gho (
	checkpoint_id INTEGER NOT NULL,
	gho_id INTEGER NOT NULL,
	token_id INTEGER NOT NULL,
	v_token_id INTEGER,
	gho_token_address VARCHAR(42) NOT NULL,
	v_token_address VARCHAR(42),
	discount_rate_strategy VARCHAR(42),
	discount_token VARCHAR(42),
	PRIMARY KEY (checkpoint_id),
	FOREIGN KEY(checkpoint_id) REFERENCES aave_v3_projection_checkpoints (id),
	CHECK (length(gho_token_address) > 0),
	CHECK ((v_token_id IS NULL) = (v_token_address IS NULL)),
	CHECK (discount_rate_strategy IS NULL OR length(discount_rate_strategy) > 0),
	CHECK (discount_token IS NULL OR length(discount_token) > 0)
);
CREATE TABLE IF NOT EXISTS aave_v3_checkpoint_positions (
	checkpoint_id INTEGER NOT NULL,
	position TEXT NOT NULL,
	position_id INTEGER NOT NULL,
	balance VARCHAR(78) NOT NULL,
	last_index VARCHAR(78),
	PRIMARY KEY (checkpoint_id, position, position_id),
	FOREIGN KEY(checkpoint_id) REFERENCES aave_v3_projection_checkpoints (id),
	CHECK (position IN ('collateral', 'debt')),
	CHECK (length(balance) > 0 AND (last_index IS NULL OR length(last_index) > 0))
);
CREATE TABLE IF NOT EXISTS aave_v3_checkpoint_users (
	checkpoint_id INTEGER NOT NULL,
	user_id INTEGER NOT NULL,
	address VARCHAR(42) NOT NULL,
	gho_discount INTEGER NOT NULL,
	stk_aave_balance VARCHAR(78),
	PRIMARY KEY (checkpoint_id, user_id),
	UNIQUE (checkpoint_id, address),
	FOREIGN KEY(checkpoint_id) REFERENCES aave_v3_projection_checkpoints (id),
	CHECK (length(address) > 0),
	CHECK (gho_discount >= 0),
	CHECK (stk_aave_balance IS NULL OR length(stk_aave_balance) > 0)
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
CREATE TABLE IF NOT EXISTS aave_v3_delta_journal (
	id INTEGER NOT NULL,
	market_id INTEGER NOT NULL,
	sequence INTEGER NOT NULL,
	position_ordinal INTEGER NOT NULL,
	chain_id INTEGER NOT NULL,
	block_number INTEGER NOT NULL,
	block_hash VARCHAR(66) NOT NULL,
	parent_hash VARCHAR(66) NOT NULL,
	tx_hash VARCHAR(66) NOT NULL,
	tx_index INTEGER NOT NULL,
	log_index INTEGER NOT NULL,
	generation_id INTEGER,
	encoding_version TEXT NOT NULL,
	delta_encoding TEXT NOT NULL,
	PRIMARY KEY (id),
	UNIQUE (market_id, sequence),
	UNIQUE (market_id, chain_id, tx_hash, log_index, position_ordinal),
	FOREIGN KEY (market_id) REFERENCES aave_v3_markets (id),
	FOREIGN KEY (chain_id, block_hash, block_number, parent_hash) REFERENCES aave_v3_block_anchors (chain_id, block_hash, block_number, parent_hash),
	FOREIGN KEY (chain_id, tx_hash, block_hash, tx_index) REFERENCES aave_v3_transaction_anchors (chain_id, tx_hash, block_hash, tx_index),
	FOREIGN KEY (market_id, generation_id) REFERENCES aave_v3_generations (market_id, id),
	CHECK (sequence > 0 AND position_ordinal > 0),
	CHECK (block_number >= 0 AND tx_index >= 0 AND log_index >= 0),
	CHECK (length(encoding_version) > 0 AND length(delta_encoding) > 0)
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
CREATE TABLE IF NOT EXISTS aave_v3_generations (
	id INTEGER NOT NULL,
	market_id INTEGER NOT NULL,
	entity TEXT NOT NULL,
	activation_kind TEXT NOT NULL,
	revision INTEGER,
	chain_id INTEGER,
	block_number INTEGER,
	block_hash VARCHAR(66),
	parent_hash VARCHAR(66),
	tx_hash VARCHAR(66),
	tx_index INTEGER,
	log_index INTEGER,
	PRIMARY KEY (id),
	UNIQUE (market_id, id),
	UNIQUE (market_id, entity, activation_kind, revision, chain_id, block_hash, tx_hash, log_index),
	FOREIGN KEY (market_id) REFERENCES aave_v3_markets (id),
	CHECK (activation_kind IN ('seed', 'event')),
	CHECK (length(entity) > 0),
	CHECK (revision IS NULL OR revision >= 0),
	CHECK ((activation_kind = 'seed' AND chain_id IS NULL AND block_number IS NULL AND block_hash IS NULL AND parent_hash IS NULL AND tx_hash IS NULL AND tx_index IS NULL AND log_index IS NULL) OR (activation_kind = 'event' AND chain_id IS NOT NULL AND block_number IS NOT NULL AND block_hash IS NOT NULL AND parent_hash IS NOT NULL AND tx_hash IS NOT NULL AND tx_index IS NOT NULL AND log_index IS NOT NULL))
);
CREATE TABLE IF NOT EXISTS aave_v3_markets (
	id INTEGER NOT NULL,
	chain_id INTEGER NOT NULL,
	name TEXT NOT NULL,
	active BOOLEAN NOT NULL,
	last_update_block INTEGER,
	PRIMARY KEY (id)
);
CREATE TABLE IF NOT EXISTS aave_v3_projection_checkpoints (
	id INTEGER NOT NULL,
	market_id INTEGER NOT NULL,
	journal_sequence INTEGER NOT NULL,
	chain_id INTEGER NOT NULL,
	block_number INTEGER NOT NULL,
	block_hash VARCHAR(66) NOT NULL,
	parent_hash VARCHAR(66) NOT NULL,
	tx_hash VARCHAR(66) NOT NULL,
	tx_index INTEGER NOT NULL,
	log_index INTEGER NOT NULL,
	range_end_block INTEGER NOT NULL,
	encoding_version TEXT NOT NULL,
	PRIMARY KEY (id),
	UNIQUE (id, market_id),
	UNIQUE (market_id, journal_sequence),
	FOREIGN KEY(market_id) REFERENCES aave_v3_markets (id),
	FOREIGN KEY(market_id, journal_sequence) REFERENCES aave_v3_delta_journal (market_id, sequence),
	FOREIGN KEY(chain_id, block_hash, block_number, parent_hash) REFERENCES aave_v3_block_anchors (chain_id, block_hash, block_number, parent_hash),
	FOREIGN KEY(chain_id, tx_hash, block_hash, tx_index) REFERENCES aave_v3_transaction_anchors (chain_id, tx_hash, block_hash, tx_index),
	CHECK (journal_sequence > 0),
	CHECK (block_number >= 0 AND tx_index >= 0 AND log_index >= 0),
	CHECK (range_end_block >= block_number),
	CHECK (substr(block_hash, 1, 2) = '0x' AND length(block_hash) = 66),
	CHECK (substr(parent_hash, 1, 2) = '0x' AND length(parent_hash) = 66),
	CHECK (substr(tx_hash, 1, 2) = '0x' AND length(tx_hash) = 66),
	CHECK (length(encoding_version) > 0)
);
CREATE TABLE IF NOT EXISTS aave_v3_projection_cursors (
	market_id INTEGER NOT NULL,
	chain_id INTEGER NOT NULL,
	block_number INTEGER NOT NULL,
	block_hash VARCHAR(66) NOT NULL,
	parent_hash VARCHAR(66) NOT NULL,
	tx_hash VARCHAR(66) NOT NULL,
	tx_index INTEGER NOT NULL,
	log_index INTEGER NOT NULL,
	journal_sequence INTEGER NOT NULL,
	range_end_block INTEGER NOT NULL,
	PRIMARY KEY (market_id),
	FOREIGN KEY (market_id) REFERENCES aave_v3_markets (id),
	FOREIGN KEY (chain_id, block_hash, block_number, parent_hash) REFERENCES aave_v3_block_anchors (chain_id, block_hash, block_number, parent_hash),
	FOREIGN KEY (chain_id, tx_hash, block_hash, tx_index) REFERENCES aave_v3_transaction_anchors (chain_id, tx_hash, block_hash, tx_index),
	FOREIGN KEY (market_id, journal_sequence) REFERENCES aave_v3_delta_journal (market_id, sequence),
	CHECK (journal_sequence > 0),
	CHECK (block_number >= 0 AND tx_index >= 0 AND log_index >= 0),
	CHECK (range_end_block >= block_number)
);
CREATE TABLE IF NOT EXISTS aave_v3_transaction_anchors (
	chain_id INTEGER NOT NULL,
	tx_hash VARCHAR(66) NOT NULL,
	block_hash VARCHAR(66) NOT NULL,
	tx_index INTEGER NOT NULL,
	PRIMARY KEY (chain_id, tx_hash),
	UNIQUE (chain_id, tx_hash, block_hash, tx_index),
	UNIQUE (chain_id, block_hash, tx_index),
	FOREIGN KEY (chain_id, block_hash) REFERENCES aave_v3_block_anchors (chain_id, block_hash),
	CHECK (tx_index >= 0),
	CHECK (substr(tx_hash, 1, 2) = '0x' AND length(tx_hash) = 66)
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
CREATE TABLE IF NOT EXISTS lfj_pools (
	pool_id INTEGER NOT NULL,
	bin_step INTEGER NOT NULL,
	PRIMARY KEY (pool_id),
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

CREATE UNIQUE INDEX IF NOT EXISTS ix_aave_asset_config_asset ON aave_v3_asset_configs (asset_id);
CREATE UNIQUE INDEX IF NOT EXISTS ix_aave_assets_underlying_asset_market ON aave_v3_assets (underlying_asset_id, market_id);
CREATE UNIQUE INDEX IF NOT EXISTS ix_aave_collateral_position_user_asset ON aave_v3_collateral_positions (user_id, asset_id);
CREATE UNIQUE INDEX IF NOT EXISTS ix_aave_debt_position_user_asset ON aave_v3_debt_positions (user_id, asset_id);
CREATE UNIQUE INDEX IF NOT EXISTS ix_aave_emode_category_market_cat ON aave_v3_emode_categories (market_id, category_id);
CREATE INDEX IF NOT EXISTS ix_aave_delta_journal_block ON aave_v3_delta_journal (market_id, block_number);
CREATE UNIQUE INDEX IF NOT EXISTS ix_aave_generations_event ON aave_v3_generations (market_id, entity, chain_id, tx_hash, log_index) WHERE activation_kind = 'event';
CREATE UNIQUE INDEX IF NOT EXISTS ix_aave_generations_seed ON aave_v3_generations (market_id, entity, COALESCE(revision, -1)) WHERE activation_kind = 'seed';
CREATE INDEX IF NOT EXISTS ix_aave_transaction_anchors_block ON aave_v3_transaction_anchors (chain_id, block_hash);
CREATE INDEX IF NOT EXISTS ix_aave_gho_tokens_token_id ON aave_gho_tokens (token_id);
CREATE INDEX IF NOT EXISTS ix_aave_gho_tokens_v_token_id ON aave_gho_tokens (v_token_id);
CREATE INDEX IF NOT EXISTS ix_aave_user_collateral_config_enabled ON aave_v3_user_collateral_configs (enabled);
CREATE UNIQUE INDEX IF NOT EXISTS ix_aave_user_collateral_config_user_asset ON aave_v3_user_collateral_configs (user_id, asset_id);
CREATE UNIQUE INDEX IF NOT EXISTS ix_aave_users_address_market ON aave_v3_users (address, market_id);
CREATE INDEX IF NOT EXISTS ix_aave_v3_asset_configs_asset_id ON aave_v3_asset_configs (asset_id);
CREATE INDEX IF NOT EXISTS ix_aave_v3_assets_a_token_id ON aave_v3_assets (a_token_id);
CREATE INDEX IF NOT EXISTS ix_aave_v3_assets_e_mode_category_id ON aave_v3_assets (e_mode_category_id);
CREATE INDEX IF NOT EXISTS ix_aave_v3_assets_market_id ON aave_v3_assets (market_id);
CREATE INDEX IF NOT EXISTS ix_aave_v3_assets_underlying_asset_id ON aave_v3_assets (underlying_asset_id);
CREATE INDEX IF NOT EXISTS ix_aave_v3_assets_v_token_id ON aave_v3_assets (v_token_id);
CREATE INDEX IF NOT EXISTS ix_aave_v3_collateral_positions_asset_id ON aave_v3_collateral_positions (asset_id);
CREATE INDEX IF NOT EXISTS ix_aave_v3_collateral_positions_user_id ON aave_v3_collateral_positions (user_id);
CREATE INDEX IF NOT EXISTS ix_aave_v3_contracts_market_id ON aave_v3_contracts (market_id);
CREATE INDEX IF NOT EXISTS ix_aave_v3_debt_positions_asset_id ON aave_v3_debt_positions (asset_id);
CREATE INDEX IF NOT EXISTS ix_aave_v3_debt_positions_user_id ON aave_v3_debt_positions (user_id);
CREATE INDEX IF NOT EXISTS ix_aave_v3_emode_categories_market_id ON aave_v3_emode_categories (market_id);
CREATE INDEX IF NOT EXISTS ix_aave_v3_user_collateral_configs_asset_id ON aave_v3_user_collateral_configs (asset_id);
CREATE INDEX IF NOT EXISTS ix_aave_v3_user_collateral_configs_user_id ON aave_v3_user_collateral_configs (user_id);
CREATE INDEX IF NOT EXISTS ix_aave_v3_users_isolation_mode_collateral_asset_id ON aave_v3_users (isolation_mode_collateral_asset_id);
CREATE INDEX IF NOT EXISTS ix_aave_v3_users_market_id ON aave_v3_users (market_id);
CREATE UNIQUE INDEX IF NOT EXISTS ix_erc20_tokens_address_chain ON erc20_tokens (address, chain);
CREATE INDEX IF NOT EXISTS ix_erc20_tokens_chain ON erc20_tokens (chain);
CREATE UNIQUE INDEX IF NOT EXISTS ix_liquidity_pool_address_chain ON pools (address, chain);
CREATE INDEX IF NOT EXISTS ix_liquidity_pools_token_ids ON pools (token0_id, token1_id);
CREATE UNIQUE INDEX IF NOT EXISTS ix_liquidity_positions_pool_id_tick ON liquidity_positions (pool_id, tick);
CREATE INDEX IF NOT EXISTS ix_managed_pool_liquidity_positions_managed_pool_id ON managed_pool_liquidity_positions (managed_pool_id);
CREATE UNIQUE INDEX IF NOT EXISTS ix_managed_pool_liquidity_positions_pool_id_tick ON managed_pool_liquidity_positions (managed_pool_id, tick);
CREATE INDEX IF NOT EXISTS ix_managed_pools_manager_id ON managed_pools (manager_id);
CREATE UNIQUE INDEX IF NOT EXISTS ix_pool_manager_address_chain ON pool_managers (address, chain);
CREATE INDEX IF NOT EXISTS ix_pools_token0_id ON pools (token0_id);
CREATE INDEX IF NOT EXISTS ix_pools_token1_id ON pools (token1_id);
CREATE INDEX IF NOT EXISTS ix_uniswap_v4_pools_currency0_id ON uniswap_v4_pools (currency0_id);
CREATE INDEX IF NOT EXISTS ix_uniswap_v4_pools_currency1_id ON uniswap_v4_pools (currency1_id);
CREATE INDEX IF NOT EXISTS ix_uniswap_v4_pools_pool_hash ON uniswap_v4_pools (pool_hash);
CREATE INDEX IF NOT EXISTS ix_uniswap_v4_pools_token_ids ON uniswap_v4_pools (currency0_id, currency1_id);
