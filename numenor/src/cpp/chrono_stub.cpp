// Bare-metal stub for std::chrono::system_clock::now()
// Used by llama-sampler.cpp for RNG seeding.
// Returns zero time point (bare metal has no real-time clock).

#include <chrono>

_LIBCPP_BEGIN_NAMESPACE_STD
namespace chrono {
system_clock::time_point system_clock::now() noexcept {
    return time_point(duration(0));
}
} // namespace chrono
_LIBCPP_END_NAMESPACE_STD
