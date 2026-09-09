#include <stddef.h>

/* The local ROS 2 SDK is compiled for the MSVC ABI, while this machine's
 * Build Tools installation does not contain vcruntime.lib. These are the two
 * symbols the bridge needs from it; UCRT supplies the rest. */
int _fltused = 0x9875;

void *memcpy(void *destination, const void *source, size_t count) {
    unsigned char *out = (unsigned char *)destination;
    const unsigned char *in = (const unsigned char *)source;
    for (size_t i = 0; i < count; i++) out[i] = in[i];
    return destination;
}
