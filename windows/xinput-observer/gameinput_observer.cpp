#include <GameInput.h>
#include <windows.h>
#include <cstring>
#include <cstdio>
#include <thread>
#include <chrono>
int main() {
    using namespace GameInput::v1;
    IGameInput* input = nullptr;
    HRESULT result = GameInputCreate(&input);
    if (FAILED(result)) {
        std::fprintf(stderr, "GameInputCreate failed: 0x%08lx\n", result);
        return 2;
    }

    const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(10);
    uint64_t readings = 0;
    uint64_t changed = 0;
    GameInputGamepadState previous{};
    bool have_previous = false;
    while (std::chrono::steady_clock::now() < deadline) {
        IGameInputReading* reading = nullptr;
        result = input->GetCurrentReading(GameInputKindGamepad, nullptr, &reading);
        if (SUCCEEDED(result) && reading != nullptr) {
            GameInputGamepadState state{};
            if (reading->GetGamepadState(&state)) {
                readings++;
                if (have_previous && std::memcmp(&state, &previous, sizeof(state)) != 0)
                    changed++;
                previous = state;
                have_previous = true;
            }
            reading->Release();
        }
        std::this_thread::sleep_for(std::chrono::milliseconds(2));
    }
    std::printf("gameinput readings %llu changed %llu\n", readings, changed);
    input->Release();
    return readings == 0 ? 4 : 0;
}
