"""ctypes access to the IRIS ROS 2 bridge, for checks that need no IRIS program.

`Bridge(path).name(args...)` calls `iris_ros2_<name>`: str arguments pass as
`const char *`, float arguments as their bit pattern (as IRIS passes them),
ints as int64. Functions ending in `get_f64` return a double, the rest int64.
"""
import ctypes
import struct


def bits(x):
    return struct.unpack("<q", struct.pack("<d", x))[0]


class Bridge:
    def __init__(self, path):
        self.lib = ctypes.CDLL(path, winmode=0)

    def fn(self, name, restype=ctypes.c_int64):
        f = getattr(self.lib, name)
        f.restype = restype

        def call(*args):
            conv = []
            for a in args:
                if isinstance(a, str):
                    conv.append(ctypes.c_char_p(a.encode()))
                elif isinstance(a, float):
                    conv.append(ctypes.c_int64(bits(a)))
                else:
                    conv.append(ctypes.c_int64(a))
            return f(*conv)
        return call

    def __getattr__(self, name):
        rt = ctypes.c_double if name.endswith("get_f64") else ctypes.c_int64
        return self.fn("iris_ros2_" + name, rt)

    def text(self, n):
        if n < 0:
            return None
        buf = ctypes.create_string_buffer(n + 1)
        self.result_copy(ctypes.addressof(buf), n + 1)
        return buf.value.decode()

    def error(self):
        return self.text(self.last_error())
