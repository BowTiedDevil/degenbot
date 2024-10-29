import pickle
from typing import Any

from degenbot.types import BoundedCache


def test_pickle_bounded_cache():
    bounded_cache: BoundedCache[Any, Any] = BoundedCache(max_items=5)
    pickled_cache = pickle.dumps(bounded_cache)
    pickle.loads(pickled_cache)


def test_bounded_cache_max_items():
    bounded_cache: BoundedCache[Any, Any] = BoundedCache(max_items=5)
    bounded_cache[0] = "a"
    bounded_cache[1] = "b"
    bounded_cache[2] = "c"
    bounded_cache[3] = "d"
    bounded_cache[4] = "e"
    assert len(bounded_cache) == 5
    assert bounded_cache.keys() == {0, 1, 2, 3, 4}

    # Adding this element should cause the first to be removed
    bounded_cache[5] = "f"
    assert len(bounded_cache) == 5
    assert bounded_cache.keys() == {1, 2, 3, 4, 5}

    # This element already exists, so it should update the value
    bounded_cache[2] = "c"
    assert len(bounded_cache) == 5
    assert bounded_cache.keys() == {1, 2, 3, 4, 5}
