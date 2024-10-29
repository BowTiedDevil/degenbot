import pickle
from typing import Any

from degenbot.types import BoundedCache


def test_pickle_bounded_cache():
    bounded_cache: BoundedCache[Any, Any] = BoundedCache(max_items=5)
    pickled_cache = pickle.dumps(bounded_cache)
    pickle.loads(pickled_cache)
