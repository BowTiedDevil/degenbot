# ruff: noqa: E501

from typing import Any

import pydantic_core

# ref: https://raw.githubusercontent.com/curvefi/metaregistry/main/contracts/interfaces/CurvePool.json
CURVE_V1_POOL_ABI: list[Any] = pydantic_core.from_json(
    """
    [{"name":"TokenExchange","inputs":[{"type":"address","name":"buyer","indexed":true},{"type":"int128","name":"sold_id","indexed":false},{"type":"uint256","name":"tokens_sold","indexed":false},{"type":"int128","name":"bought_id","indexed":false},{"type":"uint256","name":"tokens_bought","indexed":false}],"anonymous":false,"type":"event"},{"name":"TokenExchangeUnderlying","inputs":[{"type":"address","name":"buyer","indexed":true},{"type":"int128","name":"sold_id","indexed":false},{"type":"uint256","name":"tokens_sold","indexed":false},{"type":"int128","name":"bought_id","indexed":false},{"type":"uint256","name":"tokens_bought","indexed":false}],"anonymous":false,"type":"event"},{"name":"AddLiquidity","inputs":[{"type":"address","name":"provider","indexed":true},{"type":"uint256[2]","name":"token_amounts","indexed":false},{"type":"uint256[2]","name":"fees","indexed":false},{"type":"uint256","name":"invariant","indexed":false},{"type":"uint256","name":"token_supply","indexed":false}],"anonymous":false,"type":"event"},{"name":"RemoveLiquidity","inputs":[{"type":"address","name":"provider","indexed":true},{"type":"uint256[2]","name":"token_amounts","indexed":false},{"type":"uint256[2]","name":"fees","indexed":false},{"type":"uint256","name":"token_supply","indexed":false}],"anonymous":false,"type":"event"},{"name":"RemoveLiquidityOne","inputs":[{"type":"address","name":"provider","indexed":true},{"type":"uint256","name":"token_amount","indexed":false},{"type":"uint256","name":"coin_amount","indexed":false},{"type":"uint256","name":"token_supply","indexed":false}],"anonymous":false,"type":"event"},{"name":"RemoveLiquidityImbalance","inputs":[{"type":"address","name":"provider","indexed":true},{"type":"uint256[2]","name":"token_amounts","indexed":false},{"type":"uint256[2]","name":"fees","indexed":false},{"type":"uint256","name":"invariant","indexed":false},{"type":"uint256","name":"token_supply","indexed":false}],"anonymous":false,"type":"event"},{"name":"CommitNewAdmin","inputs":[{"type":"uint256","name":"deadline","indexed":true},{"type":"address","name":"admin","indexed":true}],"anonymous":false,"type":"event"},{"name":"NewAdmin","inputs":[{"type":"address","name":"admin","indexed":true}],"anonymous":false,"type":"event"},{"name":"CommitNewFee","inputs":[{"type":"uint256","name":"deadline","indexed":true},{"type":"uint256","name":"fee","indexed":false},{"type":"uint256","name":"admin_fee","indexed":false}],"anonymous":false,"type":"event"},{"name":"NewFee","inputs":[{"type":"uint256","name":"fee","indexed":false},{"type":"uint256","name":"admin_fee","indexed":false}],"anonymous":false,"type":"event"},{"name":"RampA","inputs":[{"type":"uint256","name":"old_A","indexed":false},{"type":"uint256","name":"new_A","indexed":false},{"type":"uint256","name":"initial_time","indexed":false},{"type":"uint256","name":"future_time","indexed":false}],"anonymous":false,"type":"event"},{"name":"StopRampA","inputs":[{"type":"uint256","name":"A","indexed":false},{"type":"uint256","name":"t","indexed":false}],"anonymous":false,"type":"event"},{"outputs":[],"inputs":[{"type":"address","name":"_owner"},{"type":"address[2]","name":"_coins"},{"type":"address","name":"_pool_token"},{"type":"address","name":"_base_pool"},{"type":"uint256","name":"_A"},{"type":"uint256","name":"_fee"},{"type":"uint256","name":"_admin_fee"}],"stateMutability":"nonpayable","type":"constructor"},{"name":"A","outputs":[{"type":"uint256","name":""}],"inputs":[],"stateMutability":"view","type":"function","gas":5205},{"name":"A_precise","outputs":[{"type":"uint256","name":""}],"inputs":[],"stateMutability":"view","type":"function","gas":5167},{"name":"get_virtual_price","outputs":[{"type":"uint256","name":""}],"inputs":[],"stateMutability":"view","type":"function","gas":992854},{"name":"calc_token_amount","outputs":[{"type":"uint256","name":""}],"inputs":[{"type":"uint256[2]","name":"amounts"},{"type":"bool","name":"is_deposit"}],"stateMutability":"view","type":"function","gas":3939870},{"name":"add_liquidity","outputs":[{"type":"uint256","name":""}],"inputs":[{"type":"uint256[2]","name":"amounts"},{"type":"uint256","name":"min_mint_amount"}],"stateMutability":"nonpayable","type":"function","gas":6138492},{"name":"get_dy","outputs":[{"type":"uint256","name":""}],"inputs":[{"type":"int128","name":"i"},{"type":"int128","name":"j"},{"type":"uint256","name":"dx"}],"stateMutability":"view","type":"function","gas":2390368},{"name":"get_dy_underlying","outputs":[{"type":"uint256","name":""}],"inputs":[{"type":"int128","name":"i"},{"type":"int128","name":"j"},{"type":"uint256","name":"dx"}],"stateMutability":"view","type":"function","gas":2393485},{"name":"exchange","outputs":[{"type":"uint256","name":""}],"inputs":[{"type":"int128","name":"i"},{"type":"int128","name":"j"},{"type":"uint256","name":"dx"},{"type":"uint256","name":"min_dy"}],"stateMutability":"nonpayable","type":"function","gas":2617568},{"name":"exchange_underlying","outputs":[{"type":"uint256","name":""}],"inputs":[{"type":"int128","name":"i"},{"type":"int128","name":"j"},{"type":"uint256","name":"dx"},{"type":"uint256","name":"min_dy"}],"stateMutability":"nonpayable","type":"function","gas":2632475},{"name":"remove_liquidity","outputs":[{"type":"uint256[2]","name":""}],"inputs":[{"type":"uint256","name":"_amount"},{"type":"uint256[2]","name":"min_amounts"}],"stateMutability":"nonpayable","type":"function","gas":163289},{"name":"remove_liquidity_imbalance","outputs":[{"type":"uint256","name":""}],"inputs":[{"type":"uint256[2]","name":"amounts"},{"type":"uint256","name":"max_burn_amount"}],"stateMutability":"nonpayable","type":"function","gas":6138317},{"name":"calc_withdraw_one_coin","outputs":[{"type":"uint256","name":""}],"inputs":[{"type":"uint256","name":"_token_amount"},{"type":"int128","name":"i"}],"stateMutability":"view","type":"function","gas":4335},{"name":"remove_liquidity_one_coin","outputs":[{"type":"uint256","name":""}],"inputs":[{"type":"uint256","name":"_token_amount"},{"type":"int128","name":"i"},{"type":"uint256","name":"_min_amount"}],"stateMutability":"nonpayable","type":"function","gas":3827137},{"name":"ramp_A","outputs":[],"inputs":[{"type":"uint256","name":"_future_A"},{"type":"uint256","name":"_future_time"}],"stateMutability":"nonpayable","type":"function","gas":151906},{"name":"stop_ramp_A","outputs":[],"inputs":[],"stateMutability":"nonpayable","type":"function","gas":148667},{"name":"commit_new_fee","outputs":[],"inputs":[{"type":"uint256","name":"new_fee"},{"type":"uint256","name":"new_admin_fee"}],"stateMutability":"nonpayable","type":"function","gas":110491},{"name":"apply_new_fee","outputs":[],"inputs":[],"stateMutability":"nonpayable","type":"function","gas":97272},{"name":"revert_new_parameters","outputs":[],"inputs":[],"stateMutability":"nonpayable","type":"function","gas":21925},{"name":"commit_transfer_ownership","outputs":[],"inputs":[{"type":"address","name":"_owner"}],"stateMutability":"nonpayable","type":"function","gas":74663},{"name":"apply_transfer_ownership","outputs":[],"inputs":[],"stateMutability":"nonpayable","type":"function","gas":60740},{"name":"revert_transfer_ownership","outputs":[],"inputs":[],"stateMutability":"nonpayable","type":"function","gas":22015},{"name":"admin_balances","outputs":[{"type":"uint256","name":""}],"inputs":[{"type":"uint256","name":"i"}],"stateMutability":"view","type":"function","gas":3511},{"name":"withdraw_admin_fees","outputs":[],"inputs":[],"stateMutability":"nonpayable","type":"function","gas":9248},{"name":"donate_admin_fees","outputs":[],"inputs":[],"stateMutability":"nonpayable","type":"function","gas":74995},{"name":"kill_me","outputs":[],"inputs":[],"stateMutability":"nonpayable","type":"function","gas":38028},{"name":"unkill_me","outputs":[],"inputs":[],"stateMutability":"nonpayable","type":"function","gas":22165},{"name":"coins","outputs":[{"type":"address","name":""}],"inputs":[{"type":"uint256","name":"arg0"}],"stateMutability":"view","type":"function","gas":2250},{"name":"balances","outputs":[{"type":"uint256","name":""}],"inputs":[{"type":"uint256","name":"arg0"}],"stateMutability":"view","type":"function","gas":2280},{"name":"fee","outputs":[{"type":"uint256","name":""}],"inputs":[],"stateMutability":"view","type":"function","gas":2201},{"name":"admin_fee","outputs":[{"type":"uint256","name":""}],"inputs":[],"stateMutability":"view","type":"function","gas":2231},{"name":"owner","outputs":[{"type":"address","name":""}],"inputs":[],"stateMutability":"view","type":"function","gas":2261},{"name":"base_pool","outputs":[{"type":"address","name":""}],"inputs":[],"stateMutability":"view","type":"function","gas":2291},{"name":"base_virtual_price","outputs":[{"type":"uint256","name":""}],"inputs":[],"stateMutability":"view","type":"function","gas":2321},{"name":"base_cache_updated","outputs":[{"type":"uint256","name":""}],"inputs":[],"stateMutability":"view","type":"function","gas":2351},{"name":"base_coins","outputs":[{"type":"address","name":""}],"inputs":[{"type":"uint256","name":"arg0"}],"stateMutability":"view","type":"function","gas":2490},{"name":"initial_A","outputs":[{"type":"uint256","name":""}],"inputs":[],"stateMutability":"view","type":"function","gas":2411},{"name":"future_A","outputs":[{"type":"uint256","name":""}],"inputs":[],"stateMutability":"view","type":"function","gas":2441},{"name":"initial_A_time","outputs":[{"type":"uint256","name":""}],"inputs":[],"stateMutability":"view","type":"function","gas":2471},{"name":"future_A_time","outputs":[{"type":"uint256","name":""}],"inputs":[],"stateMutability":"view","type":"function","gas":2501},{"name":"admin_actions_deadline","outputs":[{"type":"uint256","name":""}],"inputs":[],"stateMutability":"view","type":"function","gas":2531},{"name":"transfer_ownership_deadline","outputs":[{"type":"uint256","name":""}],"inputs":[],"stateMutability":"view","type":"function","gas":2561},{"name":"future_fee","outputs":[{"type":"uint256","name":""}],"inputs":[],"stateMutability":"view","type":"function","gas":2591},{"name":"future_admin_fee","outputs":[{"type":"uint256","name":""}],"inputs":[],"stateMutability":"view","type":"function","gas":2621},{"name":"future_owner","outputs":[{"type":"address","name":""}],"inputs":[],"stateMutability":"view","type":"function","gas":2651}]
    """
)

# ref: https://raw.githubusercontent.com/curvefi/metaregistry/main/contracts/interfaces/CurvePoolV2.json
CURVE_V2_POOL_ABI: list[Any] = pydantic_core.from_json(
    """
    [
  {
    "name": "TokenExchange",
    "inputs": [
      { "name": "buyer", "type": "address", "indexed": true },
      { "name": "sold_id", "type": "uint256", "indexed": false },
      { "name": "tokens_sold", "type": "uint256", "indexed": false },
      { "name": "bought_id", "type": "uint256", "indexed": false },
      { "name": "tokens_bought", "type": "uint256", "indexed": false }
    ],
    "anonymous": false,
    "type": "event"
  },
  {
    "name": "AddLiquidity",
    "inputs": [
      { "name": "provider", "type": "address", "indexed": true },
      { "name": "token_amounts", "type": "uint256[2]", "indexed": false },
      { "name": "fee", "type": "uint256", "indexed": false },
      { "name": "token_supply", "type": "uint256", "indexed": false }
    ],
    "anonymous": false,
    "type": "event"
  },
  {
    "name": "RemoveLiquidity",
    "inputs": [
      { "name": "provider", "type": "address", "indexed": true },
      { "name": "token_amounts", "type": "uint256[2]", "indexed": false },
      { "name": "token_supply", "type": "uint256", "indexed": false }
    ],
    "anonymous": false,
    "type": "event"
  },
  {
    "name": "RemoveLiquidityOne",
    "inputs": [
      { "name": "provider", "type": "address", "indexed": true },
      { "name": "token_amount", "type": "uint256", "indexed": false },
      { "name": "coin_index", "type": "uint256", "indexed": false },
      { "name": "coin_amount", "type": "uint256", "indexed": false }
    ],
    "anonymous": false,
    "type": "event"
  },
  {
    "name": "CommitNewAdmin",
    "inputs": [
      { "name": "deadline", "type": "uint256", "indexed": true },
      { "name": "admin", "type": "address", "indexed": true }
    ],
    "anonymous": false,
    "type": "event"
  },
  {
    "name": "NewAdmin",
    "inputs": [{ "name": "admin", "type": "address", "indexed": true }],
    "anonymous": false,
    "type": "event"
  },
  {
    "name": "CommitNewParameters",
    "inputs": [
      { "name": "deadline", "type": "uint256", "indexed": true },
      { "name": "admin_fee", "type": "uint256", "indexed": false },
      { "name": "mid_fee", "type": "uint256", "indexed": false },
      { "name": "out_fee", "type": "uint256", "indexed": false },
      { "name": "fee_gamma", "type": "uint256", "indexed": false },
      { "name": "allowed_extra_profit", "type": "uint256", "indexed": false },
      { "name": "adjustment_step", "type": "uint256", "indexed": false },
      { "name": "ma_half_time", "type": "uint256", "indexed": false }
    ],
    "anonymous": false,
    "type": "event"
  },
  {
    "name": "NewParameters",
    "inputs": [
      { "name": "admin_fee", "type": "uint256", "indexed": false },
      { "name": "mid_fee", "type": "uint256", "indexed": false },
      { "name": "out_fee", "type": "uint256", "indexed": false },
      { "name": "fee_gamma", "type": "uint256", "indexed": false },
      { "name": "allowed_extra_profit", "type": "uint256", "indexed": false },
      { "name": "adjustment_step", "type": "uint256", "indexed": false },
      { "name": "ma_half_time", "type": "uint256", "indexed": false }
    ],
    "anonymous": false,
    "type": "event"
  },
  {
    "name": "RampAgamma",
    "inputs": [
      { "name": "initial_A", "type": "uint256", "indexed": false },
      { "name": "future_A", "type": "uint256", "indexed": false },
      { "name": "initial_gamma", "type": "uint256", "indexed": false },
      { "name": "future_gamma", "type": "uint256", "indexed": false },
      { "name": "initial_time", "type": "uint256", "indexed": false },
      { "name": "future_time", "type": "uint256", "indexed": false }
    ],
    "anonymous": false,
    "type": "event"
  },
  {
    "name": "StopRampA",
    "inputs": [
      { "name": "current_A", "type": "uint256", "indexed": false },
      { "name": "current_gamma", "type": "uint256", "indexed": false },
      { "name": "time", "type": "uint256", "indexed": false }
    ],
    "anonymous": false,
    "type": "event"
  },
  {
    "name": "ClaimAdminFee",
    "inputs": [
      { "name": "admin", "type": "address", "indexed": true },
      { "name": "tokens", "type": "uint256", "indexed": false }
    ],
    "anonymous": false,
    "type": "event"
  },
  {
    "stateMutability": "nonpayable",
    "type": "constructor",
    "inputs": [
      { "name": "owner", "type": "address" },
      { "name": "admin_fee_receiver", "type": "address" },
      { "name": "A", "type": "uint256" },
      { "name": "gamma", "type": "uint256" },
      { "name": "mid_fee", "type": "uint256" },
      { "name": "out_fee", "type": "uint256" },
      { "name": "allowed_extra_profit", "type": "uint256" },
      { "name": "fee_gamma", "type": "uint256" },
      { "name": "adjustment_step", "type": "uint256" },
      { "name": "admin_fee", "type": "uint256" },
      { "name": "ma_half_time", "type": "uint256" },
      { "name": "initial_price", "type": "uint256" }
    ],
    "outputs": []
  },
  { "stateMutability": "payable", "type": "fallback" },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "token",
    "inputs": [],
    "outputs": [{ "name": "", "type": "address" }],
    "gas": 516
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "coins",
    "inputs": [{ "name": "i", "type": "uint256" }],
    "outputs": [{ "name": "", "type": "address" }],
    "gas": 648
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "A",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 685
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "gamma",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 11789
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "fee",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 17633
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "get_virtual_price",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 364797
  },
  {
    "stateMutability": "payable",
    "type": "function",
    "name": "exchange",
    "inputs": [
      { "name": "i", "type": "uint256" },
      { "name": "j", "type": "uint256" },
      { "name": "dx", "type": "uint256" },
      { "name": "min_dy", "type": "uint256" }
    ],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 16775598
  },
  {
    "stateMutability": "payable",
    "type": "function",
    "name": "exchange",
    "inputs": [
      { "name": "i", "type": "uint256" },
      { "name": "j", "type": "uint256" },
      { "name": "dx", "type": "uint256" },
      { "name": "min_dy", "type": "uint256" },
      { "name": "use_eth", "type": "bool" }
    ],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 16775598
  },
  {
    "stateMutability": "payable",
    "type": "function",
    "name": "exchange_underlying",
    "inputs": [
      { "name": "i", "type": "uint256" },
      { "name": "j", "type": "uint256" },
      { "name": "dx", "type": "uint256" },
      { "name": "min_dy", "type": "uint256" }
    ],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 16775396
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "get_dy",
    "inputs": [
      { "name": "i", "type": "uint256" },
      { "name": "j", "type": "uint256" },
      { "name": "dx", "type": "uint256" }
    ],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 4577515
  },
  {
    "stateMutability": "payable",
    "type": "function",
    "name": "add_liquidity",
    "inputs": [
      { "name": "amounts", "type": "uint256[2]" },
      { "name": "min_mint_amount", "type": "uint256" }
    ],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 17694821
  },
  {
    "stateMutability": "payable",
    "type": "function",
    "name": "add_liquidity",
    "inputs": [
      { "name": "amounts", "type": "uint256[2]" },
      { "name": "min_mint_amount", "type": "uint256" },
      { "name": "use_eth", "type": "bool" }
    ],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 17694821
  },
  {
    "stateMutability": "nonpayable",
    "type": "function",
    "name": "remove_liquidity",
    "inputs": [
      { "name": "_amount", "type": "uint256" },
      { "name": "min_amounts", "type": "uint256[2]" }
    ],
    "outputs": [],
    "gas": 263729
  },
  {
    "stateMutability": "nonpayable",
    "type": "function",
    "name": "remove_liquidity",
    "inputs": [
      { "name": "_amount", "type": "uint256" },
      { "name": "min_amounts", "type": "uint256[2]" },
      { "name": "use_eth", "type": "bool" }
    ],
    "outputs": [],
    "gas": 263729
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "calc_token_amount",
    "inputs": [{ "name": "amounts", "type": "uint256[2]" }],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 5200947
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "calc_withdraw_one_coin",
    "inputs": [
      { "name": "token_amount", "type": "uint256" },
      { "name": "i", "type": "uint256" }
    ],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 12584
  },
  {
    "stateMutability": "nonpayable",
    "type": "function",
    "name": "remove_liquidity_one_coin",
    "inputs": [
      { "name": "token_amount", "type": "uint256" },
      { "name": "i", "type": "uint256" },
      { "name": "min_amount", "type": "uint256" }
    ],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 16702178
  },
  {
    "stateMutability": "nonpayable",
    "type": "function",
    "name": "remove_liquidity_one_coin",
    "inputs": [
      { "name": "token_amount", "type": "uint256" },
      { "name": "i", "type": "uint256" },
      { "name": "min_amount", "type": "uint256" },
      { "name": "use_eth", "type": "bool" }
    ],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 16702178
  },
  {
    "stateMutability": "nonpayable",
    "type": "function",
    "name": "claim_admin_fees",
    "inputs": [],
    "outputs": [],
    "gas": 3250985
  },
  {
    "stateMutability": "nonpayable",
    "type": "function",
    "name": "ramp_A_gamma",
    "inputs": [
      { "name": "future_A", "type": "uint256" },
      { "name": "future_gamma", "type": "uint256" },
      { "name": "future_time", "type": "uint256" }
    ],
    "outputs": [],
    "gas": 161698
  },
  {
    "stateMutability": "nonpayable",
    "type": "function",
    "name": "stop_ramp_A_gamma",
    "inputs": [],
    "outputs": [],
    "gas": 156743
  },
  {
    "stateMutability": "nonpayable",
    "type": "function",
    "name": "commit_new_parameters",
    "inputs": [
      { "name": "_new_mid_fee", "type": "uint256" },
      { "name": "_new_out_fee", "type": "uint256" },
      { "name": "_new_admin_fee", "type": "uint256" },
      { "name": "_new_fee_gamma", "type": "uint256" },
      { "name": "_new_allowed_extra_profit", "type": "uint256" },
      { "name": "_new_adjustment_step", "type": "uint256" },
      { "name": "_new_ma_half_time", "type": "uint256" }
    ],
    "outputs": [],
    "gas": 305084
  },
  {
    "stateMutability": "nonpayable",
    "type": "function",
    "name": "apply_new_parameters",
    "inputs": [],
    "outputs": [],
    "gas": 3543175
  },
  {
    "stateMutability": "nonpayable",
    "type": "function",
    "name": "revert_new_parameters",
    "inputs": [],
    "outputs": [],
    "gas": 23142
  },
  {
    "stateMutability": "nonpayable",
    "type": "function",
    "name": "commit_transfer_ownership",
    "inputs": [{ "name": "_owner", "type": "address" }],
    "outputs": [],
    "gas": 78827
  },
  {
    "stateMutability": "nonpayable",
    "type": "function",
    "name": "apply_transfer_ownership",
    "inputs": [],
    "outputs": [],
    "gas": 67042
  },
  {
    "stateMutability": "nonpayable",
    "type": "function",
    "name": "revert_transfer_ownership",
    "inputs": [],
    "outputs": [],
    "gas": 23232
  },
  { "stateMutability": "nonpayable", "type": "function", "name": "kill_me", "inputs": [], "outputs": [], "gas": 40455 },
  {
    "stateMutability": "nonpayable",
    "type": "function",
    "name": "unkill_me",
    "inputs": [],
    "outputs": [],
    "gas": 23292
  },
  {
    "stateMutability": "nonpayable",
    "type": "function",
    "name": "set_admin_fee_receiver",
    "inputs": [{ "name": "_admin_fee_receiver", "type": "address" }],
    "outputs": [],
    "gas": 38482
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "lp_price",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 217046
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "price_scale",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 3426
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "price_oracle",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 3456
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "last_prices",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 3486
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "last_prices_timestamp",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 3516
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "initial_A_gamma",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 3546
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "future_A_gamma",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 3576
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "initial_A_gamma_time",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 3606
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "future_A_gamma_time",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 3636
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "allowed_extra_profit",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 3666
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "future_allowed_extra_profit",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 3696
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "fee_gamma",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 3726
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "future_fee_gamma",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 3756
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "adjustment_step",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 3786
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "future_adjustment_step",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 3816
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "ma_half_time",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 3846
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "future_ma_half_time",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 3876
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "mid_fee",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 3906
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "out_fee",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 3936
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "admin_fee",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 3966
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "future_mid_fee",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 3996
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "future_out_fee",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 4026
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "future_admin_fee",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 4056
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "balances",
    "inputs": [{ "name": "arg0", "type": "uint256" }],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 4131
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "D",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 4116
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "owner",
    "inputs": [],
    "outputs": [{ "name": "", "type": "address" }],
    "gas": 4146
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "future_owner",
    "inputs": [],
    "outputs": [{ "name": "", "type": "address" }],
    "gas": 4176
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "xcp_profit",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 4206
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "xcp_profit_a",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 4236
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "virtual_price",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 4266
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "is_killed",
    "inputs": [],
    "outputs": [{ "name": "", "type": "bool" }],
    "gas": 4296
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "kill_deadline",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 4326
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "transfer_ownership_deadline",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 4356
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "admin_actions_deadline",
    "inputs": [],
    "outputs": [{ "name": "", "type": "uint256" }],
    "gas": 4386
  },
  {
    "stateMutability": "view",
    "type": "function",
    "name": "admin_fee_receiver",
    "inputs": [],
    "outputs": [{ "name": "", "type": "address" }],
    "gas": 4416
  }
]
    """
)

CURVE_METAREGISTRY_ABI: list[Any] = pydantic_core.from_json(
    """
    [{"name":"CommitNewAdmin","inputs":[{"name":"deadline","type":"uint256","indexed":true},{"name":"admin","type":"address","indexed":true}],"anonymous":false,"type":"event"},{"name":"NewAdmin","inputs":[{"name":"admin","type":"address","indexed":true}],"anonymous":false,"type":"event"},{"stateMutability":"nonpayable","type":"constructor","inputs":[{"name":"_address_provider","type":"address"}],"outputs":[]},{"stateMutability":"nonpayable","type":"function","name":"add_registry_handler","inputs":[{"name":"_registry_handler","type":"address"}],"outputs":[]},{"stateMutability":"nonpayable","type":"function","name":"update_registry_handler","inputs":[{"name":"_index","type":"uint256"},{"name":"_registry_handler","type":"address"}],"outputs":[]},{"stateMutability":"view","type":"function","name":"get_registry_handlers_from_pool","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"address[10]"}]},{"stateMutability":"view","type":"function","name":"get_base_registry","inputs":[{"name":"registry_handler","type":"address"}],"outputs":[{"name":"","type":"address"}]},{"stateMutability":"view","type":"function","name":"find_pool_for_coins","inputs":[{"name":"_from","type":"address"},{"name":"_to","type":"address"}],"outputs":[{"name":"","type":"address"}]},{"stateMutability":"view","type":"function","name":"find_pool_for_coins","inputs":[{"name":"_from","type":"address"},{"name":"_to","type":"address"},{"name":"i","type":"uint256"}],"outputs":[{"name":"","type":"address"}]},{"stateMutability":"view","type":"function","name":"find_pools_for_coins","inputs":[{"name":"_from","type":"address"},{"name":"_to","type":"address"}],"outputs":[{"name":"","type":"address[]"}]},{"stateMutability":"view","type":"function","name":"get_admin_balances","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256[8]"}]},{"stateMutability":"view","type":"function","name":"get_admin_balances","inputs":[{"name":"_pool","type":"address"},{"name":"_handler_id","type":"uint256"}],"outputs":[{"name":"","type":"uint256[8]"}]},{"stateMutability":"view","type":"function","name":"get_balances","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256[8]"}]},{"stateMutability":"view","type":"function","name":"get_balances","inputs":[{"name":"_pool","type":"address"},{"name":"_handler_id","type":"uint256"}],"outputs":[{"name":"","type":"uint256[8]"}]},{"stateMutability":"view","type":"function","name":"get_base_pool","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"address"}]},{"stateMutability":"view","type":"function","name":"get_base_pool","inputs":[{"name":"_pool","type":"address"},{"name":"_handler_id","type":"uint256"}],"outputs":[{"name":"","type":"address"}]},{"stateMutability":"view","type":"function","name":"get_coin_indices","inputs":[{"name":"_pool","type":"address"},{"name":"_from","type":"address"},{"name":"_to","type":"address"}],"outputs":[{"name":"","type":"int128"},{"name":"","type":"int128"},{"name":"","type":"bool"}]},{"stateMutability":"view","type":"function","name":"get_coin_indices","inputs":[{"name":"_pool","type":"address"},{"name":"_from","type":"address"},{"name":"_to","type":"address"},{"name":"_handler_id","type":"uint256"}],"outputs":[{"name":"","type":"int128"},{"name":"","type":"int128"},{"name":"","type":"bool"}]},{"stateMutability":"view","type":"function","name":"get_coins","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"address[8]"}]},{"stateMutability":"view","type":"function","name":"get_coins","inputs":[{"name":"_pool","type":"address"},{"name":"_handler_id","type":"uint256"}],"outputs":[{"name":"","type":"address[8]"}]},{"stateMutability":"view","type":"function","name":"get_decimals","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256[8]"}]},{"stateMutability":"view","type":"function","name":"get_decimals","inputs":[{"name":"_pool","type":"address"},{"name":"_handler_id","type":"uint256"}],"outputs":[{"name":"","type":"uint256[8]"}]},{"stateMutability":"view","type":"function","name":"get_fees","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256[10]"}]},{"stateMutability":"view","type":"function","name":"get_fees","inputs":[{"name":"_pool","type":"address"},{"name":"_handler_id","type":"uint256"}],"outputs":[{"name":"","type":"uint256[10]"}]},{"stateMutability":"view","type":"function","name":"get_gauge","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"address"}]},{"stateMutability":"view","type":"function","name":"get_gauge","inputs":[{"name":"_pool","type":"address"},{"name":"gauge_idx","type":"uint256"}],"outputs":[{"name":"","type":"address"}]},{"stateMutability":"view","type":"function","name":"get_gauge","inputs":[{"name":"_pool","type":"address"},{"name":"gauge_idx","type":"uint256"},{"name":"_handler_id","type":"uint256"}],"outputs":[{"name":"","type":"address"}]},{"stateMutability":"view","type":"function","name":"get_gauge_type","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"int128"}]},{"stateMutability":"view","type":"function","name":"get_gauge_type","inputs":[{"name":"_pool","type":"address"},{"name":"gauge_idx","type":"uint256"}],"outputs":[{"name":"","type":"int128"}]},{"stateMutability":"view","type":"function","name":"get_gauge_type","inputs":[{"name":"_pool","type":"address"},{"name":"gauge_idx","type":"uint256"},{"name":"_handler_id","type":"uint256"}],"outputs":[{"name":"","type":"int128"}]},{"stateMutability":"view","type":"function","name":"get_lp_token","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"address"}]},{"stateMutability":"view","type":"function","name":"get_lp_token","inputs":[{"name":"_pool","type":"address"},{"name":"_handler_id","type":"uint256"}],"outputs":[{"name":"","type":"address"}]},{"stateMutability":"view","type":"function","name":"get_n_coins","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256"}]},{"stateMutability":"view","type":"function","name":"get_n_coins","inputs":[{"name":"_pool","type":"address"},{"name":"_handler_id","type":"uint256"}],"outputs":[{"name":"","type":"uint256"}]},{"stateMutability":"view","type":"function","name":"get_n_underlying_coins","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256"}]},{"stateMutability":"view","type":"function","name":"get_n_underlying_coins","inputs":[{"name":"_pool","type":"address"},{"name":"_handler_id","type":"uint256"}],"outputs":[{"name":"","type":"uint256"}]},{"stateMutability":"view","type":"function","name":"get_pool_asset_type","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256"}]},{"stateMutability":"view","type":"function","name":"get_pool_asset_type","inputs":[{"name":"_pool","type":"address"},{"name":"_handler_id","type":"uint256"}],"outputs":[{"name":"","type":"uint256"}]},{"stateMutability":"view","type":"function","name":"get_pool_from_lp_token","inputs":[{"name":"_token","type":"address"}],"outputs":[{"name":"","type":"address"}]},{"stateMutability":"view","type":"function","name":"get_pool_from_lp_token","inputs":[{"name":"_token","type":"address"},{"name":"_handler_id","type":"uint256"}],"outputs":[{"name":"","type":"address"}]},{"stateMutability":"view","type":"function","name":"get_pool_params","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256[20]"}]},{"stateMutability":"view","type":"function","name":"get_pool_params","inputs":[{"name":"_pool","type":"address"},{"name":"_handler_id","type":"uint256"}],"outputs":[{"name":"","type":"uint256[20]"}]},{"stateMutability":"view","type":"function","name":"get_pool_name","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"string"}]},{"stateMutability":"view","type":"function","name":"get_pool_name","inputs":[{"name":"_pool","type":"address"},{"name":"_handler_id","type":"uint256"}],"outputs":[{"name":"","type":"string"}]},{"stateMutability":"view","type":"function","name":"get_underlying_balances","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256[8]"}]},{"stateMutability":"view","type":"function","name":"get_underlying_balances","inputs":[{"name":"_pool","type":"address"},{"name":"_handler_id","type":"uint256"}],"outputs":[{"name":"","type":"uint256[8]"}]},{"stateMutability":"view","type":"function","name":"get_underlying_coins","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"address[8]"}]},{"stateMutability":"view","type":"function","name":"get_underlying_coins","inputs":[{"name":"_pool","type":"address"},{"name":"_handler_id","type":"uint256"}],"outputs":[{"name":"","type":"address[8]"}]},{"stateMutability":"view","type":"function","name":"get_underlying_decimals","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256[8]"}]},{"stateMutability":"view","type":"function","name":"get_underlying_decimals","inputs":[{"name":"_pool","type":"address"},{"name":"_handler_id","type":"uint256"}],"outputs":[{"name":"","type":"uint256[8]"}]},{"stateMutability":"view","type":"function","name":"get_virtual_price_from_lp_token","inputs":[{"name":"_token","type":"address"}],"outputs":[{"name":"","type":"uint256"}]},{"stateMutability":"view","type":"function","name":"get_virtual_price_from_lp_token","inputs":[{"name":"_token","type":"address"},{"name":"_handler_id","type":"uint256"}],"outputs":[{"name":"","type":"uint256"}]},{"stateMutability":"view","type":"function","name":"is_meta","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"bool"}]},{"stateMutability":"view","type":"function","name":"is_meta","inputs":[{"name":"_pool","type":"address"},{"name":"_handler_id","type":"uint256"}],"outputs":[{"name":"","type":"bool"}]},{"stateMutability":"view","type":"function","name":"is_registered","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"bool"}]},{"stateMutability":"view","type":"function","name":"is_registered","inputs":[{"name":"_pool","type":"address"},{"name":"_handler_id","type":"uint256"}],"outputs":[{"name":"","type":"bool"}]},{"stateMutability":"view","type":"function","name":"pool_count","inputs":[],"outputs":[{"name":"","type":"uint256"}]},{"stateMutability":"view","type":"function","name":"pool_list","inputs":[{"name":"_index","type":"uint256"}],"outputs":[{"name":"","type":"address"}]},{"stateMutability":"view","type":"function","name":"address_provider","inputs":[],"outputs":[{"name":"","type":"address"}]},{"stateMutability":"view","type":"function","name":"owner","inputs":[],"outputs":[{"name":"","type":"address"}]},{"stateMutability":"view","type":"function","name":"get_registry","inputs":[{"name":"arg0","type":"uint256"}],"outputs":[{"name":"","type":"address"}]},{"stateMutability":"view","type":"function","name":"registry_length","inputs":[],"outputs":[{"name":"","type":"uint256"}]}]
    """
)

CURVE_V1_REGISTRY_ABI: list[Any] = pydantic_core.from_json(
    """
    [{"name":"PoolAdded","inputs":[{"name":"pool","type":"address","indexed":true},{"name":"rate_method_id","type":"bytes","indexed":false}],"anonymous":false,"type":"event"},{"name":"PoolRemoved","inputs":[{"name":"pool","type":"address","indexed":true}],"anonymous":false,"type":"event"},{"stateMutability":"nonpayable","type":"constructor","inputs":[{"name":"_address_provider","type":"address"},{"name":"_gauge_controller","type":"address"}],"outputs":[]},{"stateMutability":"view","type":"function","name":"find_pool_for_coins","inputs":[{"name":"_from","type":"address"},{"name":"_to","type":"address"}],"outputs":[{"name":"","type":"address"}]},{"stateMutability":"view","type":"function","name":"find_pool_for_coins","inputs":[{"name":"_from","type":"address"},{"name":"_to","type":"address"},{"name":"i","type":"uint256"}],"outputs":[{"name":"","type":"address"}]},{"stateMutability":"view","type":"function","name":"get_n_coins","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256[2]"}],"gas":1521},{"stateMutability":"view","type":"function","name":"get_coins","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"address[8]"}],"gas":12102},{"stateMutability":"view","type":"function","name":"get_underlying_coins","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"address[8]"}],"gas":12194},{"stateMutability":"view","type":"function","name":"get_decimals","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256[8]"}],"gas":7874},{"stateMutability":"view","type":"function","name":"get_underlying_decimals","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256[8]"}],"gas":7966},{"stateMutability":"view","type":"function","name":"get_rates","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256[8]"}],"gas":36992},{"stateMutability":"view","type":"function","name":"get_gauges","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"address[10]"},{"name":"","type":"int128[10]"}],"gas":20157},{"stateMutability":"view","type":"function","name":"get_balances","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256[8]"}],"gas":16583},{"stateMutability":"view","type":"function","name":"get_underlying_balances","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256[8]"}],"gas":162842},{"stateMutability":"view","type":"function","name":"get_virtual_price_from_lp_token","inputs":[{"name":"_token","type":"address"}],"outputs":[{"name":"","type":"uint256"}],"gas":1927},{"stateMutability":"view","type":"function","name":"get_A","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256"}],"gas":1045},{"stateMutability":"view","type":"function","name":"get_parameters","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"A","type":"uint256"},{"name":"future_A","type":"uint256"},{"name":"fee","type":"uint256"},{"name":"admin_fee","type":"uint256"},{"name":"future_fee","type":"uint256"},{"name":"future_admin_fee","type":"uint256"},{"name":"future_owner","type":"address"},{"name":"initial_A","type":"uint256"},{"name":"initial_A_time","type":"uint256"},{"name":"future_A_time","type":"uint256"}],"gas":6305},{"stateMutability":"view","type":"function","name":"get_fees","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256[2]"}],"gas":1450},{"stateMutability":"view","type":"function","name":"get_admin_balances","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256[8]"}],"gas":36454},{"stateMutability":"view","type":"function","name":"get_coin_indices","inputs":[{"name":"_pool","type":"address"},{"name":"_from","type":"address"},{"name":"_to","type":"address"}],"outputs":[{"name":"","type":"int128"},{"name":"","type":"int128"},{"name":"","type":"bool"}],"gas":27131},{"stateMutability":"view","type":"function","name":"estimate_gas_used","inputs":[{"name":"_pool","type":"address"},{"name":"_from","type":"address"},{"name":"_to","type":"address"}],"outputs":[{"name":"","type":"uint256"}],"gas":32004},{"stateMutability":"view","type":"function","name":"is_meta","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"bool"}],"gas":1900},{"stateMutability":"view","type":"function","name":"get_pool_name","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"string"}],"gas":8323},{"stateMutability":"view","type":"function","name":"get_coin_swap_count","inputs":[{"name":"_coin","type":"address"}],"outputs":[{"name":"","type":"uint256"}],"gas":1951},{"stateMutability":"view","type":"function","name":"get_coin_swap_complement","inputs":[{"name":"_coin","type":"address"},{"name":"_index","type":"uint256"}],"outputs":[{"name":"","type":"address"}],"gas":2090},{"stateMutability":"view","type":"function","name":"get_pool_asset_type","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256"}],"gas":2011},{"stateMutability":"nonpayable","type":"function","name":"add_pool","inputs":[{"name":"_pool","type":"address"},{"name":"_n_coins","type":"uint256"},{"name":"_lp_token","type":"address"},{"name":"_rate_info","type":"bytes32"},{"name":"_decimals","type":"uint256"},{"name":"_underlying_decimals","type":"uint256"},{"name":"_has_initial_A","type":"bool"},{"name":"_is_v1","type":"bool"},{"name":"_name","type":"string"}],"outputs":[],"gas":61485845},{"stateMutability":"nonpayable","type":"function","name":"add_pool_without_underlying","inputs":[{"name":"_pool","type":"address"},{"name":"_n_coins","type":"uint256"},{"name":"_lp_token","type":"address"},{"name":"_rate_info","type":"bytes32"},{"name":"_decimals","type":"uint256"},{"name":"_use_rates","type":"uint256"},{"name":"_has_initial_A","type":"bool"},{"name":"_is_v1","type":"bool"},{"name":"_name","type":"string"}],"outputs":[],"gas":31306062},{"stateMutability":"nonpayable","type":"function","name":"add_metapool","inputs":[{"name":"_pool","type":"address"},{"name":"_n_coins","type":"uint256"},{"name":"_lp_token","type":"address"},{"name":"_decimals","type":"uint256"},{"name":"_name","type":"string"}],"outputs":[]},{"stateMutability":"nonpayable","type":"function","name":"add_metapool","inputs":[{"name":"_pool","type":"address"},{"name":"_n_coins","type":"uint256"},{"name":"_lp_token","type":"address"},{"name":"_decimals","type":"uint256"},{"name":"_name","type":"string"},{"name":"_base_pool","type":"address"}],"outputs":[]},{"stateMutability":"nonpayable","type":"function","name":"remove_pool","inputs":[{"name":"_pool","type":"address"}],"outputs":[],"gas":779731418758},{"stateMutability":"nonpayable","type":"function","name":"set_pool_gas_estimates","inputs":[{"name":"_addr","type":"address[5]"},{"name":"_amount","type":"uint256[2][5]"}],"outputs":[],"gas":390460},{"stateMutability":"nonpayable","type":"function","name":"set_coin_gas_estimates","inputs":[{"name":"_addr","type":"address[10]"},{"name":"_amount","type":"uint256[10]"}],"outputs":[],"gas":392047},{"stateMutability":"nonpayable","type":"function","name":"set_gas_estimate_contract","inputs":[{"name":"_pool","type":"address"},{"name":"_estimator","type":"address"}],"outputs":[],"gas":72629},{"stateMutability":"nonpayable","type":"function","name":"set_liquidity_gauges","inputs":[{"name":"_pool","type":"address"},{"name":"_liquidity_gauges","type":"address[10]"}],"outputs":[],"gas":400675},{"stateMutability":"nonpayable","type":"function","name":"set_pool_asset_type","inputs":[{"name":"_pool","type":"address"},{"name":"_asset_type","type":"uint256"}],"outputs":[],"gas":72667},{"stateMutability":"nonpayable","type":"function","name":"batch_set_pool_asset_type","inputs":[{"name":"_pools","type":"address[32]"},{"name":"_asset_types","type":"uint256[32]"}],"outputs":[],"gas":1173447},{"stateMutability":"view","type":"function","name":"address_provider","inputs":[],"outputs":[{"name":"","type":"address"}],"gas":2048},{"stateMutability":"view","type":"function","name":"gauge_controller","inputs":[],"outputs":[{"name":"","type":"address"}],"gas":2078},{"stateMutability":"view","type":"function","name":"pool_list","inputs":[{"name":"arg0","type":"uint256"}],"outputs":[{"name":"","type":"address"}],"gas":2217},{"stateMutability":"view","type":"function","name":"pool_count","inputs":[],"outputs":[{"name":"","type":"uint256"}],"gas":2138},{"stateMutability":"view","type":"function","name":"coin_count","inputs":[],"outputs":[{"name":"","type":"uint256"}],"gas":2168},{"stateMutability":"view","type":"function","name":"get_coin","inputs":[{"name":"arg0","type":"uint256"}],"outputs":[{"name":"","type":"address"}],"gas":2307},{"stateMutability":"view","type":"function","name":"get_pool_from_lp_token","inputs":[{"name":"arg0","type":"address"}],"outputs":[{"name":"","type":"address"}],"gas":2443},{"stateMutability":"view","type":"function","name":"get_lp_token","inputs":[{"name":"arg0","type":"address"}],"outputs":[{"name":"","type":"address"}],"gas":2473},{"stateMutability":"view","type":"function","name":"last_updated","inputs":[],"outputs":[{"name":"","type":"uint256"}],"gas":2288}]
    """
)

CURVE_V1_FACTORY_ABI: list[Any] = pydantic_core.from_json(
    """
  [{"stateMutability":"nonpayable","type":"constructor","inputs":[{"name":"_registry_address","type":"address"},{"name":"_base_pool_registry","type":"address"}],"outputs":[]},{"stateMutability":"view","type":"function","name":"find_pool_for_coins","inputs":[{"name":"_from","type":"address"},{"name":"_to","type":"address"}],"outputs":[{"name":"","type":"address"}]},{"stateMutability":"view","type":"function","name":"find_pool_for_coins","inputs":[{"name":"_from","type":"address"},{"name":"_to","type":"address"},{"name":"i","type":"uint256"}],"outputs":[{"name":"","type":"address"}]},{"stateMutability":"view","type":"function","name":"get_admin_balances","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256[8]"}]},{"stateMutability":"view","type":"function","name":"get_balances","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256[8]"}]},{"stateMutability":"view","type":"function","name":"get_base_pool","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"address"}]},{"stateMutability":"view","type":"function","name":"get_coin_indices","inputs":[{"name":"_pool","type":"address"},{"name":"_from","type":"address"},{"name":"_to","type":"address"}],"outputs":[{"name":"","type":"int128"},{"name":"","type":"int128"},{"name":"","type":"bool"}]},{"stateMutability":"view","type":"function","name":"get_coins","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"address[8]"}]},{"stateMutability":"view","type":"function","name":"get_decimals","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256[8]"}]},{"stateMutability":"view","type":"function","name":"get_fees","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256[10]"}]},{"stateMutability":"view","type":"function","name":"get_virtual_price_from_lp_token","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256"}]},{"stateMutability":"view","type":"function","name":"get_gauges","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"address[10]"},{"name":"","type":"int128[10]"}]},{"stateMutability":"view","type":"function","name":"get_lp_token","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"address"}]},{"stateMutability":"view","type":"function","name":"get_n_coins","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256"}]},{"stateMutability":"view","type":"function","name":"get_n_underlying_coins","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256"}]},{"stateMutability":"view","type":"function","name":"get_pool_asset_type","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256"}]},{"stateMutability":"view","type":"function","name":"get_pool_from_lp_token","inputs":[{"name":"_lp_token","type":"address"}],"outputs":[{"name":"","type":"address"}]},{"stateMutability":"view","type":"function","name":"get_pool_name","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"string"}]},{"stateMutability":"view","type":"function","name":"get_pool_params","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256[20]"}]},{"stateMutability":"view","type":"function","name":"get_underlying_balances","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256[8]"}]},{"stateMutability":"view","type":"function","name":"get_underlying_coins","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"address[8]"}]},{"stateMutability":"view","type":"function","name":"get_underlying_decimals","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"uint256[8]"}]},{"stateMutability":"view","type":"function","name":"is_meta","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"bool"}]},{"stateMutability":"view","type":"function","name":"is_registered","inputs":[{"name":"_pool","type":"address"}],"outputs":[{"name":"","type":"bool"}]},{"stateMutability":"view","type":"function","name":"pool_count","inputs":[],"outputs":[{"name":"","type":"uint256"}]},{"stateMutability":"view","type":"function","name":"pool_list","inputs":[{"name":"_index","type":"uint256"}],"outputs":[{"name":"","type":"address"}]},{"stateMutability":"view","type":"function","name":"base_registry","inputs":[],"outputs":[{"name":"","type":"address"}]},{"stateMutability":"view","type":"function","name":"base_pool_registry","inputs":[],"outputs":[{"name":"","type":"address"}]}]
  """
)
