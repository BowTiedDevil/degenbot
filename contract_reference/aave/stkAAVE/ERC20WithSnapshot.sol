// SPDX-License-Identifier: agpl-3.0
pragma solidity 0.6.12;

import {ERC20} from '../lib/ERC20.sol';
import {ITransferHook} from '../interfaces/ITransferHook.sol';

/**
 * @title ERC20WithSnapshot
 * @notice ERC20 including snapshots of balances on transfer-related actions
 * @author Aave
 **/
contract ERC20WithSnapshot is ERC20 {

    /// @dev snapshot of a value on a specific block, used for balances
    struct Snapshot {
        uint128 blockNumber;
        uint128 value;
    }

    mapping (address => mapping (uint256 => Snapshot)) public _snapshots;
    mapping (address => uint256) public _countsSnapshots;
    /// @dev reference to the Aave governance contract to call (if initialized) on _beforeTokenTransfer
    /// !!! IMPORTANT The Aave governance is considered a trustable contract, being its responsibility
    /// to control all potential reentrancies by calling back the this contract
    ITransferHook public _aaveGovernance;

    event SnapshotDone(address owner, uint128 oldValue, uint128 newValue);

    constructor(string memory name, string memory symbol, uint8 decimals) public ERC20(name, symbol, decimals) {}

    function _setAaveGovernance(ITransferHook aaveGovernance) internal virtual {
        _aaveGovernance = aaveGovernance;
    }

    /**
    * @dev Writes a snapshot for an owner of tokens
    * @param owner The owner of the tokens
    * @param oldValue The value before the operation that is gonna be executed after the snapshot
    * @param newValue The value after the operation
    */
    function _writeSnapshot(address owner, uint128 oldValue, uint128 newValue) internal virtual {
        uint128 currentBlock = uint128(block.number);

        uint256 ownerCountOfSnapshots = _countsSnapshots[owner];
        mapping (uint256 => Snapshot) storage snapshotsOwner = _snapshots[owner];

        // Doing multiple operations in the same block
        if (ownerCountOfSnapshots != 0 && snapshotsOwner[ownerCountOfSnapshots.sub(1)].blockNumber == currentBlock) {
            snapshotsOwner[ownerCountOfSnapshots.sub(1)].value = newValue;
        } else {
            snapshotsOwner[ownerCountOfSnapshots] = Snapshot(currentBlock, newValue);
            _countsSnapshots[owner] = ownerCountOfSnapshots.add(1);
        }

        emit SnapshotDone(owner, oldValue, newValue);
    }

    /**
    * @dev Writes a snapshot before any operation involving transfer of value: _transfer, _mint and _burn
    * - On _transfer, it writes snapshots for both "from" and "to"
    * - On _mint, only for _to
    * - On _burn, only for _from
    * @param from the from address
    * @param to the to address
    * @param amount the amount to transfer
    */
    function _beforeTokenTransfer(address from, address to, uint256 amount) internal override {
        if (from == to) {
            return;
        }

        if (from != address(0)) {
            uint256 fromBalance = balanceOf(from);
            _writeSnapshot(from, uint128(fromBalance), uint128(fromBalance.sub(amount)));
        }
        if (to != address(0)) {
            uint256 toBalance = balanceOf(to);
            _writeSnapshot(to, uint128(toBalance), uint128(toBalance.add(amount)));
        }

        // caching the aave governance address to avoid multiple state loads
        ITransferHook aaveGovernance = _aaveGovernance;
        if (aaveGovernance != ITransferHook(0)) {
            aaveGovernance.onTransfer(from, to, amount);
        }
    }
}