// SPDX-License-Identifier: MIT
pragma solidity ^0.8.23;

/// @dev Canonical seed contracts for the standalone-anvil test tier (T1).
/// Compiled once via `forge build`; bytecode+ABI are committed under
/// tests/standalone_anvil/out and seeded onto a non-forking anvil via
/// AnvilFork.set_code. No upstream RPC is required to run these tests.

contract SimpleToken {
    string public name;
    string public symbol;
    uint8 public decimals;
    uint256 public totalSupply;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    event Transfer(address indexed from, address indexed to, uint256 value);

    constructor(string memory name_, string memory symbol_, uint8 decimals_, uint256 supply_) {
        name = name_;
        symbol = symbol_;
        decimals = decimals_;
        totalSupply = supply_;
        balanceOf[msg.sender] = supply_;
    }

    function transfer(address to, uint256 value) external returns (bool) {
        balanceOf[msg.sender] -= value;
        balanceOf[to] += value;
        emit Transfer(msg.sender, to, value);
        return true;
    }

    function balanceOfAt(address who) external view returns (uint256) {
        return balanceOf[who];
    }
}

contract EventEmitter {
    event Ping(address indexed sender, uint256 value, bytes32 tag);
    event FullLog(address indexed a, uint256 n, string s, bool b);

    function ping(uint256 value, bytes32 tag) external {
        emit Ping(msg.sender, value, tag);
    }

    function fullLog(uint256 n, string calldata s, bool b) external {
        emit FullLog(msg.sender, n, s, b);
    }
}

contract Reverter {
    error Snappy(uint256 code);

    function alwaysRevert() external pure {
        revert("boom");
    }

    function customRevert(uint256 code) external pure {
        revert Snappy(code);
    }

    function ok(uint256 x) external pure returns (uint256) {
        return x + 1;
    }
}

contract MockChainlinkAggregator {
    int256 public answer;
    uint8 public decimals;
    string public description;

    constructor(int256 answer_, uint8 decimals_) {
        answer = answer_;
        decimals = decimals_;
        description = "MockAggregator";
    }

    function latestAnswer() external view returns (int256) {
        return answer;
    }

    function latestRoundData()
        external
        view
        returns (uint80, int256, uint256, uint256, uint80)
    {
        return (1, answer, block.timestamp, block.timestamp, 1);
    }
}
