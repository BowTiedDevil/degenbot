from ethereum.ercs import IERC20
from ethereum.ercs import IERC20Detailed

implements: IERC20
implements: IERC20Detailed

name: public(String[32])
symbol: public(String[32])
decimals: public(uint8)

MINTER: immutable(address)

totalSupply: public(uint256)
balanceOf: public(HashMap[address, uint256])
allowance: public(HashMap[address, HashMap[address, uint256]])


@deploy
def __init__(_name: String[32], _symbol: String[32], decimals: uint8, nominal_initial_supply: uint256):
    MINTER = msg.sender
    init_supply: uint256 = nominal_initial_supply * 10 ** convert(decimals, uint256)
    self.name = _name
    self.symbol = _symbol
    self.decimals = decimals
    self.balanceOf[msg.sender] = init_supply
    self.totalSupply = init_supply


@external
def transfer(_to : address, _value : uint256) -> bool:    
    self.balanceOf[msg.sender] -= _value
    self.balanceOf[_to] += _value
    return True


@external
def transferFrom(_from : address, _to : address, _value : uint256) -> bool:
    self.balanceOf[_from] -= _value
    self.balanceOf[_to] += _value
    self.allowance[_from][msg.sender] -= _value    
    return True


@external
def approve(_spender : address, _value : uint256) -> bool:    
    self.allowance[msg.sender][_spender] = _value    
    return True


@external
def mint(_to: address, _value: uint256):
    assert msg.sender == MINTER
    assert _to != empty(address)
    self.totalSupply += _value
    self.balanceOf[_to] += _value


@internal
def _burn(_to: address, _value: uint256):
    """
    @dev Internal function that burns an amount of the token of a given
         account.
    @param _to The account whose tokens will be burned.
    @param _value The amount that will be burned.
    """
    assert _to != empty(address)
    self.totalSupply -= _value
    self.balanceOf[_to] -= _value    


@external
def burn(_value: uint256):
    """
    @dev Burn an amount of the token of msg.sender.
    @param _value The amount that will be burned.
    """
    self._burn(msg.sender, _value)


@external
def burnFrom(_to: address, _value: uint256):
    """
    @dev Burn an amount of the token from a given account.
    @param _to The account whose tokens will be burned.
    @param _value The amount that will be burned.
    """
    self.allowance[_to][msg.sender] -= _value
    self._burn(_to, _value)