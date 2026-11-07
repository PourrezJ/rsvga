/*
 * d3dprobe: what Direct3D sees of the display adapter, reported to the
 * host through the VMware backdoor (GuestRPC "log" messages, which QEMU
 * traces with the rsvga vmport patch). It works even when the guest
 * shows nothing, which is the point: it explains why DWM cannot start.
 *
 * Every line also goes to stdout and to d3dprobe.log next to the program.
 *
 * Build: scripts/build-guest-tools.sh (mingw-w64).
 */
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d11.h>
#include <d3d9.h>
#include <dxgi1_6.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

/* ---- VMware backdoor: GuestRPC (RPCI channel) ------------------------ */

#define BDOOR_MAGIC 0x564D5868u
#define BDOOR_PORT 0x5658u
#define BDOOR_CMD_MESSAGE 30u
#define RPCI_PROTOCOL 0x49435052u
#define RPC_FLAG_COOKIE 0x80000000u
#define RPC_SUCCESS 0x0001u
#define RPC_DORECV 0x0002u

enum { RPC_OPEN, RPC_SENDSIZE, RPC_SENDPAYLOAD, RPC_RECVSIZE,
       RPC_RECVPAYLOAD, RPC_RECVSTATUS, RPC_CLOSE };

struct regs { uint32_t ax, bx, cx, dx, si, di; };

/* One backdoor call. Ring 3 is fine: the hypervisor traps the port. */
static void bdoor(struct regs *r)
{
    __asm__ __volatile__("inl %%dx, %%eax"
                         : "+a"(r->ax), "+b"(r->bx), "+c"(r->cx),
                           "+d"(r->dx), "+S"(r->si), "+D"(r->di)
                         :
                         : "memory");
}

static int have_backdoor = 1;

static void rpc_step(struct regs *r, uint32_t type, uint32_t channel,
                     uint32_t bx, uint32_t hi, uint32_t lo)
{
    r->ax = BDOOR_MAGIC;
    r->bx = bx;
    r->cx = BDOOR_CMD_MESSAGE | type << 16;
    r->dx = channel << 16 | BDOOR_PORT;
    r->si = hi;
    r->di = lo;
    bdoor(r);
}

/* Sends `msg` and discards the reply. Returns 0 if the host is absent. */
static int rpc_send(const char *msg)
{
    struct regs r = {0};
    uint32_t len = (uint32_t)strlen(msg), channel, hi, lo, i;

    if (!have_backdoor)
        return 0;
    rpc_step(&r, RPC_OPEN, 0, RPCI_PROTOCOL | RPC_FLAG_COOKIE, 0, 0);
    if (!(r.cx >> 16 & RPC_SUCCESS))
        return 0;
    channel = r.dx >> 16;
    hi = r.si;
    lo = r.di;
    rpc_step(&r, RPC_SENDSIZE, channel, len, hi, lo);
    for (i = 0; i < len; i += 4) {
        uint32_t word = 0;
        memcpy(&word, msg + i, len - i < 4 ? len - i : 4);
        rpc_step(&r, RPC_SENDPAYLOAD, channel, word, hi, lo);
    }
    rpc_step(&r, RPC_RECVSIZE, channel, 0, hi, lo);
    if (r.cx >> 16 & RPC_DORECV) {
        uint32_t size = r.bx;
        for (i = 0; i < size; i += 4)
            rpc_step(&r, RPC_RECVPAYLOAD, channel, RPC_SUCCESS, hi, lo);
        rpc_step(&r, RPC_RECVSTATUS, channel, RPC_SUCCESS, hi, lo);
    }
    rpc_step(&r, RPC_CLOSE, channel, 0, hi, lo);
    return 1;
}

/* ---- output ------------------------------------------------------------ */

static FILE *logfile;

static void say(const char *fmt, ...)
{
    char line[1024];
    char msg[1100];
    va_list ap;

    va_start(ap, fmt);
    vsnprintf(line, sizeof line, fmt, ap);
    va_end(ap);
    printf("%s\n", line);
    if (logfile) {
        fprintf(logfile, "%s\n", line);
        fflush(logfile);
    }
    snprintf(msg, sizeof msg, "log d3dprobe: %s", line);
    rpc_send(msg);
}

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

static void probe_d3d9(void)
{
    IDirect3D9 *d3d = Direct3DCreate9(D3D_SDK_VERSION);
    D3DCAPS9 caps;
    HRESULT hr;

    if (!d3d) {
        say("Direct3DCreate9 failed");
        return;
    }
    hr = IDirect3D9_GetDeviceCaps(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, &caps);
    say("D3D9 HAL caps: hr=0x%08lx vs=0x%08lx ps=0x%08lx max texture %lux%lu",
        (unsigned long)hr, (unsigned long)caps.VertexShaderVersion,
        (unsigned long)caps.PixelShaderVersion, (unsigned long)caps.MaxTextureWidth,
        (unsigned long)caps.MaxTextureHeight);
    IDirect3D9_Release(d3d);
}

static LONG WINAPI no_backdoor(EXCEPTION_POINTERS *e)
{
    /* A #GP on the backdoor port: not a VMware-compatible hypervisor. */
    if (e->ExceptionRecord->ExceptionCode == EXCEPTION_PRIV_INSTRUCTION ||
        e->ExceptionRecord->ExceptionCode == EXCEPTION_ACCESS_VIOLATION) {
        have_backdoor = 0;
        e->ContextRecord->Rip += 1; /* skip the one-byte `in` */
        return EXCEPTION_CONTINUE_EXECUTION;
    }
    return EXCEPTION_CONTINUE_SEARCH;
}

int main(void)
{
    char path[MAX_PATH];
    char *slash;
    OSVERSIONINFOW os;

    memset(&os, 0, sizeof os);
    os.dwOSVersionInfoSize = sizeof os;
    AddVectoredExceptionHandler(1, no_backdoor);
    GetModuleFileNameA(NULL, path, sizeof path);
    slash = strrchr(path, '\\');
    if (slash) {
        strcpy(slash + 1, "d3dprobe.log");
        logfile = fopen(path, "a");
    }
    GetVersionExW(&os);
    say("start: Windows %lu.%lu.%lu", (unsigned long)os.dwMajorVersion,
        (unsigned long)os.dwMinorVersion, (unsigned long)os.dwBuildNumber);
    probe_dxgi();
    probe_d3d11();
    probe_d3d9();
    say("done");
    if (logfile)
        fclose(logfile);
    return 0;
}
