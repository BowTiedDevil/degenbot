#pragma version ^0.5.0b2
"""
CustomExecutorProbe — the compiled contract the adapter certification executes.

A test-only executor, kept minimal so its evidence is unambiguous: `record`
stores the supplied word and nothing else needs interpreting, while `decoy`
writes only its own flag. A declared pre/post read of the public getters is
therefore proof of WHICH function ran AND WHAT argument it stored, which is the
claim calldata inspection alone cannot make.
"""

last_value: public(uint256)
decoy_flag: public(bool)


@external
def record(_value: uint256) -> bool:
    self.last_value = _value
    return True


@external
def decoy() -> bool:
    self.decoy_flag = True
    return True
