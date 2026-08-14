#pragma once

#define NOMINMAX
#include <windows.h>
#include <bugcodes.h>
#include <wudfwdm.h>
#include <wdf.h>
#include <iddcx.h>

#include <dxgi1_5.h>
#include <d3d11_2.h>
#include <avrt.h>
#include <sddl.h>
#include <wrl.h>

#include <memory>
#include <vector>

#include "Trace.h"
#include "..\PhaseContract.h"

namespace Microsoft
{
    namespace WRL
    {
        namespace Wrappers
        {
            // Adds a wrapper for thread handles to the existing set of WRL handle wrapper classes
            typedef HandleT<HandleTraits::HANDLENullTraits> Thread;
        }
    }
}

namespace Microsoft
{
    namespace IndirectDisp
    {
        /// <summary>
        /// Manages the creation and lifetime of a Direct3D render device.
        /// </summary>
        struct IndirectSampleMonitor
        {
            static constexpr size_t szEdidBlock = 128;
            static constexpr size_t szModeList = 3;

            const BYTE pEdidBlock[szEdidBlock];
            const struct SampleMonitorMode {
                DWORD Width;
                DWORD Height;
                DWORD VSync;
            } pModeList[szModeList];
            const DWORD ulPreferredModeIdx;
        };

        /// <summary>
        /// Manages the creation and lifetime of a Direct3D render device.
        /// </summary>
        struct Direct3DDevice
        {
            Direct3DDevice(LUID AdapterLuid);
            Direct3DDevice();
            HRESULT Init();

            LUID AdapterLuid;
            Microsoft::WRL::ComPtr<IDXGIFactory5> DxgiFactory;
            Microsoft::WRL::ComPtr<IDXGIAdapter1> Adapter;
            Microsoft::WRL::ComPtr<ID3D11Device> Device;
            Microsoft::WRL::ComPtr<ID3D11DeviceContext> DeviceContext;
        };

        /// <summary>
        /// Carries a phase request from whoever sent the IOCTL to the swap-chain
        /// thread, and keeps the tally of what became of every one of them.
        /// </summary>
        ///
        /// One instance for the whole driver rather than one per device. This
        /// package declares a single monitor and the laboratory runs a single
        /// virtual display, so a per-device lever would add a lookup on the frame
        /// path to distinguish between things that never both exist.
        class PhaseLever
        {
        public:
            static PhaseLever& Instance();

            /// Records a request, displacing any the frame loop has not taken yet.
            ///
            /// One slot and the newest wins. Two arriving before the loop looks
            /// means the asker measured the same uncorrected phase twice, and
            /// obeying both would move the display by an amount it was only asked
            /// for once. The displaced request is counted rather than forgotten,
            /// because afterwards a request that was superseded and one that was
            /// never sent leave the phase in exactly the same place.
            void Post(ULONG DelayNanos);

            /// Records a request that could not be understood.
            void Reject();

            /// Obeys whatever is waiting, if anything is, and reports whether the
            /// thread should carry on. False means the terminate event fired while
            /// the delay was being served.
            bool HoldBack(HANDLE TerminateEvent);

        private:
            PhaseLever();

            /// The slot deliberately does not live in the shared section. The
            /// section is published for reading and the IOCTL is the only way in,
            /// so an unprivileged process cannot move the display by writing to a
            /// page instead of asking.
            volatile LONG64 m_Slot;

            IddLabPhaseCounters* m_pCounters;
            /// Used when the section could not be created, so that failing to
            /// report cannot also disable the lever.
            IddLabPhaseCounters m_Local;

            HANDLE m_hSection;
            HANDLE m_hTimer;
            LONG64 m_TicksPerSecond;
        };

        /// <summary>
        /// Manages a thread that consumes buffers from an indirect display swap-chain object.
        /// </summary>
        class SwapChainProcessor
        {
        public:
            SwapChainProcessor(IDDCX_SWAPCHAIN hSwapChain, std::shared_ptr<Direct3DDevice> Device, HANDLE NewFrameEvent);
            ~SwapChainProcessor();

        private:
            static DWORD CALLBACK RunThread(LPVOID Argument);

            void Run();
            void RunCore();

            IDDCX_SWAPCHAIN m_hSwapChain;
            std::shared_ptr<Direct3DDevice> m_Device;
            HANDLE m_hAvailableBufferEvent;
            Microsoft::WRL::Wrappers::Thread m_hThread;
            Microsoft::WRL::Wrappers::Event m_hTerminateEvent;
        };

        /// <summary>
        /// Provides a sample implementation of an indirect display driver.
        /// </summary>
        class IndirectDeviceContext
        {
        public:
            IndirectDeviceContext(_In_ WDFDEVICE WdfDevice);
            virtual ~IndirectDeviceContext();

            void InitAdapter();
            void FinishInit(UINT ConnectorIndex);

        protected:
            WDFDEVICE m_WdfDevice;
            IDDCX_ADAPTER m_Adapter;
        };

        class IndirectMonitorContext
        {
        public:
            IndirectMonitorContext(_In_ IDDCX_MONITOR Monitor);
            virtual ~IndirectMonitorContext();

            void AssignSwapChain(IDDCX_SWAPCHAIN SwapChain, LUID RenderAdapter, HANDLE NewFrameEvent);
            void UnassignSwapChain();

        private:
            IDDCX_MONITOR m_Monitor;
            std::unique_ptr<SwapChainProcessor> m_ProcessingThread;
        } ;
    }
}