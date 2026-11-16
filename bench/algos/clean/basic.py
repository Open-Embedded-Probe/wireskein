"""Cleaning before anything reads the lines. Today WireSkein reads the raw lines in its survey (none); its glitch
filters run only later, inside a bus already grouped."""

from dataclasses import replace

from wsbench import algorithm
from wireskein._engine import kernels as K
from wireskein._engine.model import Capture


@algorithm("clean", "none", status="main", about="the lines as captured")
def none(cap):
    return cap


@algorithm("clean", "deglitch", status="trial", about="kernels.deglitch: pulses shorter than k samples dropped",
           variants={"k2": {"k": 2}, "k3": {"k": 3}, "k4": {"k": 4}})
def deglitch(cap, k):
    return Capture(cap.rate, cap.n_samples,
                   [replace(ch, edges=K.deglitch(ch.edges, k * ch.step)) for ch in cap.channels])
