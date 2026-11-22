"""declarative's memo of views: a view dropped and another made at the same address is told apart (an analysis read
the first view's answers for the second: its result changed with what had run before it in the process)."""

import weakref

from wireskein._engine.declarative import _view_id
from wireskein._engine.model import Capture


def test_the_same_view_keeps_its_key():
    memo, a = {}, Capture(1e6, 10, [])
    assert _view_id(memo, a) == _view_id(memo, a)


def test_a_view_at_the_address_of_one_gone_is_another_key():
    memo = {}
    b = Capture(1e6, 10, [])
    gone = Capture(1e6, 10, [])
    # what the memo holds when a view lived at b's address before and was dropped: its key, its weak reference
    memo["_views"] = {id(b): weakref.ref(gone)}
    memo["_view_gen"] = 1
    memo[("_gen", id(b))] = 1
    old = (id(b), 1)
    del gone                                             # the earlier view is gone
    assert _view_id(memo, b) != old
