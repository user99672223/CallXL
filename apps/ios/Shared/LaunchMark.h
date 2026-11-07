// Launch phase marks for the §1.5 launch check (CallXL and LaunchBaseline).
#pragma once

/// Wall-clock seconds (gettimeofday) when this image's C constructor ran: after every
/// dependent dylib (CallXLCore included) was loaded and initialised, just before main().
extern double callxl_launch_premain_s;
