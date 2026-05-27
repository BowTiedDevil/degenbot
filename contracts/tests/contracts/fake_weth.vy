import fake_erc20
initializes: fake_erc20
exports: (
    fake_erc20.IERC20,
    fake_erc20.IERC20Detailed,
    fake_erc20.mint,
    fake_erc20.burn,    
)

@deploy
def __init__(
    name: String[32], symbol: String[32], decimals: uint8, supply: uint256
):
    fake_erc20.__init__(name, symbol, decimals, supply)


@external
@payable
def deposit():
    fake_erc20.totalSupply += msg.value
    fake_erc20.balanceOf[msg.sender] += msg.value


@external
def withdraw(amount: uint256):
    fake_erc20.totalSupply -= amount
    fake_erc20.balanceOf[msg.sender] -= amount
    raw_call(
        msg.sender, 
        b'',
        value=amount
    )