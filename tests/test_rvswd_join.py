"""RVSWD frames split in two by a STOP misread inside one are read as one (rvswd._joined): a burst read at a WCH-LinkE's
low speed holds CLK high about twice its usual high between words, and DIO may rise in it. On a real L103 capture
(bench/data/l103-linke-2026-11-17, low/read_flash_4k) a 15-word burst came out as 468 + 115 bits, two clocks apart,
and its 15 words were lost without a word; joined, every word ch32rv read is there."""

from wireskein._engine import rvswd as R


def test_two_pieces_of_a_burst_are_one_frame():
    # 468 + 2 clocks + 115 = 585 = 15 + 38 x 15
    assert R._joined([(0, 53), (60, 528), (530, 645), (650, 703)]) == [(0, 53), (60, 645), (650, 703)]


def test_frames_of_known_lengths_stay_apart():
    out = [(0, 53), (55, 108), (110, 195)]                 # short, short, long
    assert R._joined(out) == out


def test_pieces_words_apart_are_one_frame():
    # words of all ones after the misread STOP: the next START words later (L103 low/read_pattern4k: 90 + 36 + 459)
    assert R._joined([(0, 90), (126, 585)]) == [(0, 585)]


def test_only_a_burst_is_put_together():
    # pieces that would make a burst's length but do not open as one (a fast clock's broken frames) stay apart
    assert R._joined([(0, 90), (126, 585)], lambda a: False) == [(0, 90), (126, 585)]


def test_pieces_too_far_apart_or_not_adding_up_stay_apart():
    assert R._joined([(0, 300), (500, 585)]) == [(0, 300), (500, 585)]     # 200 clocks between: two frames
    assert R._joined([(0, 400), (402, 500)]) == [(0, 400), (402, 500)]     # 500: no form has it


def test_a_piece_next_to_a_whole_frame_is_not_taken_in():
    # a 53-bit frame and a stray piece after it: the frame is whole, the piece stays apart (and reads as unread)
    assert R._joined([(0, 53), (54, 86)]) == [(0, 53), (54, 86)]
