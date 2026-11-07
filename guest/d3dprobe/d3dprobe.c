/*
 * d3dprobe: what Direct3D sees of the display adapter, reported to the
 * host through the VMware backdoor (GuestRPC "log" messages, which QEMU
 * traces with the rsvga vmport patch). It works even when the guest
 * shows nothing, which is the point: it explains why DWM cannot start.
 *
 * Every line also goes to stdout and to d3dprobe.log next to the program
 * (guest/common/hostlog.h).
 *
 * Build: scripts/build-guest-tools.sh (mingw-w64).
 */
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d11.h>
#include <d3d9.h>
#include <dxgi1_6.h>
#include <psapi.h>
#include <shlwapi.h>

#include "../common/hostlog.h"

static const char *utf8(const WCHAR *w)
{
    static char out[512];
    WideCharToMultiByte(CP_UTF8, 0, w, -1, out, sizeof out, NULL, NULL);
    return out;
}

static const char *level_name(D3D_FEATURE_LEVEL fl)
{
    switch (fl) {
    case D3D_FEATURE_LEVEL_9_1: return "9_1";
    case D3D_FEATURE_LEVEL_9_2: return "9_2";
    case D3D_FEATURE_LEVEL_9_3: return "9_3";
    case D3D_FEATURE_LEVEL_10_0: return "10_0";
    case D3D_FEATURE_LEVEL_10_1: return "10_1";
    case D3D_FEATURE_LEVEL_11_0: return "11_0";
    case D3D_FEATURE_LEVEL_11_1: return "11_1";
    case D3D_FEATURE_LEVEL_12_0: return "12_0";
    case D3D_FEATURE_LEVEL_12_1: return "12_1";
    default: return "?";
    }
}

/* ---- probes ------------------------------------------------------------ */

static IDXGIAdapter1 *adapters[8];
static UINT adapter_count;

static void probe_dxgi(void)
{
    IDXGIFactory1 *factory = NULL;
    HRESULT hr = CreateDXGIFactory1(&IID_IDXGIFactory1, (void **)&factory);
    UINT i;

    say("CreateDXGIFactory1: hr=0x%08lx", (unsigned long)hr);
    if (FAILED(hr))
        return;
    for (i = 0; i < 8; i++) {
        IDXGIAdapter1 *a = NULL;
        IDXGIAdapter4 *a4 = NULL;
        IDXGIOutput *out = NULL;
        DXGI_ADAPTER_DESC1 d;
        LARGE_INTEGER umd;
        UINT o;

        if (IDXGIFactory1_EnumAdapters1(factory, i, &a) != S_OK)
            break;
        adapters[adapter_count++] = a;
        IDXGIAdapter1_GetDesc1(a, &d);
        say("adapter %u: \"%s\" vendor=%04x device=%04x subsys=%08x rev=%u flags=0x%x",
            i, utf8(d.Description), d.VendorId, d.DeviceId, d.SubSysId,
            d.Revision, d.Flags);
        say("adapter %u: dedicated video %llu MiB, dedicated system %llu MiB, shared %llu MiB",
            i, (unsigned long long)d.DedicatedVideoMemory >> 20,
            (unsigned long long)d.DedicatedSystemMemory >> 20,
            (unsigned long long)d.SharedSystemMemory >> 20);
        hr = IDXGIAdapter1_CheckInterfaceSupport(a, &IID_IDXGIDevice, &umd);
        say("adapter %u: user-mode driver version: hr=0x%08lx %u.%u.%u.%u", i,
            (unsigned long)hr, HIWORD(umd.HighPart), LOWORD(umd.HighPart),
            HIWORD(umd.LowPart), LOWORD(umd.LowPart));
        if (SUCCEEDED(IDXGIAdapter1_QueryInterface(a, &IID_IDXGIAdapter4, (void **)&a4))) {
            DXGI_ADAPTER_DESC3 d3;
            IDXGIAdapter4_GetDesc3(a4, &d3);
            say("adapter %u: desc3 flags=0x%x graphics preemption=%u compute preemption=%u",
                i, d3.Flags, d3.GraphicsPreemptionGranularity,
                d3.ComputePreemptionGranularity);
            IDXGIAdapter4_Release(a4);
        }
        for (o = 0; IDXGIAdapter1_EnumOutputs(a, o, &out) == S_OK; o++) {
            DXGI_OUTPUT_DESC od;
            IDXGIOutput_GetDesc(out, &od);
            say("adapter %u output %u: %s attached=%d %ldx%ld", i, o,
                utf8(od.DeviceName), od.AttachedToDesktop,
                od.DesktopCoordinates.right - od.DesktopCoordinates.left,
                od.DesktopCoordinates.bottom - od.DesktopCoordinates.top);
            IDXGIOutput_Release(out);
        }
    }
    IDXGIFactory1_Release(factory);
}

static const D3D_FEATURE_LEVEL all_levels[] = {
    D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1,
    D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_9_3,  D3D_FEATURE_LEVEL_9_2,
    D3D_FEATURE_LEVEL_9_1,
};

static ID3D11Device *try_device(IDXGIAdapter *adapter, UINT flags,
                                const D3D_FEATURE_LEVEL *levels, UINT count,
                                const char *what)
{
    ID3D11Device *dev = NULL;
    ID3D11DeviceContext *ctx = NULL;
    D3D_FEATURE_LEVEL got = 0;
    HRESULT hr = D3D11CreateDevice(adapter, D3D_DRIVER_TYPE_UNKNOWN, NULL, flags,
                                   levels, count, D3D11_SDK_VERSION, &dev,
                                   &got, &ctx);

    say("D3D11CreateDevice(%s, flags=0x%x): hr=0x%08lx level=%s", what, flags,
        (unsigned long)hr, SUCCEEDED(hr) ? level_name(got) : "-");
    if (ctx)
        ID3D11DeviceContext_Release(ctx);
    return dev;
}

static void probe_formats(ID3D11Device *dev)
{
    static const struct { DXGI_FORMAT f; const char *name; } formats[] = {
        {DXGI_FORMAT_B8G8R8A8_UNORM, "B8G8R8A8_UNORM"},
        {DXGI_FORMAT_B8G8R8X8_UNORM, "B8G8R8X8_UNORM"},
        {DXGI_FORMAT_R8G8B8A8_UNORM, "R8G8B8A8_UNORM"},
        {DXGI_FORMAT_A8_UNORM, "A8_UNORM"},
        {DXGI_FORMAT_R16G16B16A16_FLOAT, "R16G16B16A16_FLOAT"},
        {DXGI_FORMAT_R10G10B10A2_UNORM, "R10G10B10A2_UNORM"},
        {DXGI_FORMAT_NV12, "NV12"},
        {DXGI_FORMAT_YUY2, "YUY2"},
        {DXGI_FORMAT_D24_UNORM_S8_UINT, "D24_UNORM_S8_UINT"},
        {DXGI_FORMAT_BC1_UNORM, "BC1_UNORM"},
    };
    UINT i;

    for (i = 0; i < sizeof formats / sizeof formats[0]; i++) {
        UINT support = 0;
        HRESULT hr = ID3D11Device_CheckFormatSupport(dev, formats[i].f, &support);
        say("format %s: hr=0x%08lx support=0x%08x", formats[i].name,
            (unsigned long)hr, support);
    }
}

/* Clears a small render target and reads it back. */
static void probe_render(ID3D11Device *dev)
{
    D3D11_TEXTURE2D_DESC td = {64, 64, 1, 1, DXGI_FORMAT_B8G8R8A8_UNORM, {1, 0},
                               D3D11_USAGE_DEFAULT, D3D11_BIND_RENDER_TARGET, 0, 0};
    ID3D11Texture2D *rt = NULL, *staging = NULL;
    ID3D11RenderTargetView *rtv = NULL;
    ID3D11DeviceContext *ctx = NULL;
    D3D11_MAPPED_SUBRESOURCE map;
    const float red[4] = {1, 0, 0, 1};
    HRESULT hr;
    int tries;

    ID3D11Device_GetImmediateContext(dev, &ctx);
    hr = ID3D11Device_CreateTexture2D(dev, &td, NULL, &rt);
    say("render: CreateTexture2D hr=0x%08lx", (unsigned long)hr);
    if (FAILED(hr))
        goto out;
    hr = ID3D11Device_CreateRenderTargetView(dev, (ID3D11Resource *)rt, NULL, &rtv);
    say("render: CreateRenderTargetView hr=0x%08lx", (unsigned long)hr);
    if (FAILED(hr))
        goto out;
    td.Usage = D3D11_USAGE_STAGING;
    td.BindFlags = 0;
    td.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    hr = ID3D11Device_CreateTexture2D(dev, &td, NULL, &staging);
    say("render: staging hr=0x%08lx", (unsigned long)hr);
    if (FAILED(hr))
        goto out;
    ID3D11DeviceContext_ClearRenderTargetView(ctx, rtv, red);
    ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)staging, (ID3D11Resource *)rt);
    ID3D11DeviceContext_Flush(ctx);
    for (tries = 0; tries < 200; tries++) {
        hr = ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)staging, 0, D3D11_MAP_READ,
                                     D3D11_MAP_FLAG_DO_NOT_WAIT, &map);
        if (hr != DXGI_ERROR_WAS_STILL_DRAWING)
            break;
        Sleep(10);
    }
    say("render: Map hr=0x%08lx after %d tries", (unsigned long)hr, tries);
    if (SUCCEEDED(hr)) {
        say("render: pixel 0 = 0x%08x (red is 0xffff0000)", *(UINT *)map.pData);
        ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)staging, 0);
    }
    say("render: device removed reason 0x%08lx",
        (unsigned long)ID3D11Device_GetDeviceRemovedReason(dev));
out:
    if (staging)
        ID3D11Texture2D_Release(staging);
    if (rtv)
        ID3D11RenderTargetView_Release(rtv);
    if (rt)
        ID3D11Texture2D_Release(rt);
    if (ctx)
        ID3D11DeviceContext_Release(ctx);
}

static void probe_d3d11(void)
{
    static const UINT flag_sets[] = {
        0,
        D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
        D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_SINGLETHREADED,
    };
    ID3D11Device *keep = NULL;
    IDXGIAdapter *a;
    UINT i;

    if (adapter_count == 0) {
        say("no DXGI adapter");
        return;
    }
    a = (IDXGIAdapter *)adapters[0];
    for (i = 0; i < sizeof flag_sets / sizeof flag_sets[0]; i++) {
        ID3D11Device *dev = try_device(a, flag_sets[i], all_levels,
                                       sizeof all_levels / sizeof all_levels[0],
                                       "adapter 0, 11_1..9_1");
        if (dev && !keep)
            keep = dev;
        else if (dev)
            ID3D11Device_Release(dev);
    }
    for (i = 0; i < sizeof all_levels / sizeof all_levels[0]; i++) {
        ID3D11Device *dev = try_device(a, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                                       &all_levels[i], 1, level_name(all_levels[i]));
        if (dev)
            ID3D11Device_Release(dev);
    }
    if (keep) {
        probe_formats(keep);
        probe_render(keep);
        ID3D11Device_Release(keep);
    }
}

/* The modules of this process whose name contains one of `parts`. */
static void list_modules(const char *when)
{
    HMODULE modules[512];
    DWORD bytes = 0;
    UINT i;

    if (!EnumProcessModules(GetCurrentProcess(), modules, sizeof modules, &bytes))
        return;
    for (i = 0; i < bytes / sizeof modules[0] && i < 512; i++) {
        char name[MAX_PATH];
        if (!GetModuleFileNameA(modules[i], name, sizeof name))
            continue;
        if (strstr(name, "vm3d") || StrStrIA(name, "d3d9") || StrStrIA(name, "d3d10") ||
            StrStrIA(name, "d3d11") || StrStrIA(name, "dxgi"))
            say("modules %s: %s", when, name);
    }
}

static LRESULT CALLBACK plain_window(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp)
{
    return DefWindowProcA(hwnd, msg, wp, lp);
}

static void probe_d3d9(void)
{
    IDirect3D9 *d3d = Direct3DCreate9(D3D_SDK_VERSION);
    IDirect3D9Ex *d3d_ex = NULL;
    IDirect3DDevice9 *device = NULL;
    D3DADAPTER_IDENTIFIER9 id;
    D3DPRESENT_PARAMETERS pp;
    D3DCAPS9 caps;
    WNDCLASSA wc;
    HWND hwnd;
    HRESULT hr;

    if (!d3d) {
        say("Direct3DCreate9 failed");
        return;
    }
    say("D3D9 adapters: %u", IDirect3D9_GetAdapterCount(d3d));
    memset(&id, 0, sizeof id);
    hr = IDirect3D9_GetAdapterIdentifier(d3d, D3DADAPTER_DEFAULT, 0, &id);
    say("D3D9 adapter: hr=0x%08lx driver \"%s\" \"%s\" version %u.%u.%u.%u vendor=%04lx device=%04lx",
        (unsigned long)hr, id.Driver, id.Description, HIWORD(id.DriverVersion.HighPart),
        LOWORD(id.DriverVersion.HighPart), HIWORD(id.DriverVersion.LowPart),
        LOWORD(id.DriverVersion.LowPart), (unsigned long)id.VendorId,
        (unsigned long)id.DeviceId);
    memset(&caps, 0, sizeof caps);
    hr = IDirect3D9_GetDeviceCaps(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, &caps);
    say("D3D9 HAL caps: hr=0x%08lx vs=0x%08lx ps=0x%08lx max texture %lux%lu",
        (unsigned long)hr, (unsigned long)caps.VertexShaderVersion,
        (unsigned long)caps.PixelShaderVersion, (unsigned long)caps.MaxTextureWidth,
        (unsigned long)caps.MaxTextureHeight);
    memset(&wc, 0, sizeof wc);
    wc.lpfnWndProc = plain_window;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.lpszClassName = "d3dprobe";
    RegisterClassA(&wc);
    hwnd = CreateWindowA("d3dprobe", "d3dprobe", WS_OVERLAPPEDWINDOW, 0, 0, 64, 64, NULL, NULL,
                         wc.hInstance, NULL);
    memset(&pp, 0, sizeof pp);
    pp.Windowed = TRUE;
    pp.SwapEffect = D3DSWAPEFFECT_DISCARD;
    pp.BackBufferFormat = D3DFMT_UNKNOWN;
    pp.hDeviceWindow = hwnd;
    hr = IDirect3D9_CreateDevice(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd,
                                 D3DCREATE_HARDWARE_VERTEXPROCESSING, &pp, &device);
    say("D3D9 CreateDevice (HAL, hardware vertices): hr=0x%08lx", (unsigned long)hr);
    if (device)
        IDirect3DDevice9_Release(device);
    list_modules("after D3D9");
    IDirect3D9_Release(d3d);
    hr = Direct3DCreate9Ex(D3D_SDK_VERSION, &d3d_ex);
    if (SUCCEEDED(hr)) {
        memset(&caps, 0, sizeof caps);
        hr = IDirect3D9Ex_GetDeviceCaps(d3d_ex, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, &caps);
        say("D3D9Ex HAL caps: hr=0x%08lx vs=0x%08lx ps=0x%08lx", (unsigned long)hr,
            (unsigned long)caps.VertexShaderVersion, (unsigned long)caps.PixelShaderVersion);
        IDirect3D9Ex_Release(d3d_ex);
    } else {
        say("Direct3DCreate9Ex: hr=0x%08lx", (unsigned long)hr);
    }
    if (hwnd)
        DestroyWindow(hwnd);
}

int main(void)
{
    OSVERSIONINFOW os;

    memset(&os, 0, sizeof os);
    os.dwOSVersionInfoSize = sizeof os;
    log_open("d3dprobe");
    log_debug_output();
    GetVersionExW(&os);
    say("start: Windows %lu.%lu.%lu", (unsigned long)os.dwMajorVersion,
        (unsigned long)os.dwMinorVersion, (unsigned long)os.dwBuildNumber);
    probe_dxgi();
    probe_d3d11();
    probe_d3d9();
    say("done");
    log_close();
    return 0;
}
