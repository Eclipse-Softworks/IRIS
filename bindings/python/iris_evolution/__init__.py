"""IRIS Evolution Engine: Python bindings for polyglot code genome synthesis,

cybernetic homeostasis, and real-time autonomous adaptation.
"""

from .genome import GeneNode, GenomeState
from .search import EvolutionarySearch

__all__ = [
    "GeneNode",
    "GenomeState",
    "EvolutionarySearch",
]

__version__ = "1.0.0-rc1"
