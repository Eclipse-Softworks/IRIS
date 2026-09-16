"""C-ABI foreign function interface loader and ctypes bindings for IRIS Evolution Engine."""

import ctypes
import os
import platform
import sys
from typing import Optional



def _find_library() -> str:
    """Finds the path to the iris_evolution dynamic library."""
    lib_name = ""
    system = platform.system()
    if system == "Windows":
        lib_name = "iris_evolution.dll"
    elif system == "Darwin":
        lib_name = "libiris_evolution.dylib"
    else:
        lib_name = "libiris_evolution.so"

    # 1. Environment variable override
    env_path = os.environ.get("IRIS_EVOLUTION_LIB")
    if env_path and os.path.exists(env_path):
        return env_path

    # 2. Look relative to this file and pick the newest library
    current_dir = os.path.dirname(os.path.abspath(__file__))
    candidates = [
        # Inside python package
        os.path.join(current_dir, lib_name),
        # Target release/debug relative to repo root
        os.path.join(current_dir, "..", "..", "..", "target", "release", lib_name),
        os.path.join(current_dir, "..", "..", "..", "target", "debug", lib_name),
        # Workspace root
        os.path.join(os.getcwd(), "target", "release", lib_name),
        os.path.join(os.getcwd(), "target", "debug", lib_name),
    ]

    existing = [
        os.path.abspath(p)
        for p in candidates
        if os.path.exists(os.path.abspath(p))
    ]
    if existing:
        # Pick the most recently modified binary
        return max(existing, key=os.path.getmtime)

    raise FileNotFoundError(
        f"Could not locate '{lib_name}'. Run 'cargo build --release -p iris_ffi' or set IRIS_EVOLUTION_LIB."
    )


# Load library and configure signatures
_LIB_PATH = _find_library()
_lib = ctypes.CDLL(_LIB_PATH)

# Search Session API
_lib.iris_evolution_search_create.argtypes = [
    ctypes.c_uint32,  # pop_size
    ctypes.c_uint32,  # generations
    ctypes.c_double,  # mutation_rate
    ctypes.c_double,  # crossover_rate
    ctypes.c_double,  # parsimony_weight
    ctypes.c_uint64,  # seed
]
_lib.iris_evolution_search_create.restype = ctypes.c_void_p

_lib.iris_evolution_search_add_case.argtypes = [
    ctypes.c_void_p,  # session
    ctypes.c_int64,   # input
    ctypes.c_int64,   # expected
    ctypes.c_double,  # weight
]
_lib.iris_evolution_search_add_case.restype = None

_lib.iris_evolution_search_run.argtypes = [ctypes.c_void_p]
_lib.iris_evolution_search_run.restype = ctypes.c_void_p

_lib.iris_evolution_search_free.argtypes = [ctypes.c_void_p]
_lib.iris_evolution_search_free.restype = None

# Genome API
_lib.iris_genome_eval.argtypes = [ctypes.c_void_p, ctypes.c_int64]
_lib.iris_genome_eval.restype = ctypes.c_int64

_lib.iris_genome_to_iris_expr.argtypes = [ctypes.c_void_p]
_lib.iris_genome_to_iris_expr.restype = ctypes.c_void_p

_lib.iris_genome_to_python_expr.argtypes = [ctypes.c_void_p]
_lib.iris_genome_to_python_expr.restype = ctypes.c_void_p

_lib.iris_genome_to_python_func.argtypes = [
    ctypes.c_void_p,
    ctypes.c_char_p,
    ctypes.c_char_p,
]
_lib.iris_genome_to_python_func.restype = ctypes.c_void_p

_lib.iris_genome_to_python_lambda.argtypes = [
    ctypes.c_void_p,
    ctypes.c_char_p,
]
_lib.iris_genome_to_python_lambda.restype = ctypes.c_void_p

_lib.iris_genome_free.argtypes = [ctypes.c_void_p]
_lib.iris_genome_free.restype = None

# Genome Vector & State API
_lib.iris_genome_state_create.argtypes = []
_lib.iris_genome_state_create.restype = ctypes.c_void_p

_lib.iris_genome_state_free.argtypes = [ctypes.c_void_p]
_lib.iris_genome_state_free.restype = None

_lib.iris_genome_state_reset.argtypes = [ctypes.c_void_p]
_lib.iris_genome_state_reset.restype = None

_lib.iris_genome_eval_vec4.argtypes = [
    ctypes.c_void_p,
    ctypes.POINTER(ctypes.c_int64),
    ctypes.POINTER(ctypes.c_int64),
]
_lib.iris_genome_eval_vec4.restype = ctypes.c_int64

_lib.iris_genome_eval_stateful_vec4.argtypes = [
    ctypes.c_void_p,
    ctypes.c_void_p,
    ctypes.POINTER(ctypes.c_int64),
    ctypes.POINTER(ctypes.c_int64),
]
_lib.iris_genome_eval_stateful_vec4.restype = ctypes.c_int64

_lib.iris_string_free.argtypes = [ctypes.c_void_p]
_lib.iris_string_free.restype = None


def ptr_to_str(ptr: Optional[int]) -> str:
    """Decodes a C string pointer and frees it using iris_string_free."""
    if not ptr:
        return ""
    try:
        raw_bytes = ctypes.string_at(ptr)
        return raw_bytes.decode("utf-8")
    finally:
        _lib.iris_string_free(ptr)
