#include <iostream>
#include <vector>

#include <windows.h>
#include <swdevice.h>
#include <wrl.h>

struct CreationState
{
    HANDLE event;
    HRESULT result;
};

VOID WINAPI
CreationCallback(
    _In_ HSWDEVICE hSwDevice,
    _In_ HRESULT hrCreateResult,
    _In_opt_ PVOID pContext,
    _In_opt_ PCWSTR pszDeviceInstanceId
    )
{
    auto* state = static_cast<CreationState*>(pContext);
    state->result = hrCreateResult;
    SetEvent(state->event);
    UNREFERENCED_PARAMETER(hSwDevice);
    UNREFERENCED_PARAMETER(pszDeviceInstanceId);
}

int __cdecl main(int argc, wchar_t *argv[])
{
    UNREFERENCED_PARAMETER(argc);
    UNREFERENCED_PARAMETER(argv);

    HANDLE hEvent = CreateEvent(nullptr, FALSE, FALSE, nullptr);
    HSWDEVICE hSwDevice = nullptr;
    CreationState creation{ hEvent, E_PENDING };
    SW_DEVICE_CREATE_INFO createInfo = { 0 };
    PCWSTR description = L"LanPlay IDD-LAB 1080p120";

    // These match the PnP IDs in the INF.
    PCWSTR instanceId = L"LanPlayIddLab";
    PCWSTR hardwareIds = L"LanPlayIddLab\0\0";
    PCWSTR compatibleIds = L"LanPlayIddLab\0\0";

    createInfo.cbSize = sizeof(createInfo);
    createInfo.pszzCompatibleIds = compatibleIds;
    createInfo.pszInstanceId = instanceId;
    createInfo.pszzHardwareIds = hardwareIds;
    createInfo.pszDeviceDescription = description;

    createInfo.CapabilityFlags = SWDeviceCapabilitiesRemovable |
                                 SWDeviceCapabilitiesSilentInstall |
                                 SWDeviceCapabilitiesDriverRequired;

    // Create the device
    HRESULT hr = SwDeviceCreate(L"LanPlayIddLab",
                                L"HTREE\\ROOT\\0",
                                &createInfo,
                                0,
                                nullptr,
                                CreationCallback,
                                &creation,
                                &hSwDevice);
    if (FAILED(hr))
    {
        printf("LanPlay IDD-LAB SwDeviceCreate failed with 0x%lx\n", hr);
        return 1;
    }

    // Wait for callback to signal that the device has been created
    printf("Waiting for LanPlay IDD-LAB device creation...\n");
    DWORD waitResult = WaitForSingleObject(hEvent, 10 * 1000);
    if (waitResult != WAIT_OBJECT_0)
    {
        printf("LanPlay IDD-LAB device creation timed out\n");
        return 1;
    }
    if (FAILED(creation.result))
    {
        printf("LanPlay IDD-LAB device creation failed with 0x%lx\n", creation.result);
        return 1;
    }
    printf("LanPlay IDD-LAB device created; keep this process alive.\n\n");

    // The software device exists for exactly as long as this process. A lab
    // runner stops the process to remove the display; no console input is
    // required, so the controller also works from a hidden scheduled task.
    printf("Terminate this process to remove the laboratory display.\n");
    Sleep(INFINITE);
    
    // Stop the device, this will cause the sample to be unloaded
    SwDeviceClose(hSwDevice);
    CloseHandle(hEvent);

    return 0;
}