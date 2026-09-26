#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <mmdeviceapi.h>
#include <audioclient.h>
#include <audioclientactivationparams.h>
#include <wrl/client.h>
#include <wrl/implements.h>
#include <tlhelp32.h>
#include <vector>
#include <atomic>
#include <thread>
#include <chrono>
#include <cmath>
#include <cstdio>

using namespace Microsoft::WRL;

// Async activation completion handler with Free-Threaded Marshaler
class ProcessLoopbackActivationHandler :
    public RuntimeClass<RuntimeClassFlags<ClassicCom>, FtmBase, IActivateAudioInterfaceCompletionHandler>
{
public:
    HANDLE m_hCompleted;
    HRESULT m_hr;
    ComPtr<IUnknown> m_punkAudioInterface;

    ProcessLoopbackActivationHandler(HANDLE hCompleted)
        : m_hCompleted(hCompleted), m_hr(E_PENDING) {}

    STDMETHODIMP ActivateCompleted(IActivateAudioInterfaceAsyncOperation* op) override {
        HRESULT hrActivate = S_OK;
        IUnknown* unk = nullptr;
        m_hr = op->GetActivateResult(&hrActivate, &unk);
        if (SUCCEEDED(m_hr)) {
            m_hr = hrActivate;
            m_punkAudioInterface.Attach(unk);
        }
        if (m_hCompleted) {
            SetEvent(m_hCompleted);
        }
        return S_OK;
    }
};

extern "C" {
    typedef void (*DiscordAudioCallback)(const float* samples, int num_samples, int sample_rate, int channels, void* user_data);

    int start_discord_process_loopback(DiscordAudioCallback callback, void* user_data);
    void stop_discord_process_loopback();
    bool is_discord_process_loopback_running();
}

static std::atomic<bool> g_running{false};
static std::atomic<bool> g_stop_requested{false};
static std::thread g_worker_thread;

// Find root Discord.exe process ID
static DWORD FindDiscordMainProcessId() {
    HANDLE hSnap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
    if (hSnap == INVALID_HANDLE_VALUE) return 0;

    PROCESSENTRY32W pe32;
    pe32.dwSize = sizeof(pe32);

    struct ProcInfo {
        DWORD pid;
        DWORD ppid;
    };
    std::vector<ProcInfo> discords;

    if (Process32FirstW(hSnap, &pe32)) {
        do {
            if (_wcsicmp(pe32.szExeFile, L"Discord.exe") == 0) {
                discords.push_back({ pe32.th32ProcessID, pe32.th32ParentProcessID });
            }
        } while (Process32NextW(hSnap, &pe32));
    }
    CloseHandle(hSnap);

    if (discords.empty()) return 0;

    // Find the Discord process whose parent is NOT Discord.exe (root of Discord process tree)
    for (const auto& proc : discords) {
        bool parentIsDiscord = false;
        for (const auto& other : discords) {
            if (proc.ppid == other.pid) {
                parentIsDiscord = true;
                break;
            }
        }
        if (!parentIsDiscord) {
            return proc.pid;
        }
    }

    return discords[0].pid;
}

static void ProcessLoopbackWorker(DiscordAudioCallback callback, void* user_data) {
    HRESULT hr = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
    bool coInitialized = SUCCEEDED(hr);

    while (!g_stop_requested.load()) {
        DWORD discordPid = FindDiscordMainProcessId();
        if (discordPid == 0) {
            // Discord not found, retry after 1s
            for (int i = 0; i < 10 && !g_stop_requested.load(); i++) {
                std::this_thread::sleep_for(std::chrono::milliseconds(100));
            }
            continue;
        }

        HANDLE hEvent = CreateEventW(nullptr, FALSE, FALSE, nullptr);
        if (!hEvent) {
            std::this_thread::sleep_for(std::chrono::milliseconds(500));
            continue;
        }

        AUDIOCLIENT_ACTIVATION_PARAMS params = {};
        params.ActivationType = AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK;
        params.ProcessLoopbackParams.TargetProcessId = discordPid;
        params.ProcessLoopbackParams.ProcessLoopbackMode = PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE;

        PROPVARIANT prop = {};
        prop.vt = VT_BLOB;
        prop.blob.cbSize = sizeof(params);
        prop.blob.pBlobData = (BYTE*)&params;

        auto handler = Make<ProcessLoopbackActivationHandler>(hEvent);
        ComPtr<IActivateAudioInterfaceAsyncOperation> asyncOp;

        hr = ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            __uuidof(IAudioClient),
            &prop,
            handler.Get(),
            &asyncOp
        );

        if (FAILED(hr)) {
            CloseHandle(hEvent);
            std::this_thread::sleep_for(std::chrono::milliseconds(1000));
            continue;
        }

        while (!g_stop_requested.load()) {
            DWORD waitRes = WaitForSingleObject(hEvent, 100);
            if (waitRes == WAIT_OBJECT_0) break;
        }
        CloseHandle(hEvent);

        if (g_stop_requested.load()) break;

        if (FAILED(handler->m_hr) || !handler->m_punkAudioInterface) {
            std::this_thread::sleep_for(std::chrono::milliseconds(1000));
            continue;
        }

        ComPtr<IAudioClient> client;
        hr = handler->m_punkAudioInterface.As(&client);
        if (FAILED(hr) || !client) {
            std::this_thread::sleep_for(std::chrono::milliseconds(1000));
            continue;
        }

        const DWORD sampleRate = 48000;
        const WORD channels = 2;
        WAVEFORMATEX wfx = {};
        wfx.wFormatTag = WAVE_FORMAT_PCM;
        wfx.nChannels = channels;
        wfx.nSamplesPerSec = sampleRate;
        wfx.wBitsPerSample = 16;
        wfx.nBlockAlign = (wfx.nChannels * wfx.wBitsPerSample) / 8;
        wfx.nAvgBytesPerSec = wfx.nSamplesPerSec * wfx.nBlockAlign;

        REFERENCE_TIME hnsBufferDuration = 10000000; // 1 second buffer
        hr = client->Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
            hnsBufferDuration,
            0,
            &wfx,
            nullptr
        );

        if (FAILED(hr)) {
            std::this_thread::sleep_for(std::chrono::milliseconds(1000));
            continue;
        }

        ComPtr<IAudioCaptureClient> capture;
        hr = client->GetService(IID_PPV_ARGS(&capture));
        if (FAILED(hr) || !capture) {
            std::this_thread::sleep_for(std::chrono::milliseconds(1000));
            continue;
        }

        hr = client->Start();
        if (FAILED(hr)) {
            std::this_thread::sleep_for(std::chrono::milliseconds(1000));
            continue;
        }

        // Polling loop (20ms interval)
        std::vector<float> sampleBuf;
        sampleBuf.reserve(4800 * channels);

        while (!g_stop_requested.load()) {
            std::this_thread::sleep_for(std::chrono::milliseconds(20));

            sampleBuf.clear();
            UINT32 packetLength = 0;
            bool streamError = false;

            while (SUCCEEDED(capture->GetNextPacketSize(&packetLength)) && packetLength > 0) {
                BYTE* pData = nullptr;
                UINT32 numFrames = 0;
                DWORD flags = 0;

                HRESULT hrBuf = capture->GetBuffer(&pData, &numFrames, &flags, nullptr, nullptr);
                if (SUCCEEDED(hrBuf)) {
                    if (numFrames > 0) {
                        size_t totalSamples = (size_t)numFrames * channels;
                        size_t oldSize = sampleBuf.size();
                        sampleBuf.resize(oldSize + totalSamples);

                        if (flags & AUDCLNT_BUFFERFLAGS_SILENT) {
                            for (size_t i = oldSize; i < oldSize + totalSamples; i++) {
                                sampleBuf[i] = 0.0f;
                            }
                        } else if (pData) {
                            const short* pcm16 = (const short*)pData;
                            for (size_t i = 0; i < totalSamples; i++) {
                                sampleBuf[oldSize + i] = (float)pcm16[i] / 32768.0f;
                            }
                        }
                    }
                    capture->ReleaseBuffer(numFrames);
                } else {
                    if (hrBuf != AUDCLNT_S_BUFFER_EMPTY) {
                        streamError = true;
                    }
                    break;
                }
            }

            if (!sampleBuf.empty() && callback) {
                callback(sampleBuf.data(), (int)sampleBuf.size(), sampleRate, channels, user_data);
            }

            if (streamError) {
                break; // Re-initialize loop on stream failure
            }
        }

        client->Stop();
    }

    if (coInitialized) {
        CoUninitialize();
    }
    g_running.store(false);
}

int start_discord_process_loopback(DiscordAudioCallback callback, void* user_data) {
    if (g_running.load()) {
        return 0; // Already running
    }

    g_stop_requested.store(false);
    g_running.store(true);

    if (g_worker_thread.joinable()) {
        g_worker_thread.join();
    }

    g_worker_thread = std::thread(ProcessLoopbackWorker, callback, user_data);
    return 0;
}

void stop_discord_process_loopback() {
    if (!g_running.load()) return;

    g_stop_requested.store(true);
    if (g_worker_thread.joinable()) {
        g_worker_thread.join();
    }
    g_running.store(false);
}

bool is_discord_process_loopback_running() {
    return g_running.load();
}
