#include "LaunchMark.h"

#include <stddef.h>
#include <sys/time.h>

double callxl_launch_premain_s = 0;

__attribute__((constructor)) static void callxl_launch_mark(void) {
    struct timeval tv;
    gettimeofday(&tv, NULL);
    callxl_launch_premain_s = (double)tv.tv_sec + (double)tv.tv_usec / 1e6;
}
