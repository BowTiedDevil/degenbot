// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// Shared minimal full-ERC20 for the executor grammar harness (UQOAHA): one
/// token contract so V2 + V3 pools share intermediate-token custody.
/// Minimal full-ERC20 for the executor grammar harness (UQOAHA).
///
/// Supplies exactly what the real `cmd_executor` + a Uniswap V2 pair read:
/// `balanceOf` (K-check + flash custody), `transfer` (auto-pay / output),
/// `approve` + `transferFrom` (the pair pulls the swap input from the
/// executor), and `mint` (reserve + seed seeding). Intentional small — the
/// swap/repay MATH is already proven by the tier-3 V2 oracles; this stub only
/// gives the harness shareable token custody across pools.
contract Token {
    string public name;
    string public symbol;
    uint8 public decimals;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);

    function mint(address to, uint256 amount) external {
        balanceOf[to] += amount;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        _transfer(msg.sender, to, amount);
        return true;
    }

    function approve(address spender, uint256 amount) external returns (bool) {
        allowance[msg.sender][spender] = amount;
        emit Approval(msg.sender, spender, amount);
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external returns (bool) {
        uint256 a = allowance[from][msg.sender];
        if (a != type(uint256).max) {
            require(a >= amount, "T:allowance");
            allowance[from][msg.sender] = a - amount;
        }
        _transfer(from, to, amount);
        return true;
    }

    function _transfer(address from, address to, uint256 amount) internal {
        require(balanceOf[from] >= amount, "T:balance");
        balanceOf[from] -= amount;
        balanceOf[to] += amount;
        emit Transfer(from, to, amount);
    }
}
