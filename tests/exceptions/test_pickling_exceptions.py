import pickle

from degenbot.exceptions.arbitrage import NoSolverSolution
from degenbot.exceptions.liquidity_pool import IncompleteSwap, PossibleInaccurateResult


def test_no_solver_solution_pickling() -> None:
    """
    Test that the `NoSolverSolution` exception's `__reduce__` method allows the exception to be
    pickled and unpickled correctly.
    """

    # Create an instance with a custom message
    original_message = "Custom solver error message"
    original_exception = NoSolverSolution(message=original_message)

    # Pickle the exception
    pickled_data = pickle.dumps(original_exception)

    # Unpickle the exception
    unpickled_exception = pickle.loads(pickled_data)

    # Verify the unpickled exception has the same type
    assert type(unpickled_exception) is NoSolverSolution

    # Verify the unpickled exception has the same message
    assert unpickled_exception.message == original_message
    assert str(unpickled_exception) == original_message


def test_no_solver_solution_default_message_pickling() -> None:
    """
    Test that NoSolverSolution exception with default message can be pickled and unpickled.
    """
    # Create an instance with the default message
    original_exception = NoSolverSolution()

    # Pickle the exception
    pickled_data = pickle.dumps(original_exception)

    # Unpickle the exception
    unpickled_exception = pickle.loads(pickled_data)

    # Verify the unpickled exception has the same type
    assert type(unpickled_exception) is NoSolverSolution

    # Verify the unpickled exception has the default message
    assert unpickled_exception.message == "Solver failed to converge on a solution."
    assert str(unpickled_exception) == "Solver failed to converge on a solution."


def test_incomplete_swap_pickling() -> None:
    """
    Test that the `IncompleteSwap` exception's `__reduce__` method allows the exception to be
    pickled and unpickled correctly.
    """
    # Create an instance with custom amount_in and amount_out values
    original_amount_in = 1000
    original_amount_out = 500
    original_exception = IncompleteSwap(amount_in=original_amount_in, amount_out=original_amount_out)

    # Pickle the exception
    pickled_data = pickle.dumps(original_exception)

    # Unpickle the exception
    unpickled_exception = pickle.loads(pickled_data)

    # Verify the unpickled exception has the same type
    assert type(unpickled_exception) is IncompleteSwap

    # Verify the unpickled exception has the same attribute values
    assert unpickled_exception.amount_in == original_amount_in
    assert unpickled_exception.amount_out == original_amount_out
    assert unpickled_exception.message == "Insufficient liquidity to swap for the requested amount."


def test_possible_inaccurate_result_pickling() -> None:
    """
    Test that the `PossibleInaccurateResult` exception's `__reduce__` method allows the exception to be
    pickled and unpickled correctly.
    """
    # Create an instance with custom amount_in, amount_out, and hooks values
    original_amount_in = 2000
    original_amount_out = 1000
    original_hooks = {"hook1", "hook2", "hook3"}  # Using a set of strings as mock hooks
    original_exception = PossibleInaccurateResult(
        amount_in=original_amount_in,
        amount_out=original_amount_out,
        hooks=original_hooks
    )

    # Pickle the exception
    pickled_data = pickle.dumps(original_exception)

    # Unpickle the exception
    unpickled_exception = pickle.loads(pickled_data)

    # Verify the unpickled exception has the same type
    assert type(unpickled_exception) is PossibleInaccurateResult

    # Verify the unpickled exception has the same attribute values
    assert unpickled_exception.amount_in == original_amount_in
    assert unpickled_exception.amount_out == original_amount_out
    assert unpickled_exception.hooks == original_hooks
    assert unpickled_exception.message == "The pool has one or more hooks that might invalidate the calculated result."
